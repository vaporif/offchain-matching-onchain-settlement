use alloy::primitives::U256;
use types::{EngineOrder, OrderId, OrderType, Side};

use crate::book::OrderBook;

#[derive(Debug, Clone)]
pub struct EngineFill {
    pub maker_id: OrderId,
    pub taker_id: OrderId,
    pub price: U256,
    pub quantity: U256,
}

#[derive(Debug)]
pub struct MatchResult {
    pub fills: Vec<EngineFill>,
    pub resting_qty: U256,
    pub resting: bool,
}

pub struct MatchingEngine {
    book: OrderBook,
    next_id: OrderId,
}

impl Default for MatchingEngine {
    fn default() -> Self {
        Self {
            book: OrderBook::new(),
            next_id: 1,
        }
    }
}

impl MatchingEngine {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn submit(
        &mut self,
        side: Side,
        price: U256,
        quantity: U256,
        order_type: OrderType,
    ) -> MatchResult {
        // Market orders are all-or-nothing: reject upfront if insufficient liquidity
        // to avoid partially consuming the book then discarding fills.
        if order_type == OrderType::Market {
            let available = match side {
                Side::Buy => self.book.total_ask_qty(),
                Side::Sell => self.book.total_bid_qty(),
            };
            if available < quantity {
                return MatchResult {
                    fills: vec![],
                    resting_qty: U256::ZERO,
                    resting: false,
                };
            }
        }

        let id = self.next_id;
        self.next_id += 1;

        let mut order = EngineOrder {
            id,
            side,
            price,
            quantity,
        };

        let mut fills = Vec::new();
        let mut remaining = order.quantity;

        match side {
            Side::Buy => {
                while remaining > U256::ZERO {
                    let Some(best_ask) = self.book.best_ask() else {
                        break;
                    };
                    if order_type == OrderType::Limit && best_ask > order.price {
                        break;
                    }
                    let fill_price = best_ask;

                    let take = self
                        .book
                        .take_best_ask(remaining)
                        .expect("best_ask exists so take must succeed");

                    fills.push(EngineFill {
                        maker_id: take.maker_id,
                        taker_id: id,
                        price: fill_price,
                        quantity: take.fill_qty,
                    });

                    remaining -= take.fill_qty;
                }
            }
            Side::Sell => {
                while remaining > U256::ZERO {
                    let Some(best_bid) = self.book.best_bid() else {
                        break;
                    };
                    if order_type == OrderType::Limit && best_bid < order.price {
                        break;
                    }
                    let fill_price = best_bid;

                    let take = self
                        .book
                        .take_best_bid(remaining)
                        .expect("best_bid exists so take must succeed");

                    fills.push(EngineFill {
                        maker_id: take.maker_id,
                        taker_id: id,
                        price: fill_price,
                        quantity: take.fill_qty,
                    });

                    remaining -= take.fill_qty;
                }
            }
        }

        let resting = remaining > U256::ZERO && order_type == OrderType::Limit;
        if resting {
            order.quantity = remaining;
            self.book.insert(order);
        }

        MatchResult {
            fills,
            resting_qty: remaining,
            resting,
        }
    }

    pub fn cancel(&mut self, order_id: OrderId) -> bool {
        self.book.cancel(order_id).is_some()
    }

    /// Inserts with a pre-existing ID without advancing `next_id`.
    pub fn restore_order(&mut self, id: OrderId, side: Side, price: U256, quantity: U256) {
        let order = EngineOrder {
            id,
            side,
            price,
            quantity,
        };
        self.book.insert(order);
    }

    pub fn best_bid(&self) -> Option<U256> {
        self.book.best_bid()
    }

    pub fn best_ask(&self) -> Option<U256> {
        self.book.best_ask()
    }

    pub fn bid_depth(&self) -> usize {
        self.book.bid_depth()
    }

    pub fn ask_depth(&self) -> usize {
        self.book.ask_depth()
    }

    pub fn last_order_id(&self) -> OrderId {
        self.next_id - 1
    }

    pub fn bid_levels(&self, limit: usize) -> Vec<(U256, U256)> {
        self.book.bid_levels(limit)
    }

    pub fn ask_levels(&self, limit: usize) -> Vec<(U256, U256)> {
        self.book.ask_levels(limit)
    }

    pub fn set_next_id(&mut self, id: u64) {
        self.next_id = id;
    }

    /// Returns true if the order is still resting on the book.
    pub fn has_order(&self, id: OrderId) -> bool {
        self.book.has_order(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limit_order_rests_on_empty_book() {
        let mut engine = MatchingEngine::new();
        let result = engine.submit(Side::Buy, U256::from(100), U256::from(10), OrderType::Limit);
        assert!(result.fills.is_empty());
        assert!(result.resting);
        assert_eq!(result.resting_qty, U256::from(10));
        assert_eq!(engine.best_bid(), Some(U256::from(100)));
    }

    #[test]
    fn crossing_limit_orders_produce_fill() {
        let mut engine = MatchingEngine::new();
        engine.submit(Side::Sell, U256::from(100), U256::from(5), OrderType::Limit);
        let result = engine.submit(Side::Buy, U256::from(100), U256::from(5), OrderType::Limit);
        assert_eq!(result.fills.len(), 1);
        assert_eq!(result.fills[0].quantity, U256::from(5));
        assert_eq!(result.fills[0].price, U256::from(100));
        assert!(!result.resting);
    }

    #[test]
    fn partial_fill_leaves_remainder_on_book() {
        let mut engine = MatchingEngine::new();
        engine.submit(Side::Sell, U256::from(100), U256::from(3), OrderType::Limit);
        let result = engine.submit(Side::Buy, U256::from(100), U256::from(5), OrderType::Limit);
        assert_eq!(result.fills.len(), 1);
        assert_eq!(result.fills[0].quantity, U256::from(3));
        assert!(result.resting);
        assert_eq!(result.resting_qty, U256::from(2));
        assert_eq!(engine.best_bid(), Some(U256::from(100)));
    }

    #[test]
    fn buy_does_not_cross_higher_ask() {
        let mut engine = MatchingEngine::new();
        engine.submit(Side::Sell, U256::from(200), U256::from(5), OrderType::Limit);
        let result = engine.submit(Side::Buy, U256::from(100), U256::from(5), OrderType::Limit);
        assert!(result.fills.is_empty());
        assert!(result.resting);
    }

    #[test]
    fn sell_does_not_cross_lower_bid() {
        let mut engine = MatchingEngine::new();
        engine.submit(Side::Buy, U256::from(100), U256::from(5), OrderType::Limit);
        let result = engine.submit(Side::Sell, U256::from(200), U256::from(5), OrderType::Limit);
        assert!(result.fills.is_empty());
        assert!(result.resting);
    }

    #[test]
    fn fills_at_maker_price() {
        let mut engine = MatchingEngine::new();
        engine.submit(Side::Sell, U256::from(100), U256::from(5), OrderType::Limit);
        let result = engine.submit(Side::Buy, U256::from(110), U256::from(5), OrderType::Limit);
        assert_eq!(result.fills[0].price, U256::from(100));
    }

    #[test]
    fn multi_level_fill() {
        let mut engine = MatchingEngine::new();
        engine.submit(Side::Sell, U256::from(100), U256::from(3), OrderType::Limit);
        engine.submit(Side::Sell, U256::from(101), U256::from(3), OrderType::Limit);
        let result = engine.submit(Side::Buy, U256::from(101), U256::from(5), OrderType::Limit);
        assert_eq!(result.fills.len(), 2);
        assert_eq!(result.fills[0].price, U256::from(100));
        assert_eq!(result.fills[0].quantity, U256::from(3));
        assert_eq!(result.fills[1].price, U256::from(101));
        assert_eq!(result.fills[1].quantity, U256::from(2));
        assert!(!result.resting);
    }

    #[test]
    fn cancel_removes_from_book() {
        let mut engine = MatchingEngine::new();
        engine.submit(Side::Buy, U256::from(100), U256::from(10), OrderType::Limit);
        let id = engine.last_order_id();
        assert!(engine.cancel(id));
        assert_eq!(engine.bid_depth(), 0);
    }

    #[test]
    fn market_buy_fills_completely_or_rejects() {
        let mut engine = MatchingEngine::new();
        engine.submit(Side::Sell, U256::from(100), U256::from(5), OrderType::Limit);
        let result = engine.submit(Side::Buy, U256::ZERO, U256::from(5), OrderType::Market);
        assert_eq!(result.fills.len(), 1);
        assert!(!result.resting);

        let result = engine.submit(Side::Buy, U256::ZERO, U256::from(10), OrderType::Market);
        assert!(result.fills.is_empty());
    }

    #[test]
    fn market_order_does_not_corrupt_book_on_insufficient_liquidity() {
        let mut engine = MatchingEngine::new();
        engine.submit(Side::Sell, U256::from(100), U256::from(3), OrderType::Limit);
        let maker_id = engine.last_order_id();

        let result = engine.submit(Side::Buy, U256::ZERO, U256::from(5), OrderType::Market);
        assert!(result.fills.is_empty());

        assert_eq!(engine.ask_depth(), 1);
        assert_eq!(engine.best_ask(), Some(U256::from(100)));

        let result = engine.submit(Side::Buy, U256::from(100), U256::from(3), OrderType::Limit);
        assert_eq!(result.fills.len(), 1);
        assert_eq!(result.fills[0].maker_id, maker_id);
    }

    #[test]
    fn restore_order_preserves_id() {
        let mut engine = MatchingEngine::new();
        engine.restore_order(5, Side::Sell, U256::from(100), U256::from(10));
        assert!(engine.has_order(5));
        // The next submitted order gets ID 1 (not 6), since restore doesn't advance next_id
        let result = engine.submit(Side::Buy, U256::from(100), U256::from(5), OrderType::Limit);
        assert_eq!(result.fills[0].maker_id, 5);
        assert_eq!(result.fills[0].taker_id, 1);
    }

    #[test]
    fn restore_order_is_matchable() {
        let mut engine = MatchingEngine::new();
        engine.restore_order(10, Side::Sell, U256::from(100), U256::from(20));
        let result = engine.submit(Side::Buy, U256::from(100), U256::from(5), OrderType::Limit);
        assert_eq!(result.fills.len(), 1);
        assert_eq!(result.fills[0].maker_id, 10);
        assert_eq!(result.fills[0].quantity, U256::from(5));
    }

    #[test]
    fn fifo_within_same_price() {
        let mut engine = MatchingEngine::new();
        engine.submit(Side::Sell, U256::from(100), U256::from(1), OrderType::Limit);
        let first_id = engine.last_order_id();
        engine.submit(Side::Sell, U256::from(100), U256::from(1), OrderType::Limit);
        let result = engine.submit(Side::Buy, U256::from(100), U256::from(1), OrderType::Limit);
        assert_eq!(result.fills[0].maker_id, first_id);
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    prop_compose! {
        fn arb_order()(
            side in prop_oneof![Just(Side::Buy), Just(Side::Sell)],
            price in 1u64..10000,
            qty in 1u64..1000,
        ) -> (Side, u64, u64) {
            (side, price, qty)
        }
    }

    proptest! {
        #[test]
        fn no_negative_depth(orders in proptest::collection::vec(arb_order(), 1..50)) {
            let mut engine = MatchingEngine::new();
            for (side, price, qty) in orders {
                engine.submit(side, U256::from(price), U256::from(qty), OrderType::Limit);
            }
            let _ = engine.bid_depth();
            let _ = engine.ask_depth();
        }

        #[test]
        fn best_bid_below_best_ask(orders in proptest::collection::vec(arb_order(), 1..100)) {
            let mut engine = MatchingEngine::new();
            for (side, price, qty) in orders {
                engine.submit(side, U256::from(price), U256::from(qty), OrderType::Limit);
            }
            if let (Some(bid), Some(ask)) = (engine.best_bid(), engine.best_ask()) {
                prop_assert!(bid < ask, "bid {} >= ask {}", bid, ask);
            }
        }

        #[test]
        fn fill_price_within_both_limits(
            maker_price in 50u64..150,
            taker_price in 50u64..150,
            qty in 1u64..100,
        ) {
            let mut engine = MatchingEngine::new();
            engine.submit(Side::Sell, U256::from(maker_price), U256::from(qty), OrderType::Limit);
            let result = engine.submit(Side::Buy, U256::from(taker_price), U256::from(qty), OrderType::Limit);
            for fill in &result.fills {
                prop_assert_eq!(fill.price, U256::from(maker_price));
                prop_assert!(fill.price >= U256::from(maker_price));
                prop_assert!(fill.price <= U256::from(taker_price));
            }
        }
    }
}
