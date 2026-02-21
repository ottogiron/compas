//! Storage layer backed by SQLite with WAL mode.
//!
//! Three core tables:
//! - `threads`    — unit of work lifecycle
//! - `messages`   — conversation record between operator and agents
//! - `executions` — job queue AND execution lifecycle (single source of truth)
//! - `worker_heartbeats` — worker liveness tracking

use sqlx::SqlitePool;

// ── Row types ────────────────────────────────────────────────────────────────

/// Thread status enum — stored as TEXT in SQLite, validated in Rust.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThreadStatus {
    Active,
    ReviewPending,
    Completed,
    Failed,
    Abandoned,
}

impl ThreadStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "Active",
            Self::ReviewPending => "ReviewPending",
            Self::Completed => "Completed",
            Self::Failed => "Failed",
            Self::Abandoned => "Abandoned",
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Abandoned)
    }
}

impl std::fmt::Display for ThreadStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for ThreadStatus {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "Active" => Ok(Self::Active),
            "ReviewPending" => Ok(Self::ReviewPending),
            "Completed" => Ok(Self::Completed),
            "Failed" => Ok(Self::Failed),
            "Abandoned" => Ok(Self::Abandoned),
            other => Err(format!("unknown thread status: '{}'", other)),
        }
    }
}

/// Execution status enum — stored as TEXT in SQLite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionStatus {
    Queued,
    PickedUp,
    Executing,
    Completed,
    Failed,
    TimedOut,
    Crashed,
    Cancelled,
}

impl ExecutionStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::PickedUp => "picked_up",
            Self::Executing => "executing",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::TimedOut => "timed_out",
            Self::Crashed => "crashed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::TimedOut | Self::Crashed | Self::Cancelled
        )
    }

    /// Is this execution currently occupying an agent slot?
    pub fn is_active(&self) -> bool {
        matches!(self, Self::PickedUp | Self::Executing)
    }
}

impl std::fmt::Display for ExecutionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for ExecutionStatus {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "queued" => Ok(Self::Queued),
            "picked_up" => Ok(Self::PickedUp),
            "executing" => Ok(Self::Executing),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "timed_out" => Ok(Self::TimedOut),
            "crashed" => Ok(Self::Crashed),
            "cancelled" => Ok(Self::Cancelled),
            other => Err(format!("unknown execution status: '{}'", other)),
        }
    }
}

/// A stored message row.
#[derive(Debug, Clone)]
pub struct MessageRow {
    pub id: i64,
    pub thread_id: String,
    pub from_alias: String,
    pub to_alias: String,
    pub intent: String,
    pub body: String,
    pub batch_id: Option<String>,
    pub created_at: i64,
}

/// A stored execution row.
#[derive(Debug, Clone)]
pub struct ExecutionRow {
    pub id: String,
    pub thread_id: String,
    pub agent_alias: String,
    pub status: String,
    pub queued_at: i64,
    pub picked_up_at: Option<i64>,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub duration_ms: Option<i64>,
    pub exit_code: Option<i32>,
    pub output_preview: Option<String>,
    pub error_detail: Option<String>,
    pub parsed_intent: Option<String>,
}

/// A stored thread row.
#[derive(Debug, Clone)]
pub struct ThreadRow {
    pub thread_id: String,
    pub batch_id: Option<String>,
    pub status: String,
    pub created_at: i64,
    pub updated_at: i64,
}

// ── Store ────────────────────────────────────────────────────────────────────

/// Store wraps the shared SQLite pool.
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

    /// Create all tables and enable WAL mode.
    pub async fn setup(&self) -> Result<(), sqlx::Error> {
        // WAL mode for concurrent read/write from MCP + worker processes
        sqlx::query("PRAGMA journal_mode=WAL")
            .execute(&self.pool)
            .await?;
        // Busy timeout: wait up to 5s for locks instead of failing immediately
        sqlx::query("PRAGMA busy_timeout=5000")
            .execute(&self.pool)
            .await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS threads (
                thread_id  TEXT PRIMARY KEY,
                batch_id   TEXT,
                status     TEXT NOT NULL DEFAULT 'Active',
                created_at INTEGER NOT NULL DEFAULT (strftime('%s','now')),
                updated_at INTEGER NOT NULL DEFAULT (strftime('%s','now'))
            )",
        )
        .execute(&self.pool)
        .await?;

        sqlx::query("CREATE INDEX IF NOT EXISTS idx_threads_batch ON threads(batch_id)")
            .execute(&self.pool)
            .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_threads_status ON threads(status)")
            .execute(&self.pool)
            .await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS messages (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                thread_id  TEXT NOT NULL,
                from_alias TEXT NOT NULL,
                to_alias   TEXT NOT NULL,
                intent     TEXT NOT NULL,
                body       TEXT NOT NULL,
                batch_id   TEXT,
                created_at INTEGER NOT NULL DEFAULT (strftime('%s','now'))
            )",
        )
        .execute(&self.pool)
        .await?;

        sqlx::query("CREATE INDEX IF NOT EXISTS idx_messages_thread ON messages(thread_id)")
            .execute(&self.pool)
            .await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS executions (
                id             TEXT PRIMARY KEY,
                thread_id      TEXT NOT NULL,
                agent_alias    TEXT NOT NULL,
                status         TEXT NOT NULL DEFAULT 'queued',
                queued_at      INTEGER NOT NULL DEFAULT (strftime('%s','now')),
                picked_up_at   INTEGER,
                started_at     INTEGER,
                finished_at    INTEGER,
                duration_ms    INTEGER,
                exit_code      INTEGER,
                output_preview TEXT,
                error_detail   TEXT,
                parsed_intent  TEXT
            )",
        )
        .execute(&self.pool)
        .await?;

        sqlx::query("CREATE INDEX IF NOT EXISTS idx_exec_status ON executions(status)")
            .execute(&self.pool)
            .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_exec_thread ON executions(thread_id)")
            .execute(&self.pool)
            .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_exec_agent_status ON executions(agent_alias, status)",
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS worker_heartbeats (
                worker_id    TEXT PRIMARY KEY,
                last_beat_at INTEGER NOT NULL,
                started_at   INTEGER NOT NULL,
                version      TEXT
            )",
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    // ── Thread operations ────────────────────────────────────────────────

    /// Ensure a thread record exists. Creates if missing, updates batch_id if provided.
    pub async fn ensure_thread(
        &self,
        thread_id: &str,
        batch_id: Option<&str>,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO threads (thread_id, batch_id)
             VALUES (?, ?)
             ON CONFLICT(thread_id) DO UPDATE SET
               batch_id = COALESCE(excluded.batch_id, threads.batch_id),
               updated_at = strftime('%s','now')",
        )
        .bind(thread_id)
        .bind(batch_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn update_thread_status(
        &self,
        thread_id: &str,
        status: ThreadStatus,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "UPDATE threads SET status = ?, updated_at = strftime('%s','now')
             WHERE thread_id = ?",
        )
        .bind(status.as_str())
        .bind(thread_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_thread_status(&self, thread_id: &str) -> Result<Option<String>, sqlx::Error> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT status FROM threads WHERE thread_id = ?")
                .bind(thread_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.map(|r| r.0))
    }

    pub async fn get_thread(&self, thread_id: &str) -> Result<Option<ThreadRow>, sqlx::Error> {
        let row: Option<(String, Option<String>, String, i64, i64)> = sqlx::query_as(
            "SELECT thread_id, batch_id, status, created_at, updated_at
             FROM threads WHERE thread_id = ?",
        )
        .bind(thread_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| ThreadRow {
            thread_id: r.0,
            batch_id: r.1,
            status: r.2,
            created_at: r.3,
            updated_at: r.4,
        }))
    }

    /// List threads with optional filters.
    pub async fn list_threads(
        &self,
        batch_id: Option<&str>,
        status: Option<&str>,
        limit: i64,
    ) -> Result<Vec<ThreadRow>, sqlx::Error> {
        let mut sql = String::from(
            "SELECT thread_id, batch_id, status, created_at, updated_at FROM threads WHERE 1=1",
        );
        if batch_id.is_some() {
            sql.push_str(" AND batch_id = ?");
        }
        if status.is_some() {
            sql.push_str(" AND status = ?");
        }
        sql.push_str(" ORDER BY updated_at DESC LIMIT ?");

        let mut query = sqlx::query_as::<_, (String, Option<String>, String, i64, i64)>(&sql);
        if let Some(b) = batch_id {
            query = query.bind(b);
        }
        if let Some(s) = status {
            query = query.bind(s);
        }
        query = query.bind(limit);

        let rows = query.fetch_all(&self.pool).await?;
        Ok(rows
            .into_iter()
            .map(|r| ThreadRow {
                thread_id: r.0,
                batch_id: r.1,
                status: r.2,
                created_at: r.3,
                updated_at: r.4,
            })
            .collect())
    }

    // ── Message operations ───────────────────────────────────────────────

    /// Insert a message. Also ensures the thread record exists.
    pub async fn insert_message(
        &self,
        thread_id: &str,
        from_alias: &str,
        to_alias: &str,
        intent: &str,
        body: &str,
        batch_id: Option<&str>,
    ) -> Result<i64, sqlx::Error> {
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

    pub async fn get_thread_messages(
        &self,
        thread_id: &str,
    ) -> Result<Vec<MessageRow>, sqlx::Error> {
        let rows: Vec<(
            i64,
            String,
            String,
            String,
            String,
            String,
            Option<String>,
            i64,
        )> = sqlx::query_as(
            "SELECT id, thread_id, from_alias, to_alias, intent, body, batch_id, created_at
                 FROM messages WHERE thread_id = ? ORDER BY id ASC",
        )
        .bind(thread_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(row_to_message).collect())
    }

    /// Get messages since a given ID for a thread.
    pub async fn get_messages_since(
        &self,
        thread_id: &str,
        after_id: i64,
    ) -> Result<Vec<MessageRow>, sqlx::Error> {
        let rows: Vec<(
            i64,
            String,
            String,
            String,
            String,
            String,
            Option<String>,
            i64,
        )> = sqlx::query_as(
            "SELECT id, thread_id, from_alias, to_alias, intent, body, batch_id, created_at
                 FROM messages WHERE thread_id = ? AND id > ? ORDER BY id ASC",
        )
        .bind(thread_id)
        .bind(after_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(row_to_message).collect())
    }

    pub async fn get_message(&self, id: i64) -> Result<Option<MessageRow>, sqlx::Error> {
        let row: Option<(
            i64,
            String,
            String,
            String,
            String,
            String,
            Option<String>,
            i64,
        )> = sqlx::query_as(
            "SELECT id, thread_id, from_alias, to_alias, intent, body, batch_id, created_at
                 FROM messages WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(row_to_message))
    }

    pub async fn latest_message_id(&self, thread_id: &str) -> Result<Option<i64>, sqlx::Error> {
        let row: Option<(Option<i64>,)> =
            sqlx::query_as("SELECT MAX(id) FROM messages WHERE thread_id = ?")
                .bind(thread_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.and_then(|r| r.0))
    }

    // ── Execution operations ─────────────────────────────────────────────

    /// Insert a new queued execution. Returns the execution ID.
    pub async fn insert_execution(
        &self,
        thread_id: &str,
        agent_alias: &str,
    ) -> Result<String, sqlx::Error> {
        let id = ulid::Ulid::new().to_string();
        sqlx::query(
            "INSERT INTO executions (id, thread_id, agent_alias, status)
             VALUES (?, ?, ?, 'queued')",
        )
        .bind(&id)
        .bind(thread_id)
        .bind(agent_alias)
        .execute(&self.pool)
        .await?;
        Ok(id)
    }

    /// Atomically claim the next queued execution, respecting per-agent concurrency.
    /// Returns None if no work is available.
    pub async fn claim_next_execution(
        &self,
        max_per_agent: usize,
    ) -> Result<Option<ExecutionRow>, sqlx::Error> {
        let mut tx = self.pool.begin().await?;

        let candidate: Option<(String,)> = sqlx::query_as(
            "SELECT e.id FROM executions e
             WHERE e.status = 'queued'
             AND (SELECT COUNT(*) FROM executions e2
                  WHERE e2.agent_alias = e.agent_alias
                  AND e2.status IN ('picked_up', 'executing')) < ?
             ORDER BY e.queued_at ASC
             LIMIT 1",
        )
        .bind(max_per_agent as i64)
        .fetch_optional(&mut *tx)
        .await?;

        let Some(candidate) = candidate else {
            tx.commit().await?;
            return Ok(None);
        };
        let exec_id = candidate.0;

        let result = sqlx::query(
            "UPDATE executions
             SET status = 'picked_up', picked_up_at = strftime('%s','now')
             WHERE id = ? AND status = 'queued'",
        )
        .bind(&exec_id)
        .execute(&mut *tx)
        .await?;

        if result.rows_affected() == 0 {
            tx.commit().await?;
            return Ok(None);
        }

        let row: Option<(
            String,
            String,
            String,
            String,
            i64,
            Option<i64>,
            Option<i64>,
            Option<i64>,
            Option<i64>,
            Option<i32>,
            Option<String>,
            Option<String>,
            Option<String>,
        )> = sqlx::query_as(
            "SELECT id, thread_id, agent_alias, status, queued_at,
                    picked_up_at, started_at, finished_at, duration_ms,
                    exit_code, output_preview, error_detail, parsed_intent
             FROM executions WHERE id = ?",
        )
        .bind(&exec_id)
        .fetch_optional(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(row.map(row_to_execution))
    }

    /// Update execution to 'executing' status.
    pub async fn mark_execution_executing(&self, id: &str) -> Result<(), sqlx::Error> {
        sqlx::query(
            "UPDATE executions
             SET status = 'executing', started_at = strftime('%s','now')
             WHERE id = ?",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Mark execution completed with results.
    pub async fn complete_execution(
        &self,
        id: &str,
        exit_code: Option<i32>,
        output_preview: Option<&str>,
        parsed_intent: Option<&str>,
        duration_ms: i64,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "UPDATE executions
             SET status = 'completed',
                 finished_at = strftime('%s','now'),
                 exit_code = ?,
                 output_preview = ?,
                 parsed_intent = ?,
                 duration_ms = ?
             WHERE id = ?",
        )
        .bind(exit_code)
        .bind(output_preview)
        .bind(parsed_intent)
        .bind(duration_ms)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Mark execution failed with error details.
    pub async fn fail_execution(
        &self,
        id: &str,
        error_detail: &str,
        exit_code: Option<i32>,
        duration_ms: i64,
        status: ExecutionStatus,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "UPDATE executions
             SET status = ?,
                 finished_at = strftime('%s','now'),
                 error_detail = ?,
                 exit_code = ?,
                 duration_ms = ?
             WHERE id = ?",
        )
        .bind(status.as_str())
        .bind(error_detail)
        .bind(exit_code)
        .bind(duration_ms)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Mark orphaned executions (picked_up or executing) as crashed.
    pub async fn mark_orphaned_executions_crashed(&self) -> Result<u64, sqlx::Error> {
        let result = sqlx::query(
            "UPDATE executions
             SET status = 'crashed',
                 finished_at = strftime('%s','now'),
                 error_detail = 'worker crashed during execution'
             WHERE status IN ('picked_up', 'executing')",
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    /// Cancel all active executions for a thread.
    pub async fn cancel_thread_executions(&self, thread_id: &str) -> Result<u64, sqlx::Error> {
        let result = sqlx::query(
            "UPDATE executions
             SET status = 'cancelled',
                 finished_at = strftime('%s','now'),
                 error_detail = 'thread abandoned'
             WHERE thread_id = ? AND status IN ('queued', 'picked_up', 'executing')",
        )
        .bind(thread_id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    /// Get all executions for a thread, ordered by queued_at.
    pub async fn get_thread_executions(
        &self,
        thread_id: &str,
    ) -> Result<Vec<ExecutionRow>, sqlx::Error> {
        let rows: Vec<(
            String,
            String,
            String,
            String,
            i64,
            Option<i64>,
            Option<i64>,
            Option<i64>,
            Option<i64>,
            Option<i32>,
            Option<String>,
            Option<String>,
            Option<String>,
        )> = sqlx::query_as(
            "SELECT id, thread_id, agent_alias, status, queued_at,
                    picked_up_at, started_at, finished_at, duration_ms,
                    exit_code, output_preview, error_detail, parsed_intent
             FROM executions WHERE thread_id = ? ORDER BY queued_at ASC",
        )
        .bind(thread_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(row_to_execution).collect())
    }

    /// Get latest execution for a thread.
    pub async fn latest_execution(
        &self,
        thread_id: &str,
    ) -> Result<Option<ExecutionRow>, sqlx::Error> {
        let row: Option<(
            String,
            String,
            String,
            String,
            i64,
            Option<i64>,
            Option<i64>,
            Option<i64>,
            Option<i64>,
            Option<i32>,
            Option<String>,
            Option<String>,
            Option<String>,
        )> = sqlx::query_as(
            "SELECT id, thread_id, agent_alias, status, queued_at,
                    picked_up_at, started_at, finished_at, duration_ms,
                    exit_code, output_preview, error_detail, parsed_intent
             FROM executions WHERE thread_id = ? ORDER BY queued_at DESC LIMIT 1",
        )
        .bind(thread_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(row_to_execution))
    }

    /// Get queue depth (number of queued executions).
    pub async fn queue_depth(&self) -> Result<i64, sqlx::Error> {
        let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM executions WHERE status = 'queued'")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.0)
    }

    /// Get the most recent executions for a specific agent, newest first.
    pub async fn recent_agent_executions(
        &self,
        agent_alias: &str,
        limit: i64,
    ) -> Result<Vec<ExecutionRow>, sqlx::Error> {
        let rows: Vec<(
            String,
            String,
            String,
            String,
            i64,
            Option<i64>,
            Option<i64>,
            Option<i64>,
            Option<i64>,
            Option<i32>,
            Option<String>,
            Option<String>,
            Option<String>,
        )> = sqlx::query_as(
            "SELECT id, thread_id, agent_alias, status, queued_at,
                    picked_up_at, started_at, finished_at, duration_ms,
                    exit_code, output_preview, error_detail, parsed_intent
             FROM executions WHERE agent_alias = ? ORDER BY queued_at DESC LIMIT ?",
        )
        .bind(agent_alias)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(row_to_execution).collect())
    }

    /// Get the most recent executions across all agents, newest first.
    pub async fn recent_executions(&self, limit: i64) -> Result<Vec<ExecutionRow>, sqlx::Error> {
        let rows: Vec<(
            String,
            String,
            String,
            String,
            i64,
            Option<i64>,
            Option<i64>,
            Option<i64>,
            Option<i64>,
            Option<i32>,
            Option<String>,
            Option<String>,
            Option<String>,
        )> = sqlx::query_as(
            "SELECT id, thread_id, agent_alias, status, queued_at,
                    picked_up_at, started_at, finished_at, duration_ms,
                    exit_code, output_preview, error_detail, parsed_intent
             FROM executions ORDER BY queued_at DESC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(row_to_execution).collect())
    }

    /// Count active executions per agent.
    pub async fn active_executions_by_agent(&self) -> Result<Vec<(String, i64)>, sqlx::Error> {
        sqlx::query_as(
            "SELECT agent_alias, COUNT(*) FROM executions
             WHERE status IN ('picked_up', 'executing')
             GROUP BY agent_alias",
        )
        .fetch_all(&self.pool)
        .await
    }

    // ── Status view (threads + latest execution) ─────────────────────────

    /// Combined thread + latest execution view for orch_status.
    pub async fn status_view(
        &self,
        thread_id: Option<&str>,
        agent: Option<&str>,
        batch: Option<&str>,
        limit: i64,
    ) -> Result<Vec<ThreadStatusView>, sqlx::Error> {
        let mut sql = String::from(
            "SELECT t.thread_id, t.batch_id, t.status, t.created_at, t.updated_at,
                    e.id, COALESCE(e.agent_alias, m.to_alias), e.status, e.queued_at,
                    e.started_at, e.finished_at, e.duration_ms,
                    e.error_detail, e.parsed_intent
             FROM threads t
             LEFT JOIN executions e ON e.thread_id = t.thread_id
               AND e.queued_at = (SELECT MAX(e2.queued_at) FROM executions e2 WHERE e2.thread_id = t.thread_id)
             LEFT JOIN messages m ON m.thread_id = t.thread_id
               AND m.id = (SELECT MAX(m2.id) FROM messages m2 WHERE m2.thread_id = t.thread_id)
             WHERE 1=1",
        );
        if thread_id.is_some() {
            sql.push_str(" AND t.thread_id = ?");
        }
        if agent.is_some() {
            sql.push_str(" AND COALESCE(e.agent_alias, m.to_alias) = ?");
        }
        if batch.is_some() {
            sql.push_str(" AND t.batch_id = ?");
        }
        sql.push_str(" ORDER BY t.updated_at DESC LIMIT ?");

        type Row = (
            String,
            Option<String>,
            String,
            i64,
            i64,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<i64>,
            Option<i64>,
            Option<i64>,
            Option<i64>,
            Option<String>,
            Option<String>,
        );
        let mut query = sqlx::query_as::<_, Row>(&sql);
        if let Some(t) = thread_id {
            query = query.bind(t);
        }
        if let Some(a) = agent {
            query = query.bind(a);
        }
        if let Some(b) = batch {
            query = query.bind(b);
        }
        query = query.bind(limit);

        let rows = query.fetch_all(&self.pool).await?;
        Ok(rows
            .into_iter()
            .map(|r| ThreadStatusView {
                thread_id: r.0,
                batch_id: r.1,
                thread_status: r.2,
                thread_created_at: r.3,
                thread_updated_at: r.4,
                execution_id: r.5,
                agent_alias: r.6,
                execution_status: r.7,
                queued_at: r.8,
                started_at: r.9,
                finished_at: r.10,
                duration_ms: r.11,
                error_detail: r.12,
                parsed_intent: r.13,
            })
            .collect())
    }

    // ── Heartbeat operations ─────────────────────────────────────────────

    pub async fn write_heartbeat(&self, worker_id: &str, version: &str) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO worker_heartbeats (worker_id, last_beat_at, started_at, version)
             VALUES (?, strftime('%s','now'), strftime('%s','now'), ?)
             ON CONFLICT(worker_id) DO UPDATE SET
               last_beat_at = strftime('%s','now'),
               version = excluded.version",
        )
        .bind(worker_id)
        .bind(version)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn latest_heartbeat(
        &self,
    ) -> Result<Option<(String, i64, i64, Option<String>)>, sqlx::Error> {
        sqlx::query_as(
            "SELECT worker_id, last_beat_at, started_at, version
             FROM worker_heartbeats ORDER BY last_beat_at DESC LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await
    }

    // ── Aggregate counts ─────────────────────────────────────────────────

    pub async fn thread_counts(&self) -> Result<Vec<(String, i64)>, sqlx::Error> {
        sqlx::query_as("SELECT status, COUNT(*) FROM threads GROUP BY status")
            .fetch_all(&self.pool)
            .await
    }

    pub async fn message_count(&self) -> Result<i64, sqlx::Error> {
        let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM messages")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.0)
    }
}

/// Combined thread + execution view.
#[derive(Debug, Clone)]
pub struct ThreadStatusView {
    pub thread_id: String,
    pub batch_id: Option<String>,
    pub thread_status: String,
    pub thread_created_at: i64,
    pub thread_updated_at: i64,
    pub execution_id: Option<String>,
    pub agent_alias: Option<String>,
    pub execution_status: Option<String>,
    pub queued_at: Option<i64>,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub duration_ms: Option<i64>,
    pub error_detail: Option<String>,
    pub parsed_intent: Option<String>,
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn row_to_message(
    r: (
        i64,
        String,
        String,
        String,
        String,
        String,
        Option<String>,
        i64,
    ),
) -> MessageRow {
    MessageRow {
        id: r.0,
        thread_id: r.1,
        from_alias: r.2,
        to_alias: r.3,
        intent: r.4,
        body: r.5,
        batch_id: r.6,
        created_at: r.7,
    }
}

fn row_to_execution(
    r: (
        String,
        String,
        String,
        String,
        i64,
        Option<i64>,
        Option<i64>,
        Option<i64>,
        Option<i64>,
        Option<i32>,
        Option<String>,
        Option<String>,
        Option<String>,
    ),
) -> ExecutionRow {
    ExecutionRow {
        id: r.0,
        thread_id: r.1,
        agent_alias: r.2,
        status: r.3,
        queued_at: r.4,
        picked_up_at: r.5,
        started_at: r.6,
        finished_at: r.7,
        duration_ms: r.8,
        exit_code: r.9,
        output_preview: r.10,
        error_detail: r.11,
        parsed_intent: r.12,
    }
}

/// Format a message ID as a reference string.
pub fn message_ref(id: i64) -> String {
    format!("db:{}", id)
}

/// Parse a message reference like "db:123" or "123" into a numeric ID.
pub fn parse_message_ref(reference: &str) -> Result<i64, String> {
    let s = reference.strip_prefix("db:").unwrap_or(reference);
    s.parse::<i64>()
        .map_err(|_| format!("invalid message reference: '{}'", reference))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_store() -> Store {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        let store = Store::new(pool);
        store.setup().await.unwrap();
        store
    }

    #[tokio::test]
    async fn test_thread_lifecycle() {
        let store = test_store().await;
        store.ensure_thread("t-1", Some("batch-1")).await.unwrap();
        let status = store.get_thread_status("t-1").await.unwrap();
        assert_eq!(status.as_deref(), Some("Active"));

        store
            .update_thread_status("t-1", ThreadStatus::Completed)
            .await
            .unwrap();
        let status = store.get_thread_status("t-1").await.unwrap();
        assert_eq!(status.as_deref(), Some("Completed"));
    }

    #[tokio::test]
    async fn test_ensure_thread_updates_batch() {
        let store = test_store().await;
        store.ensure_thread("t-1", None).await.unwrap();
        let thread = store.get_thread("t-1").await.unwrap().unwrap();
        assert_eq!(thread.batch_id, None);

        store.ensure_thread("t-1", Some("batch-1")).await.unwrap();
        let thread = store.get_thread("t-1").await.unwrap().unwrap();
        assert_eq!(thread.batch_id.as_deref(), Some("batch-1"));
    }

    #[tokio::test]
    async fn test_message_insert_and_query() {
        let store = test_store().await;
        let id = store
            .insert_message("t-1", "operator", "focused", "dispatch", "do work", None)
            .await
            .unwrap();
        assert!(id > 0);

        let msgs = store.get_thread_messages("t-1").await.unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].from_alias, "operator");
        assert_eq!(msgs[0].intent, "dispatch");
    }

    #[tokio::test]
    async fn test_execution_lifecycle() {
        let store = test_store().await;
        store.ensure_thread("t-1", None).await.unwrap();

        let exec_id = store.insert_execution("t-1", "focused").await.unwrap();
        assert!(!exec_id.is_empty());

        let claimed = store.claim_next_execution(2).await.unwrap();
        assert!(claimed.is_some());
        let claimed = claimed.unwrap();
        assert_eq!(claimed.id, exec_id);
        assert_eq!(claimed.status, "picked_up");

        store.mark_execution_executing(&exec_id).await.unwrap();

        store
            .complete_execution(
                &exec_id,
                Some(0),
                Some("output"),
                Some("review-request"),
                5000,
            )
            .await
            .unwrap();

        let exec = store.latest_execution("t-1").await.unwrap().unwrap();
        assert_eq!(exec.status, "completed");
        assert_eq!(exec.exit_code, Some(0));
        assert_eq!(exec.parsed_intent.as_deref(), Some("review-request"));
    }

    #[tokio::test]
    async fn test_per_agent_concurrency() {
        let store = test_store().await;
        store.ensure_thread("t-1", None).await.unwrap();
        store.ensure_thread("t-2", None).await.unwrap();

        store.insert_execution("t-1", "focused").await.unwrap();
        store.insert_execution("t-2", "focused").await.unwrap();

        let first = store.claim_next_execution(1).await.unwrap();
        assert!(first.is_some());
        let second = store.claim_next_execution(1).await.unwrap();
        assert!(second.is_none());
    }

    #[tokio::test]
    async fn test_crash_recovery() {
        let store = test_store().await;
        store.ensure_thread("t-1", None).await.unwrap();
        let exec_id = store.insert_execution("t-1", "focused").await.unwrap();
        let _ = store.claim_next_execution(2).await.unwrap();
        store.mark_execution_executing(&exec_id).await.unwrap();

        let count = store.mark_orphaned_executions_crashed().await.unwrap();
        assert_eq!(count, 1);

        let exec = store.latest_execution("t-1").await.unwrap().unwrap();
        assert_eq!(exec.status, "crashed");
    }

    #[tokio::test]
    async fn test_heartbeat() {
        let store = test_store().await;
        store.write_heartbeat("worker-1", "0.2.0").await.unwrap();
        let hb = store.latest_heartbeat().await.unwrap().unwrap();
        assert_eq!(hb.0, "worker-1");
        assert_eq!(hb.3.as_deref(), Some("0.2.0"));
    }

    #[tokio::test]
    async fn test_queue_depth() {
        let store = test_store().await;
        store.ensure_thread("t-1", None).await.unwrap();
        store.ensure_thread("t-2", None).await.unwrap();
        store.insert_execution("t-1", "focused").await.unwrap();
        store.insert_execution("t-2", "chill").await.unwrap();
        assert_eq!(store.queue_depth().await.unwrap(), 2);
    }

    #[tokio::test]
    async fn test_status_view_uses_latest_message_agent_when_no_execution() {
        let store = test_store().await;
        store
            .insert_message(
                "t-1",
                "operator",
                "focused",
                "dispatch",
                "body",
                Some("b-1"),
            )
            .await
            .unwrap();

        let rows = store
            .status_view(Some("t-1"), None, None, 10)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].execution_id, None);
        assert_eq!(rows[0].agent_alias.as_deref(), Some("focused"));

        let filtered = store
            .status_view(Some("t-1"), Some("focused"), None, 10)
            .await
            .unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].agent_alias.as_deref(), Some("focused"));
    }
}
