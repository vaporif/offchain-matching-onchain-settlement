use std::collections::{BTreeMap, HashMap, VecDeque};

use alloy::primitives::U256;
use types::{EngineOrder, OrderId, Side};

#[derive(Debug, Clone)]
struct OrderLocation {
    side: Side,
    price: U256,
}

pub(crate) struct TakeResult {
    pub maker_id: OrderId,
    pub fill_qty: U256,
}

/// Price-time priority book.
#[derive(Default)]
pub struct OrderBook {
    bids: BTreeMap<U256, VecDeque<EngineOrder>>,
    asks: BTreeMap<U256, VecDeque<EngineOrder>>,
    locations: HashMap<OrderId, OrderLocation>,
}

impl OrderBook {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, order: EngineOrder) {
        let location = OrderLocation {
            side: order.side,
            price: order.price,
        };
        let queue = match order.side {
            Side::Buy => self.bids.entry(order.price).or_default(),
            Side::Sell => self.asks.entry(order.price).or_default(),
        };
        queue.push_back(order);
        self.locations
            .insert(queue.back().expect("just pushed").id, location);
    }

    pub fn cancel(&mut self, order_id: OrderId) -> Option<EngineOrder> {
        let location = self.locations.remove(&order_id)?;
        let queue = match location.side {
            Side::Buy => self.bids.get_mut(&location.price)?,
            Side::Sell => self.asks.get_mut(&location.price)?,
        };
        let pos = queue.iter().position(|o| o.id == order_id)?;
        let order = queue.remove(pos)?;
        if queue.is_empty() {
            match location.side {
                Side::Buy => self.bids.remove(&location.price),
                Side::Sell => self.asks.remove(&location.price),
            };
        }
        Some(order)
    }

    pub fn best_bid(&self) -> Option<U256> {
        self.bids.last_key_value().map(|(p, _)| *p)
    }

    pub fn best_ask(&self) -> Option<U256> {
        self.asks.first_key_value().map(|(p, _)| *p)
    }

    /// Fill up to `max_qty` from best ask.
    pub(crate) fn take_best_ask(&mut self, max_qty: U256) -> Option<TakeResult> {
        let best_price = *self.asks.first_key_value()?.0;
        let queue = self.asks.get_mut(&best_price)?;
        let maker = queue.front_mut()?;

        let fill_qty = max_qty.min(maker.quantity);
        let maker_id = maker.id;
        maker.quantity -= fill_qty;

        if maker.quantity == U256::ZERO {
            queue.pop_front();
            self.locations.remove(&maker_id);
            if queue.is_empty() {
                self.asks.remove(&best_price);
            }
        }

        Some(TakeResult { maker_id, fill_qty })
    }

    /// Fill up to `max_qty` from best bid.
    pub(crate) fn take_best_bid(&mut self, max_qty: U256) -> Option<TakeResult> {
        let best_price = *self.bids.last_key_value()?.0;
        let queue = self.bids.get_mut(&best_price)?;
        let maker = queue.front_mut()?;

        let fill_qty = max_qty.min(maker.quantity);
        let maker_id = maker.id;
        maker.quantity -= fill_qty;

        if maker.quantity == U256::ZERO {
            queue.pop_front();
            self.locations.remove(&maker_id);
            if queue.is_empty() {
                self.bids.remove(&best_price);
            }
        }

        Some(TakeResult { maker_id, fill_qty })
    }

    #[must_use]
    pub fn total_ask_qty(&self) -> U256 {
        self.asks
            .values()
            .flatten()
            .map(|o| o.quantity)
            .fold(U256::ZERO, |acc, q| acc + q)
    }

    #[must_use]
    pub fn total_bid_qty(&self) -> U256 {
        self.bids
            .values()
            .flatten()
            .map(|o| o.quantity)
            .fold(U256::ZERO, |acc, q| acc + q)
    }

    pub fn bid_depth(&self) -> usize {
        self.bids.values().map(VecDeque::len).sum()
    }

    pub fn ask_depth(&self) -> usize {
        self.asks.values().map(VecDeque::len).sum()
    }

    /// Aggregated bid levels, highest price first.
    #[must_use]
    pub fn bid_levels(&self, limit: usize) -> Vec<(U256, U256)> {
        self.bids
            .iter()
            .rev()
            .take(limit)
            .map(|(price, queue)| {
                let qty = queue
                    .iter()
                    .map(|o| o.quantity)
                    .fold(U256::ZERO, |acc, q| acc + q);
                (*price, qty)
            })
            .collect()
    }

    /// Returns true if the given order is still on the book.
    #[must_use]
    pub fn has_order(&self, id: OrderId) -> bool {
        self.locations.contains_key(&id)
    }

    /// Aggregated ask levels, lowest price first.
    #[must_use]
    pub fn ask_levels(&self, limit: usize) -> Vec<(U256, U256)> {
        self.asks
            .iter()
            .take(limit)
            .map(|(price, queue)| {
                let qty = queue
                    .iter()
                    .map(|o| o.quantity)
                    .fold(U256::ZERO, |acc, q| acc + q);
                (*price, qty)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_order(id: OrderId, side: Side, price: u64, qty: u64) -> EngineOrder {
        EngineOrder {
            id,
            side,
            price: U256::from(price),
            quantity: U256::from(qty),
        }
    }

    #[test]
    fn empty_book_has_no_best() {
        let book = OrderBook::new();
        assert_eq!(book.best_bid(), None);
        assert_eq!(book.best_ask(), None);
    }

    #[test]
    fn insert_bid_updates_best() {
        let mut book = OrderBook::new();
        book.insert(make_order(1, Side::Buy, 100, 10));
        assert_eq!(book.best_bid(), Some(U256::from(100)));
        assert_eq!(book.best_ask(), None);
    }

    #[test]
    fn insert_ask_updates_best() {
        let mut book = OrderBook::new();
        book.insert(make_order(1, Side::Sell, 200, 5));
        assert_eq!(book.best_ask(), Some(U256::from(200)));
        assert_eq!(book.best_bid(), None);
    }

    #[test]
    fn best_bid_is_highest() {
        let mut book = OrderBook::new();
        book.insert(make_order(1, Side::Buy, 100, 10));
        book.insert(make_order(2, Side::Buy, 150, 10));
        book.insert(make_order(3, Side::Buy, 120, 10));
        assert_eq!(book.best_bid(), Some(U256::from(150)));
    }

    #[test]
    fn best_ask_is_lowest() {
        let mut book = OrderBook::new();
        book.insert(make_order(1, Side::Sell, 300, 5));
        book.insert(make_order(2, Side::Sell, 200, 5));
        book.insert(make_order(3, Side::Sell, 250, 5));
        assert_eq!(book.best_ask(), Some(U256::from(200)));
    }

    #[test]
    fn cancel_removes_order() {
        let mut book = OrderBook::new();
        book.insert(make_order(1, Side::Buy, 100, 10));
        assert_eq!(book.bid_depth(), 1);
        let cancelled = book.cancel(1);
        assert!(cancelled.is_some());
        assert_eq!(book.bid_depth(), 0);
        assert_eq!(book.best_bid(), None);
    }

    #[test]
    fn cancel_nonexistent_returns_none() {
        let mut book = OrderBook::new();
        assert!(book.cancel(999).is_none());
    }

    #[test]
    fn price_time_priority_fifo_within_price() {
        let mut book = OrderBook::new();
        book.insert(make_order(1, Side::Sell, 200, 5));
        book.insert(make_order(2, Side::Sell, 200, 3));
        let queue = book.asks.get(&U256::from(200)).expect("level exists");
        assert_eq!(queue[0].id, 1);
        assert_eq!(queue[1].id, 2);
    }

    #[test]
    fn bid_levels_returns_highest_first() {
        let mut book = OrderBook::new();
        book.insert(make_order(1, Side::Buy, 100, 10));
        book.insert(make_order(2, Side::Buy, 200, 5));
        book.insert(make_order(3, Side::Buy, 100, 3)); // same price as order 1

        let levels = book.bid_levels(10);
        assert_eq!(levels.len(), 2);
        assert_eq!(levels[0], (U256::from(200), U256::from(5)));
        assert_eq!(levels[1], (U256::from(100), U256::from(13)));
    }

    #[test]
    fn ask_levels_returns_lowest_first() {
        let mut book = OrderBook::new();
        book.insert(make_order(1, Side::Sell, 300, 7));
        book.insert(make_order(2, Side::Sell, 200, 4));
        book.insert(make_order(3, Side::Sell, 200, 6)); // same price as order 2

        let levels = book.ask_levels(10);
        assert_eq!(levels.len(), 2);
        assert_eq!(levels[0], (U256::from(200), U256::from(10)));
        assert_eq!(levels[1], (U256::from(300), U256::from(7)));
    }

    #[test]
    fn levels_respects_limit() {
        let mut book = OrderBook::new();
        book.insert(make_order(1, Side::Buy, 100, 10));
        book.insert(make_order(2, Side::Buy, 200, 5));
        book.insert(make_order(3, Side::Buy, 300, 1));

        let levels = book.bid_levels(2);
        assert_eq!(levels.len(), 2);
        assert_eq!(levels[0].0, U256::from(300));
        assert_eq!(levels[1].0, U256::from(200));
    }

    #[test]
    fn levels_empty_book() {
        let book = OrderBook::new();
        assert!(book.bid_levels(10).is_empty());
        assert!(book.ask_levels(10).is_empty());
    }
}
