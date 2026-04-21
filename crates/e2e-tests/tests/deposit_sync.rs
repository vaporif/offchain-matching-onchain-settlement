use std::sync::Arc;
use std::time::Duration;

use alloy::{
    network::EthereumWallet,
    node_bindings::Anvil,
    primitives::U256,
    providers::{Provider, ProviderBuilder},
    signers::local::PrivateKeySigner,
    sol,
};
use gateway::deposit::DepositService;
use gateway::persistence::Db;
use gateway::state::AppState;
use gateway::ws_registry::WsRegistry;
use tokio::sync::RwLock;

sol!(
    #[sol(rpc)]
    Exchange,
    "../../contracts/out/Exchange.sol/Exchange.json"
);

sol!(
    #[sol(rpc)]
    MockERC20,
    "../../contracts/out/MockERC20.sol/MockERC20.json"
);

#[tokio::test]
async fn deposit_sync_replays_historical_and_catches_live() {
    let anvil = Anvil::new().try_spawn().expect("failed to spawn anvil");
    let ws_url = anvil.ws_endpoint();

    let operator_signer: PrivateKeySigner = anvil.keys()[0].clone().into();
    let user_signer: PrivateKeySigner = anvil.keys()[1].clone().into();
    let user_addr = user_signer.address();

    let operator_provider = ProviderBuilder::new()
        .wallet(EthereumWallet::new(operator_signer.clone()))
        .connect(ws_url.as_str())
        .await
        .unwrap();

    let user_provider = ProviderBuilder::new()
        .wallet(EthereumWallet::new(user_signer.clone()))
        .connect(ws_url.as_str())
        .await
        .unwrap();

    // Deploy contracts
    let base_token = MockERC20::deploy(&operator_provider, "Base".to_string(), "BASE".to_string())
        .await
        .expect("deploy base");
    let quote_token =
        MockERC20::deploy(&operator_provider, "Quote".to_string(), "QUOTE".to_string())
            .await
            .expect("deploy quote");
    let exchange = Exchange::deploy(
        &operator_provider,
        operator_signer.address(),
        *base_token.address(),
        *quote_token.address(),
    )
    .await
    .expect("deploy exchange");

    let token_addr = *base_token.address();
    let exchange_addr = *exchange.address();
    let deploy_block = operator_provider.get_block_number().await.unwrap();

    // Mint tokens to user and approve exchange
    let mint_amount = U256::from(10_000u64);
    base_token
        .mint(user_addr, mint_amount)
        .send()
        .await
        .unwrap()
        .get_receipt()
        .await
        .unwrap();

    let base_as_user = MockERC20::new(token_addr, &user_provider);
    base_as_user
        .approve(exchange_addr, mint_amount)
        .send()
        .await
        .unwrap()
        .get_receipt()
        .await
        .unwrap();

    // Make 3 historical deposits
    let exchange_as_user = Exchange::new(exchange_addr, &user_provider);
    for amount in [100u64, 200, 300] {
        exchange_as_user
            .deposit(token_addr, U256::from(amount))
            .send()
            .await
            .unwrap()
            .get_receipt()
            .await
            .unwrap();
    }

    // Set up DepositService with a WS provider for subscription
    let ws_provider = ProviderBuilder::new().connect(&ws_url).await.unwrap();
    let settlement = Arc::new(settlement_evm::EvmSettlement::new(
        ws_provider.clone(),
        exchange_addr,
    ));

    let db_dir = tempfile::TempDir::new().unwrap();
    let db = Arc::new(Db::open(&db_dir.path().join("test.db")).unwrap());

    let ws_registry = Arc::new(RwLock::new(WsRegistry::new()));
    let state = AppState::new(
        anvil.chain_id(),
        exchange_addr,
        token_addr,
        *quote_token.address(),
        ws_registry,
    );

    let head = ws_provider.get_block_number().await.unwrap();
    let svc = Arc::new(DepositService::new(
        settlement,
        db.clone(),
        state.clone(),
        deploy_block,
    ));
    let _live_handle = svc.clone().sync_and_subscribe(head).await.unwrap();

    // Assert historical deposits credited (100 + 200 + 300 = 600)
    {
        let state = state.lock().await;
        assert_eq!(
            state.ledger.available(user_addr, token_addr),
            U256::from(600)
        );
    }

    // Make one more deposit (live)
    exchange_as_user
        .deposit(token_addr, U256::from(400))
        .send()
        .await
        .unwrap()
        .get_receipt()
        .await
        .unwrap();

    // Give the live stream a moment to process
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Assert live deposit credited (600 + 400 = 1000)
    {
        let state = state.lock().await;
        assert_eq!(
            state.ledger.available(user_addr, token_addr),
            U256::from(1000)
        );
    }
}
