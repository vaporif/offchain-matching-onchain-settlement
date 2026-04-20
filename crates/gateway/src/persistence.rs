use std::path::Path;
use std::sync::Mutex;

use eyre::{Result, WrapErr};
use rusqlite::OptionalExtension;

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
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS sync_state (
                key TEXT PRIMARY KEY,
                value INTEGER NOT NULL
            )",
        )
        .wrap_err("creating sync_state table")?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn last_synced_block(&self) -> Result<Option<u64>> {
        let conn = self.conn.lock().unwrap();
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
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO sync_state (key, value) VALUES ('last_synced_block', ?1)",
            [value],
        )
        .wrap_err("updating last_synced_block")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn temp_db() -> (TempDir, Db) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.db");
        let db = Db::open(&path).unwrap();
        (dir, db)
    }

    #[test]
    fn last_synced_block_returns_none_on_fresh_db() {
        let (_dir, db) = temp_db();
        assert_eq!(db.last_synced_block().unwrap(), None);
    }

    #[test]
    fn set_and_get_last_synced_block() {
        let (_dir, db) = temp_db();
        db.set_last_synced_block(42).unwrap();
        assert_eq!(db.last_synced_block().unwrap(), Some(42));
    }

    #[test]
    fn set_last_synced_block_overwrites() {
        let (_dir, db) = temp_db();
        db.set_last_synced_block(10).unwrap();
        db.set_last_synced_block(20).unwrap();
        assert_eq!(db.last_synced_block().unwrap(), Some(20));
    }

    #[test]
    fn open_creates_directory_if_missing() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nested/dirs/test.db");
        let db = Db::open(&path).unwrap();
        db.set_last_synced_block(1).unwrap();
        assert_eq!(db.last_synced_block().unwrap(), Some(1));
    }

    #[test]
    fn table_creation_is_idempotent() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.db");
        let db1 = Db::open(&path).unwrap();
        db1.set_last_synced_block(5).unwrap();
        drop(db1);
        let db2 = Db::open(&path).unwrap();
        assert_eq!(db2.last_synced_block().unwrap(), Some(5));
    }

    #[test]
    fn rejects_block_number_exceeding_i64_max() {
        let (_dir, db) = temp_db();
        assert!(db.set_last_synced_block(u64::MAX).is_err());
    }
}
