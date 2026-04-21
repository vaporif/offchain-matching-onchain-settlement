use std::sync::Arc;

use eyre::{Result, WrapErr};
use settlement_core::Settlement;
use tokio::sync::Mutex;
use tokio_stream::StreamExt;
use tracing::{info, warn};

use crate::persistence::Db;
use crate::state::AppState;

const CHUNK_SIZE: u64 = 2000;

pub struct DepositService<S> {
    settlement: Arc<S>,
    db: Arc<Db>,
    state: Arc<Mutex<AppState>>,
    contract_deploy_block: u64,
}

impl<S: Settlement + 'static> DepositService<S> {
    pub fn new(
        settlement: Arc<S>,
        db: Arc<Db>,
        state: Arc<Mutex<AppState>>,
        contract_deploy_block: u64,
    ) -> Self {
        Self {
            settlement,
            db,
            state,
            contract_deploy_block,
        }
    }

    pub async fn sync_historical(&self, head: u64) -> Result<()> {
        let start_block = self
            .db
            .last_synced_block()?
            .map(|b| b + 1)
            .unwrap_or(self.contract_deploy_block);

        if start_block > head {
            info!(start_block, head, "already synced to head");
            return Ok(());
        }

        info!(
            from = start_block,
            to = head,
            "replaying historical deposits"
        );

        let mut current = start_block;
        while current <= head {
            let end = (current + CHUNK_SIZE - 1).min(head);
            let deposits = self
                .settlement
                .get_deposits_in_range(current, end)
                .await
                .wrap_err_with(|| format!("fetching deposits for blocks {current}..={end}"))?;

            if !deposits.is_empty() {
                let mut state = self.state.lock().await;
                for deposit in &deposits {
                    state
                        .ledger
                        .credit(deposit.user, deposit.token, deposit.amount);
                }
                info!(
                    count = deposits.len(),
                    from = current,
                    to = end,
                    "credited historical deposits"
                );
            }

            tokio::task::spawn_blocking({
                let db = self.db.clone();
                move || db.set_last_synced_block(end)
            })
            .await??;

            current = end + 1;
        }

        Ok(())
    }

    pub async fn sync_and_subscribe(
        self: Arc<Self>,
        head: u64,
    ) -> Result<tokio::task::JoinHandle<Result<()>>> {
        let mut stream = self.settlement.subscribe_deposits().await?;

        self.sync_historical(head).await?;

        // Use the higher of head and persisted block as dedup boundary.
        // On reconnect after extended downtime, the DB cursor may be ahead of the new head.
        let last_synced = self
            .db
            .last_synced_block()?
            .map(|b| b.max(head))
            .unwrap_or(head);

        let svc = self.clone();
        let handle = tokio::spawn(async move {
            let mut last_persisted_block = last_synced;
            while let Some(deposit) = stream.next().await {
                // Skip events already covered by historical sync
                if deposit.block_number <= last_synced {
                    continue;
                }

                {
                    let mut state = svc.state.lock().await;
                    state
                        .ledger
                        .credit(deposit.user, deposit.token, deposit.amount);
                }

                // Only persist when we advance to a new block
                if deposit.block_number > last_persisted_block {
                    last_persisted_block = deposit.block_number;
                    tokio::task::spawn_blocking({
                        let db = svc.db.clone();
                        move || db.set_last_synced_block(last_persisted_block)
                    })
                    .await??;
                }

                info!(
                    user = %deposit.user,
                    token = %deposit.token,
                    amount = %deposit.amount,
                    block = deposit.block_number,
                    "live deposit credited"
                );
            }

            warn!("deposit subscription stream ended");
            Ok(())
        });

        Ok(handle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{Address, U256, address};
    use std::pin::Pin;
    use tokio_stream::Stream;
    use types::{BatchSettlement, Deposit};

    struct MockSettlement {
        deposits: Vec<Deposit>,
    }

    impl Settlement for MockSettlement {
        async fn submit_batch(
            &self,
            _batch: BatchSettlement,
        ) -> eyre::Result<alloy::primitives::B256> {
            unimplemented!()
        }

        async fn get_balance(&self, _user: Address, _token: Address) -> eyre::Result<U256> {
            unimplemented!()
        }

        async fn subscribe_deposits(
            &self,
        ) -> eyre::Result<Pin<Box<dyn Stream<Item = Deposit> + Send>>> {
            let (_tx, rx) = tokio::sync::mpsc::channel(16);
            Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)))
        }

        async fn get_deposits_in_range(
            &self,
            from_block: u64,
            to_block: u64,
        ) -> eyre::Result<Vec<Deposit>> {
            Ok(self
                .deposits
                .iter()
                .filter(|d| d.block_number >= from_block && d.block_number <= to_block)
                .cloned()
                .collect())
        }
    }

    fn test_deposits() -> Vec<Deposit> {
        let user = address!("0x1111111111111111111111111111111111111111");
        let token = address!("0x2222222222222222222222222222222222222222");
        vec![
            Deposit {
                user,
                token,
                amount: U256::from(100),
                block_number: 10,
            },
            Deposit {
                user,
                token,
                amount: U256::from(200),
                block_number: 15,
            },
            Deposit {
                user,
                token,
                amount: U256::from(300),
                block_number: 2005,
            },
        ]
    }

    #[tokio::test]
    async fn sync_historical_credits_ledger() {
        let dir = tempfile::TempDir::new().unwrap();
        let db = Arc::new(Db::open(&dir.path().join("test.db")).unwrap());
        let settlement = Arc::new(MockSettlement {
            deposits: test_deposits(),
        });

        let (state, _rx) = AppState::new(
            1,
            Address::ZERO,
            address!("0x2222222222222222222222222222222222222222"),
            address!("0x3333333333333333333333333333333333333333"),
        );

        let svc = DepositService::new(settlement, db.clone(), state.clone(), 0);
        svc.sync_historical(2100).await.unwrap();

        let state_locked = state.lock().await;
        let user = address!("0x1111111111111111111111111111111111111111");
        let token = address!("0x2222222222222222222222222222222222222222");
        assert_eq!(state_locked.ledger.available(user, token), U256::from(600));
    }

    #[tokio::test]
    async fn sync_historical_persists_block_number() {
        let dir = tempfile::TempDir::new().unwrap();
        let db = Arc::new(Db::open(&dir.path().join("test.db")).unwrap());
        let settlement = Arc::new(MockSettlement {
            deposits: test_deposits(),
        });

        let (state, _rx) = AppState::new(
            1,
            Address::ZERO,
            address!("0x2222222222222222222222222222222222222222"),
            address!("0x3333333333333333333333333333333333333333"),
        );

        let svc = DepositService::new(settlement, db.clone(), state, 0);
        svc.sync_historical(2100).await.unwrap();

        assert_eq!(db.last_synced_block().unwrap(), Some(2100));
    }

    #[tokio::test]
    async fn sync_resumes_from_last_synced_block() {
        let dir = tempfile::TempDir::new().unwrap();
        let db = Arc::new(Db::open(&dir.path().join("test.db")).unwrap());
        db.set_last_synced_block(12).unwrap();

        let settlement = Arc::new(MockSettlement {
            deposits: test_deposits(),
        });

        let (state, _rx) = AppState::new(
            1,
            Address::ZERO,
            address!("0x2222222222222222222222222222222222222222"),
            address!("0x3333333333333333333333333333333333333333"),
        );

        let svc = DepositService::new(settlement, db.clone(), state.clone(), 0);
        svc.sync_historical(2100).await.unwrap();

        let state_locked = state.lock().await;
        let user = address!("0x1111111111111111111111111111111111111111");
        let token = address!("0x2222222222222222222222222222222222222222");
        // Only deposits at block 15 and 2005 (block 10 is <= 12, skipped by range start)
        assert_eq!(state_locked.ledger.available(user, token), U256::from(500));
    }

    #[tokio::test]
    async fn sync_with_empty_deposits() {
        let dir = tempfile::TempDir::new().unwrap();
        let db = Arc::new(Db::open(&dir.path().join("test.db")).unwrap());
        let settlement = Arc::new(MockSettlement { deposits: vec![] });

        let (state, _rx) = AppState::new(
            1,
            Address::ZERO,
            address!("0x2222222222222222222222222222222222222222"),
            address!("0x3333333333333333333333333333333333333333"),
        );

        let svc = DepositService::new(settlement, db.clone(), state, 0);
        svc.sync_historical(100).await.unwrap();

        assert_eq!(db.last_synced_block().unwrap(), Some(100));
    }
}
