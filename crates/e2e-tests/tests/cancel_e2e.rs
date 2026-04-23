use alloy::primitives::{Bytes, U256};
use e2e_tests::TestEnv;
use types::{OrderType, Side, SignedOrder};

const E18: u128 = 1_000_000_000_000_000_000;

#[tokio::test]
async fn cancel_resting_order() {
    let _ = tracing_subscriber::fmt::try_init();
    let env = TestEnv::new().await;
    let maker = env.anvil_key(1);

    let e18 = U256::from(E18);
    env.credit_ledger(maker.address(), env.base_token, U256::from(1000) * e18)
        .await;

    let nonce = U256::from(42);
    let order = SignedOrder {
        side: Side::Sell,
        maker: maker.address(),
        base_token: env.base_token,
        quote_token: env.quote_token,
        price: U256::from(100) * e18,
        quantity: U256::from(10) * e18,
        nonce,
        expiry: U256::from(u64::MAX),
        signature: Bytes::new(),
    };
    let signed = env.sign_order(&order, &maker).await;

    // Place the order
    let resp = env.place_order(signed, OrderType::Limit).await;
    assert_eq!(resp.status(), 200);

    // Verify it's in the book
    let book = env.get_orderbook().await;
    assert_eq!(book["asks"].as_array().unwrap().len(), 1);

    // Cancel it
    let cancel_sig = env.sign_cancel(nonce, &maker).await;
    let resp = env.cancel_order(nonce, cancel_sig).await;
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"].as_str().unwrap(), "cancelled");

    // Verify removed from book
    let book = env.get_orderbook().await;
    assert!(book["asks"].as_array().unwrap().is_empty());

    // Verify balance restored
    let balances = env.get_balances(maker.address()).await;
    assert_eq!(
        balances["base"].as_str().unwrap(),
        (U256::from(1000) * e18).to_string()
    );
}

#[tokio::test]
async fn cancel_nonexistent_order_fails() {
    let env = TestEnv::new().await;
    let maker = env.anvil_key(1);

    let nonce = U256::from(999);
    let cancel_sig = env.sign_cancel(nonce, &maker).await;
    let resp = env.cancel_order(nonce, cancel_sig).await;
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn cancel_by_wrong_signer_fails() {
    let _ = tracing_subscriber::fmt::try_init();
    let env = TestEnv::new().await;
    let maker = env.anvil_key(1);
    let impersonator = env.anvil_key(2);

    let e18 = U256::from(E18);
    env.credit_ledger(maker.address(), env.base_token, U256::from(1000) * e18)
        .await;

    let nonce = U256::from(42);
    let order = SignedOrder {
        side: Side::Sell,
        maker: maker.address(),
        base_token: env.base_token,
        quote_token: env.quote_token,
        price: U256::from(100) * e18,
        quantity: U256::from(10) * e18,
        nonce,
        expiry: U256::from(u64::MAX),
        signature: Bytes::new(),
    };
    let signed = env.sign_order(&order, &maker).await;
    let resp = env.place_order(signed, OrderType::Limit).await;
    assert_eq!(resp.status(), 200);

    // Try to cancel with wrong signer
    let cancel_sig = env.sign_cancel(nonce, &impersonator).await;
    let resp = env.cancel_order(nonce, cancel_sig).await;
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["error"].as_str().unwrap().contains("does not match"));
}
