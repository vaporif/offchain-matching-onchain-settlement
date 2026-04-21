use alloy::primitives::{Address, Bytes, U256};
use serde::{Deserialize, Serialize};

pub type OrderId = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Buy,
    Sell,
}

/// Internal order — no crypto, no addresses. Gateway maps to/from SignedOrder.
#[derive(Debug, Clone)]
pub struct EngineOrder {
    pub id: OrderId,
    pub side: Side,
    pub price: U256,
    pub quantity: U256,
}

/// EIP-712 order — mirrors the Solidity struct 1:1.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedOrder {
    pub side: Side,
    pub maker: Address,
    pub base_token: Address,
    pub quote_token: Address,
    pub price: U256,
    pub quantity: U256,
    pub nonce: U256,
    pub expiry: U256,
    pub signature: Bytes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum OrderType {
    #[default]
    Limit,
    Market,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn side_serde_roundtrip() {
        let buy = Side::Buy;
        let json = serde_json::to_string(&buy).unwrap();
        assert_eq!(json, r#""buy""#);
        let parsed: Side = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, buy);
    }

    #[test]
    fn engine_order_fields() {
        let order = EngineOrder {
            id: 42,
            side: Side::Sell,
            price: U256::from(2000),
            quantity: U256::from(1),
        };
        assert_eq!(order.id, 42);
        assert_eq!(order.side, Side::Sell);
    }

    #[test]
    fn order_type_serde_roundtrip() {
        let market = OrderType::Market;
        let json = serde_json::to_string(&market).unwrap();
        assert_eq!(json, r#""market""#);
        let parsed: OrderType = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, market);

        let limit = OrderType::Limit;
        let json = serde_json::to_string(&limit).unwrap();
        assert_eq!(json, r#""limit""#);
        let parsed: OrderType = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, limit);
    }

    #[test]
    fn order_type_default_is_limit() {
        assert_eq!(OrderType::default(), OrderType::Limit);
    }
}
