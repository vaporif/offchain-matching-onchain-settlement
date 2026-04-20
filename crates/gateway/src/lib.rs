pub mod batch;
pub mod eip712;
pub mod ledger;
pub mod routes;
pub mod state;
pub mod ws;

use axum::{
    Router,
    routing::{get, post},
};
use tower_http::trace::TraceLayer;

use crate::routes::SharedState;

pub fn build_router(state: SharedState) -> Router {
    Router::new()
        .route("/orders", post(routes::submit_order))
        .route("/orderbook", get(routes::get_orderbook))
        .route("/balances/{address}", get(routes::get_balances))
        .route("/ws", get(ws::ws_handler))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
