use std::sync::Arc;

use persistence::Db;
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

        let (tagged_trades, db) = {
            let mut state = state.lock().await;
            if state.pending_trades.is_empty() {
                continue;
            }
            (state.drain_batch(), state.db.clone())
        };

        let (trade_ids, trades): (Vec<i64>, Vec<_>) = tagged_trades.into_iter().unzip();
        let trade_count = trades.len();
        let batch = BatchSettlement { trades };

        match settlement.submit_batch(batch).await {
            Ok(tx_hash) => {
                info!(%tx_hash, trade_count, "batch settled on-chain");
                mark_trades_done(db, trade_ids, tx_hash).await;
            }
            Err(e) => {
                error!(%e, "batch settlement failed");
            }
        }
    }
}

async fn mark_trades_done(db: Arc<Db>, ids: Vec<i64>, tx_hash: alloy::primitives::B256) {
    let result = tokio::task::spawn_blocking(move || -> eyre::Result<()> {
        db.mark_trades_submitted(&ids, tx_hash)?;
        db.mark_trades_confirmed(&ids)?;
        db.delete_confirmed_trades()?;
        Ok(())
    })
    .await;

    match result {
        Ok(Ok(())) => {}
        Ok(Err(e)) => error!(%e, "failed to update trade status in db"),
        Err(e) => error!(%e, "trade status update task panicked"),
    }
}
