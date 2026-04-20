use std::sync::Arc;

use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::Serialize;
use tokio::sync::Mutex;
use types::SignedOrder;

use crate::state::AppState;

pub type SharedState = Arc<Mutex<AppState>>;

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
    Json(order): Json<SignedOrder>,
) -> impl IntoResponse {
    let mut state = state.lock().await;
    match state.submit_order(order) {
        Ok((order_id, fills)) => {
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

pub async fn get_orderbook(State(_state): State<SharedState>) -> impl IntoResponse {
    let resp = BookResponse {
        bids: vec![],
        asks: vec![],
    };
    Json(serde_json::to_value(resp).expect("BookResponse serializes"))
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
