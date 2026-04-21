use alloy::primitives::{Address, I256, U256};
use serde::{Deserialize, Serialize};

use crate::trade::Trade;

/// Trades to settle. Deltas computed at settlement time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchSettlement {
    pub trades: Vec<Trade>,
}

/// Net delta for one (user, token) pair.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BalanceDelta {
    pub user: Address,
    pub token: Address,
    pub delta: I256,
}

/// Deposit event from the contract.
#[derive(Debug, Clone)]
pub struct Deposit {
    pub user: Address,
    pub token: Address,
    pub amount: U256,
    pub block_number: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_batch_is_valid() {
        let batch = BatchSettlement { trades: vec![] };
        assert!(batch.trades.is_empty());
    }

    #[test]
    fn balance_delta_positive_and_negative() {
        let credit = BalanceDelta {
            user: Address::ZERO,
            token: Address::ZERO,
            delta: I256::try_from(100i64).unwrap(),
        };
        assert!(credit.delta > I256::ZERO);

        let debit = BalanceDelta {
            user: Address::ZERO,
            token: Address::ZERO,
            delta: I256::try_from(-50i64).unwrap(),
        };
        assert!(debit.delta < I256::ZERO);
    }
}
