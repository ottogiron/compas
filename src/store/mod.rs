//! Storage layer backed by apalis-sqlite (sqlx).
//!
//! Replaces the old rusqlite-based `mailbox::store` module.
//! Jobs table is managed by apalis. Thread metadata is a lightweight companion table.

use sqlx::SqlitePool;

/// Lightweight thread metadata (companion to apalis Jobs table).
pub struct ThreadRecord {
    pub thread_id: String,
    pub status: String,
    pub batch_id: Option<String>,
    pub created_at: i64,
}

/// Store wraps the shared SQLite pool for non-apalis queries
/// (thread metadata, batch lookups, etc.).
#[derive(Clone, Debug)]
pub struct Store {
    pool: SqlitePool,
}

impl Store {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Create the threads companion table (apalis handles Jobs + Workers).
    pub async fn setup(&self) -> Result<(), sqlx::Error> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS threads (
                thread_id TEXT NOT NULL PRIMARY KEY,
                status TEXT NOT NULL DEFAULT 'Active',
                batch_id TEXT,
                created_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
            )",
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_threads_batch ON threads(batch_id)",
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_threads_status ON threads(status)",
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }
}
