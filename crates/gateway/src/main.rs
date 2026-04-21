use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use alloy::primitives::Address;
use alloy::providers::{Provider, ProviderBuilder};
use clap::Parser;
use eyre::Result;
use tracing::info;

use gateway::build_router;
use gateway::deposit::DepositService;
use gateway::persistence::Db;
use gateway::state::AppState;

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

    let db = Arc::new(Db::open(&args.db_path)?);

    let provider = ProviderBuilder::new().connect(&args.ws_url).await?;

    let settlement = Arc::new(settlement_evm::EvmSettlement::new(
        provider.clone(),
        args.contract_address,
    ));

    let (state, _ws_rx) = AppState::new(
        args.chain_id,
        args.contract_address,
        args.base_token,
        args.quote_token,
    );

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

    // Reconnection wrapper — handle intentionally detached
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

    let router = build_router(state);
    let listener = tokio::net::TcpListener::bind(&args.listen).await?;
    info!(address = %args.listen, "server listening");
    axum::serve(listener, router).await?;

    Ok(())
}
