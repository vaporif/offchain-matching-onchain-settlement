pub mod abi;

use std::pin::Pin;

use alloy::{
    primitives::{Address, B256, U256},
    providers::Provider,
    rpc::types::Filter,
    sol_types::SolEvent,
};
use eyre::Result;
use settlement_core::Settlement;
use tokio_stream::{Stream, StreamExt};
use tracing::info;
use types::{BatchSettlement, Deposit, Side, SignedOrder};

use crate::abi::Exchange;

fn decode_deposit(log: &alloy::rpc::types::Log) -> Option<Deposit> {
    let Some(block_number) = log.block_number else {
        tracing::warn!(?log, "deposit log missing block_number, skipping");
        return None;
    };
    match Exchange::Deposited::decode_log(&log.inner) {
        Ok(decoded) => Some(Deposit {
            user: decoded.user,
            token: decoded.token,
            amount: decoded.amount,
            block_number,
        }),
        Err(e) => {
            tracing::warn!(error = %e, block_number, "failed to decode deposit log");
            None
        }
    }
}

pub struct EvmSettlement<P: Provider + Clone> {
    contract: Exchange::ExchangeInstance<P>,
    provider: P,
}

impl<P: Provider + Clone> EvmSettlement<P> {
    pub fn new(provider: P, contract_address: Address) -> Self {
        let contract = Exchange::new(contract_address, provider.clone());
        Self { contract, provider }
    }

    pub fn contract_address(&self) -> Address {
        *self.contract.address()
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

impl<P: Provider + Clone + Send + Sync + 'static> Settlement for EvmSettlement<P> {
    async fn submit_batch(&self, batch: BatchSettlement) -> Result<B256> {
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

    async fn get_balance(&self, user: Address, token: Address) -> Result<U256> {
        let balance = self.contract.balances(user, token).call().await?;
        Ok(balance)
    }

    async fn subscribe_deposits(&self) -> Result<Pin<Box<dyn Stream<Item = Deposit> + Send>>> {
        let filter = Filter::new()
            .address(*self.contract.address())
            .event_signature(Exchange::Deposited::SIGNATURE_HASH);

        let sub = self.provider.subscribe_logs(&filter).await?;
        let stream = sub.into_stream().filter_map(|log| decode_deposit(&log));

        Ok(Box::pin(stream))
    }

    async fn get_deposits_in_range(&self, from_block: u64, to_block: u64) -> Result<Vec<Deposit>> {
        let filter = Filter::new()
            .address(*self.contract.address())
            .event_signature(Exchange::Deposited::SIGNATURE_HASH)
            .from_block(from_block)
            .to_block(to_block);

        let logs = self.provider.get_logs(&filter).await?;

        let deposits = logs
            .into_iter()
            .filter_map(|log| decode_deposit(&log))
            .collect();

        Ok(deposits)
    }
}
