//! Storage layer backed by SQLite with WAL mode.
#![allow(clippy::type_complexity)]
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
    Completed,
    Failed,
    Abandoned,
}

impl ThreadStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "Active",
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
    pub batch_id: Option<String>,
    pub agent_alias: String,
    pub dispatch_message_id: Option<i64>,
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
    pub prompt_hash: Option<String>,
    pub attempt_number: i32,
    pub retry_after: Option<i64>,
    pub error_category: Option<String>,
}

/// A stored execution event row (real-time telemetry).
#[derive(Debug, Clone)]
pub struct ExecutionEventRow {
    pub id: i64,
    pub execution_id: String,
    pub event_type: String,
    pub summary: String,
    pub detail: Option<String>,
    pub timestamp_ms: i64,
    pub event_index: i32,
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
                id                  TEXT PRIMARY KEY,
                thread_id           TEXT NOT NULL,
                agent_alias         TEXT NOT NULL,
                dispatch_message_id INTEGER,
                status              TEXT NOT NULL DEFAULT 'queued',
                queued_at           INTEGER NOT NULL DEFAULT (strftime('%s','now')),
                picked_up_at        INTEGER,
                started_at          INTEGER,
                finished_at         INTEGER,
                duration_ms         INTEGER,
                exit_code           INTEGER,
                output_preview      TEXT,
                error_detail        TEXT,
                parsed_intent       TEXT,
                backend_session_id  TEXT,
                prompt_hash         TEXT
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
        // Forward-compatible migration for existing DBs created before
        // dispatch_message_id existed.
        let columns: Vec<(i64, String, String, i64, Option<String>, i64)> =
            sqlx::query_as("PRAGMA table_info(executions)")
                .fetch_all(&self.pool)
                .await?;
        let has_dispatch_message_id = columns.iter().any(|c| c.1 == "dispatch_message_id");
        if !has_dispatch_message_id {
            sqlx::query("ALTER TABLE executions ADD COLUMN dispatch_message_id INTEGER")
                .execute(&self.pool)
                .await?;
        }
        let has_backend_session_id = columns.iter().any(|c| c.1 == "backend_session_id");
        if !has_backend_session_id {
            sqlx::query("ALTER TABLE executions ADD COLUMN backend_session_id TEXT")
                .execute(&self.pool)
                .await?;
        }
        let has_prompt_hash = columns.iter().any(|c| c.1 == "prompt_hash");
        if !has_prompt_hash {
            sqlx::query("ALTER TABLE executions ADD COLUMN prompt_hash TEXT")
                .execute(&self.pool)
                .await?;
        }
        // ORCH-EVO-12: retry support columns
        let has_attempt_number = columns.iter().any(|c| c.1 == "attempt_number");
        if !has_attempt_number {
            sqlx::query(
                "ALTER TABLE executions ADD COLUMN attempt_number INTEGER NOT NULL DEFAULT 0",
            )
            .execute(&self.pool)
            .await?;
        }
        let has_retry_after = columns.iter().any(|c| c.1 == "retry_after");
        if !has_retry_after {
            sqlx::query("ALTER TABLE executions ADD COLUMN retry_after INTEGER")
                .execute(&self.pool)
                .await?;
        }
        let has_error_category = columns.iter().any(|c| c.1 == "error_category");
        if !has_error_category {
            sqlx::query("ALTER TABLE executions ADD COLUMN error_category TEXT")
                .execute(&self.pool)
                .await?;
        }
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_exec_dispatch_msg ON executions(dispatch_message_id)",
        )
        .execute(&self.pool)
        .await?;

        // Partial UNIQUE index: prevents double-enqueue for the same dispatch
        // message (race safety when multiple workers scan concurrently).
        sqlx::query(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_exec_dispatch_msg_unique
             ON executions(dispatch_message_id) WHERE dispatch_message_id IS NOT NULL",
        )
        .execute(&self.pool)
        .await?;

        // Backfill migration: link old executions (dispatch_message_id = NULL)
        // to their originating dispatch/handoff message so
        // find_untriggered_messages won't re-trigger them.
        //
        // Only update the most-recent unlinked execution per (thread, agent)
        // and only if the target message isn't already claimed by another
        // execution.  The thread-status filter in find_untriggered_messages
        // handles any remaining unlinked rows.
        sqlx::query(
            "UPDATE executions
             SET dispatch_message_id = (
                 SELECT m.id FROM messages m
                 WHERE m.thread_id = executions.thread_id
                   AND m.to_alias = executions.agent_alias
                   AND m.intent IN ('dispatch', 'handoff')
                   AND NOT EXISTS (
                       SELECT 1 FROM executions e2
                       WHERE e2.dispatch_message_id = m.id
                   )
                 ORDER BY m.created_at DESC
                 LIMIT 1
             )
             WHERE dispatch_message_id IS NULL
               AND rowid IN (
                   SELECT MAX(rowid) FROM executions
                   WHERE dispatch_message_id IS NULL
                   GROUP BY thread_id, agent_alias
               )",
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

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS execution_events (
                id             INTEGER PRIMARY KEY AUTOINCREMENT,
                execution_id   TEXT NOT NULL,
                event_type     TEXT NOT NULL,
                summary        TEXT NOT NULL,
                detail         TEXT,
                timestamp_ms   INTEGER NOT NULL,
                event_index    INTEGER NOT NULL
            )",
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_exec_events_lookup
             ON execution_events(execution_id, timestamp_ms)",
        )
        .execute(&self.pool)
        .await?;

        // Legacy compatibility: fold old review workflow status into Active.
        sqlx::query(
            "UPDATE threads
             SET status = 'Active', updated_at = strftime('%s','now')
             WHERE status IN ('ReviewPending', 'review_pending')",
        )
        .execute(&self.pool)
        .await?;

        // ORCH-EVO-7: worktree path tracking
        let thread_columns: Vec<(i64, String, String, i64, Option<String>, i64)> =
            sqlx::query_as("PRAGMA table_info(threads)")
                .fetch_all(&self.pool)
                .await?;
        let has_worktree_path = thread_columns.iter().any(|c| c.1 == "worktree_path");
        if !has_worktree_path {
            sqlx::query("ALTER TABLE threads ADD COLUMN worktree_path TEXT")
                .execute(&self.pool)
                .await?;
        }
        let has_worktree_repo_root = thread_columns.iter().any(|c| c.1 == "worktree_repo_root");
        if !has_worktree_repo_root {
            sqlx::query("ALTER TABLE threads ADD COLUMN worktree_repo_root TEXT")
                .execute(&self.pool)
                .await?;
        }

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

    /// Mark a thread as failed only when it is currently Active.
    pub async fn mark_thread_failed_if_active(&self, thread_id: &str) -> Result<bool, sqlx::Error> {
        let result = sqlx::query(
            "UPDATE threads
             SET status = 'Failed', updated_at = strftime('%s','now')
             WHERE thread_id = ? AND status = 'Active'",
        )
        .bind(thread_id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
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

    // ── Worktree operations ──────────────────────────────────────────────

    /// Store the worktree path and originating repo root for a thread.
    pub async fn set_thread_worktree_path(
        &self,
        thread_id: &str,
        path: &std::path::Path,
        repo_root: &std::path::Path,
    ) -> Result<(), String> {
        sqlx::query(
            "UPDATE threads SET worktree_path = ?, worktree_repo_root = ? WHERE thread_id = ?",
        )
        .bind(path.to_string_lossy().as_ref())
        .bind(repo_root.to_string_lossy().as_ref())
        .bind(thread_id)
        .execute(&self.pool)
        .await
        .map_err(|e| format!("set_thread_worktree_path failed: {}", e))?;
        Ok(())
    }

    /// Get the worktree path for a thread, if set.
    pub async fn get_thread_worktree_path(
        &self,
        thread_id: &str,
    ) -> Result<Option<std::path::PathBuf>, String> {
        let row: Option<(Option<String>,)> =
            sqlx::query_as("SELECT worktree_path FROM threads WHERE thread_id = ?")
                .bind(thread_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| format!("get_thread_worktree_path failed: {}", e))?;
        Ok(row.and_then(|(p,)| p).map(std::path::PathBuf::from))
    }

    /// Find threads with worktrees that have reached terminal state and need cleanup.
    ///
    /// Returns `(thread_id, worktree_path, worktree_repo_root)` tuples.
    pub async fn threads_with_stale_worktrees(
        &self,
    ) -> Result<Vec<(String, String, String)>, String> {
        let rows: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT thread_id, worktree_path, worktree_repo_root FROM threads
             WHERE worktree_path IS NOT NULL
             AND worktree_repo_root IS NOT NULL
             AND status IN ('Completed', 'Abandoned')",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| format!("threads_with_stale_worktrees failed: {}", e))?;
        Ok(rows)
    }

    /// Clear the worktree path and repo root for a thread after cleanup.
    pub async fn clear_thread_worktree_path(&self, thread_id: &str) -> Result<(), String> {
        sqlx::query(
            "UPDATE threads SET worktree_path = NULL, worktree_repo_root = NULL WHERE thread_id = ?",
        )
        .bind(thread_id)
        .execute(&self.pool)
        .await
        .map_err(|e| format!("clear_thread_worktree_path failed: {}", e))?;
        Ok(())
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
    ///
    /// Calls without `dispatch_message_id` always insert (no dedup applies).
    pub async fn insert_execution(
        &self,
        thread_id: &str,
        agent_alias: &str,
    ) -> Result<String, sqlx::Error> {
        self.insert_execution_with_dispatch(thread_id, agent_alias, None, None)
            .await
            .map(|opt| opt.expect("insert without dispatch_message_id should always succeed"))
    }

    /// Insert a new queued execution with optional strict dispatch linkage.
    ///
    /// Uses `INSERT OR IGNORE` so that duplicate enqueues for the same
    /// `dispatch_message_id` (guarded by a partial UNIQUE index) silently
    /// succeed. Returns `Some(id)` on insert, `None` if the message was
    /// already enqueued.
    ///
    /// `prompt_hash` is the SHA-256 hex digest of the agent prompt at dispatch
    /// time, stored once for prompt-to-outcome correlation.
    pub async fn insert_execution_with_dispatch(
        &self,
        thread_id: &str,
        agent_alias: &str,
        dispatch_message_id: Option<i64>,
        prompt_hash: Option<&str>,
    ) -> Result<Option<String>, sqlx::Error> {
        let id = ulid::Ulid::new().to_string();
        let result = sqlx::query(
            "INSERT OR IGNORE INTO executions (id, thread_id, agent_alias, dispatch_message_id, status, prompt_hash)
             VALUES (?, ?, ?, ?, 'queued', ?)",
        )
        .bind(&id)
        .bind(thread_id)
        .bind(agent_alias)
        .bind(dispatch_message_id)
        .bind(prompt_hash)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            Ok(None)
        } else {
            Ok(Some(id))
        }
    }

    /// Find messages that should trigger an execution but haven't yet.
    ///
    /// A message is "untriggered" when:
    /// - its `intent` is in `trigger_intents`
    /// - its `to_alias` is in `worker_aliases`
    /// - the thread is still `Active` (terminal threads are never re-triggered)
    /// - no execution row has `dispatch_message_id = message.id`
    ///
    /// Returns `(message_id, thread_id, to_alias)` tuples ordered by creation time.
    pub async fn find_untriggered_messages(
        &self,
        trigger_intents: &[String],
        worker_aliases: &[String],
    ) -> Result<Vec<(i64, String, String)>, sqlx::Error> {
        if trigger_intents.is_empty() || worker_aliases.is_empty() {
            return Ok(vec![]);
        }

        // Build dynamic IN-clause placeholders.
        let intent_placeholders: Vec<&str> = trigger_intents.iter().map(|_| "?").collect();
        let alias_placeholders: Vec<&str> = worker_aliases.iter().map(|_| "?").collect();

        let sql = format!(
            "SELECT m.id, m.thread_id, m.to_alias
             FROM messages m
             JOIN threads t ON t.thread_id = m.thread_id
             WHERE m.intent IN ({intents})
               AND m.to_alias IN ({aliases})
               AND t.status = 'Active'
               AND NOT EXISTS (
                   SELECT 1 FROM executions e
                   WHERE e.dispatch_message_id = m.id
               )
             ORDER BY m.created_at ASC",
            intents = intent_placeholders.join(","),
            aliases = alias_placeholders.join(","),
        );

        let mut query = sqlx::query_as::<_, (i64, String, String)>(&sql);
        for intent in trigger_intents {
            query = query.bind(intent);
        }
        for alias in worker_aliases {
            query = query.bind(alias);
        }

        query.fetch_all(&self.pool).await
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
             AND (e.retry_after IS NULL OR e.retry_after <= strftime('%s','now'))
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

        let row: Option<ExecutionRowDb> = sqlx::query_as(
            "SELECT e.id, e.thread_id, t.batch_id, e.agent_alias, e.dispatch_message_id, e.status, e.queued_at,
                    picked_up_at, started_at, finished_at, duration_ms,
                    exit_code, output_preview, error_detail, parsed_intent, prompt_hash,
                    e.attempt_number, e.retry_after, e.error_category
             FROM executions e
             LEFT JOIN threads t ON t.thread_id = e.thread_id
             WHERE e.id = ?",
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
    ///
    /// Returns the number of rows affected (0 if the execution was already
    /// in a terminal state, e.g., marked crashed by the stale execution
    /// check before the backend returned).
    pub async fn complete_execution(
        &self,
        id: &str,
        exit_code: Option<i32>,
        output_preview: Option<&str>,
        parsed_intent: Option<&str>,
        duration_ms: i64,
    ) -> Result<u64, sqlx::Error> {
        let result = sqlx::query(
            "UPDATE executions
             SET status = 'completed',
                 finished_at = strftime('%s','now'),
                 exit_code = ?,
                 output_preview = ?,
                 parsed_intent = ?,
                 duration_ms = ?
             WHERE id = ?
               AND status IN ('picked_up', 'executing')",
        )
        .bind(exit_code)
        .bind(output_preview)
        .bind(parsed_intent)
        .bind(duration_ms)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    /// Mark execution failed with error details.
    ///
    /// Returns the number of rows affected (0 if the execution was already
    /// in a terminal state, e.g., marked crashed by the stale execution
    /// check before the backend returned).
    pub async fn fail_execution(
        &self,
        id: &str,
        error_detail: &str,
        exit_code: Option<i32>,
        duration_ms: i64,
        status: ExecutionStatus,
    ) -> Result<u64, sqlx::Error> {
        let result = sqlx::query(
            "UPDATE executions
             SET status = ?,
                 finished_at = strftime('%s','now'),
                 error_detail = ?,
                 exit_code = ?,
                 duration_ms = ?
             WHERE id = ?
               AND status IN ('picked_up', 'executing')",
        )
        .bind(status.as_str())
        .bind(error_detail)
        .bind(exit_code)
        .bind(duration_ms)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    /// Mark orphaned executions (picked_up or executing) as crashed.
    ///
    /// **Startup-only**: this marks ALL in-flight executions as crashed.
    /// Only safe to call before the worker loop starts (when no live
    /// executions belong to this worker).
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

    /// Mark executions stuck in `picked_up` or `executing` beyond the
    /// configured timeout as crashed.
    ///
    /// Safe to call during normal worker operation — only affects
    /// executions whose age exceeds `timeout_secs`. Age is computed from
    /// `COALESCE(started_at, picked_up_at, queued_at)`:
    /// - `started_at` for executions in `executing` status (normal path).
    /// - `picked_up_at` for executions stuck in `picked_up` that never
    ///   transitioned to `executing`.
    /// - `queued_at` as a last resort for data-repaired or manually
    ///   inserted rows where both timestamps are NULL.
    pub async fn mark_stale_executions_crashed(
        &self,
        timeout_secs: u64,
    ) -> Result<u64, sqlx::Error> {
        let result = sqlx::query(
            "UPDATE executions
             SET status = 'crashed',
                 finished_at = strftime('%s','now'),
                 error_detail = 'execution exceeded timeout (stale)'
             WHERE status IN ('picked_up', 'executing')
               AND COALESCE(started_at, picked_up_at, queued_at)
                   <= strftime('%s','now') - ?",
        )
        .bind(timeout_secs.min(i64::MAX as u64) as i64)
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
        let rows: Vec<ExecutionRowDb> = sqlx::query_as(
            "SELECT e.id, e.thread_id, t.batch_id, e.agent_alias, e.dispatch_message_id, e.status, e.queued_at,
                    picked_up_at, started_at, finished_at, duration_ms,
                    exit_code, output_preview, error_detail, parsed_intent, prompt_hash,
                    e.attempt_number, e.retry_after, e.error_category
             FROM executions e
             LEFT JOIN threads t ON t.thread_id = e.thread_id
             WHERE e.thread_id = ?
             ORDER BY e.queued_at ASC",
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
        let row: Option<ExecutionRowDb> = sqlx::query_as(
            "SELECT e.id, e.thread_id, t.batch_id, e.agent_alias, e.dispatch_message_id, e.status, e.queued_at,
                    picked_up_at, started_at, finished_at, duration_ms,
                    exit_code, output_preview, error_detail, parsed_intent, prompt_hash,
                    e.attempt_number, e.retry_after, e.error_category
             FROM executions e
             LEFT JOIN threads t ON t.thread_id = e.thread_id
             WHERE e.thread_id = ?
             ORDER BY e.queued_at DESC LIMIT 1",
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
        let rows: Vec<ExecutionRowDb> = sqlx::query_as(
            "SELECT e.id, e.thread_id, t.batch_id, e.agent_alias, e.dispatch_message_id, e.status, e.queued_at,
                    picked_up_at, started_at, finished_at, duration_ms,
                    exit_code, output_preview, error_detail, parsed_intent, prompt_hash,
                    e.attempt_number, e.retry_after, e.error_category
             FROM executions e
             LEFT JOIN threads t ON t.thread_id = e.thread_id
             WHERE e.agent_alias = ?
             ORDER BY e.queued_at DESC LIMIT ?",
        )
        .bind(agent_alias)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(row_to_execution).collect())
    }

    /// Get the most recent executions across all agents, newest first.
    pub async fn recent_executions(&self, limit: i64) -> Result<Vec<ExecutionRow>, sqlx::Error> {
        let rows: Vec<ExecutionRowDb> = sqlx::query_as(
            "SELECT e.id, e.thread_id, t.batch_id, e.agent_alias, e.dispatch_message_id, e.status, e.queued_at,
                    picked_up_at, started_at, finished_at, duration_ms,
                    exit_code, output_preview, error_detail, parsed_intent, prompt_hash,
                    e.attempt_number, e.retry_after, e.error_category
             FROM executions e
             LEFT JOIN threads t ON t.thread_id = e.thread_id
             ORDER BY e.queued_at DESC LIMIT ?",
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
                    e.error_detail, e.parsed_intent, e.prompt_hash
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
                prompt_hash: r.14,
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

    pub async fn get_execution(&self, id: &str) -> Result<Option<ExecutionRow>, sqlx::Error> {
        let row: Option<ExecutionRowDb> = sqlx::query_as(
            "SELECT e.id, e.thread_id, t.batch_id, e.agent_alias, e.dispatch_message_id, e.status, e.queued_at,
                    picked_up_at, started_at, finished_at, duration_ms,
                    exit_code, output_preview, error_detail, parsed_intent, prompt_hash,
                    e.attempt_number, e.retry_after, e.error_category
             FROM executions e
             LEFT JOIN threads t ON t.thread_id = e.thread_id
             WHERE e.id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(row_to_execution))
    }

    /// Retrieve the backend-specific session ID from the most recent completed
    /// execution for a given thread+agent pair.
    ///
    /// Used by the executor to resume a prior CLI session rather than starting
    /// a fresh one on every dispatch.
    pub async fn get_last_backend_session_id(
        &self,
        thread_id: &str,
        agent_alias: &str,
    ) -> Result<Option<String>, sqlx::Error> {
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT backend_session_id FROM executions
             WHERE thread_id = ? AND agent_alias = ? AND backend_session_id IS NOT NULL
               AND status = 'completed'
             ORDER BY finished_at DESC, id DESC LIMIT 1",
        )
        .bind(thread_id)
        .bind(agent_alias)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| r.0))
    }

    /// Persist the backend-specific session ID for an execution so future
    /// dispatches to the same thread+agent can resume it.
    pub async fn set_backend_session_id(
        &self,
        execution_id: &str,
        session_id: &str,
    ) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE executions SET backend_session_id = ? WHERE id = ?")
            .bind(session_id)
            .bind(execution_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // ── Retry support ───────────────────────────────────────────────────

    /// Set the error category on an execution after failure classification.
    pub async fn set_error_category(
        &self,
        execution_id: &str,
        category: &str,
    ) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE executions SET error_category = ? WHERE id = ?")
            .bind(category)
            .bind(execution_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Get the attempt number of an execution.
    pub async fn get_execution_attempt_number(
        &self,
        execution_id: &str,
    ) -> Result<i32, sqlx::Error> {
        let row: Option<(i32,)> =
            sqlx::query_as("SELECT attempt_number FROM executions WHERE id = ?")
                .bind(execution_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.map(|r| r.0).unwrap_or(0))
    }

    /// Insert a retry execution for a failed execution.
    ///
    /// Creates a new queued execution with an incremented attempt number
    /// and a `retry_after` timestamp for exponential backoff.
    ///
    /// The `dispatch_message_id` is intentionally NOT set on retry executions
    /// to avoid violating the partial UNIQUE index. Retry chains are tracked
    /// by (thread_id, agent_alias, attempt_number > 0).
    ///
    /// Returns the new execution ID.
    pub async fn insert_retry_execution(
        &self,
        thread_id: &str,
        agent_alias: &str,
        _dispatch_message_id: Option<i64>,
        prompt_hash: Option<&str>,
        attempt_number: i32,
        retry_after: i64,
    ) -> Result<String, sqlx::Error> {
        let id = ulid::Ulid::new().to_string();
        sqlx::query(
            "INSERT INTO executions (id, thread_id, agent_alias, status, prompt_hash, attempt_number, retry_after)
             VALUES (?, ?, ?, 'queued', ?, ?, ?)",
        )
        .bind(&id)
        .bind(thread_id)
        .bind(agent_alias)
        .bind(prompt_hash)
        .bind(attempt_number)
        .bind(retry_after)
        .execute(&self.pool)
        .await?;
        Ok(id)
    }

    // ── Execution event telemetry ────────────────────────────────────────

    /// Batch insert execution events in a single transaction.
    pub async fn insert_execution_events(
        &self,
        execution_id: &str,
        events: &[crate::backend::ExecutionEvent],
    ) -> Result<(), sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        for event in events {
            sqlx::query(
                "INSERT INTO execution_events (execution_id, event_type, summary, detail, timestamp_ms, event_index)
                 VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(execution_id)
            .bind(&event.event_type)
            .bind(&event.summary)
            .bind(&event.detail)
            .bind(event.timestamp_ms)
            .bind(event.event_index)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Retrieve execution events, optionally filtered by timestamp, event_index cursor, and limited.
    pub async fn get_execution_events(
        &self,
        execution_id: &str,
        since_timestamp: Option<i64>,
        since_event_index: Option<i32>,
        limit: Option<i64>,
    ) -> Result<Vec<ExecutionEventRow>, sqlx::Error> {
        let since_ts = since_timestamp.unwrap_or(0);
        let since_idx = since_event_index.unwrap_or(-1);
        let lim = limit.unwrap_or(100);

        let rows: Vec<(i64, String, String, String, Option<String>, i64, i32)> = sqlx::query_as(
            "SELECT id, execution_id, event_type, summary, detail, timestamp_ms, event_index
             FROM execution_events
             WHERE execution_id = ? AND timestamp_ms >= ? AND event_index > ?
             ORDER BY event_index ASC
             LIMIT ?",
        )
        .bind(execution_id)
        .bind(since_ts)
        .bind(since_idx)
        .bind(lim)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| ExecutionEventRow {
                id: r.0,
                execution_id: r.1,
                event_type: r.2,
                summary: r.3,
                detail: r.4,
                timestamp_ms: r.5,
                event_index: r.6,
            })
            .collect())
    }

    /// Fetch the single most recent execution event for an execution (by event_index DESC).
    pub async fn get_latest_execution_event(
        &self,
        execution_id: &str,
    ) -> Result<Option<ExecutionEventRow>, sqlx::Error> {
        let row: Option<(i64, String, String, String, Option<String>, i64, i32)> = sqlx::query_as(
            "SELECT id, execution_id, event_type, summary, detail, timestamp_ms, event_index
                 FROM execution_events
                 WHERE execution_id = ?
                 ORDER BY event_index DESC
                 LIMIT 1",
        )
        .bind(execution_id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| ExecutionEventRow {
            id: r.0,
            execution_id: r.1,
            event_type: r.2,
            summary: r.3,
            detail: r.4,
            timestamp_ms: r.5,
            event_index: r.6,
        }))
    }

    /// Delete execution_events rows for executions that are not among the
    /// `retention_count` most recent executions (by `queued_at`). This prevents
    /// unbounded growth of the telemetry table.
    pub async fn prune_execution_events(&self, retention_count: i64) -> Result<u64, sqlx::Error> {
        let result = sqlx::query(
            "DELETE FROM execution_events
             WHERE execution_id NOT IN (
                 SELECT id FROM executions ORDER BY queued_at DESC LIMIT ?
             )",
        )
        .bind(retention_count)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
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
    pub prompt_hash: Option<String>,
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

/// Raw row struct for execution queries.
///
/// Uses `#[derive(sqlx::FromRow)]` instead of tuple derivation to avoid
/// the 16-element tuple limit. Column aliases must match field names exactly.
#[derive(sqlx::FromRow)]
struct ExecutionRowDb {
    id: String,
    thread_id: String,
    batch_id: Option<String>,
    agent_alias: String,
    dispatch_message_id: Option<i64>,
    status: String,
    queued_at: i64,
    picked_up_at: Option<i64>,
    started_at: Option<i64>,
    finished_at: Option<i64>,
    duration_ms: Option<i64>,
    exit_code: Option<i32>,
    output_preview: Option<String>,
    error_detail: Option<String>,
    parsed_intent: Option<String>,
    prompt_hash: Option<String>,
    attempt_number: i32,
    retry_after: Option<i64>,
    error_category: Option<String>,
}

fn row_to_execution(r: ExecutionRowDb) -> ExecutionRow {
    ExecutionRow {
        id: r.id,
        thread_id: r.thread_id,
        batch_id: r.batch_id,
        agent_alias: r.agent_alias,
        dispatch_message_id: r.dispatch_message_id,
        status: r.status,
        queued_at: r.queued_at,
        picked_up_at: r.picked_up_at,
        started_at: r.started_at,
        finished_at: r.finished_at,
        duration_ms: r.duration_ms,
        exit_code: r.exit_code,
        output_preview: r.output_preview,
        error_detail: r.error_detail,
        parsed_intent: r.parsed_intent,
        prompt_hash: r.prompt_hash,
        attempt_number: r.attempt_number,
        retry_after: r.retry_after,
        error_category: r.error_category,
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
                Some("status-update"),
                5000,
            )
            .await
            .unwrap();

        let exec = store.latest_execution("t-1").await.unwrap().unwrap();
        assert_eq!(exec.status, "completed");
        assert_eq!(exec.exit_code, Some(0));
        assert_eq!(exec.parsed_intent.as_deref(), Some("status-update"));
    }

    #[tokio::test]
    async fn test_execution_dispatch_linkage_roundtrip() {
        let store = test_store().await;
        store.ensure_thread("t-1", None).await.unwrap();
        let dispatch_id = store
            .insert_message(
                "t-1",
                "operator",
                "focused",
                "dispatch",
                "linked input",
                None,
            )
            .await
            .unwrap();

        let exec_id = store
            .insert_execution_with_dispatch("t-1", "focused", Some(dispatch_id), None)
            .await
            .unwrap()
            .expect("first insert should succeed");
        let exec = store.get_execution(&exec_id).await.unwrap().unwrap();
        assert_eq!(exec.dispatch_message_id, Some(dispatch_id));
    }

    #[tokio::test]
    async fn test_prompt_hash_stored_and_retrieved() {
        let store = test_store().await;
        store.ensure_thread("t-1", None).await.unwrap();

        let exec_id = store
            .insert_execution_with_dispatch("t-1", "focused", None, Some("abc123hash"))
            .await
            .unwrap()
            .expect("insert should succeed");

        let exec = store.get_execution(&exec_id).await.unwrap().unwrap();
        assert_eq!(exec.prompt_hash.as_deref(), Some("abc123hash"));
    }

    #[tokio::test]
    async fn test_prompt_hash_null_when_not_provided() {
        let store = test_store().await;
        store.ensure_thread("t-1", None).await.unwrap();

        let exec_id = store.insert_execution("t-1", "focused").await.unwrap();

        let exec = store.get_execution(&exec_id).await.unwrap().unwrap();
        assert_eq!(exec.prompt_hash, None);
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
    async fn test_stale_execution_marked_crashed() {
        let store = test_store().await;
        store.ensure_thread("t-1", None).await.unwrap();
        let exec_id = store.insert_execution("t-1", "focused").await.unwrap();
        let _ = store.claim_next_execution(2).await.unwrap();
        store.mark_execution_executing(&exec_id).await.unwrap();

        // Backdate started_at to simulate an execution that has been
        // running far longer than the timeout.
        sqlx::query("UPDATE executions SET started_at = strftime('%s','now') - 9999 WHERE id = ?")
            .bind(&exec_id)
            .execute(&store.pool)
            .await
            .unwrap();

        let count = store.mark_stale_executions_crashed(600).await.unwrap();
        assert_eq!(count, 1);

        let exec = store.latest_execution("t-1").await.unwrap().unwrap();
        assert_eq!(exec.status, "crashed");
    }

    #[tokio::test]
    async fn test_fresh_execution_not_marked_crashed() {
        let store = test_store().await;
        store.ensure_thread("t-1", None).await.unwrap();
        let exec_id = store.insert_execution("t-1", "focused").await.unwrap();
        let _ = store.claim_next_execution(2).await.unwrap();
        store.mark_execution_executing(&exec_id).await.unwrap();

        // started_at is set to now by mark_execution_executing — well within timeout.
        let count = store.mark_stale_executions_crashed(600).await.unwrap();
        assert_eq!(count, 0);

        let exec = store.latest_execution("t-1").await.unwrap().unwrap();
        assert_eq!(exec.status, "executing");
    }

    #[tokio::test]
    async fn test_terminal_execution_not_marked_stale() {
        let store = test_store().await;
        store.ensure_thread("t-1", None).await.unwrap();
        let exec_id = store.insert_execution("t-1", "focused").await.unwrap();
        let _ = store.claim_next_execution(2).await.unwrap();
        store.mark_execution_executing(&exec_id).await.unwrap();
        store
            .complete_execution(&exec_id, Some(0), Some("ok"), None, 5000)
            .await
            .unwrap();

        // Backdate to ensure time check would fire if status matched.
        sqlx::query("UPDATE executions SET started_at = strftime('%s','now') - 9999 WHERE id = ?")
            .bind(&exec_id)
            .execute(&store.pool)
            .await
            .unwrap();

        // Already completed — should not be touched.
        let count = store.mark_stale_executions_crashed(600).await.unwrap();
        assert_eq!(count, 0);

        let exec = store.latest_execution("t-1").await.unwrap().unwrap();
        assert_eq!(exec.status, "completed");
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

    #[tokio::test]
    async fn test_set_and_get_backend_session_id() {
        let store = test_store().await;
        store.ensure_thread("t-1", None).await.unwrap();

        let exec_id = store.insert_execution("t-1", "focused").await.unwrap();
        let _ = store.claim_next_execution(2).await.unwrap();
        store.mark_execution_executing(&exec_id).await.unwrap();
        store
            .complete_execution(&exec_id, Some(0), Some("ok"), None, 1000)
            .await
            .unwrap();

        // No session ID persisted yet.
        let sid = store
            .get_last_backend_session_id("t-1", "focused")
            .await
            .unwrap();
        assert_eq!(sid, None);

        // Persist a session ID.
        store
            .set_backend_session_id(&exec_id, "claude-sid-abc")
            .await
            .unwrap();

        // Should now be retrievable.
        let sid = store
            .get_last_backend_session_id("t-1", "focused")
            .await
            .unwrap();
        assert_eq!(sid.as_deref(), Some("claude-sid-abc"));
    }

    #[tokio::test]
    async fn test_get_last_backend_session_id_returns_latest() {
        let store = test_store().await;
        store.ensure_thread("t-1", None).await.unwrap();

        // First execution for thread t-1, agent focused.
        let exec_id_1 = store.insert_execution("t-1", "focused").await.unwrap();
        let _ = store.claim_next_execution(2).await.unwrap();
        store.mark_execution_executing(&exec_id_1).await.unwrap();
        store
            .complete_execution(&exec_id_1, Some(0), Some("ok"), None, 1000)
            .await
            .unwrap();
        store
            .set_backend_session_id(&exec_id_1, "claude-sid-first")
            .await
            .unwrap();

        // Second execution for the same thread+agent.
        let exec_id_2 = store.insert_execution("t-1", "focused").await.unwrap();
        let _ = store.claim_next_execution(2).await.unwrap();
        store.mark_execution_executing(&exec_id_2).await.unwrap();
        store
            .complete_execution(&exec_id_2, Some(0), Some("ok"), None, 1000)
            .await
            .unwrap();
        store
            .set_backend_session_id(&exec_id_2, "claude-sid-latest")
            .await
            .unwrap();

        // get_last_backend_session_id must return the latest completed one.
        let sid = store
            .get_last_backend_session_id("t-1", "focused")
            .await
            .unwrap();
        assert_eq!(sid.as_deref(), Some("claude-sid-latest"));
    }

    #[tokio::test]
    async fn test_insert_retry_execution_and_attempt_number() {
        let store = test_store().await;
        store.ensure_thread("t-1", None).await.unwrap();

        let retry_after = chrono::Utc::now().timestamp() - 10; // in the past
        let exec_id = store
            .insert_retry_execution("t-1", "focused", None, Some("hash123"), 2, retry_after)
            .await
            .unwrap();

        let attempt = store.get_execution_attempt_number(&exec_id).await.unwrap();
        assert_eq!(attempt, 2);
    }

    #[tokio::test]
    async fn test_claim_respects_retry_after_future() {
        let store = test_store().await;
        store.ensure_thread("t-1", None).await.unwrap();

        // Insert a retry execution with retry_after far in the future
        let future_ts = chrono::Utc::now().timestamp() + 9999;
        let _exec_id = store
            .insert_retry_execution("t-1", "focused", None, None, 1, future_ts)
            .await
            .unwrap();

        // Should NOT be claimable — retry_after is in the future
        let claimed = store.claim_next_execution(2).await.unwrap();
        assert!(
            claimed.is_none(),
            "retry execution should not be claimed before retry_after"
        );
    }

    #[tokio::test]
    async fn test_claim_picks_up_retry_after_past() {
        let store = test_store().await;
        store.ensure_thread("t-1", None).await.unwrap();

        // Insert a retry execution with retry_after in the past
        let past_ts = chrono::Utc::now().timestamp() - 10;
        let exec_id = store
            .insert_retry_execution("t-1", "focused", None, None, 1, past_ts)
            .await
            .unwrap();

        // Should be claimable — retry_after is in the past
        let claimed = store.claim_next_execution(2).await.unwrap();
        assert!(
            claimed.is_some(),
            "retry execution should be claimed after retry_after"
        );
        assert_eq!(claimed.unwrap().id, exec_id);
    }

    #[tokio::test]
    async fn test_set_and_get_error_category() {
        let store = test_store().await;
        store.ensure_thread("t-1", None).await.unwrap();

        let exec_id = store.insert_execution("t-1", "focused").await.unwrap();
        store
            .set_error_category(&exec_id, "transient")
            .await
            .unwrap();

        let exec = store.get_execution(&exec_id).await.unwrap().unwrap();
        assert_eq!(exec.error_category.as_deref(), Some("transient"));
    }

    #[tokio::test]
    async fn test_get_last_backend_session_id_ignores_failed_executions() {
        let store = test_store().await;
        store.ensure_thread("t-1", None).await.unwrap();

        // An execution that failed with a backend_session_id set.
        let exec_id = store.insert_execution("t-1", "focused").await.unwrap();
        let _ = store.claim_next_execution(2).await.unwrap();
        store.mark_execution_executing(&exec_id).await.unwrap();
        store
            .fail_execution(
                &exec_id,
                "some error",
                Some(1),
                500,
                ExecutionStatus::Failed,
            )
            .await
            .unwrap();
        store
            .set_backend_session_id(&exec_id, "claude-sid-failed")
            .await
            .unwrap();

        // Should not return session IDs from failed executions.
        let sid = store
            .get_last_backend_session_id("t-1", "focused")
            .await
            .unwrap();
        assert_eq!(sid, None);
    }
}
