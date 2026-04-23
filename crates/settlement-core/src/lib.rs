use std::pin::Pin;

use alloy::primitives::{Address, B256, U256};
use eyre::Result;
use tokio_stream::Stream;
use types::{BatchSettlement, Deposit};

/// On-chain batch settlement.
pub trait Settlement: Send + Sync {
    fn submit_batch(&self, batch: BatchSettlement) -> impl Future<Output = Result<B256>> + Send;

    fn get_balance(
        &self,
        user: Address,
        token: Address,
    ) -> impl Future<Output = Result<U256>> + Send;

    fn subscribe_deposits(
        &self,
    ) -> impl Future<Output = Result<Pin<Box<dyn Stream<Item = Deposit> + Send>>>> + Send;

    fn get_deposits_in_range(
        &self,
        from_block: u64,
        to_block: u64,
    ) -> impl Future<Output = Result<Vec<Deposit>>> + Send;

    fn cancel_nonce(&self, maker: Address, nonce: U256) -> impl Future<Output = Result<()>> + Send;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockSettlement;

    impl Settlement for MockSettlement {
        fn submit_batch(
            &self,
            _batch: BatchSettlement,
        ) -> impl Future<Output = Result<B256>> + Send {
            async { Ok(B256::ZERO) }
        }

        fn get_balance(
            &self,
            _user: Address,
            _token: Address,
        ) -> impl Future<Output = Result<U256>> + Send {
            async { Ok(U256::from(1000)) }
        }

        fn subscribe_deposits(
            &self,
        ) -> impl Future<Output = Result<Pin<Box<dyn Stream<Item = Deposit> + Send>>>> + Send
        {
            async {
                Ok(Box::pin(tokio_stream::empty()) as Pin<Box<dyn Stream<Item = Deposit> + Send>>)
            }
        }

        fn get_deposits_in_range(
            &self,
            _from_block: u64,
            _to_block: u64,
        ) -> impl Future<Output = Result<Vec<Deposit>>> + Send {
            async { Ok(vec![]) }
        }

        fn cancel_nonce(
            &self,
            _maker: Address,
            _nonce: U256,
        ) -> impl Future<Output = Result<()>> + Send {
            async { Ok(()) }
        }
    }

    #[tokio::test]
    async fn mock_submit_returns_zero_hash() {
        let settlement = MockSettlement;
        let batch = BatchSettlement { trades: vec![] };
        let hash = settlement.submit_batch(batch).await.unwrap();
        assert_eq!(hash, B256::ZERO);
    }

    #[tokio::test]
    async fn mock_balance_returns_value() {
        let settlement = MockSettlement;
        let balance = settlement
            .get_balance(Address::ZERO, Address::ZERO)
            .await
            .unwrap();
        assert_eq!(balance, U256::from(1000));
    }
}
