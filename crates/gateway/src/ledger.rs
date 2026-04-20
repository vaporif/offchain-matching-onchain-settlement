use std::collections::HashMap;

use alloy::primitives::{Address, U256};

#[derive(Debug, Default)]
struct TokenBalance {
    available: U256,
    reserved: U256,
}

impl TokenBalance {
    fn total(&self) -> U256 {
        self.available + self.reserved
    }
}

#[derive(Debug, Default)]
pub struct Ledger {
    balances: HashMap<Address, HashMap<Address, TokenBalance>>,
}

impl Ledger {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn credit(&mut self, user: Address, token: Address, amount: U256) {
        self.entry(user, token).available += amount;
    }

    pub fn reserve(&mut self, user: Address, token: Address, amount: U256) -> bool {
        let bal = self.entry(user, token);
        if bal.available < amount {
            return false;
        }
        bal.available -= amount;
        bal.reserved += amount;
        true
    }

    pub fn release(&mut self, user: Address, token: Address, amount: U256) {
        let bal = self.entry(user, token);
        bal.reserved -= amount;
        bal.available += amount;
    }

    pub fn settle_fill(
        &mut self,
        seller: Address,
        buyer: Address,
        base_token: Address,
        quote_token: Address,
        base_amount: U256,
        quote_amount: U256,
    ) {
        self.entry(seller, base_token).reserved -= base_amount;
        self.entry(seller, quote_token).available += quote_amount;
        self.entry(buyer, quote_token).reserved -= quote_amount;
        self.entry(buyer, base_token).available += base_amount;
    }

    pub fn available(&self, user: Address, token: Address) -> U256 {
        self.balances
            .get(&user)
            .and_then(|m| m.get(&token))
            .map_or(U256::ZERO, |b| b.available)
    }

    pub fn total(&self, user: Address, token: Address) -> U256 {
        self.balances
            .get(&user)
            .and_then(|m| m.get(&token))
            .map_or(U256::ZERO, |b| b.total())
    }

    pub fn set_from_chain(&mut self, user: Address, token: Address, amount: U256) {
        let bal = self.entry(user, token);
        bal.available = amount;
        bal.reserved = U256::ZERO;
    }

    fn entry(&mut self, user: Address, token: Address) -> &mut TokenBalance {
        self.balances
            .entry(user)
            .or_default()
            .entry(token)
            .or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(n: u8) -> Address {
        Address::with_last_byte(n)
    }

    #[test]
    fn credit_increases_available() {
        let mut ledger = Ledger::new();
        let user = addr(1);
        let token = addr(10);
        ledger.credit(user, token, U256::from(100));
        assert_eq!(ledger.available(user, token), U256::from(100));
    }

    #[test]
    fn reserve_moves_to_reserved() {
        let mut ledger = Ledger::new();
        let user = addr(1);
        let token = addr(10);
        ledger.credit(user, token, U256::from(100));
        assert!(ledger.reserve(user, token, U256::from(60)));
        assert_eq!(ledger.available(user, token), U256::from(40));
        assert_eq!(ledger.total(user, token), U256::from(100));
    }

    #[test]
    fn reserve_fails_on_insufficient() {
        let mut ledger = Ledger::new();
        let user = addr(1);
        let token = addr(10);
        ledger.credit(user, token, U256::from(50));
        assert!(!ledger.reserve(user, token, U256::from(60)));
        assert_eq!(ledger.available(user, token), U256::from(50));
    }

    #[test]
    fn release_restores_available() {
        let mut ledger = Ledger::new();
        let user = addr(1);
        let token = addr(10);
        ledger.credit(user, token, U256::from(100));
        ledger.reserve(user, token, U256::from(60));
        ledger.release(user, token, U256::from(60));
        assert_eq!(ledger.available(user, token), U256::from(100));
    }

    #[test]
    fn settle_fill_moves_tokens() {
        let mut ledger = Ledger::new();
        let seller = addr(1);
        let buyer = addr(2);
        let base = addr(10);
        let quote = addr(11);

        ledger.credit(seller, base, U256::from(100));
        ledger.reserve(seller, base, U256::from(10));
        ledger.credit(buyer, quote, U256::from(100));
        ledger.reserve(buyer, quote, U256::from(20));

        ledger.settle_fill(seller, buyer, base, quote, U256::from(10), U256::from(20));

        assert_eq!(ledger.available(seller, quote), U256::from(20));
        assert_eq!(ledger.available(buyer, base), U256::from(10));
        assert_eq!(ledger.total(seller, base), U256::from(90));
        assert_eq!(ledger.total(buyer, quote), U256::from(80));
    }
}
