use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use alloy::primitives::{Address, B256, U256};
use eyre::{Result, bail};
use matching_engine::MatchingEngine;
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
    pub pending_trades: Vec<Trade>,
    pub domain_separator: B256,
    pub base_token: Address,
    pub quote_token: Address,
    pub ws_registry: Arc<RwLock<WsRegistry>>,
    pub batch_size: usize,
    pub batch_timeout_secs: u64,
}

impl AppState {
    pub fn new(
        chain_id: u64,
        contract_address: Address,
        base_token: Address,
        quote_token: Address,
        ws_registry: Arc<RwLock<WsRegistry>>,
    ) -> Arc<Mutex<Self>> {
        let state = Self {
            engine: MatchingEngine::new(),
            ledger: Ledger::new(),
            accepted_nonces: HashSet::new(),
            order_map: HashMap::new(),
            pending_trades: Vec::new(),
            domain_separator: compute_domain_separator(chain_id, contract_address),
            base_token,
            quote_token,
            ws_registry,
            batch_size: 10,
            batch_timeout_secs: 5,
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
        let mut remaining = order.quantity;

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
            self.pending_trades.push(trade);

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
            self.order_map.insert(order_id, order);
        }

        Ok((order_id, client_fills, dispatches))
    }

    pub fn drain_batch(&mut self) -> Vec<Trade> {
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
    use types::Side;

    fn setup_state() -> AppState {
        let registry = Arc::new(RwLock::new(crate::ws_registry::WsRegistry::new()));
        let state = AppState::new(
            31337,
            Address::with_last_byte(99),
            Address::with_last_byte(1),
            Address::with_last_byte(2),
            registry,
        );
        Arc::try_unwrap(state)
            .ok()
            .expect("single Arc reference")
            .into_inner()
    }

    async fn make_signed_order(price: U256, quantity: U256, side: Side) -> (SignedOrder, AppState) {
        let signer = PrivateKeySigner::random();
        let address = signer.address();
        let mut state = setup_state();

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

        let sol_order = crate::eip712::to_sol_order(&order);
        use alloy::sol_types::SolStruct;
        let struct_hash = sol_order.eip712_hash_struct();
        let digest = alloy::primitives::keccak256(
            [
                &[0x19, 0x01],
                state.domain_separator.as_slice(),
                struct_hash.as_slice(),
            ]
            .concat(),
        );
        let sig = signer.sign_hash(&digest).await.unwrap();
        let signed = SignedOrder {
            signature: sig.as_bytes().to_vec().into(),
            ..order
        };

        (signed, state)
    }

    #[tokio::test]
    async fn market_order_rejects_zero_price() {
        let (order, mut state) = make_signed_order(U256::ZERO, U256::from(5), Side::Buy).await;
        let result = state.submit_order(order, OrderType::Market);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("price"));
    }

    #[tokio::test]
    async fn market_order_rolls_back_on_no_fill() {
        let (order, mut state) =
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
        let mut state = setup_state();

        let fund_amount = U256::from(1_000_000) * U256::from(10).pow(U256::from(18));
        state
            .ledger
            .credit(signer_maker.address(), state.base_token, fund_amount);
        state
            .ledger
            .credit(signer_taker.address(), state.quote_token, fund_amount);

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
}
