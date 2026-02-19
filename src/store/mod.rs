//! Storage layer backed by sqlx (SQLite).
//!
//! Two tables:
//! - `messages` — conversation ledger (permanent). MCP tools read/write here.
//! - `threads`  — thread lifecycle status.
//!
//! The apalis `Jobs` table (managed by apalis-sqlite) handles the worker queue separately.

use apalis::prelude::{Task, TaskId, TaskSink};
use apalis_core::backend::TaskSinkError;
use apalis_sqlite::{SqliteContext, SqliteStorage, TaskBuilderExt};
use sqlx::SqlitePool;

// ── Row types ────────────────────────────────────────────────────────────────

/// A stored message row.
#[derive(Debug, Clone)]
pub struct MessageRow {
    pub id: i64,
    pub thread_id: String,
    pub from_alias: String,
    pub to_alias: String,
    pub intent: String,
    pub body: String,
    pub status: String,
    pub batch_id: Option<String>,
    pub review_token: Option<String>,
    pub created_at: i64,
}

/// Thread metadata row.
pub struct ThreadRecord {
    pub thread_id: String,
    pub status: String,
    pub batch_id: Option<String>,
    pub created_at: i64,
}

/// Aggregate metrics snapshot.
#[derive(Debug, Clone, Default)]
pub struct Metrics {
    pub total_messages: i64,
    pub active_threads: i64,
    pub completed_threads: i64,
    pub failed_threads: i64,
    pub abandoned_threads: i64,
    pub pending_messages: i64,
}

// ── Store ────────────────────────────────────────────────────────────────────

/// Store wraps the shared SQLite pool for conversation data.
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

    /// Create all companion tables (apalis handles Jobs + Workers).
    pub async fn setup(&self) -> Result<(), sqlx::Error> {
        // Messages table — the conversation ledger
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                thread_id TEXT NOT NULL,
                from_alias TEXT NOT NULL,
                to_alias TEXT NOT NULL,
                intent TEXT NOT NULL,
                body TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'new',
                batch_id TEXT,
                review_token TEXT,
                created_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
            )",
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_messages_thread ON messages(thread_id)",
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_messages_to_status ON messages(to_alias, status)",
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_messages_batch ON messages(batch_id)",
        )
        .execute(&self.pool)
        .await?;

        // Threads table — lifecycle status
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

    // ── Message operations ───────────────────────────────────────────────

    /// Insert a message. Returns the new message ID.
    /// Also ensures the thread record exists.
    pub async fn insert_message(
        &self,
        thread_id: &str,
        from_alias: &str,
        to_alias: &str,
        intent: &str,
        body: &str,
        batch_id: Option<&str>,
    ) -> Result<i64, sqlx::Error> {
        // Ensure thread exists
        self.ensure_thread(thread_id, batch_id).await?;

        let row: (i64,) = sqlx::query_as(
            "INSERT INTO messages (thread_id, from_alias, to_alias, intent, body, batch_id)
             VALUES (?, ?, ?, ?, ?, ?)
             RETURNING id",
        )
        .bind(thread_id)
        .bind(from_alias)
        .bind(to_alias)
        .bind(intent)
        .bind(body)
        .bind(batch_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.0)
    }

    /// Get a single message by ID.
    pub async fn get_message(&self, id: i64) -> Result<Option<MessageRow>, sqlx::Error> {
        let row: Option<(i64, String, String, String, String, String, String, Option<String>, Option<String>, i64)> =
            sqlx::query_as(
                "SELECT id, thread_id, from_alias, to_alias, intent, body, status, batch_id, review_token, created_at
                 FROM messages WHERE id = ?",
            )
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(row_to_message))
    }

    /// Get all messages for a thread, ordered by ID.
    pub async fn get_thread_messages(
        &self,
        thread_id: &str,
    ) -> Result<Vec<MessageRow>, sqlx::Error> {
        let rows: Vec<(i64, String, String, String, String, String, String, Option<String>, Option<String>, i64)> =
            sqlx::query_as(
                "SELECT id, thread_id, from_alias, to_alias, intent, body, status, batch_id, review_token, created_at
                 FROM messages WHERE thread_id = ? ORDER BY id ASC",
            )
            .bind(thread_id)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.into_iter().map(row_to_message).collect())
    }

    /// Get messages filtered by agent and/or thread.
    pub async fn list_messages(
        &self,
        agent: Option<&str>,
        thread_id: Option<&str>,
    ) -> Result<Vec<MessageRow>, sqlx::Error> {
        // Build query dynamically based on filters
        let mut sql = String::from(
            "SELECT id, thread_id, from_alias, to_alias, intent, body, status, batch_id, review_token, created_at
             FROM messages WHERE 1=1",
        );
        if agent.is_some() {
            sql.push_str(" AND to_alias = ?");
        }
        if thread_id.is_some() {
            sql.push_str(" AND thread_id = ?");
        }
        sql.push_str(" ORDER BY id DESC LIMIT 100");

        let mut query = sqlx::query_as::<_, (i64, String, String, String, String, String, String, Option<String>, Option<String>, i64)>(&sql);
        if let Some(a) = agent {
            query = query.bind(a);
        }
        if let Some(t) = thread_id {
            query = query.bind(t);
        }

        let rows = query.fetch_all(&self.pool).await?;
        Ok(rows.into_iter().map(row_to_message).collect())
    }

    /// Get messages since a given message ID for a thread.
    pub async fn get_thread_messages_since(
        &self,
        thread_id: &str,
        after_id: i64,
    ) -> Result<Vec<MessageRow>, sqlx::Error> {
        let rows: Vec<(i64, String, String, String, String, String, String, Option<String>, Option<String>, i64)> =
            sqlx::query_as(
                "SELECT id, thread_id, from_alias, to_alias, intent, body, status, batch_id, review_token, created_at
                 FROM messages WHERE thread_id = ? AND id > ? ORDER BY id ASC",
            )
            .bind(thread_id)
            .bind(after_id)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.into_iter().map(row_to_message).collect())
    }

    /// Get the latest message ID for a thread.
    pub async fn latest_thread_message_id(
        &self,
        thread_id: &str,
    ) -> Result<Option<i64>, sqlx::Error> {
        let row: Option<(i64,)> =
            sqlx::query_as("SELECT MAX(id) FROM messages WHERE thread_id = ?")
                .bind(thread_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.map(|r| r.0))
    }

    /// Get latest intent for a thread.
    pub async fn latest_thread_intent(
        &self,
        thread_id: &str,
    ) -> Result<Option<String>, sqlx::Error> {
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT intent FROM messages WHERE thread_id = ? ORDER BY id DESC LIMIT 1",
        )
        .bind(thread_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| r.0))
    }

    /// Update a message's status.
    pub async fn update_message_status(
        &self,
        id: i64,
        status: &str,
    ) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE messages SET status = ? WHERE id = ?")
            .bind(status)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Set review_token on a message.
    pub async fn set_message_review_token(
        &self,
        id: i64,
        token: &str,
    ) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE messages SET review_token = ? WHERE id = ?")
            .bind(token)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Get all messages for a batch.
    pub async fn get_batch_messages(
        &self,
        batch_id: &str,
    ) -> Result<Vec<MessageRow>, sqlx::Error> {
        let rows: Vec<(i64, String, String, String, String, String, String, Option<String>, Option<String>, i64)> =
            sqlx::query_as(
                "SELECT id, thread_id, from_alias, to_alias, intent, body, status, batch_id, review_token, created_at
                 FROM messages WHERE batch_id = ? ORDER BY id ASC",
            )
            .bind(batch_id)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.into_iter().map(row_to_message).collect())
    }

    /// Aggregate metrics.
    pub async fn metrics(&self) -> Result<Metrics, sqlx::Error> {
        let (total_messages,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM messages")
                .fetch_one(&self.pool)
                .await?;
        let (pending_messages,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM messages WHERE status = 'new'")
                .fetch_one(&self.pool)
                .await?;
        let (active_threads,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM threads WHERE status = 'Active'")
                .fetch_one(&self.pool)
                .await?;
        let (completed_threads,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM threads WHERE status = 'Completed'")
                .fetch_one(&self.pool)
                .await?;
        let (failed_threads,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM threads WHERE status = 'Failed'")
                .fetch_one(&self.pool)
                .await?;
        let (abandoned_threads,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM threads WHERE status = 'Abandoned'")
                .fetch_one(&self.pool)
                .await?;
        Ok(Metrics {
            total_messages,
            active_threads,
            completed_threads,
            failed_threads,
            abandoned_threads,
            pending_messages,
        })
    }

    // ── Thread operations ────────────────────────────────────────────────

    /// Ensure a thread record exists (upsert — creates if missing, no-op if present).
    pub async fn ensure_thread(
        &self,
        thread_id: &str,
        batch_id: Option<&str>,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO threads (thread_id, status, batch_id)
             VALUES (?, 'Active', ?)
             ON CONFLICT(thread_id) DO NOTHING",
        )
        .bind(thread_id)
        .bind(batch_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Update thread status.
    pub async fn update_thread_status(
        &self,
        thread_id: &str,
        status: &str,
    ) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE threads SET status = ? WHERE thread_id = ?")
            .bind(status)
            .bind(thread_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Get thread status.
    pub async fn get_thread_status(
        &self,
        thread_id: &str,
    ) -> Result<Option<String>, sqlx::Error> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT status FROM threads WHERE thread_id = ?")
                .bind(thread_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.map(|r| r.0))
    }

    /// Get thread batch_id (if set).
    pub async fn get_thread_batch_id(
        &self,
        thread_id: &str,
    ) -> Result<Option<String>, sqlx::Error> {
        let row: Option<(Option<String>,)> =
            sqlx::query_as("SELECT batch_id FROM threads WHERE thread_id = ?")
                .bind(thread_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.and_then(|r| r.0))
    }

    /// List threads by status.
    pub async fn list_threads_by_status(
        &self,
        status: &str,
    ) -> Result<Vec<ThreadRecord>, sqlx::Error> {
        let rows: Vec<(String, String, Option<String>, i64)> = sqlx::query_as(
            "SELECT thread_id, status, batch_id, created_at FROM threads WHERE status = ?",
        )
        .bind(status)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(row_to_thread).collect())
    }

    /// List all thread statuses.
    pub async fn list_all_threads(&self) -> Result<Vec<ThreadRecord>, sqlx::Error> {
        let rows: Vec<(String, String, Option<String>, i64)> = sqlx::query_as(
            "SELECT thread_id, status, batch_id, created_at FROM threads ORDER BY created_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(row_to_thread).collect())
    }

    /// Get all threads for a batch.
    pub async fn get_batch_threads(
        &self,
        batch_id: &str,
    ) -> Result<Vec<ThreadRecord>, sqlx::Error> {
        let rows: Vec<(String, String, Option<String>, i64)> = sqlx::query_as(
            "SELECT thread_id, status, batch_id, created_at FROM threads WHERE batch_id = ?",
        )
        .bind(batch_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(row_to_thread).collect())
    }

    // ── Job queue operations ─────────────────────────────────────────────

    /// Push a trigger job into the apalis queue using the backend TaskSink API.
    pub async fn push_trigger_job(
        &self,
        job: &crate::worker::TriggerJob,
        queue_name: &str,
    ) -> Result<String, sqlx::Error> {
        let job_id = ulid::Ulid::new();
        let mut storage: SqliteStorage<crate::worker::TriggerJob, _, _> =
            SqliteStorage::new_in_queue(&self.pool, queue_name);

        let task = Task::<crate::worker::TriggerJob, SqliteContext, ulid::Ulid>::builder(
            job.clone(),
        )
        .with_task_id(TaskId::new(job_id))
        // No automatic retries: failed trigger attempts should not be re-executed.
        .max_attempts(1)
        .priority(0)
        .build();

        match storage.push_task(task).await {
            Ok(()) => Ok(job_id.to_string()),
            Err(TaskSinkError::PushError(e)) => Err(e),
            Err(TaskSinkError::CodecError(e)) => Err(sqlx::Error::Encode(e)),
        }
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn row_to_message(
    r: (i64, String, String, String, String, String, String, Option<String>, Option<String>, i64),
) -> MessageRow {
    MessageRow {
        id: r.0,
        thread_id: r.1,
        from_alias: r.2,
        to_alias: r.3,
        intent: r.4,
        body: r.5,
        status: r.6,
        batch_id: r.7,
        review_token: r.8,
        created_at: r.9,
    }
}

fn row_to_thread(r: (String, String, Option<String>, i64)) -> ThreadRecord {
    ThreadRecord {
        thread_id: r.0,
        status: r.1,
        batch_id: r.2,
        created_at: r.3,
    }
}

// ── Reference parsing ────────────────────────────────────────────────────────

/// Parse a message reference like "db:123" or "123" into a numeric ID.
pub fn parse_message_ref(reference: &str) -> Result<i64, String> {
    let s = reference.strip_prefix("db:").unwrap_or(reference);
    s.parse::<i64>()
        .map_err(|_| format!("invalid message reference: '{}'", reference))
}

/// Format a message ID as a reference string.
pub fn message_ref(id: i64) -> String {
    format!("db:{}", id)
}

#[cfg(test)]
mod tests {
    use super::Store;
    use crate::worker::TriggerJob;
    use apalis_sqlite::SqliteStorage;

    #[tokio::test]
    async fn test_push_trigger_job_uses_apalis_sink_api() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        SqliteStorage::setup(&pool).await.unwrap();
        let store = Store::new(pool.clone());
        store.setup().await.unwrap();

        let job = TriggerJob {
            thread_id: "t-test-001".into(),
            agent_alias: "focused".into(),
            message_body: "test body".into(),
            from_alias: "operator".into(),
            intent: "dispatch".into(),
            batch_id: Some("BATCH-1".into()),
        };

        let job_id = store.push_trigger_job(&job, "trigger-queue").await.unwrap();

        let row: (Vec<u8>, String, String, i64, i32, i32) = sqlx::query_as(
            "SELECT job, id, job_type, run_at, max_attempts, priority
             FROM Jobs
             WHERE id = ?",
        )
        .bind(&job_id)
        .fetch_one(&pool)
        .await
        .unwrap();

        assert_eq!(row.1, job_id);
        assert_eq!(row.2, "trigger-queue");
        assert_eq!(row.4, 25);
        assert_eq!(row.5, 0);
        assert!(row.3 > 0);

        let decoded: TriggerJob = serde_json::from_slice(&row.0).unwrap();
        assert_eq!(decoded.thread_id, "t-test-001");
        assert_eq!(decoded.agent_alias, "focused");
        assert_eq!(decoded.intent, "dispatch");
        assert_eq!(decoded.batch_id.as_deref(), Some("BATCH-1"));
    }
}
