use std::time::{SystemTime, UNIX_EPOCH};

use alloy::{
    network::EthereumWallet,
    primitives::{Address, Bytes, U256},
    providers::{Provider, ProviderBuilder},
    signers::{Signer, local::PrivateKeySigner},
    sol,
    sol_types::SolStruct,
};
use clap::{Parser, Subcommand};
use eyre::Result;
use types::{Side, SignedOrder};

sol! {
    #[derive(Debug)]
    struct Order {
        uint8 side;
        address maker;
        address baseToken;
        address quoteToken;
        uint256 price;
        uint256 quantity;
        uint256 nonce;
        uint256 expiry;
    }
}

sol! {
    #[sol(rpc)]
    contract IERC20 {
        function approve(address spender, uint256 amount) external returns (bool);
    }

    #[sol(rpc)]
    contract IExchange {
        function deposit(address token, uint256 amount) external;
        function withdraw(address token, uint256 amount) external;
        function balances(address user, address token) external view returns (uint256);
    }
}

#[derive(Parser)]
#[command(name = "hybrid-exchange")]
struct Cli {
    /// Private key (hex, no 0x prefix) or set PRIVATE_KEY env var
    #[arg(long, env = "PRIVATE_KEY")]
    private_key: String,

    /// Gateway URL
    #[arg(long, default_value = "http://localhost:3000")]
    gateway: String,

    /// RPC URL
    #[arg(long, default_value = "http://localhost:8545")]
    rpc: String,

    /// Exchange contract address
    #[arg(long)]
    exchange: Address,

    /// Base token address
    #[arg(long)]
    base_token: Address,

    /// Quote token address
    #[arg(long)]
    quote_token: Address,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Deposit tokens into the exchange
    Deposit {
        #[arg(long)]
        token: Address,
        #[arg(long)]
        amount: String,
    },
    /// Withdraw tokens from the exchange
    Withdraw {
        #[arg(long)]
        token: Address,
        #[arg(long)]
        amount: String,
    },
    /// Place an order
    PlaceOrder {
        #[arg(long)]
        side: String,
        #[arg(long)]
        price: String,
        #[arg(long)]
        qty: String,
    },
    /// Cancel an order
    Cancel {
        /// Nonce of the order to cancel
        #[arg(long)]
        nonce: u64,
    },
    /// Show exchange balances
    Status,
    /// Show order book
    Book,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    let signer: PrivateKeySigner = cli.private_key.parse()?;
    let address = signer.address();
    let wallet = EthereumWallet::from(signer.clone());

    let provider = ProviderBuilder::new()
        .wallet(wallet)
        .connect_http(cli.rpc.parse()?);

    match cli.command {
        Commands::Deposit { token, amount } => {
            let amount: U256 = amount.parse()?;
            let erc20 = IERC20::new(token, provider.clone());
            let tx = erc20
                .approve(cli.exchange, amount)
                .send()
                .await?
                .watch()
                .await?;
            println!("Approved: {tx}");
            let exchange = IExchange::new(cli.exchange, provider.clone());
            let tx = exchange
                .deposit(token, amount)
                .send()
                .await?
                .watch()
                .await?;
            println!("Deposited: {tx}");
        }
        Commands::Withdraw { token, amount } => {
            let amount: U256 = amount.parse()?;
            let exchange = IExchange::new(cli.exchange, provider.clone());
            let tx = exchange
                .withdraw(token, amount)
                .send()
                .await?
                .watch()
                .await?;
            println!("Withdrawn: {tx}");
        }
        Commands::PlaceOrder { side, price, qty } => {
            let side = match side.to_lowercase().as_str() {
                "buy" => Side::Buy,
                "sell" => Side::Sell,
                _ => eyre::bail!("side must be 'buy' or 'sell'"),
            };
            let price: U256 = price.parse()?;
            let qty: U256 = qty.parse()?;
            let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
            let nonce = U256::from(now);
            let expiry = U256::from(now + 300);

            let sol_order = Order {
                side: match side {
                    Side::Buy => 0,
                    Side::Sell => 1,
                },
                maker: address,
                baseToken: cli.base_token,
                quoteToken: cli.quote_token,
                price,
                quantity: qty,
                nonce,
                expiry,
            };

            let chain_id = provider.get_chain_id().await?;
            let domain_separator = {
                use alloy::primitives::keccak256;
                use alloy::sol_types::SolValue;
                let domain_type_hash = keccak256(
                    "EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)",
                );
                let name_hash = keccak256("HybridExchange");
                let version_hash = keccak256("1");
                keccak256(
                    (
                        domain_type_hash,
                        name_hash,
                        version_hash,
                        U256::from(chain_id),
                        cli.exchange,
                    )
                        .abi_encode(),
                )
            };

            let struct_hash = sol_order.eip712_hash_struct();
            let digest = alloy::primitives::keccak256(
                [
                    &[0x19, 0x01],
                    domain_separator.as_slice(),
                    struct_hash.as_slice(),
                ]
                .concat(),
            );
            let sig = signer.sign_hash(&digest).await?;

            let signed = SignedOrder {
                side,
                maker: address,
                base_token: cli.base_token,
                quote_token: cli.quote_token,
                price,
                quantity: qty,
                nonce,
                expiry,
                signature: Bytes::from(sig.as_bytes().to_vec()),
            };

            let client = reqwest::Client::new();
            let resp = client
                .post(format!("{}/orders", cli.gateway))
                .json(&signed)
                .send()
                .await?;

            let body: serde_json::Value = resp.json().await?;
            println!("{}", serde_json::to_string_pretty(&body)?);
        }
        Commands::Cancel { nonce } => {
            let chain_id = provider.get_chain_id().await?;
            let domain_separator = {
                use alloy::primitives::keccak256;
                use alloy::sol_types::SolValue;
                let domain_type_hash = keccak256(
                    "EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)",
                );
                let name_hash = keccak256("HybridExchange");
                let version_hash = keccak256("1");
                keccak256(
                    (
                        domain_type_hash,
                        name_hash,
                        version_hash,
                        U256::from(chain_id),
                        cli.exchange,
                    )
                        .abi_encode(),
                )
            };

            let typehash = alloy::primitives::keccak256(b"CancelOrder(uint256 nonce)");
            let struct_hash = alloy::primitives::keccak256(
                [typehash.as_slice(), &U256::from(nonce).to_be_bytes::<32>()].concat(),
            );
            let digest = alloy::primitives::keccak256(
                [
                    &[0x19, 0x01],
                    domain_separator.as_slice(),
                    struct_hash.as_slice(),
                ]
                .concat(),
            );

            let sig = signer.sign_hash(&digest).await?;
            let signature = Bytes::from(sig.as_bytes().to_vec());

            let client = reqwest::Client::new();
            let resp = client
                .post(format!("{}/cancel", cli.gateway))
                .json(&serde_json::json!({
                    "nonce": U256::from(nonce).to_string(),
                    "signature": signature,
                }))
                .send()
                .await?;

            let status = resp.status();
            let body: serde_json::Value = resp.json().await?;
            if status.is_success() {
                println!("Order cancelled: {}", serde_json::to_string_pretty(&body)?);
            } else {
                eprintln!("Cancel failed: {}", body["error"]);
            }
        }
        Commands::Status => {
            let client = reqwest::Client::new();
            let resp = client
                .get(format!("{}/balances/{}", cli.gateway, address))
                .send()
                .await?;
            let body: serde_json::Value = resp.json().await?;
            println!("{}", serde_json::to_string_pretty(&body)?);
        }
        Commands::Book => {
            let client = reqwest::Client::new();
            let resp = client
                .get(format!("{}/orderbook", cli.gateway))
                .send()
                .await?;
            let body: serde_json::Value = resp.json().await?;
            println!("{}", serde_json::to_string_pretty(&body)?);
        }
    }

    Ok(())
}
