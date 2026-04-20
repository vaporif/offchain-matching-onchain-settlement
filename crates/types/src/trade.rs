use alloy::primitives::U256;
use serde::{Deserialize, Serialize};

use crate::order::{OrderId, SignedOrder};

/// Matched trade — keeps both signed orders for on-chain sig checks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trade {
    pub maker_order: SignedOrder,
    pub taker_order: SignedOrder,
    pub price: U256,
    pub quantity: U256,
    pub timestamp: u64,
}

/// Fill pushed to clients over WS.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fill {
    pub order_id: OrderId,
    pub price: U256,
    pub filled_qty: U256,
    pub remaining_qty: U256,
    pub is_maker: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::order::Side;
    use alloy::primitives::{Address, Bytes};

    #[test]
    fn trade_carries_both_orders() {
        let order = SignedOrder {
            side: Side::Buy,
            maker: Address::ZERO,
            base_token: Address::ZERO,
            quote_token: Address::ZERO,
            price: U256::from(2000),
            quantity: U256::from(1),
            nonce: U256::ZERO,
            expiry: U256::from(u64::MAX),
            signature: Bytes::new(),
        };
        let trade = Trade {
            maker_order: order.clone(),
            taker_order: order,
            price: U256::from(2000),
            quantity: U256::from(1),
            timestamp: 0,
        };
        assert_eq!(trade.price, U256::from(2000));
        assert_eq!(trade.quantity, U256::from(1));
    }
}
