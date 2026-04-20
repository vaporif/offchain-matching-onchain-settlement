pub mod abi;

use std::pin::Pin;

use alloy::{
    primitives::{Address, B256, U256},
    providers::Provider,
};
use eyre::Result;
use tokio_stream::Stream;
use tracing::info;
use types::{BatchSettlement, Deposit, Side, SignedOrder};

use crate::abi::Exchange;

pub struct EvmSettlement<P: Provider + Clone> {
    contract: Exchange::ExchangeInstance<P>,
    #[allow(dead_code)]
    provider: P,
}

impl<P: Provider + Clone> EvmSettlement<P> {
    pub fn new(provider: P, contract_address: Address) -> Self {
        let contract = Exchange::new(contract_address, provider.clone());
        Self { contract, provider }
    }
}

fn signed_order_to_abi(order: &SignedOrder) -> Exchange::Order {
    Exchange::Order {
        side: match order.side {
            Side::Buy => 0,
            Side::Sell => 1,
        },
        maker: order.maker,
        baseToken: order.base_token,
        quoteToken: order.quote_token,
        price: order.price,
        quantity: order.quantity,
        nonce: order.nonce,
        expiry: order.expiry,
    }
}

impl<P: Provider + Clone + Send + Sync> EvmSettlement<P> {
    pub async fn submit_batch(&self, batch: BatchSettlement) -> Result<B256> {
        let len = batch.trades.len();
        let mut maker_orders = Vec::with_capacity(len);
        let mut taker_orders = Vec::with_capacity(len);
        let mut maker_sigs = Vec::with_capacity(len);
        let mut taker_sigs = Vec::with_capacity(len);
        let mut quantities = Vec::with_capacity(len);
        let mut prices = Vec::with_capacity(len);

        for trade in &batch.trades {
            maker_orders.push(signed_order_to_abi(&trade.maker_order));
            taker_orders.push(signed_order_to_abi(&trade.taker_order));
            maker_sigs.push(trade.maker_order.signature.clone());
            taker_sigs.push(trade.taker_order.signature.clone());
            quantities.push(trade.quantity);
            prices.push(trade.price);
        }

        let tx = self
            .contract
            .settleBatch(
                maker_orders,
                taker_orders,
                maker_sigs,
                taker_sigs,
                quantities,
                prices,
            )
            .send()
            .await?
            .watch()
            .await?;

        info!(tx_hash = %tx, trades = len, "batch settled");
        Ok(tx)
    }

    pub async fn get_balance(&self, user: Address, token: Address) -> Result<U256> {
        let balance = self.contract.balances(user, token).call().await?;
        Ok(balance)
    }

    pub async fn subscribe_deposits(&self) -> Result<Pin<Box<dyn Stream<Item = Deposit> + Send>>> {
        // TODO: wire up ws provider for deposit events
        Ok(Box::pin(tokio_stream::empty()))
    }
}
