use std::sync::Arc;

use alloy::primitives::U256;
use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use types::{OrderType, SignedOrder};

use crate::state::AppState;

pub type SharedState = Arc<Mutex<AppState>>;

#[derive(Deserialize)]
pub struct SubmitOrderRequest {
    pub order: SignedOrder,
    #[serde(default)]
    pub order_type: OrderType,
}

#[derive(Serialize)]
pub struct OrderResponse {
    pub order_id: u64,
    pub status: String,
    pub fills: Vec<types::Fill>,
}

#[derive(Serialize)]
pub struct ErrorResponse {
    pub error: String,
}

pub async fn submit_order(
    State(state): State<SharedState>,
    Json(req): Json<SubmitOrderRequest>,
) -> impl IntoResponse {
    let (result, registry) = {
        let mut s = state.lock().await;
        let registry = s.ws_registry.clone();
        let result = s.submit_order(req.order, req.order_type);
        (result, registry)
    };

    match result {
        Ok((order_id, fills, dispatches)) => {
            {
                let mut reg = registry.write().await;
                for dispatch in dispatches {
                    reg.send_to(&dispatch.address, &dispatch.message);
                }
            }

            let resp = OrderResponse {
                order_id,
                status: "accepted".into(),
                fills,
            };
            (
                StatusCode::OK,
                Json(serde_json::to_value(resp).expect("OrderResponse serializes")),
            )
        }
        Err(e) => {
            let resp = ErrorResponse {
                error: e.to_string(),
            };
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::to_value(resp).expect("ErrorResponse serializes")),
            )
        }
    }
}

#[derive(Serialize)]
pub struct BookLevel {
    pub price: String,
    pub quantity: String,
}

#[derive(Serialize)]
pub struct BookResponse {
    pub bids: Vec<BookLevel>,
    pub asks: Vec<BookLevel>,
}

pub async fn get_orderbook(State(state): State<SharedState>) -> impl IntoResponse {
    let state = state.lock().await;
    let bids = state
        .engine
        .bid_levels(50)
        .into_iter()
        .map(|(price, qty)| BookLevel {
            price: price.to_string(),
            quantity: qty.to_string(),
        })
        .collect();
    let asks = state
        .engine
        .ask_levels(50)
        .into_iter()
        .map(|(price, qty)| BookLevel {
            price: price.to_string(),
            quantity: qty.to_string(),
        })
        .collect();
    Json(serde_json::to_value(BookResponse { bids, asks }).expect("BookResponse serializes"))
}

pub async fn get_balances(
    State(state): State<SharedState>,
    axum::extract::Path(addr): axum::extract::Path<String>,
) -> impl IntoResponse {
    let state = state.lock().await;
    let address: alloy::primitives::Address = addr.parse().unwrap_or_default();
    let base_balance = state.ledger.available(address, state.base_token);
    let quote_balance = state.ledger.available(address, state.quote_token);
    Json(serde_json::json!({
        "base": base_balance.to_string(),
        "quote": quote_balance.to_string(),
    }))
}

#[derive(Deserialize)]
pub struct CancelOrderRequest {
    pub nonce: U256,
    pub signature: alloy::primitives::Bytes,
}

#[derive(Serialize)]
pub struct CancelResponse {
    pub order_id: u64,
    pub status: String,
}

pub async fn cancel_order(
    State(state): State<SharedState>,
    Json(req): Json<CancelOrderRequest>,
) -> impl IntoResponse {
    let (result, registry) = {
        let mut s = state.lock().await;
        let registry = s.ws_registry.clone();
        let result = s.cancel_order(req.nonce, &req.signature);
        (result, registry)
    };

    match result {
        Ok((order_id, maker)) => {
            {
                let mut reg = registry.write().await;
                let msg = crate::state::WsMessage {
                    msg_type: "cancel".into(),
                    data: serde_json::json!({
                        "order_id": order_id,
                        "status": "cancelled"
                    }),
                };
                reg.send_to(&maker, &msg);
            }
            let resp = CancelResponse {
                order_id,
                status: "cancelled".into(),
            };
            (
                StatusCode::OK,
                Json(serde_json::to_value(resp).expect("CancelResponse serializes")),
            )
        }
        Err(e) => {
            let resp = ErrorResponse {
                error: e.to_string(),
            };
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::to_value(resp).expect("ErrorResponse serializes")),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn submit_order_request_deser_with_type() {
        let json = serde_json::json!({
            "order": {
                "side": "buy",
                "maker": "0x0000000000000000000000000000000000000001",
                "base_token": "0x0000000000000000000000000000000000000002",
                "quote_token": "0x0000000000000000000000000000000000000003",
                "price": "1000",
                "quantity": "5",
                "nonce": "1",
                "expiry": "999999999999",
                "signature": "0x"
            },
            "order_type": "market"
        });
        let req: SubmitOrderRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.order_type, types::OrderType::Market);
    }

    #[test]
    fn submit_order_request_deser_default_limit() {
        let json = serde_json::json!({
            "order": {
                "side": "buy",
                "maker": "0x0000000000000000000000000000000000000001",
                "base_token": "0x0000000000000000000000000000000000000002",
                "quote_token": "0x0000000000000000000000000000000000000003",
                "price": "1000",
                "quantity": "5",
                "nonce": "1",
                "expiry": "999999999999",
                "signature": "0x"
            }
        });
        let req: SubmitOrderRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.order_type, types::OrderType::Limit);
    }
}
