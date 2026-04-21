use alloy::{
    network::EthereumWallet,
    node_bindings::Anvil,
    primitives::{Bytes, U256},
    providers::ProviderBuilder,
    signers::{Signer, local::PrivateKeySigner},
    sol,
    sol_types::{SolStruct, eip712_domain},
};

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

async fn sign_order(
    signer: &PrivateKeySigner,
    order: &Exchange::Order,
    domain: &alloy::sol_types::Eip712Domain,
) -> Bytes {
    let signing_hash = order.eip712_signing_hash(domain);
    let signature = signer.sign_hash(&signing_hash).await.unwrap();
    Bytes::from(signature.as_bytes().to_vec())
}

#[tokio::test]
async fn e2e_deposit_match_settle_withdraw() {
    let anvil = Anvil::new().try_spawn().expect("failed to spawn anvil");

    let operator_signer: PrivateKeySigner = anvil.keys()[0].clone().into();
    let maker_signer: PrivateKeySigner = anvil.keys()[1].clone().into();
    let taker_signer: PrivateKeySigner = anvil.keys()[2].clone().into();

    let operator_addr = operator_signer.address();
    let maker_addr = maker_signer.address();
    let taker_addr = taker_signer.address();

    let rpc_url = anvil.endpoint_url();
    let operator_provider = ProviderBuilder::new()
        .wallet(EthereumWallet::new(operator_signer.clone()))
        .connect(rpc_url.as_str())
        .await
        .unwrap();
    let maker_provider = ProviderBuilder::new()
        .wallet(EthereumWallet::new(maker_signer.clone()))
        .connect(rpc_url.as_str())
        .await
        .unwrap();
    let taker_provider = ProviderBuilder::new()
        .wallet(EthereumWallet::new(taker_signer.clone()))
        .connect(rpc_url.as_str())
        .await
        .unwrap();

    let base_token = MockERC20::deploy(&operator_provider, "Base".to_string(), "BASE".to_string())
        .await
        .expect("deploy base");
    let quote_token =
        MockERC20::deploy(&operator_provider, "Quote".to_string(), "QUOTE".to_string())
            .await
            .expect("deploy quote");
    let exchange = Exchange::deploy(
        &operator_provider,
        operator_addr,
        *base_token.address(),
        *quote_token.address(),
    )
    .await
    .expect("deploy exchange");

    let base_addr = *base_token.address();
    let quote_addr = *quote_token.address();
    let exchange_addr = *exchange.address();

    let mint_amount = U256::from(1_000_000_000_000_000_000_000u128);
    base_token
        .mint(maker_addr, mint_amount)
        .send()
        .await
        .unwrap()
        .get_receipt()
        .await
        .unwrap();
    quote_token
        .mint(taker_addr, mint_amount)
        .send()
        .await
        .unwrap()
        .get_receipt()
        .await
        .unwrap();

    let base_as_maker = MockERC20::new(base_addr, &maker_provider);
    base_as_maker
        .approve(exchange_addr, mint_amount)
        .send()
        .await
        .unwrap()
        .get_receipt()
        .await
        .unwrap();

    let exchange_as_maker = Exchange::new(exchange_addr, &maker_provider);
    let deposit_amount = U256::from(100_000_000_000_000_000_000u128);
    exchange_as_maker
        .deposit(base_addr, deposit_amount)
        .send()
        .await
        .expect("maker deposit")
        .get_receipt()
        .await
        .expect("maker deposit receipt");

    let quote_as_taker = MockERC20::new(quote_addr, &taker_provider);
    quote_as_taker
        .approve(exchange_addr, mint_amount)
        .send()
        .await
        .unwrap()
        .get_receipt()
        .await
        .unwrap();

    let exchange_as_taker = Exchange::new(exchange_addr, &taker_provider);
    let taker_deposit = U256::from(500_000_000_000_000_000_000u128);
    exchange_as_taker
        .deposit(quote_addr, taker_deposit)
        .send()
        .await
        .expect("taker deposit")
        .get_receipt()
        .await
        .expect("taker deposit receipt");

    assert_eq!(
        exchange
            .balances(maker_addr, base_addr)
            .call()
            .await
            .unwrap(),
        deposit_amount
    );
    assert_eq!(
        exchange
            .balances(taker_addr, quote_addr)
            .call()
            .await
            .unwrap(),
        taker_deposit
    );

    let domain = eip712_domain! {
        name: "HybridExchange",
        version: "1",
        chain_id: anvil.chain_id(),
        verifying_contract: exchange_addr,
    };

    let trade_qty = U256::from(10_000_000_000_000_000_000u128);
    let trade_price = U256::from(2_000_000_000_000_000_000u128);
    let expiry = U256::from(u64::MAX);

    let maker_order = Exchange::Order {
        side: 1,
        maker: maker_addr,
        baseToken: base_addr,
        quoteToken: quote_addr,
        price: trade_price,
        quantity: trade_qty,
        nonce: U256::from(1u64),
        expiry,
    };
    let taker_order = Exchange::Order {
        side: 0,
        maker: taker_addr,
        baseToken: base_addr,
        quoteToken: quote_addr,
        price: trade_price,
        quantity: trade_qty,
        nonce: U256::from(1u64),
        expiry,
    };

    let maker_sig = sign_order(&maker_signer, &maker_order, &domain).await;
    let taker_sig = sign_order(&taker_signer, &taker_order, &domain).await;

    exchange
        .settleBatch(
            vec![maker_order],
            vec![taker_order],
            vec![maker_sig],
            vec![taker_sig],
            vec![trade_qty],
            vec![trade_price],
        )
        .send()
        .await
        .expect("settle send")
        .get_receipt()
        .await
        .expect("settle receipt");

    let quote_received = (trade_qty * trade_price) / U256::from(10u64).pow(U256::from(18u64));
    assert_eq!(
        exchange
            .balances(maker_addr, base_addr)
            .call()
            .await
            .unwrap(),
        deposit_amount - trade_qty
    );
    assert_eq!(
        exchange
            .balances(maker_addr, quote_addr)
            .call()
            .await
            .unwrap(),
        quote_received
    );
    assert_eq!(
        exchange
            .balances(taker_addr, base_addr)
            .call()
            .await
            .unwrap(),
        trade_qty
    );
    assert_eq!(
        exchange
            .balances(taker_addr, quote_addr)
            .call()
            .await
            .unwrap(),
        taker_deposit - quote_received
    );

    exchange_as_maker
        .withdraw(quote_addr, quote_received)
        .send()
        .await
        .expect("maker withdraw")
        .get_receipt()
        .await
        .expect("maker withdraw receipt");
    assert_eq!(
        quote_token.balanceOf(maker_addr).call().await.unwrap(),
        quote_received
    );

    exchange_as_taker
        .withdraw(base_addr, trade_qty)
        .send()
        .await
        .expect("taker withdraw")
        .get_receipt()
        .await
        .expect("taker withdraw receipt");
    assert_eq!(
        base_token.balanceOf(taker_addr).call().await.unwrap(),
        trade_qty
    );
}
