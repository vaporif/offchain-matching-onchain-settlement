use std::sync::Arc;

use alloy::{
    network::EthereumWallet,
    node_bindings::{Anvil, AnvilInstance},
    primitives::{Address, B256, U256},
    providers::ProviderBuilder,
    signers::{Signer, local::PrivateKeySigner},
    sol,
    sol_types::SolStruct,
};
use gateway::{
    eip712::{compute_domain_separator, to_sol_order},
    state::AppState,
    ws_registry::WsRegistry,
};
use tokio::sync::RwLock;
use types::{OrderType, SignedOrder};

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

pub struct TestEnv {
    // Dropping AnvilInstance kills the child process
    #[allow(dead_code)]
    anvil: AnvilInstance,
    pub base_token: Address,
    pub quote_token: Address,
    pub exchange_addr: Address,
    pub state: Arc<tokio::sync::Mutex<AppState>>,
    pub base_url: String,
    pub domain_separator: B256,
    pub client: reqwest::Client,
}

impl TestEnv {
    pub async fn new() -> Self {
        let anvil = Anvil::new().try_spawn().expect("spawn anvil");
        let chain_id = anvil.chain_id();
        let rpc_url = anvil.endpoint_url();

        let operator: PrivateKeySigner = anvil.keys()[0].clone().into();
        let operator_provider = ProviderBuilder::new()
            .wallet(EthereumWallet::new(operator.clone()))
            .connect(rpc_url.as_str())
            .await
            .expect("connect to anvil");

        let base_deploy = MockERC20::deploy(&operator_provider, "Base".into(), "BASE".into())
            .await
            .expect("deploy base token");
        let quote_deploy = MockERC20::deploy(&operator_provider, "Quote".into(), "QUOTE".into())
            .await
            .expect("deploy quote token");
        let exchange_deploy = Exchange::deploy(
            &operator_provider,
            operator.address(),
            *base_deploy.address(),
            *quote_deploy.address(),
        )
        .await
        .expect("deploy exchange");

        let base_token = *base_deploy.address();
        let quote_token = *quote_deploy.address();
        let exchange_addr = *exchange_deploy.address();

        let settlement = Arc::new(settlement_evm::EvmSettlement::new(
            operator_provider,
            exchange_addr,
        ));

        let ws_registry = Arc::new(RwLock::new(WsRegistry::new()));
        let state = AppState::new(
            chain_id,
            exchange_addr,
            base_token,
            quote_token,
            ws_registry,
        );

        {
            let mut s = state.lock().await;
            s.batch_timeout_secs = 1;
        }

        tokio::spawn(gateway::batch::batch_loop(state.clone(), settlement));

        let router = gateway::build_router(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test listener");
        let addr = listener.local_addr().expect("get local addr");
        tokio::spawn(async move {
            axum::serve(listener, router).await.expect("axum serve");
        });

        let domain_separator = compute_domain_separator(chain_id, exchange_addr);

        Self {
            anvil,
            base_token,
            quote_token,
            exchange_addr,
            state,
            base_url: format!("http://{addr}"),
            domain_separator,
            client: reqwest::Client::new(),
        }
    }

    pub fn anvil_key(&self, index: usize) -> PrivateKeySigner {
        self.anvil.keys()[index].clone().into()
    }

    pub fn anvil_rpc_url(&self) -> String {
        self.anvil.endpoint_url().to_string()
    }

    pub async fn sign_order(&self, order: &SignedOrder, signer: &PrivateKeySigner) -> SignedOrder {
        let sol_order = to_sol_order(order);
        let struct_hash = sol_order.eip712_hash_struct();
        let digest = alloy::primitives::keccak256(
            [
                &[0x19, 0x01],
                self.domain_separator.as_slice(),
                struct_hash.as_slice(),
            ]
            .concat(),
        );
        let sig = signer.sign_hash(&digest).await.expect("sign order");
        SignedOrder {
            signature: sig.as_bytes().to_vec().into(),
            ..order.clone()
        }
    }

    pub async fn place_order(
        &self,
        order: SignedOrder,
        order_type: OrderType,
    ) -> reqwest::Response {
        self.client
            .post(format!("{}/orders", self.base_url))
            .json(&serde_json::json!({
                "order": order,
                "order_type": order_type,
            }))
            .send()
            .await
            .expect("send order request")
    }

    pub async fn get_orderbook(&self) -> serde_json::Value {
        self.client
            .get(format!("{}/orderbook", self.base_url))
            .send()
            .await
            .expect("get orderbook")
            .json()
            .await
            .expect("parse orderbook json")
    }

    pub async fn get_balances(&self, address: Address) -> serde_json::Value {
        self.client
            .get(format!("{}/balances/{address}", self.base_url))
            .send()
            .await
            .expect("get balances")
            .json()
            .await
            .expect("parse balances json")
    }

    pub async fn credit_ledger(&self, user: Address, token: Address, amount: U256) {
        let mut s = self.state.lock().await;
        s.ledger.credit(user, token, amount);
    }
}
