use alloy::primitives::{Bytes, U256};
use e2e_tests::TestEnv;
use types::{OrderType, Side, SignedOrder};

const E18: u128 = 1_000_000_000_000_000_000;

#[tokio::test]
async fn orders_survive_restart() {
    let _ = tracing_subscriber::fmt::try_init();
    let env = TestEnv::new().await;
    let maker = env.anvil_key(1);

    let e18 = U256::from(E18);
    env.credit_ledger(maker.address(), env.base_token, U256::from(1000) * e18)
        .await;

    let price = U256::from(50) * e18;
    let quantity = U256::from(10) * e18;
    let nonce = U256::from(1);

    let order = SignedOrder {
        side: Side::Sell,
        maker: maker.address(),
        base_token: env.base_token,
        quote_token: env.quote_token,
        price,
        quantity,
        nonce,
        expiry: U256::from(u64::MAX),
        signature: Bytes::new(),
    };
    let signed = env.sign_order(&order, &maker).await;

    // Place order
    let resp = env.place_order(signed, OrderType::Limit).await;
    assert_eq!(resp.status(), 200);

    // Verify in book
    let book = env.get_orderbook().await;
    assert_eq!(book["asks"].as_array().unwrap().len(), 1);

    // Simulate restart: read resting orders from the same DB
    let db = {
        let s = env.state.lock().await;
        s.db.clone()
    };

    let resting = db.load_resting_orders().unwrap();
    assert_eq!(resting.len(), 1, "DB should have 1 resting order");

    let (restored_id, blob, restored_nonce, resting_qty) = &resting[0];
    assert_eq!(*restored_nonce, nonce);
    assert_eq!(*resting_qty, quantity);

    let restored_order: SignedOrder = bincode::deserialize(blob).unwrap();
    assert_eq!(restored_order.price, price);
    assert_eq!(restored_order.quantity, quantity);
    assert_eq!(restored_order.maker, maker.address());

    // Create a fresh matching engine and restore
    let mut engine = matching_engine::MatchingEngine::new();
    engine.restore_order(*restored_id, restored_order.side, restored_order.price, *resting_qty);
    assert!(engine.has_order(*restored_id));

    // Verify the restored order is matchable
    let result = engine.submit(Side::Buy, price, U256::from(5) * e18, OrderType::Limit);
    assert_eq!(result.fills.len(), 1);
    assert_eq!(result.fills[0].maker_id, *restored_id);
    assert_eq!(result.fills[0].quantity, U256::from(5) * e18);
}
