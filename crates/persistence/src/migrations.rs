use eyre::{Result, WrapErr};
use rusqlite::Connection;

pub fn run(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS sync_state (
            key TEXT PRIMARY KEY,
            value INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS ledger_balances (
            user_address BLOB NOT NULL,
            token_address BLOB NOT NULL,
            available BLOB NOT NULL,
            reserved BLOB NOT NULL,
            PRIMARY KEY (user_address, token_address)
        );

        CREATE TABLE IF NOT EXISTS orders (
            order_id INTEGER PRIMARY KEY,
            maker_address BLOB NOT NULL,
            signed_order BLOB NOT NULL,
            status TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS pending_trades (
            trade_id INTEGER PRIMARY KEY AUTOINCREMENT,
            trade_data BLOB NOT NULL,
            status TEXT NOT NULL,
            batch_tx_hash BLOB,
            created_at INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS nonces (
            nonce BLOB PRIMARY KEY,
            expires_at INTEGER NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_nonces_expires ON nonces(expires_at);
        CREATE INDEX IF NOT EXISTS idx_pending_trades_status ON pending_trades(status);
        CREATE INDEX IF NOT EXISTS idx_orders_status ON orders(status);
        ",
    )
    .wrap_err("running database migrations")?;

    Ok(())
}
