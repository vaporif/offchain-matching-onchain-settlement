use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use alloy::primitives::{Address, U256};
use alloy::providers::{Provider, ProviderBuilder};
use clap::Parser;
use eyre::Result;
use persistence::Db;
use settlement_core::Settlement;
use tokio::sync::RwLock;
use tracing::info;

use gateway::build_router;
use gateway::deposit::DepositService;
use gateway::ledger::Ledger;
use gateway::state::AppState;
use gateway::ws_registry::WsRegistry;

#[derive(Parser)]
#[command(name = "gateway")]
struct Args {
    /// WebSocket RPC URL (must support eth_subscribe)
    #[arg(long, env = "WS_URL")]
    ws_url: String,

    /// Exchange contract address
    #[arg(long, env = "CONTRACT_ADDRESS")]
    contract_address: Address,

    /// Block number at which the Exchange contract was deployed
    #[arg(long, env = "DEPLOY_BLOCK")]
    deploy_block: u64,

    /// Path to SQLite database file
    #[arg(long, env = "DB_PATH", default_value = "./data/gateway.db")]
    db_path: PathBuf,

    /// Base token address
    #[arg(long, env = "BASE_TOKEN")]
    base_token: Address,

    /// Quote token address
    #[arg(long, env = "QUOTE_TOKEN")]
    quote_token: Address,

    /// Chain ID
    #[arg(long, env = "CHAIN_ID")]
    chain_id: u64,

    /// Server listen address
    #[arg(long, env = "LISTEN", default_value = "0.0.0.0:3000")]
    listen: String,
}

fn log_handle_result(result: Result<Result<()>, tokio::task::JoinError>) {
    match result {
        Ok(Ok(())) => info!("deposit stream ended cleanly, restarting"),
        Ok(Err(e)) => tracing::warn!(error = %e, "deposit stream failed, reconnecting"),
        Err(e) => tracing::error!(error = %e, "deposit task panicked, reconnecting"),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let args = Args::parse();

    let db = Arc::new(
        tokio::task::spawn_blocking({
            let path = args.db_path.clone();
            move || Db::open(&path)
        })
        .await??,
    );

    // -- Recovery sequence --
    let (ledger, max_id, pending_trades, unexpired_nonces, resting_orders, pending_cancels) = {
        let db = db.clone();
        tokio::task::spawn_blocking(move || -> Result<_> {
            let balances = db.load_all_balances()?;
            let ledger = Ledger::from_balances(balances);

            let max_id = db.load_max_order_id()?;

            let resting_orders = db.load_resting_orders()?;
            info!(
                count = resting_orders.len(),
                "loaded resting orders for restoration"
            );

            let pending_cancels = db.load_pending_cancels()?;
            info!(
                count = pending_cancels.len(),
                "loaded pending on-chain cancels"
            );

            let pending = db.load_pending_trades()?;
            info!(count = pending.len(), "recovered pending trades");

            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock before epoch")
                .as_secs();
            let pruned = db.prune_expired_nonces(now)?;
            if pruned > 0 {
                info!(count = pruned, "pruned expired nonces");
            }

            let unexpired_nonces = db.load_unexpired_nonces(now)?;
            info!(count = unexpired_nonces.len(), "recovered unexpired nonces");

            Ok((
                ledger,
                max_id,
                pending,
                unexpired_nonces,
                resting_orders,
                pending_cancels,
            ))
        })
        .await??
    };

    let provider = ProviderBuilder::new().connect(&args.ws_url).await?;

    let settlement = Arc::new(settlement_evm::EvmSettlement::new(
        provider.clone(),
        args.contract_address,
    ));

    let ws_registry = Arc::new(RwLock::new(WsRegistry::new()));

    let state = AppState::new(
        args.chain_id,
        args.contract_address,
        args.base_token,
        args.quote_token,
        ws_registry,
        db.clone(),
    );

    // Apply recovered state
    {
        let mut s = state.lock().await;
        s.ledger = ledger;
        if let Some(id) = max_id {
            s.engine.set_next_id(id + 1);
        }
        s.pending_trades = pending_trades;
        for nonce in unexpired_nonces {
            s.accepted_nonces.insert(nonce);
        }
    }

    // Restore resting orders
    {
        let mut s = state.lock().await;
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock before epoch")
            .as_secs();

        for (order_id, blob, nonce, resting_qty) in &resting_orders {
            let order: types::SignedOrder = match bincode::deserialize(blob) {
                Ok(o) => o,
                Err(e) => {
                    tracing::warn!(order_id, error = %e, "skipping undeserializable resting order");
                    continue;
                }
            };

            let expiry_secs = order.expiry.try_into().unwrap_or(u64::MAX);
            if expiry_secs < now_secs {
                info!(order_id, "expiring stale resting order");
                let db = db.clone();
                let oid = *order_id;
                tokio::task::spawn_blocking(move || {
                    let _ = db.update_order_status(oid, "cancelled");
                });
                continue;
            }

            s.engine
                .restore_order(*order_id, order.side, order.price, *resting_qty);

            let hash = gateway::eip712::order_hash(&order);
            s.accepted_nonces.insert(hash);

            let collateral_token = if order.side == types::Side::Buy {
                s.quote_token
            } else {
                s.base_token
            };
            let collateral = if order.side == types::Side::Buy {
                (*resting_qty * order.price) / U256::from(10u64).pow(U256::from(18u64))
            } else {
                *resting_qty
            };
            s.order_map.insert(*order_id, order.clone());
            s.order_nonce_map.insert(
                *order_id,
                (order.maker, *nonce, collateral_token, collateral),
            );

            info!(order_id, "restored resting order");
        }
    }

    // Retry pending on-chain cancels
    if !pending_cancels.is_empty() {
        let settlement_for_cancel = settlement.clone();
        let db_for_cancel = db.clone();
        tokio::spawn(async move {
            for (maker, nonce) in pending_cancels {
                info!(%maker, %nonce, "retrying on-chain cancel");
                match settlement_for_cancel.cancel_nonce(maker, nonce).await {
                    Ok(()) => {
                        let db = db_for_cancel.clone();
                        let _ = tokio::task::spawn_blocking(move || {
                            db.delete_pending_cancel(maker, &nonce)
                        })
                        .await;
                        info!(%maker, %nonce, "on-chain cancel confirmed");
                    }
                    Err(e) => {
                        tracing::warn!(%maker, %nonce, error = %e, "on-chain cancel failed, will retry next startup");
                    }
                }
            }
        });
    }

    let head = provider.get_block_number().await?;
    info!(head, "current chain head");

    let deposit_svc = Arc::new(DepositService::new(
        settlement.clone(),
        db.clone(),
        state.clone(),
        args.deploy_block,
    ));
    let live_handle = deposit_svc.clone().sync_and_subscribe(head).await?;
    info!("historical deposit sync complete, starting server");

    // Reconnection wrapper -- handle intentionally detached
    let _reconnect = tokio::spawn({
        let deposit_svc = deposit_svc.clone();
        let provider = provider.clone();
        async move {
            log_handle_result(live_handle.await);

            let mut backoff = Duration::from_secs(1);
            loop {
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(60));

                let head = match provider.get_block_number().await {
                    Ok(h) => h,
                    Err(e) => {
                        tracing::error!(error = %e, "failed to get block number during reconnect");
                        continue;
                    }
                };

                match deposit_svc.clone().sync_and_subscribe(head).await {
                    Ok(new_handle) => {
                        backoff = Duration::from_secs(1);
                        log_handle_result(new_handle.await);
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "failed to re-subscribe");
                    }
                }
            }
        }
    });

    // Nonce pruning: runs every hour
    tokio::spawn({
        let db = db.clone();
        async move {
            let mut interval = tokio::time::interval(Duration::from_secs(3600));
            interval.tick().await; // skip immediate first tick
            loop {
                interval.tick().await;
                let db = db.clone();
                let result = tokio::task::spawn_blocking(move || {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .expect("system clock before epoch")
                        .as_secs();
                    db.prune_expired_nonces(now)
                })
                .await;

                match result {
                    Ok(Ok(count)) if count > 0 => {
                        info!(count, "pruned expired nonces");
                    }
                    Ok(Err(e)) => {
                        tracing::error!(%e, "nonce pruning failed");
                    }
                    Err(e) => {
                        tracing::error!(%e, "nonce pruning task panicked");
                    }
                    _ => {}
                }
            }
        }
    });

    tokio::spawn(gateway::batch::batch_loop(
        state.clone(),
        settlement.clone(),
    ));

    let router = build_router(state);
    let listener = tokio::net::TcpListener::bind(&args.listen).await?;
    info!(address = %args.listen, "server listening");
    axum::serve(listener, router).await?;

    Ok(())
}
