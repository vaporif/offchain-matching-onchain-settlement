// Internal crate -- doc sections for errors/panics are noise
#![allow(clippy::missing_errors_doc, clippy::missing_panics_doc)]

pub mod migrations;
pub mod serialization;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use alloy::primitives::{Address, B256, U256};
use eyre::{Result, WrapErr};
use rusqlite::OptionalExtension;
use types::{SignedOrder, Trade};

use crate::serialization::{
    address_from_bytes, address_to_bytes, b256_from_bytes, b256_to_bytes, u256_from_bytes,
    u256_to_bytes,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BalanceRow {
    pub available: U256,
    pub reserved: U256,
}

pub struct Db {
    conn: Mutex<rusqlite::Connection>,
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .wrap_err_with(|| format!("creating db directory: {}", parent.display()))?;
        }
        let conn = rusqlite::Connection::open(path)
            .wrap_err_with(|| format!("opening database: {}", path.display()))?;
        conn.execute_batch("PRAGMA journal_mode=WAL")
            .wrap_err("enabling WAL mode")?;
        migrations::run(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    // -- Sync State --

    pub fn last_synced_block(&self) -> Result<Option<u64>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare("SELECT value FROM sync_state WHERE key = 'last_synced_block'")
            .wrap_err("preparing last_synced_block query")?;
        let result = stmt
            .query_row([], |row| row.get::<_, i64>(0))
            .optional()
            .wrap_err("querying last_synced_block")?;
        result
            .map(|v| u64::try_from(v).wrap_err("stored block number is negative"))
            .transpose()
    }

    pub fn set_last_synced_block(&self, block: u64) -> Result<()> {
        let value =
            i64::try_from(block).wrap_err("block number too large for SQLite i64 storage")?;
        let conn = self.lock();
        conn.execute(
            "INSERT OR REPLACE INTO sync_state (key, value) VALUES ('last_synced_block', ?1)",
            [value],
        )
        .wrap_err("updating last_synced_block")?;
        Ok(())
    }

    // -- Ledger --

    pub fn save_balance(
        &self,
        user: Address,
        token: Address,
        available: U256,
        reserved: U256,
    ) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT OR REPLACE INTO ledger_balances (user_address, token_address, available, reserved)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                address_to_bytes(user).as_slice(),
                address_to_bytes(token).as_slice(),
                u256_to_bytes(available).as_slice(),
                u256_to_bytes(reserved).as_slice(),
            ],
        )
        .wrap_err("saving balance")?;
        Ok(())
    }

    #[allow(clippy::type_complexity)] // nested HashMap is the natural shape here
    pub fn load_all_balances(&self) -> Result<HashMap<Address, HashMap<Address, BalanceRow>>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare("SELECT user_address, token_address, available, reserved FROM ledger_balances")
            .wrap_err("preparing load_all_balances query")?;
        let rows = stmt
            .query_map([], |row| {
                let user: Vec<u8> = row.get(0)?;
                let token: Vec<u8> = row.get(1)?;
                let available: Vec<u8> = row.get(2)?;
                let reserved: Vec<u8> = row.get(3)?;
                Ok((user, token, available, reserved))
            })
            .wrap_err("querying balances")?;

        let mut result: HashMap<Address, HashMap<Address, BalanceRow>> = HashMap::new();
        for row in rows {
            let (user, token, available, reserved) = row.wrap_err("reading balance row")?;
            let user_addr = address_from_bytes(&user);
            let token_addr = address_from_bytes(&token);
            let balance = BalanceRow {
                available: u256_from_bytes(&available),
                reserved: u256_from_bytes(&reserved),
            };
            result
                .entry(user_addr)
                .or_default()
                .insert(token_addr, balance);
        }
        Ok(result)
    }

    // -- Orders --

    pub fn save_order(
        &self,
        order_id: u64,
        maker: Address,
        signed_order: &SignedOrder,
        status: &str,
    ) -> Result<()> {
        let id = i64::try_from(order_id).expect("order_id exceeds i64::MAX");
        let blob = bincode::serialize(signed_order).wrap_err("serializing signed order")?;
        let now = now_secs();
        let conn = self.lock();
        conn.execute(
            "INSERT OR REPLACE INTO orders (order_id, maker_address, signed_order, status, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                id,
                address_to_bytes(maker).as_slice(),
                blob,
                status,
                now,
                now,
            ],
        )
        .wrap_err("saving order")?;
        Ok(())
    }

    pub fn update_order_status(&self, order_id: u64, status: &str) -> Result<()> {
        let id = i64::try_from(order_id).expect("order_id exceeds i64::MAX");
        let now = now_secs();
        let conn = self.lock();
        conn.execute(
            "UPDATE orders SET status = ?1, updated_at = ?2 WHERE order_id = ?3",
            rusqlite::params![status, now, id],
        )
        .wrap_err("updating order status")?;
        Ok(())
    }

    /// Sets all orders with status 'resting' to 'cancelled'. Returns the count.
    pub fn cancel_all_resting_orders(&self) -> Result<u64> {
        let now = now_secs();
        let conn = self.lock();
        let count = conn
            .execute(
                "UPDATE orders SET status = 'cancelled', updated_at = ?1 WHERE status = 'resting'",
                [now],
            )
            .wrap_err("cancelling resting orders")?;
        Ok(count as u64)
    }

    pub fn load_max_order_id(&self) -> Result<Option<u64>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare("SELECT MAX(order_id) FROM orders")
            .wrap_err("preparing load_max_order_id query")?;
        let result = stmt
            .query_row([], |row| row.get::<_, Option<i64>>(0))
            .optional()
            .wrap_err("querying max order_id")?;
        // query_row always returns a row for MAX(), but the value is NULL if table is empty
        Ok(result
            .flatten()
            .map(|v| u64::try_from(v).expect("stored order_id is negative")))
    }

    // -- Pending Trades --

    pub fn save_pending_trade(&self, trade: &Trade) -> Result<i64> {
        let blob = bincode::serialize(trade).wrap_err("serializing trade")?;
        let now = now_secs();
        let conn = self.lock();
        conn.execute(
            "INSERT INTO pending_trades (trade_data, status, created_at) VALUES (?1, 'pending', ?2)",
            rusqlite::params![blob, now],
        )
        .wrap_err("saving pending trade")?;
        Ok(conn.last_insert_rowid())
    }

    pub fn load_pending_trades(&self) -> Result<Vec<(i64, Trade)>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT trade_id, trade_data FROM pending_trades WHERE status IN ('pending', 'submitted')",
            )
            .wrap_err("preparing load_pending_trades query")?;
        let rows = stmt
            .query_map([], |row| {
                let id: i64 = row.get(0)?;
                let blob: Vec<u8> = row.get(1)?;
                Ok((id, blob))
            })
            .wrap_err("querying pending trades")?;

        let mut result = Vec::new();
        for row in rows {
            let (id, blob) = row.wrap_err("reading pending trade row")?;
            let trade: Trade =
                bincode::deserialize(&blob).wrap_err("deserializing pending trade")?;
            result.push((id, trade));
        }
        Ok(result)
    }

    pub fn mark_trades_submitted(&self, trade_ids: &[i64], tx_hash: B256) -> Result<()> {
        let conn = self.lock();
        let hash_bytes = b256_to_bytes(tx_hash);
        for &id in trade_ids {
            conn.execute(
                "UPDATE pending_trades SET status = 'submitted', batch_tx_hash = ?1 WHERE trade_id = ?2",
                rusqlite::params![hash_bytes.as_slice(), id],
            )
            .wrap_err("marking trade submitted")?;
        }
        Ok(())
    }

    pub fn mark_trades_confirmed(&self, trade_ids: &[i64]) -> Result<()> {
        let conn = self.lock();
        for &id in trade_ids {
            conn.execute(
                "UPDATE pending_trades SET status = 'confirmed' WHERE trade_id = ?1",
                [id],
            )
            .wrap_err("marking trade confirmed")?;
        }
        Ok(())
    }

    pub fn delete_confirmed_trades(&self) -> Result<()> {
        let conn = self.lock();
        conn.execute("DELETE FROM pending_trades WHERE status = 'confirmed'", [])
            .wrap_err("deleting confirmed trades")?;
        Ok(())
    }

    // -- Nonces --

    pub fn save_nonce(&self, nonce: B256, expires_at: u64) -> Result<()> {
        let exp = i64::try_from(expires_at).expect("expires_at exceeds i64::MAX");
        let conn = self.lock();
        conn.execute(
            "INSERT OR IGNORE INTO nonces (nonce, expires_at) VALUES (?1, ?2)",
            rusqlite::params![b256_to_bytes(nonce).as_slice(), exp],
        )
        .wrap_err("saving nonce")?;
        Ok(())
    }

    pub fn nonce_exists(&self, nonce: B256) -> Result<bool> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare("SELECT 1 FROM nonces WHERE nonce = ?1")
            .wrap_err("preparing nonce_exists query")?;
        let exists = stmt
            .query_row(rusqlite::params![b256_to_bytes(nonce).as_slice()], |_| {
                Ok(())
            })
            .optional()
            .wrap_err("querying nonce")?;
        Ok(exists.is_some())
    }

    /// Deletes nonces where `expires_at < now`. Returns the count deleted.
    pub fn prune_expired_nonces(&self, now: u64) -> Result<u64> {
        let now_i = i64::try_from(now).expect("now exceeds i64::MAX");
        let conn = self.lock();
        let count = conn
            .execute("DELETE FROM nonces WHERE expires_at < ?1", [now_i])
            .wrap_err("pruning expired nonces")?;
        Ok(count as u64)
    }

    /// Returns all nonces that haven't expired yet (`expires_at >= now`).
    pub fn load_unexpired_nonces(&self, now: u64) -> Result<Vec<B256>> {
        let now_i = i64::try_from(now).expect("now exceeds i64::MAX");
        let conn = self.lock();
        let mut stmt = conn
            .prepare("SELECT nonce FROM nonces WHERE expires_at >= ?1")
            .wrap_err("preparing load_unexpired_nonces")?;
        let rows = stmt
            .query_map([now_i], |row| {
                let blob: Vec<u8> = row.get(0)?;
                Ok(blob)
            })
            .wrap_err("querying unexpired nonces")?;
        let mut result = Vec::new();
        for row in rows {
            let blob = row.wrap_err("reading nonce row")?;
            result.push(b256_from_bytes(&blob));
        }
        Ok(result)
    }

    // -- Atomic Operations --

    /// Atomically persists everything from a single order fill:
    /// nonce, trades, balance updates, filled maker status changes, and optional new resting order.
    /// Returns the assigned `trade_id` values.
    #[allow(clippy::too_many_arguments)]
    pub fn save_order_fill(
        &self,
        nonce: B256,
        nonce_expires_at: u64,
        trades: &[Trade],
        balance_updates: &[(Address, Address, U256, U256)],
        filled_maker_ids: &[u64],
        resting_order: Option<(u64, Address, &SignedOrder)>,
    ) -> Result<Vec<i64>> {
        let conn = self.lock();

        // unchecked_transaction avoids lifetime issues with rusqlite's Transaction type
        conn.execute_batch("BEGIN")
            .wrap_err("beginning transaction")?;

        let result = (|| -> Result<Vec<i64>> {
            // 1. Save nonce
            let exp = i64::try_from(nonce_expires_at).expect("nonce_expires_at exceeds i64::MAX");
            conn.execute(
                "INSERT OR IGNORE INTO nonces (nonce, expires_at) VALUES (?1, ?2)",
                rusqlite::params![b256_to_bytes(nonce).as_slice(), exp],
            )
            .wrap_err("saving nonce in fill")?;

            // 2. Save trades
            let now = now_secs();
            let mut trade_ids = Vec::with_capacity(trades.len());
            for trade in trades {
                let blob = bincode::serialize(trade).wrap_err("serializing trade in fill")?;
                conn.execute(
                    "INSERT INTO pending_trades (trade_data, status, created_at) VALUES (?1, 'pending', ?2)",
                    rusqlite::params![blob, now],
                )
                .wrap_err("saving trade in fill")?;
                trade_ids.push(conn.last_insert_rowid());
            }

            // 3. Balance updates
            for &(user, token, available, reserved) in balance_updates {
                conn.execute(
                    "INSERT OR REPLACE INTO ledger_balances (user_address, token_address, available, reserved)
                     VALUES (?1, ?2, ?3, ?4)",
                    rusqlite::params![
                        address_to_bytes(user).as_slice(),
                        address_to_bytes(token).as_slice(),
                        u256_to_bytes(available).as_slice(),
                        u256_to_bytes(reserved).as_slice(),
                    ],
                )
                .wrap_err("saving balance in fill")?;
            }

            // 4. Mark filled makers
            for &maker_id in filled_maker_ids {
                let id = i64::try_from(maker_id).expect("maker order_id exceeds i64::MAX");
                conn.execute(
                    "UPDATE orders SET status = 'filled', updated_at = ?1 WHERE order_id = ?2",
                    rusqlite::params![now, id],
                )
                .wrap_err("marking maker filled")?;
            }

            // 5. Optional resting order
            if let Some((order_id, maker, signed_order)) = resting_order {
                let id = i64::try_from(order_id).expect("resting order_id exceeds i64::MAX");
                let blob =
                    bincode::serialize(signed_order).wrap_err("serializing resting order")?;
                conn.execute(
                    "INSERT OR REPLACE INTO orders (order_id, maker_address, signed_order, status, created_at, updated_at)
                     VALUES (?1, ?2, ?3, 'resting', ?4, ?5)",
                    rusqlite::params![
                        id,
                        address_to_bytes(maker).as_slice(),
                        blob,
                        now,
                        now,
                    ],
                )
                .wrap_err("saving resting order in fill")?;
            }

            Ok(trade_ids)
        })();

        match &result {
            Ok(_) => conn
                .execute_batch("COMMIT")
                .wrap_err("committing fill transaction")?,
            Err(_) => {
                if let Err(rb_err) = conn.execute_batch("ROLLBACK") {
                    tracing::error!(%rb_err, "rollback failed after fill error");
                }
            }
        }

        result
    }

    #[allow(clippy::missing_panics_doc)] // mutex poisoning is unrecoverable
    fn lock(&self) -> std::sync::MutexGuard<'_, rusqlite::Connection> {
        self.conn
            .lock()
            .expect("database mutex poisoned -- a prior operation panicked")
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_secs()
        .cast_signed()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use alloy::primitives::Bytes;
    use tempfile::TempDir;
    use types::Side;

    fn temp_db() -> (TempDir, Db) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.db");
        let db = Db::open(&path).unwrap();
        (dir, db)
    }

    fn dummy_signed_order() -> SignedOrder {
        SignedOrder {
            side: Side::Buy,
            maker: Address::ZERO,
            base_token: Address::from([0x01; 20]),
            quote_token: Address::from([0x02; 20]),
            price: U256::from(2000),
            quantity: U256::from(1),
            nonce: U256::from(42),
            expiry: U256::from(u64::MAX),
            signature: Bytes::new(),
        }
    }

    fn dummy_trade() -> Trade {
        Trade {
            maker_order: dummy_signed_order(),
            taker_order: dummy_signed_order(),
            price: U256::from(2000),
            quantity: U256::from(1),
            timestamp: 1_000_000,
        }
    }

    // -- Sync State --

    #[test]
    fn sync_state_empty_returns_none() {
        let (_dir, db) = temp_db();
        assert_eq!(db.last_synced_block().unwrap(), None);
    }

    #[test]
    fn sync_state_set_and_get() {
        let (_dir, db) = temp_db();
        db.set_last_synced_block(42).unwrap();
        assert_eq!(db.last_synced_block().unwrap(), Some(42));
    }

    #[test]
    fn sync_state_overwrites() {
        let (_dir, db) = temp_db();
        db.set_last_synced_block(10).unwrap();
        db.set_last_synced_block(20).unwrap();
        assert_eq!(db.last_synced_block().unwrap(), Some(20));
    }

    // -- Balances --

    #[test]
    fn balances_save_and_load() {
        let (_dir, db) = temp_db();
        let user = Address::from([0xAA; 20]);
        let token = Address::from([0xBB; 20]);
        db.save_balance(user, token, U256::from(100), U256::from(10))
            .unwrap();

        let all = db.load_all_balances().unwrap();
        let row = &all[&user][&token];
        assert_eq!(row.available, U256::from(100));
        assert_eq!(row.reserved, U256::from(10));
    }

    #[test]
    fn balances_upsert() {
        let (_dir, db) = temp_db();
        let user = Address::from([0xAA; 20]);
        let token = Address::from([0xBB; 20]);
        db.save_balance(user, token, U256::from(100), U256::from(10))
            .unwrap();
        db.save_balance(user, token, U256::from(200), U256::from(20))
            .unwrap();

        let all = db.load_all_balances().unwrap();
        let row = &all[&user][&token];
        assert_eq!(row.available, U256::from(200));
        assert_eq!(row.reserved, U256::from(20));
    }

    #[test]
    fn balances_empty() {
        let (_dir, db) = temp_db();
        let all = db.load_all_balances().unwrap();
        assert!(all.is_empty());
    }

    #[test]
    fn balances_multiple_users_tokens() {
        let (_dir, db) = temp_db();
        let user1 = Address::from([0x01; 20]);
        let user2 = Address::from([0x02; 20]);
        let token_a = Address::from([0xA0; 20]);
        let token_b = Address::from([0xB0; 20]);

        db.save_balance(user1, token_a, U256::from(10), U256::from(1))
            .unwrap();
        db.save_balance(user1, token_b, U256::from(20), U256::from(2))
            .unwrap();
        db.save_balance(user2, token_a, U256::from(30), U256::from(3))
            .unwrap();

        let all = db.load_all_balances().unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[&user1].len(), 2);
        assert_eq!(all[&user2].len(), 1);
        assert_eq!(all[&user1][&token_a].available, U256::from(10));
        assert_eq!(all[&user1][&token_b].available, U256::from(20));
        assert_eq!(all[&user2][&token_a].available, U256::from(30));
    }

    // -- Orders --

    #[test]
    fn orders_save_and_load_max_id() {
        let (_dir, db) = temp_db();
        assert_eq!(db.load_max_order_id().unwrap(), None);

        db.save_order(1, Address::ZERO, &dummy_signed_order(), "resting")
            .unwrap();
        db.save_order(5, Address::ZERO, &dummy_signed_order(), "resting")
            .unwrap();
        db.save_order(3, Address::ZERO, &dummy_signed_order(), "filled")
            .unwrap();

        assert_eq!(db.load_max_order_id().unwrap(), Some(5));
    }

    #[test]
    fn orders_cancel_all_resting() {
        let (_dir, db) = temp_db();
        db.save_order(1, Address::ZERO, &dummy_signed_order(), "resting")
            .unwrap();
        db.save_order(2, Address::ZERO, &dummy_signed_order(), "resting")
            .unwrap();
        db.save_order(3, Address::ZERO, &dummy_signed_order(), "filled")
            .unwrap();

        let count = db.cancel_all_resting_orders().unwrap();
        assert_eq!(count, 2);

        // Calling again cancels none
        let count = db.cancel_all_resting_orders().unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn orders_update_status() {
        let (_dir, db) = temp_db();
        db.save_order(1, Address::ZERO, &dummy_signed_order(), "resting")
            .unwrap();
        db.update_order_status(1, "filled").unwrap();

        // Verify it's no longer resting
        let count = db.cancel_all_resting_orders().unwrap();
        assert_eq!(count, 0);
    }

    // -- Pending Trades --

    #[test]
    fn pending_trades_save_and_load() {
        let (_dir, db) = temp_db();
        let trade = dummy_trade();
        let id = db.save_pending_trade(&trade).unwrap();
        assert!(id > 0);

        let loaded = db.load_pending_trades().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].0, id);
        assert_eq!(loaded[0].1.price, trade.price);
        assert_eq!(loaded[0].1.quantity, trade.quantity);
    }

    #[test]
    fn pending_trades_confirmed_not_loaded() {
        let (_dir, db) = temp_db();
        let id = db.save_pending_trade(&dummy_trade()).unwrap();
        db.mark_trades_confirmed(&[id]).unwrap();

        let loaded = db.load_pending_trades().unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn pending_trades_submitted_still_loaded() {
        let (_dir, db) = temp_db();
        let id = db.save_pending_trade(&dummy_trade()).unwrap();
        db.mark_trades_submitted(&[id], B256::from([0xFF; 32]))
            .unwrap();

        let loaded = db.load_pending_trades().unwrap();
        assert_eq!(loaded.len(), 1);
    }

    #[test]
    fn pending_trades_delete_confirmed() {
        let (_dir, db) = temp_db();
        let id1 = db.save_pending_trade(&dummy_trade()).unwrap();
        let id2 = db.save_pending_trade(&dummy_trade()).unwrap();
        db.mark_trades_confirmed(&[id1]).unwrap();
        db.delete_confirmed_trades().unwrap();

        // id2 is still pending, id1 is gone
        let loaded = db.load_pending_trades().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].0, id2);
    }

    // -- Nonces --

    #[test]
    fn nonces_save_and_exists() {
        let (_dir, db) = temp_db();
        let nonce = B256::from([0x11; 32]);
        db.save_nonce(nonce, 1000).unwrap();
        assert!(db.nonce_exists(nonce).unwrap());
    }

    #[test]
    fn nonces_not_exists() {
        let (_dir, db) = temp_db();
        assert!(!db.nonce_exists(B256::from([0x99; 32])).unwrap());
    }

    #[test]
    fn nonces_prune_expired() {
        let (_dir, db) = temp_db();
        db.save_nonce(B256::from([0x01; 32]), 100).unwrap();
        db.save_nonce(B256::from([0x02; 32]), 200).unwrap();
        db.save_nonce(B256::from([0x03; 32]), 300).unwrap();

        let pruned = db.prune_expired_nonces(250).unwrap();
        assert_eq!(pruned, 2);

        // The one at 300 survives
        assert!(db.nonce_exists(B256::from([0x03; 32])).unwrap());
        assert!(!db.nonce_exists(B256::from([0x01; 32])).unwrap());
    }

    #[test]
    fn nonces_duplicate_insert_ignored() {
        let (_dir, db) = temp_db();
        let nonce = B256::from([0xAA; 32]);
        db.save_nonce(nonce, 100).unwrap();
        // Should not error
        db.save_nonce(nonce, 200).unwrap();
        assert!(db.nonce_exists(nonce).unwrap());
    }

    #[test]
    fn load_unexpired_nonces_returns_valid_only() {
        let (_dir, db) = temp_db();
        db.save_nonce(B256::from([0x01; 32]), 100).unwrap();
        db.save_nonce(B256::from([0x02; 32]), 200).unwrap();
        db.save_nonce(B256::from([0x03; 32]), 300).unwrap();

        let nonces = db.load_unexpired_nonces(200).unwrap();
        assert_eq!(nonces.len(), 2);
        assert!(nonces.contains(&B256::from([0x02; 32])));
        assert!(nonces.contains(&B256::from([0x03; 32])));
    }

    #[test]
    fn load_unexpired_nonces_empty_table() {
        let (_dir, db) = temp_db();
        let nonces = db.load_unexpired_nonces(0).unwrap();
        assert!(nonces.is_empty());
    }

    // -- Atomic: save_order_fill --

    #[test]
    fn atomic_save_order_fill() {
        let (_dir, db) = temp_db();
        let nonce = B256::from([0xDD; 32]);
        let trade = dummy_trade();
        let user = Address::from([0xAA; 20]);
        let token = Address::from([0xBB; 20]);

        // Pre-insert a maker order to be filled
        db.save_order(10, Address::ZERO, &dummy_signed_order(), "resting")
            .unwrap();

        let resting = dummy_signed_order();
        let trade_ids = db
            .save_order_fill(
                nonce,
                5000,
                &[trade],
                &[(user, token, U256::from(50), U256::from(5))],
                &[10],
                Some((20, Address::from([0xCC; 20]), &resting)),
            )
            .unwrap();

        assert_eq!(trade_ids.len(), 1);

        // Verify nonce saved
        assert!(db.nonce_exists(nonce).unwrap());

        // Verify trade saved
        let trades = db.load_pending_trades().unwrap();
        assert_eq!(trades.len(), 1);
        assert_eq!(trades[0].0, trade_ids[0]);

        // Verify balance saved
        let balances = db.load_all_balances().unwrap();
        assert_eq!(balances[&user][&token].available, U256::from(50));

        // Verify maker marked filled (no longer resting)
        let cancelled = db.cancel_all_resting_orders().unwrap();
        // Only the new resting order (id=20) should be resting, id=10 was filled
        assert_eq!(cancelled, 1);

        // Verify resting order saved
        assert_eq!(db.load_max_order_id().unwrap(), Some(20));
    }

    #[test]
    fn atomic_save_order_fill_marks_filled_makers() {
        let (_dir, db) = temp_db();
        db.save_order(1, Address::ZERO, &dummy_signed_order(), "resting")
            .unwrap();
        db.save_order(2, Address::ZERO, &dummy_signed_order(), "resting")
            .unwrap();
        db.save_order(3, Address::ZERO, &dummy_signed_order(), "resting")
            .unwrap();

        let _ = db
            .save_order_fill(
                B256::from([0x01; 32]),
                1000,
                &[],
                &[],
                &[1, 3], // fill makers 1 and 3
                None,
            )
            .unwrap();

        // Only order 2 should still be resting
        let cancelled = db.cancel_all_resting_orders().unwrap();
        assert_eq!(cancelled, 1);
    }
}
