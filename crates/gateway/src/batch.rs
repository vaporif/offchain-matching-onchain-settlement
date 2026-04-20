use std::sync::Arc;

use settlement_core::Settlement;
use tokio::sync::Mutex;
use tracing::{error, info};
use types::BatchSettlement;

use crate::state::AppState;

pub async fn batch_loop<S: Settlement>(state: Arc<Mutex<AppState>>, settlement: Arc<S>) {
    loop {
        let timeout = {
            let state = state.lock().await;
            std::time::Duration::from_secs(state.batch_timeout_secs)
        };

        tokio::time::sleep(timeout).await;

        let trades = {
            let mut state = state.lock().await;
            if state.pending_trades.is_empty() {
                continue;
            }
            state.drain_batch()
        };

        let trade_count = trades.len();
        let batch = BatchSettlement { trades };

        match settlement.submit_batch(batch).await {
            Ok(tx_hash) => {
                info!(%tx_hash, trade_count, "batch settled on-chain");
                let state = state.lock().await;
                let _ = state.ws_tx.send(crate::state::WsMessage {
                    msg_type: "batch_settled".into(),
                    data: serde_json::json!({
                        "tx_hash": format!("{tx_hash}"),
                        "trade_count": trade_count,
                    }),
                });
            }
            Err(e) => {
                error!(%e, "batch settlement failed");
            }
        }
    }
}
