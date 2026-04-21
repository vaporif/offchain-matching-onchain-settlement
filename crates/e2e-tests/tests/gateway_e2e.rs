use std::time::Duration;

use alloy::{
    network::EthereumWallet,
    primitives::{Bytes, U256},
    providers::ProviderBuilder,
};
use e2e_tests::{Exchange, MockERC20, TestEnv};
use types::{OrderType, Side, SignedOrder};

const E18: u128 = 1_000_000_000_000_000_000;

#[tokio::test]
async fn full_trade_lifecycle() {
    let _ = tracing_subscriber::fmt::try_init();
    let env = TestEnv::new().await;
    let maker = env.anvil_key(1);
    let taker = env.anvil_key(2);
    let rpc_url = env.anvil_rpc_url();

    let deposit_base = U256::from(100 * E18);
    let deposit_quote = U256::from(500 * E18);

    let maker_provider = ProviderBuilder::new()
        .wallet(EthereumWallet::new(maker.clone()))
        .connect(rpc_url.as_str())
        .await
        .unwrap();
    let taker_provider = ProviderBuilder::new()
        .wallet(EthereumWallet::new(taker.clone()))
        .connect(rpc_url.as_str())
        .await
        .unwrap();

    // Avoid key 0 — batch_loop uses the operator provider and would hit nonce conflicts.
    let base_as_maker = MockERC20::new(env.base_token, &maker_provider);
    base_as_maker
        .mint(maker.address(), deposit_base)
        .send()
        .await
        .unwrap()
        .get_receipt()
        .await
        .unwrap();
    base_as_maker
        .approve(env.exchange_addr, deposit_base)
        .send()
        .await
        .unwrap()
        .get_receipt()
        .await
        .unwrap();
    let exchange_as_maker = Exchange::new(env.exchange_addr, &maker_provider);
    exchange_as_maker
        .deposit(env.base_token, deposit_base)
        .send()
        .await
        .unwrap()
        .get_receipt()
        .await
        .unwrap();

    let quote_as_taker = MockERC20::new(env.quote_token, &taker_provider);
    quote_as_taker
        .mint(taker.address(), deposit_quote)
        .send()
        .await
        .unwrap()
        .get_receipt()
        .await
        .unwrap();
    quote_as_taker
        .approve(env.exchange_addr, deposit_quote)
        .send()
        .await
        .unwrap()
        .get_receipt()
        .await
        .unwrap();
    let exchange_as_taker = Exchange::new(env.exchange_addr, &taker_provider);
    exchange_as_taker
        .deposit(env.quote_token, deposit_quote)
        .send()
        .await
        .unwrap()
        .get_receipt()
        .await
        .unwrap();

    env.credit_ledger(maker.address(), env.base_token, deposit_base)
        .await;
    env.credit_ledger(taker.address(), env.quote_token, deposit_quote)
        .await;

    let price = U256::from(2 * E18);
    let quantity = U256::from(10 * E18);

    let sell = SignedOrder {
        side: Side::Sell,
        maker: maker.address(),
        base_token: env.base_token,
        quote_token: env.quote_token,
        price,
        quantity,
        nonce: U256::from(1),
        expiry: U256::from(u64::MAX),
        signature: Bytes::new(),
    };
    let sell_signed = env.sign_order(&sell, &maker).await;
    let resp = env.place_order(sell_signed, OrderType::Limit).await;
    assert_eq!(resp.status(), 200);

    let buy = SignedOrder {
        side: Side::Buy,
        maker: taker.address(),
        base_token: env.base_token,
        quote_token: env.quote_token,
        price,
        quantity,
        nonce: U256::from(1),
        expiry: U256::from(u64::MAX),
        signature: Bytes::new(),
    };
    let buy_signed = env.sign_order(&buy, &taker).await;
    let resp = env.place_order(buy_signed, OrderType::Limit).await;
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let fills = body["fills"].as_array().unwrap();
    assert_eq!(fills.len(), 1);

    tokio::time::sleep(Duration::from_secs(4)).await;

    let exchange = Exchange::new(env.exchange_addr, &maker_provider);
    let quote_amount = (quantity * price) / U256::from(E18);

    assert_eq!(
        exchange
            .balances(maker.address(), env.base_token)
            .call()
            .await
            .unwrap(),
        deposit_base - quantity
    );
    assert_eq!(
        exchange
            .balances(maker.address(), env.quote_token)
            .call()
            .await
            .unwrap(),
        quote_amount
    );
    assert_eq!(
        exchange
            .balances(taker.address(), env.base_token)
            .call()
            .await
            .unwrap(),
        quantity
    );
    assert_eq!(
        exchange
            .balances(taker.address(), env.quote_token)
            .call()
            .await
            .unwrap(),
        deposit_quote - quote_amount
    );
}

#[tokio::test]
async fn market_order_empty_book_rejects() {
    let env = TestEnv::new().await;
    let user = env.anvil_key(1);

    let e18 = U256::from(E18);
    env.credit_ledger(user.address(), env.quote_token, U256::from(1000) * e18)
        .await;

    let order = SignedOrder {
        side: Side::Buy,
        maker: user.address(),
        base_token: env.base_token,
        quote_token: env.quote_token,
        price: U256::from(100) * e18,
        quantity: U256::from(10) * e18,
        nonce: U256::from(1),
        expiry: U256::from(u64::MAX),
        signature: Bytes::new(),
    };
    let signed = env.sign_order(&order, &user).await;
    let resp = env.place_order(signed, OrderType::Market).await;

    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["error"].as_str().unwrap().contains("not fillable"),
        "expected 'not fillable', got: {}",
        body["error"]
    );
}

#[tokio::test]
async fn insufficient_balance_rejects() {
    let env = TestEnv::new().await;
    let user = env.anvil_key(1);

    let e18 = U256::from(E18);
    env.credit_ledger(user.address(), env.base_token, U256::from(10) * e18)
        .await;

    let order = SignedOrder {
        side: Side::Sell,
        maker: user.address(),
        base_token: env.base_token,
        quote_token: env.quote_token,
        price: U256::from(2) * e18,
        quantity: U256::from(100) * e18,
        nonce: U256::from(1),
        expiry: U256::from(u64::MAX),
        signature: Bytes::new(),
    };
    let signed = env.sign_order(&order, &user).await;
    let resp = env.place_order(signed, OrderType::Limit).await;

    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("insufficient balance"),
        "expected 'insufficient balance', got: {}",
        body["error"]
    );
}

#[tokio::test]
async fn duplicate_nonce_rejects() {
    let env = TestEnv::new().await;
    let user = env.anvil_key(1);

    let e18 = U256::from(E18);
    env.credit_ledger(user.address(), env.base_token, U256::from(1000) * e18)
        .await;

    let order = SignedOrder {
        side: Side::Sell,
        maker: user.address(),
        base_token: env.base_token,
        quote_token: env.quote_token,
        price: U256::from(100) * e18,
        quantity: U256::from(5) * e18,
        nonce: U256::from(42),
        expiry: U256::from(u64::MAX),
        signature: Bytes::new(),
    };
    let signed = env.sign_order(&order, &user).await;

    let resp = env.place_order(signed.clone(), OrderType::Limit).await;
    assert_eq!(resp.status(), 200);

    let resp = env.place_order(signed, OrderType::Limit).await;
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["error"].as_str().unwrap().contains("duplicate nonce"),
        "expected 'duplicate nonce', got: {}",
        body["error"]
    );
}

#[tokio::test]
async fn orderbook_reflects_resting_orders() {
    let env = TestEnv::new().await;
    let user = env.anvil_key(1);

    let e18 = U256::from(E18);
    env.credit_ledger(user.address(), env.base_token, U256::from(1000) * e18)
        .await;

    let price = U256::from(50) * e18;
    let quantity = U256::from(7) * e18;

    let order = SignedOrder {
        side: Side::Sell,
        maker: user.address(),
        base_token: env.base_token,
        quote_token: env.quote_token,
        price,
        quantity,
        nonce: U256::from(1),
        expiry: U256::from(u64::MAX),
        signature: Bytes::new(),
    };
    let signed = env.sign_order(&order, &user).await;
    let resp = env.place_order(signed, OrderType::Limit).await;
    assert_eq!(resp.status(), 200);

    let book = env.get_orderbook().await;
    let bids = book["bids"].as_array().unwrap();
    let asks = book["asks"].as_array().unwrap();

    assert!(bids.is_empty());
    assert_eq!(asks.len(), 1);
    assert_eq!(asks[0]["price"].as_str().unwrap(), price.to_string());
    assert_eq!(asks[0]["quantity"].as_str().unwrap(), quantity.to_string());
}
