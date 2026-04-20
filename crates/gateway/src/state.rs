use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use alloy::primitives::{Address, B256, U256};
use eyre::{Result, bail};
use matching_engine::MatchingEngine;
use tokio::sync::{Mutex, broadcast};
use types::{Fill, OrderId, OrderType, Side, SignedOrder, Trade};

use crate::{
    eip712::{compute_domain_separator, order_hash, recover_signer},
    ledger::Ledger,
};

#[derive(Debug, Clone, serde::Serialize)]
pub struct WsMessage {
    #[serde(rename = "type")]
    pub msg_type: String,
    pub data: serde_json::Value,
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
    pub ws_tx: broadcast::Sender<WsMessage>,
    pub batch_size: usize,
    pub batch_timeout_secs: u64,
}

impl AppState {
    pub fn new(
        chain_id: u64,
        contract_address: Address,
        base_token: Address,
        quote_token: Address,
    ) -> (Arc<Mutex<Self>>, broadcast::Receiver<WsMessage>) {
        let (ws_tx, ws_rx) = broadcast::channel(1024);
        let state = Self {
            engine: MatchingEngine::new(),
            ledger: Ledger::new(),
            accepted_nonces: HashSet::new(),
            order_map: HashMap::new(),
            pending_trades: Vec::new(),
            domain_separator: compute_domain_separator(chain_id, contract_address),
            base_token,
            quote_token,
            ws_tx,
            batch_size: 10,
            batch_timeout_secs: 5,
        };
        (Arc::new(Mutex::new(state)), ws_rx)
    }

    pub fn submit_order(&mut self, order: SignedOrder) -> Result<(OrderId, Vec<Fill>)> {
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
            .submit(order.side, order.price, order.quantity, OrderType::Limit);

        let order_id = self.engine.last_order_id();
        self.order_map.insert(order_id, order.clone());

        let mut client_fills = Vec::new();
        let mut remaining = order.quantity;

        for engine_fill in &result.fills {
            remaining -= engine_fill.quantity;

            let maker_signed = self
                .order_map
                .get(&engine_fill.maker_id)
                .expect("maker order must exist in order_map")
                .clone();

            let trade = Trade {
                maker_order: maker_signed,
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

            let _ = self.ws_tx.send(WsMessage {
                msg_type: "fill".into(),
                data: serde_json::to_value(&maker_fill).expect("Fill serializes"),
            });
            let _ = self.ws_tx.send(WsMessage {
                msg_type: "fill".into(),
                data: serde_json::to_value(&taker_fill).expect("Fill serializes"),
            });

            client_fills.push(taker_fill);
        }

        if result.resting {
            self.order_map.insert(order_id, order);
        }

        Ok((order_id, client_fills))
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
