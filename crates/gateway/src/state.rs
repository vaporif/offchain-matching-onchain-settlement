use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use alloy::primitives::{Address, B256, U256};
use eyre::{Result, bail};
use matching_engine::MatchingEngine;
use persistence::Db;
use tokio::sync::{Mutex, RwLock};
use types::{Fill, OrderId, OrderType, Side, SignedOrder, Trade};

use crate::{
    eip712::{compute_domain_separator, order_hash, recover_signer},
    ledger::Ledger,
    ws_registry::WsRegistry,
};

#[derive(Debug, Clone, serde::Serialize)]
pub struct WsMessage {
    #[serde(rename = "type")]
    pub msg_type: String,
    pub data: serde_json::Value,
}

#[derive(Debug)]
pub struct FillDispatch {
    pub address: Address,
    pub message: WsMessage,
}

pub struct AppState {
    pub engine: MatchingEngine,
    pub ledger: Ledger,
    pub accepted_nonces: HashSet<B256>,
    pub order_map: HashMap<OrderId, SignedOrder>,
    pub order_nonce_map: HashMap<OrderId, (Address, U256, Address, U256)>,
    pub pending_trades: Vec<(i64, Trade)>,
    pub domain_separator: B256,
    pub base_token: Address,
    pub quote_token: Address,
    pub ws_registry: Arc<RwLock<WsRegistry>>,
    pub batch_size: usize,
    pub batch_timeout_secs: u64,
    pub db: Arc<Db>,
    pub nonce_expiry_secs: u64,
}

impl AppState {
    pub fn new(
        chain_id: u64,
        contract_address: Address,
        base_token: Address,
        quote_token: Address,
        ws_registry: Arc<RwLock<WsRegistry>>,
        db: Arc<Db>,
    ) -> Arc<Mutex<Self>> {
        let state = Self {
            engine: MatchingEngine::new(),
            ledger: Ledger::new(),
            accepted_nonces: HashSet::new(),
            order_map: HashMap::new(),
            order_nonce_map: HashMap::new(),
            pending_trades: Vec::new(),
            domain_separator: compute_domain_separator(chain_id, contract_address),
            base_token,
            quote_token,
            ws_registry,
            batch_size: 10,
            batch_timeout_secs: 5,
            db,
            nonce_expiry_secs: 86400,
        };
        Arc::new(Mutex::new(state))
    }

    pub fn submit_order(
        &mut self,
        order: SignedOrder,
        order_type: OrderType,
    ) -> Result<(OrderId, Vec<Fill>, Vec<FillDispatch>)> {
        if order_type == OrderType::Market && order.price == U256::ZERO {
            bail!("market order requires price > 0 for price protection");
        }

        let signer = recover_signer(&order, self.domain_separator)?;
        if signer != order.maker {
            bail!("signature does not match maker");
        }

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before epoch")
            .as_secs();
        if order.expiry < U256::from(now) {
            bail!("order expired");
        }

        let hash = order_hash(&order);
        if self.accepted_nonces.contains(&hash) {
            bail!("duplicate nonce");
        }

        let required = collateral_required(&order);
        let collateral_token = match order.side {
            Side::Buy => order.quote_token,
            Side::Sell => order.base_token,
        };
        if !self.ledger.reserve(order.maker, collateral_token, required) {
            bail!("insufficient balance");
        }

        self.accepted_nonces.insert(hash);

        let result = self
            .engine
            .submit(order.side, order.price, order.quantity, order_type);

        if order_type == OrderType::Market && result.fills.is_empty() {
            self.accepted_nonces.remove(&hash);
            self.ledger.release(order.maker, collateral_token, required);
            bail!("market order not fillable");
        }

        let order_id = self.engine.last_order_id();

        let mut client_fills = Vec::with_capacity(result.fills.len());
        let mut dispatches = Vec::with_capacity(result.fills.len() * 2);
        let mut trades = Vec::with_capacity(result.fills.len());
        let mut remaining = order.quantity;
        let e18 = U256::from(10).pow(U256::from(18));

        for engine_fill in &result.fills {
            remaining -= engine_fill.quantity;

            let maker_signed = self
                .order_map
                .get(&engine_fill.maker_id)
                .expect("maker order must exist in order_map")
                .clone();

            let trade = Trade {
                maker_order: maker_signed.clone(),
                taker_order: order.clone(),
                price: engine_fill.price,
                quantity: engine_fill.quantity,
                timestamp: now,
            };
            trades.push(trade);

            let base_amount = engine_fill.quantity;
            let quote_amount = (base_amount * engine_fill.price) / e18;

            let (seller, buyer) = match order.side {
                Side::Buy => (maker_signed.maker, order.maker),
                Side::Sell => (order.maker, maker_signed.maker),
            };

            self.ledger.settle_fill(
                seller,
                buyer,
                self.base_token,
                self.quote_token,
                base_amount,
                quote_amount,
            );

            // Buy taker reserved collateral at their limit price, but the fill
            // executes at the maker's (lower) price. Release the difference.
            if order.side == Side::Buy && engine_fill.price < order.price {
                let surplus = (base_amount * (order.price - engine_fill.price)) / e18;
                if surplus > U256::ZERO {
                    self.ledger.release(order.maker, self.quote_token, surplus);
                }
            }

            let maker_fill = Fill {
                order_id: engine_fill.maker_id,
                price: engine_fill.price,
                filled_qty: engine_fill.quantity,
                remaining_qty: U256::ZERO,
                is_maker: true,
            };

            let taker_fill = Fill {
                order_id,
                price: engine_fill.price,
                filled_qty: engine_fill.quantity,
                remaining_qty: remaining,
                is_maker: false,
            };

            dispatches.push(FillDispatch {
                address: maker_signed.maker,
                message: WsMessage {
                    msg_type: "fill".into(),
                    data: serde_json::to_value(&maker_fill).expect("Fill serializes"),
                },
            });
            dispatches.push(FillDispatch {
                address: order.maker,
                message: WsMessage {
                    msg_type: "fill".into(),
                    data: serde_json::to_value(&taker_fill).expect("Fill serializes"),
                },
            });

            client_fills.push(taker_fill);
        }

        if result.resting {
            self.order_map.insert(order_id, order.clone());
            let collateral_token = match order.side {
                Side::Buy => self.quote_token,
                Side::Sell => self.base_token,
            };
            let resting_collateral = match order.side {
                Side::Buy => {
                    (result.resting_qty * order.price) / U256::from(10).pow(U256::from(18))
                }
                Side::Sell => result.resting_qty,
            };
            self.order_nonce_map.insert(
                order_id,
                (
                    order.maker,
                    order.nonce,
                    collateral_token,
                    resting_collateral,
                ),
            );
        }

        // Persist atomically: nonce, trades, balances, filled makers, resting order
        let balance_updates = self.ledger.snapshot();
        let filled_maker_ids: Vec<u64> = result
            .fills
            .iter()
            .map(|f| f.maker_id)
            .filter(|id| !self.engine.has_order(*id))
            .collect();
        let resting_order = if result.resting {
            Some((order_id, order.maker, &order, result.resting_qty))
        } else {
            None
        };
        let nonce_expires_at = now + self.nonce_expiry_secs;

        match self.db.save_order_fill(
            hash,
            nonce_expires_at,
            &trades,
            &balance_updates,
            &filled_maker_ids,
            resting_order,
        ) {
            Ok(trade_ids) => {
                for (id, trade) in trade_ids.into_iter().zip(trades) {
                    self.pending_trades.push((id, trade));
                }
            }
            Err(e) => {
                tracing::error!(%e, "fatal: failed to persist order fill, exiting");
                std::process::exit(1);
            }
        }

        Ok((order_id, client_fills, dispatches))
    }

    pub fn cancel_order(
        &mut self,
        nonce: U256,
        signature: &alloy::primitives::Bytes,
    ) -> Result<(u64, Address)> {
        let signer = crate::eip712::recover_cancel_signer(nonce, signature, self.domain_separator)?;

        let (&order_id, &(maker, _nonce, collateral_token, collateral_amount)) = self
            .order_nonce_map
            .iter()
            .find(|(_, (_, n, _, _))| *n == nonce)
            .ok_or_else(|| eyre::eyre!("no resting order with nonce {nonce}"))?;

        if signer != maker {
            bail!("signer does not match order maker");
        }

        let removed = self.engine.cancel(order_id);
        if !removed {
            bail!("order not found in book");
        }

        self.ledger
            .release(maker, collateral_token, collateral_amount);

        let balance_updates = self.ledger.snapshot();

        self.db
            .cancel_order(order_id, &balance_updates, (maker, &nonce))?;

        self.order_map.remove(&order_id);
        self.order_nonce_map.remove(&order_id);

        Ok((order_id, maker))
    }

    pub fn drain_batch(&mut self) -> Vec<(i64, Trade)> {
        std::mem::take(&mut self.pending_trades)
    }
}

fn collateral_required(order: &SignedOrder) -> U256 {
    match order.side {
        Side::Buy => (order.quantity * order.price) / U256::from(10).pow(U256::from(18)),
        Side::Sell => order.quantity,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::Bytes;
    use alloy::signers::{Signer, local::PrivateKeySigner};
    use tempfile::TempDir;
    use types::Side;

    fn setup_state() -> (TempDir, AppState) {
        let dir = TempDir::new().expect("create temp dir");
        let db = Arc::new(Db::open(&dir.path().join("test.db")).expect("open test db"));
        let registry = Arc::new(RwLock::new(crate::ws_registry::WsRegistry::new()));
        let state = AppState::new(
            31337,
            Address::with_last_byte(99),
            Address::with_last_byte(1),
            Address::with_last_byte(2),
            registry,
            db,
        );
        let inner = Arc::try_unwrap(state)
            .ok()
            .expect("single Arc reference")
            .into_inner();
        (dir, inner)
    }

    async fn sign_order(
        order: &SignedOrder,
        signer: &PrivateKeySigner,
        domain: B256,
    ) -> SignedOrder {
        use alloy::sol_types::SolStruct;
        let sol_order = crate::eip712::to_sol_order(order);
        let struct_hash = sol_order.eip712_hash_struct();
        let digest = alloy::primitives::keccak256(
            [&[0x19, 0x01], domain.as_slice(), struct_hash.as_slice()].concat(),
        );
        let sig = signer.sign_hash(&digest).await.unwrap();
        SignedOrder {
            signature: sig.as_bytes().to_vec().into(),
            ..order.clone()
        }
    }

    async fn make_signed_order(
        price: U256,
        quantity: U256,
        side: Side,
    ) -> (SignedOrder, TempDir, AppState) {
        let signer = PrivateKeySigner::random();
        let address = signer.address();
        let (_dir, mut state) = setup_state();

        let token = match side {
            Side::Buy => state.quote_token,
            Side::Sell => state.base_token,
        };
        let amount = U256::from(1_000_000) * U256::from(10).pow(U256::from(18));
        state.ledger.credit(address, token, amount);

        let order = SignedOrder {
            side,
            maker: address,
            base_token: state.base_token,
            quote_token: state.quote_token,
            price,
            quantity,
            nonce: U256::from(1),
            expiry: U256::from(u64::MAX),
            signature: Bytes::new(),
        };

        let signed = sign_order(&order, &signer, state.domain_separator).await;

        (signed, _dir, state)
    }

    #[tokio::test]
    async fn market_order_rejects_zero_price() {
        let (order, _dir, mut state) =
            make_signed_order(U256::ZERO, U256::from(5), Side::Buy).await;
        let result = state.submit_order(order, OrderType::Market);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("price"));
    }

    #[tokio::test]
    async fn market_order_rolls_back_on_no_fill() {
        let (order, _dir, mut state) =
            make_signed_order(U256::from(1000), U256::from(5), Side::Buy).await;

        let hash = crate::eip712::order_hash(&order);
        let result = state.submit_order(order.clone(), OrderType::Market);

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not fillable"));
        assert!(!state.accepted_nonces.contains(&hash));

        let token = state.quote_token;
        let available = state.ledger.available(order.maker, token);
        let expected = U256::from(1_000_000) * U256::from(10).pow(U256::from(18));
        assert_eq!(available, expected);
    }

    #[tokio::test]
    async fn market_order_fills_against_resting_limit() {
        let signer_maker = PrivateKeySigner::random();
        let signer_taker = PrivateKeySigner::random();
        let (_dir, mut state) = setup_state();

        let fund_amount = U256::from(1_000_000) * U256::from(10).pow(U256::from(18));
        state
            .ledger
            .credit(signer_maker.address(), state.base_token, fund_amount);
        state
            .ledger
            .credit(signer_taker.address(), state.quote_token, fund_amount);

        let sell = SignedOrder {
            side: Side::Sell,
            maker: signer_maker.address(),
            base_token: state.base_token,
            quote_token: state.quote_token,
            price: U256::from(100),
            quantity: U256::from(5),
            nonce: U256::from(1),
            expiry: U256::from(u64::MAX),
            signature: Bytes::new(),
        };
        let sell_signed = sign_order(&sell, &signer_maker, state.domain_separator).await;
        let (_sell_id, _, _) = state.submit_order(sell_signed, OrderType::Limit).unwrap();

        let buy = SignedOrder {
            side: Side::Buy,
            maker: signer_taker.address(),
            base_token: state.base_token,
            quote_token: state.quote_token,
            price: U256::from(100),
            quantity: U256::from(5),
            nonce: U256::from(2),
            expiry: U256::from(u64::MAX),
            signature: Bytes::new(),
        };
        let buy_signed = sign_order(&buy, &signer_taker, state.domain_separator).await;
        let (_, fills, _) = state.submit_order(buy_signed, OrderType::Market).unwrap();

        assert_eq!(fills.len(), 1);
        assert_eq!(fills[0].filled_qty, U256::from(5));
        assert_eq!(fills[0].price, U256::from(100));
    }

    #[tokio::test]
    async fn settle_fill_transfers_balances_on_match() {
        let signer_maker = PrivateKeySigner::random();
        let signer_taker = PrivateKeySigner::random();
        let (_dir, mut state) = setup_state();

        let e18 = U256::from(10).pow(U256::from(18));
        let fund = U256::from(1000) * e18;
        state
            .ledger
            .credit(signer_maker.address(), state.base_token, fund);
        state
            .ledger
            .credit(signer_taker.address(), state.quote_token, fund);

        let price = U256::from(2) * e18;
        let quantity = U256::from(10) * e18;

        let sell = SignedOrder {
            side: Side::Sell,
            maker: signer_maker.address(),
            base_token: state.base_token,
            quote_token: state.quote_token,
            price,
            quantity,
            nonce: U256::from(1),
            expiry: U256::from(u64::MAX),
            signature: Bytes::new(),
        };
        let sell_signed = sign_order(&sell, &signer_maker, state.domain_separator).await;
        state.submit_order(sell_signed, OrderType::Limit).unwrap();

        let buy = SignedOrder {
            side: Side::Buy,
            maker: signer_taker.address(),
            base_token: state.base_token,
            quote_token: state.quote_token,
            price,
            quantity,
            nonce: U256::from(2),
            expiry: U256::from(u64::MAX),
            signature: Bytes::new(),
        };
        let buy_signed = sign_order(&buy, &signer_taker, state.domain_separator).await;
        let (_, fills, _) = state.submit_order(buy_signed, OrderType::Limit).unwrap();

        assert_eq!(fills.len(), 1);

        let expected_quote = (quantity * price) / e18;

        // Maker sold base, received quote
        assert_eq!(
            state
                .ledger
                .available(signer_maker.address(), state.base_token),
            fund - quantity
        );
        assert_eq!(
            state
                .ledger
                .available(signer_maker.address(), state.quote_token),
            expected_quote
        );

        // Taker bought base, paid quote
        assert_eq!(
            state
                .ledger
                .available(signer_taker.address(), state.base_token),
            quantity
        );
        assert_eq!(
            state
                .ledger
                .available(signer_taker.address(), state.quote_token),
            fund - expected_quote
        );

        // No reserved balance lingering
        assert_eq!(
            state.ledger.total(signer_maker.address(), state.base_token),
            fund - quantity
        );
        assert_eq!(
            state
                .ledger
                .total(signer_taker.address(), state.quote_token),
            fund - expected_quote
        );
    }

    async fn sign_cancel(nonce: U256, signer: &PrivateKeySigner, domain: B256) -> Bytes {
        let struct_hash = crate::eip712::cancel_order_hash(nonce);
        let digest = alloy::primitives::keccak256(
            [&[0x19, 0x01], domain.as_slice(), struct_hash.as_slice()].concat(),
        );
        let sig = signer.sign_hash(&digest).await.unwrap();
        sig.as_bytes().to_vec().into()
    }

    #[tokio::test]
    async fn cancel_resting_order_releases_collateral() {
        let signer = PrivateKeySigner::random();
        let (_dir, mut state) = setup_state();

        let e18 = U256::from(10).pow(U256::from(18));
        let fund = U256::from(1000) * e18;
        state
            .ledger
            .credit(signer.address(), state.base_token, fund);

        let price = U256::from(2) * e18;
        let quantity = U256::from(10) * e18;
        let nonce = U256::from(1);

        let sell = SignedOrder {
            side: Side::Sell,
            maker: signer.address(),
            base_token: state.base_token,
            quote_token: state.quote_token,
            price,
            quantity,
            nonce,
            expiry: U256::from(u64::MAX),
            signature: Bytes::new(),
        };
        let sell_signed = sign_order(&sell, &signer, state.domain_separator).await;
        state.submit_order(sell_signed, OrderType::Limit).unwrap();

        // Collateral is reserved
        assert_eq!(
            state.ledger.available(signer.address(), state.base_token),
            fund - quantity
        );

        let cancel_sig = sign_cancel(nonce, &signer, state.domain_separator).await;
        let (order_id, maker) = state.cancel_order(nonce, &cancel_sig).unwrap();
        assert_eq!(maker, signer.address());
        assert!(order_id > 0);

        // Collateral released
        assert_eq!(
            state.ledger.available(signer.address(), state.base_token),
            fund
        );
        assert!(!state.order_map.contains_key(&order_id));
        assert!(!state.order_nonce_map.contains_key(&order_id));
    }

    #[tokio::test]
    async fn cancel_wrong_signer_fails() {
        let maker_signer = PrivateKeySigner::random();
        let impersonator = PrivateKeySigner::random();
        let (_dir, mut state) = setup_state();

        let e18 = U256::from(10).pow(U256::from(18));
        let fund = U256::from(1000) * e18;
        state
            .ledger
            .credit(maker_signer.address(), state.base_token, fund);

        let nonce = U256::from(1);
        let sell = SignedOrder {
            side: Side::Sell,
            maker: maker_signer.address(),
            base_token: state.base_token,
            quote_token: state.quote_token,
            price: U256::from(100) * e18,
            quantity: U256::from(10) * e18,
            nonce,
            expiry: U256::from(u64::MAX),
            signature: Bytes::new(),
        };
        let sell_signed = sign_order(&sell, &maker_signer, state.domain_separator).await;
        state.submit_order(sell_signed, OrderType::Limit).unwrap();

        let cancel_sig = sign_cancel(nonce, &impersonator, state.domain_separator).await;
        let result = state.cancel_order(nonce, &cancel_sig);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("does not match"));
    }

    #[tokio::test]
    async fn cancel_nonexistent_nonce_fails() {
        let signer = PrivateKeySigner::random();
        let (_dir, mut state) = setup_state();

        let nonce = U256::from(999);
        let cancel_sig = sign_cancel(nonce, &signer, state.domain_separator).await;
        let result = state.cancel_order(nonce, &cancel_sig);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("no resting order"));
    }

    #[tokio::test]
    async fn cancel_buy_order_releases_quote_collateral() {
        let signer = PrivateKeySigner::random();
        let (_dir, mut state) = setup_state();

        let e18 = U256::from(10).pow(U256::from(18));
        let fund = U256::from(1000) * e18;
        state
            .ledger
            .credit(signer.address(), state.quote_token, fund);

        let price = U256::from(5) * e18;
        let quantity = U256::from(10) * e18;
        let nonce = U256::from(1);

        let buy = SignedOrder {
            side: Side::Buy,
            maker: signer.address(),
            base_token: state.base_token,
            quote_token: state.quote_token,
            price,
            quantity,
            nonce,
            expiry: U256::from(u64::MAX),
            signature: Bytes::new(),
        };
        let buy_signed = sign_order(&buy, &signer, state.domain_separator).await;
        state.submit_order(buy_signed, OrderType::Limit).unwrap();

        let expected_collateral = (quantity * price) / e18;
        assert_eq!(
            state.ledger.available(signer.address(), state.quote_token),
            fund - expected_collateral
        );

        let cancel_sig = sign_cancel(nonce, &signer, state.domain_separator).await;
        state.cancel_order(nonce, &cancel_sig).unwrap();

        assert_eq!(
            state.ledger.available(signer.address(), state.quote_token),
            fund
        );
    }

    #[tokio::test]
    async fn price_improvement_releases_surplus() {
        let signer_maker = PrivateKeySigner::random();
        let signer_taker = PrivateKeySigner::random();
        let (_dir, mut state) = setup_state();

        let e18 = U256::from(10).pow(U256::from(18));
        let fund = U256::from(1000) * e18;
        state
            .ledger
            .credit(signer_maker.address(), state.base_token, fund);
        state
            .ledger
            .credit(signer_taker.address(), state.quote_token, fund);

        let maker_price = U256::from(2) * e18;
        let taker_price = U256::from(3) * e18;
        let quantity = U256::from(10) * e18;

        let sell = SignedOrder {
            side: Side::Sell,
            maker: signer_maker.address(),
            base_token: state.base_token,
            quote_token: state.quote_token,
            price: maker_price,
            quantity,
            nonce: U256::from(1),
            expiry: U256::from(u64::MAX),
            signature: Bytes::new(),
        };
        let sell_signed = sign_order(&sell, &signer_maker, state.domain_separator).await;
        state.submit_order(sell_signed, OrderType::Limit).unwrap();

        let buy = SignedOrder {
            side: Side::Buy,
            maker: signer_taker.address(),
            base_token: state.base_token,
            quote_token: state.quote_token,
            price: taker_price,
            quantity,
            nonce: U256::from(2),
            expiry: U256::from(u64::MAX),
            signature: Bytes::new(),
        };
        let buy_signed = sign_order(&buy, &signer_taker, state.domain_separator).await;
        let (_, fills, _) = state.submit_order(buy_signed, OrderType::Limit).unwrap();

        assert_eq!(fills.len(), 1);
        assert_eq!(fills[0].price, maker_price);

        let actual_cost = (quantity * maker_price) / e18;

        // Taker's quote: fund - actual_cost (surplus released)
        assert_eq!(
            state
                .ledger
                .available(signer_taker.address(), state.quote_token),
            fund - actual_cost
        );
        // No quote stuck in reserved
        assert_eq!(
            state
                .ledger
                .total(signer_taker.address(), state.quote_token),
            fund - actual_cost
        );
    }
}
