//! Storage layer backed by SQLite with WAL mode.
#![allow(clippy::type_complexity)]
//!
//! Five core tables:
//! - `threads`           — unit of work lifecycle
//! - `messages`          — conversation record between operator and agents
//! - `executions`        — job queue AND execution lifecycle (single source of truth)
//! - `worker_heartbeats` — worker liveness tracking
//! - `merge_operations`  — merge queue for branch integration

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

/// Merge operation status enum — stored as lowercase TEXT in SQLite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeOperationStatus {
    Queued,
    Claimed,
    Executing,
    Completed,
    Failed,
    Cancelled,
}

impl MergeOperationStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Claimed => "claimed",
            Self::Executing => "executing",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

impl std::fmt::Display for MergeOperationStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for MergeOperationStatus {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "queued" => Ok(Self::Queued),
            "claimed" => Ok(Self::Claimed),
            "executing" => Ok(Self::Executing),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            other => Err(format!("unknown merge operation status: '{}'", other)),
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
    /// Points to the original dispatch message for retry executions.
    /// Not part of the UNIQUE index (unlike `dispatch_message_id`).
    pub original_dispatch_message_id: Option<i64>,
    /// OS PID of the spawned backend CLI process (for orphan detection).
    pub pid: Option<i64>,
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
    pub tool_name: Option<String>,
}

/// A stored thread row.
#[derive(Debug, Clone)]
pub struct ThreadRow {
    pub thread_id: String,
    pub batch_id: Option<String>,
    pub source_thread_id: Option<String>,
    pub summary: Option<String>,
    pub status: String,
    pub created_at: i64,
    pub updated_at: i64,
}

/// A thread with an active worktree path (from the threads table).
pub struct ThreadWorktreeEntry {
    pub thread_id: String,
    pub worktree_path: String,
}

/// Parameters for the atomic reply + fan-out transaction.
pub struct ReplyAndFanoutParams<'a> {
    pub reply_thread_id: &'a str,
    pub reply_from: &'a str,
    pub reply_to: &'a str,
    pub reply_intent: &'a str,
    pub reply_body: &'a str,
    pub source_thread_id: &'a str,
    pub batch_id: &'a str,
    pub targets: &'a [String],
    pub handoff_from: &'a str,
    pub handoff_body: &'a str,
}

/// Per-tool call statistics aggregated from execution_events.
#[derive(Debug, Clone)]
pub struct ToolCallStat {
    pub tool_name: String,
    pub call_count: i64,
    pub error_count: i64,
    pub error_rate: f64,
}

/// Summary of execution costs and token usage.
#[derive(Debug, Clone)]
pub struct CostSummary {
    pub total_cost_usd: f64,
    pub avg_cost_usd: f64,
    pub total_tokens_in: i64,
    pub total_tokens_out: i64,
    pub execution_count: i64,
    /// Count of executions where cost_usd IS NOT NULL (i.e., cost was recorded).
    pub executions_with_cost: i64,
}

/// Per-agent cost and token breakdown.
#[derive(Debug, Clone)]
pub struct AgentCostSummary {
    pub agent_alias: String,
    pub total_cost_usd: f64,
    pub total_tokens_in: i64,
    pub total_tokens_out: i64,
    pub execution_count: i64,
}

/// A stored merge operation row.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct MergeOperation {
    pub id: String,
    pub thread_id: String,
    pub source_branch: String,
    pub target_branch: String,
    pub merge_strategy: String,
    pub requested_by: String,
    pub status: String,
    pub push_requested: bool,
    pub queued_at: i64,
    pub claimed_at: Option<i64>,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub duration_ms: Option<i64>,
    pub result_summary: Option<String>,
    pub error_detail: Option<String>,
    pub conflict_files: Option<String>,
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
                thread_id        TEXT PRIMARY KEY,
                batch_id         TEXT,
                source_thread_id TEXT,
                status           TEXT NOT NULL DEFAULT 'Active',
                created_at       INTEGER NOT NULL DEFAULT (strftime('%s','now')),
                updated_at       INTEGER NOT NULL DEFAULT (strftime('%s','now'))
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
        let has_orig_dispatch = columns
            .iter()
            .any(|c| c.1 == "original_dispatch_message_id");
        if !has_orig_dispatch {
            sqlx::query("ALTER TABLE executions ADD COLUMN original_dispatch_message_id INTEGER")
                .execute(&self.pool)
                .await?;
        }
        // Orphan detection: PID and worker_id tracking
        let has_pid = columns.iter().any(|c| c.1 == "pid");
        if !has_pid {
            sqlx::query("ALTER TABLE executions ADD COLUMN pid INTEGER")
                .execute(&self.pool)
                .await?;
        }
        // OBS-01: cost/token telemetry columns
        let has_cost_usd = columns.iter().any(|c| c.1 == "cost_usd");
        if !has_cost_usd {
            sqlx::query("ALTER TABLE executions ADD COLUMN cost_usd REAL")
                .execute(&self.pool)
                .await?;
        }
        let has_tokens_in = columns.iter().any(|c| c.1 == "tokens_in");
        if !has_tokens_in {
            sqlx::query("ALTER TABLE executions ADD COLUMN tokens_in INTEGER")
                .execute(&self.pool)
                .await?;
        }
        let has_tokens_out = columns.iter().any(|c| c.1 == "tokens_out");
        if !has_tokens_out {
            sqlx::query("ALTER TABLE executions ADD COLUMN tokens_out INTEGER")
                .execute(&self.pool)
                .await?;
        }
        let has_num_turns = columns.iter().any(|c| c.1 == "num_turns");
        if !has_num_turns {
            sqlx::query("ALTER TABLE executions ADD COLUMN num_turns INTEGER")
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

        // OBS-01: tool_name column for execution_events
        let event_columns: Vec<(i64, String, String, i64, Option<String>, i64)> =
            sqlx::query_as("PRAGMA table_info(execution_events)")
                .fetch_all(&self.pool)
                .await?;
        let has_tool_name = event_columns.iter().any(|c| c.1 == "tool_name");
        if !has_tool_name {
            sqlx::query("ALTER TABLE execution_events ADD COLUMN tool_name TEXT")
                .execute(&self.pool)
                .await?;
        }

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

        // ADR-014 Phase 2: source_thread_id for fan-out thread linkage
        let has_source_thread_id = thread_columns.iter().any(|c| c.1 == "source_thread_id");
        if !has_source_thread_id {
            sqlx::query("ALTER TABLE threads ADD COLUMN source_thread_id TEXT")
                .execute(&self.pool)
                .await?;
        }
        let has_summary = thread_columns.iter().any(|c| c.1 == "summary");
        if !has_summary {
            sqlx::query("ALTER TABLE threads ADD COLUMN summary TEXT")
                .execute(&self.pool)
                .await?;
        }

        // Index must be created AFTER the column migration for existing DBs.
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_threads_source ON threads(source_thread_id)")
            .execute(&self.pool)
            .await?;

        // MERGE-1: merge operations table for merge queue
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS merge_operations (
                id              TEXT PRIMARY KEY,
                thread_id       TEXT NOT NULL,
                source_branch   TEXT NOT NULL,
                target_branch   TEXT NOT NULL,
                merge_strategy  TEXT NOT NULL DEFAULT 'merge',
                requested_by    TEXT NOT NULL,
                status          TEXT NOT NULL DEFAULT 'queued',
                push_requested  INTEGER NOT NULL DEFAULT 0,
                queued_at       INTEGER NOT NULL DEFAULT (strftime('%s','now')),
                claimed_at      INTEGER,
                started_at      INTEGER,
                finished_at     INTEGER,
                duration_ms     INTEGER,
                result_summary  TEXT,
                error_detail    TEXT,
                conflict_files  TEXT
            )",
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_merge_ops_target_status
             ON merge_operations(target_branch, status)",
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_merge_ops_thread
             ON merge_operations(thread_id)",
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
        summary: Option<&str>,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO threads (thread_id, batch_id, summary)
             VALUES (?, ?, ?)
             ON CONFLICT(thread_id) DO UPDATE SET
               batch_id = COALESCE(excluded.batch_id, threads.batch_id),
               summary = COALESCE(excluded.summary, threads.summary),
               updated_at = strftime('%s','now')",
        )
        .bind(thread_id)
        .bind(batch_id)
        .bind(summary)
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
        let row: Option<(
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            String,
            i64,
            i64,
        )> = sqlx::query_as(
            "SELECT thread_id, batch_id, source_thread_id, summary, status, created_at, updated_at
                 FROM threads WHERE thread_id = ?",
        )
        .bind(thread_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| ThreadRow {
            thread_id: r.0,
            batch_id: r.1,
            source_thread_id: r.2,
            summary: r.3,
            status: r.4,
            created_at: r.5,
            updated_at: r.6,
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
            "SELECT thread_id, batch_id, source_thread_id, summary, status, created_at, updated_at \
             FROM threads WHERE 1=1",
        );
        if batch_id.is_some() {
            sql.push_str(" AND batch_id = ?");
        }
        if status.is_some() {
            sql.push_str(" AND status = ?");
        }
        sql.push_str(" ORDER BY updated_at DESC LIMIT ?");

        let mut query = sqlx::query_as::<
            _,
            (
                String,
                Option<String>,
                Option<String>,
                Option<String>,
                String,
                i64,
                i64,
            ),
        >(&sql);
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
                source_thread_id: r.2,
                summary: r.3,
                status: r.4,
                created_at: r.5,
                updated_at: r.6,
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

    /// Get both the worktree path and its originating repo root for a thread.
    ///
    /// Used by the executor to decide whether a non-worktree agent should
    /// inherit the thread's worktree (same-repo check).
    pub async fn get_thread_worktree_info(
        &self,
        thread_id: &str,
    ) -> Result<Option<(std::path::PathBuf, std::path::PathBuf)>, String> {
        let row: Option<(Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT worktree_path, worktree_repo_root FROM threads WHERE thread_id = ?",
        )
        .bind(thread_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| format!("get_thread_worktree_info failed: {}", e))?;
        Ok(row.and_then(|(p, r)| match (p, r) {
            (Some(path), Some(root)) => Some((
                std::path::PathBuf::from(path),
                std::path::PathBuf::from(root),
            )),
            _ => None,
        }))
    }

    /// Return all threads with a worktree path set (may include threads pending cleanup).
    pub async fn threads_with_worktree_paths(&self) -> Result<Vec<ThreadWorktreeEntry>, String> {
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT thread_id, worktree_path FROM threads WHERE worktree_path IS NOT NULL",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| format!("threads_with_worktree_paths failed: {}", e))?;
        Ok(rows
            .into_iter()
            .map(|(thread_id, worktree_path)| ThreadWorktreeEntry {
                thread_id,
                worktree_path,
            })
            .collect())
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
             AND status IN ('Completed', 'Abandoned')
             AND thread_id NOT IN (
                 SELECT thread_id FROM merge_operations
                 WHERE status IN ('queued', 'claimed', 'executing')
             )",
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
    #[allow(clippy::too_many_arguments)]
    pub async fn insert_message(
        &self,
        thread_id: &str,
        from_alias: &str,
        to_alias: &str,
        intent: &str,
        body: &str,
        batch_id: Option<&str>,
        summary: Option<&str>,
    ) -> Result<i64, sqlx::Error> {
        self.ensure_thread(thread_id, batch_id, summary).await?;
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

    /// Count messages with `intent = 'handoff'` in a thread (for chain depth tracking).
    pub async fn count_handoff_messages(&self, thread_id: &str) -> Result<i64, String> {
        let row: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM messages WHERE thread_id = ? AND intent = 'handoff'",
        )
        .bind(thread_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| format!("count_handoff_messages failed: {}", e))?;
        Ok(row.0)
    }

    /// Count pending chain work for a thread.
    ///
    /// Returns the sum of:
    /// - Active executions (queued, picked_up, or executing) on the thread.
    /// - Handoff messages that have not yet been linked to an execution.
    ///
    /// A single compound query closes the race window between handoff message
    /// insertion and execution enqueue.
    pub async fn count_pending_chain_work(&self, thread_id: &str) -> Result<i64, String> {
        let row: (i64,) = sqlx::query_as(
            "SELECT
              (SELECT COUNT(*) FROM executions
               WHERE thread_id = ? AND status IN ('queued', 'picked_up', 'executing')) +
              (SELECT COUNT(*) FROM messages m
               WHERE m.thread_id = ? AND m.intent = 'handoff'
               AND NOT EXISTS (
                 SELECT 1 FROM executions e WHERE e.dispatch_message_id = m.id
               ))
            AS pending_work",
        )
        .bind(thread_id)
        .bind(thread_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| format!("count_pending_chain_work failed: {}", e))?;
        Ok(row.0)
    }

    /// Count pending chain work on a thread AND its direct fan-out child threads.
    ///
    /// Extends `count_pending_chain_work` to also check threads linked via
    /// `source_thread_id`. This ensures `--await-chain` blocks until fan-out
    /// reviewer threads have settled.
    ///
    /// **Scope: direct children only.** If a fan-out child itself triggers
    /// further fan-out (grandchildren), those threads are NOT counted. This is
    /// acceptable given `max_chain_depth` limits and the current single-depth
    /// fan-out design.
    ///
    /// Returns the sum of:
    /// - Active executions on the thread and its direct fan-out children.
    /// - Untriggered handoff messages on the thread and its direct fan-out children.
    pub async fn count_pending_chain_and_fanout_work(
        &self,
        thread_id: &str,
    ) -> Result<i64, String> {
        let row: (i64,) = sqlx::query_as(
            "SELECT
              (SELECT COUNT(*) FROM executions
               WHERE thread_id = ?1 AND status IN ('queued', 'picked_up', 'executing'))
              +
              (SELECT COUNT(*) FROM messages m
               WHERE m.thread_id = ?1 AND m.intent = 'handoff'
               AND NOT EXISTS (SELECT 1 FROM executions e WHERE e.dispatch_message_id = m.id))
              +
              (SELECT COUNT(*) FROM executions
               WHERE thread_id IN (SELECT thread_id FROM threads WHERE source_thread_id = ?1)
               AND status IN ('queued', 'picked_up', 'executing'))
              +
              (SELECT COUNT(*) FROM messages m
               WHERE m.thread_id IN (SELECT thread_id FROM threads WHERE source_thread_id = ?1)
               AND m.intent = 'handoff'
               AND NOT EXISTS (SELECT 1 FROM executions e WHERE e.dispatch_message_id = m.id))
            AS total_pending",
        )
        .bind(thread_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| format!("count_pending_chain_and_fanout_work failed: {}", e))?;
        Ok(row.0)
    }

    /// Atomically check chain depth and insert a handoff message if under the limit.
    ///
    /// Returns:
    /// - `Ok(Some(message_id))` — handoff inserted (depth was under limit).
    /// - `Ok(None)` — depth limit reached, no message inserted.
    /// - `Err(...)` — DB error.
    ///
    /// The depth check and insert run in a single SQLite transaction to prevent
    /// TOCTOU races where concurrent executions could both pass the depth check
    /// before either inserts.
    pub async fn insert_handoff_if_under_depth(
        &self,
        thread_id: &str,
        from_alias: &str,
        to_alias: &str,
        body: &str,
        max_depth: i64,
    ) -> Result<Option<i64>, String> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| format!("insert_handoff_if_under_depth: begin failed: {}", e))?;

        // Ensure thread exists inside the transaction.
        sqlx::query(
            "INSERT INTO threads (thread_id)
             VALUES (?)
             ON CONFLICT(thread_id) DO UPDATE SET
               updated_at = strftime('%s','now')",
        )
        .bind(thread_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("insert_handoff_if_under_depth: ensure_thread failed: {}", e))?;

        // Count existing handoff messages inside the same transaction.
        let row: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM messages WHERE thread_id = ? AND intent = 'handoff'",
        )
        .bind(thread_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| format!("insert_handoff_if_under_depth: count failed: {}", e))?;
        let current_depth = row.0;

        if current_depth >= max_depth {
            tx.commit()
                .await
                .map_err(|e| format!("insert_handoff_if_under_depth: commit failed: {}", e))?;
            return Ok(None);
        }

        // Insert the handoff message.
        let row: (i64,) = sqlx::query_as(
            "INSERT INTO messages (thread_id, from_alias, to_alias, intent, body)
             VALUES (?, ?, ?, 'handoff', ?)
             RETURNING id",
        )
        .bind(thread_id)
        .bind(from_alias)
        .bind(to_alias)
        .bind(body)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| format!("insert_handoff_if_under_depth: insert failed: {}", e))?;

        tx.commit()
            .await
            .map_err(|e| format!("insert_handoff_if_under_depth: commit failed: {}", e))?;

        Ok(Some(row.0))
    }

    /// Create N new threads + N handoff messages in a single transaction (fan-out).
    ///
    /// Each created thread gets an auto-generated ULID thread_id and the shared
    /// `batch_id`. Returns vec of `(thread_id, message_id)` pairs in target order.
    pub async fn insert_fanout_handoffs(
        &self,
        source_thread_id: &str,
        batch_id: &str,
        targets: &[String],
        from_alias: &str,
        body: &str,
    ) -> Result<Vec<(String, i64)>, String> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| format!("insert_fanout_handoffs: begin failed: {}", e))?;

        let results = insert_fanout_threads_in_tx(
            &mut tx,
            source_thread_id,
            batch_id,
            targets,
            from_alias,
            body,
            "insert_fanout_handoffs",
        )
        .await?;

        tx.commit()
            .await
            .map_err(|e| format!("insert_fanout_handoffs: commit failed: {}", e))?;

        Ok(results)
    }

    /// Atomically insert a reply message on the source thread AND create fan-out
    /// threads + handoff messages in a single transaction.
    ///
    /// This prevents the race where `--await-chain` sees the reply message but
    /// the fan-out threads haven't been created yet.
    ///
    /// Returns `(reply_message_id, Vec<(fanout_thread_id, fanout_message_id)>)`.
    pub async fn insert_reply_and_fanout(
        &self,
        params: &ReplyAndFanoutParams<'_>,
    ) -> Result<(i64, Vec<(String, i64)>), String> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| format!("insert_reply_and_fanout: begin failed: {}", e))?;

        // Ensure the reply thread exists.
        sqlx::query(
            "INSERT INTO threads (thread_id)
             VALUES (?)
             ON CONFLICT(thread_id) DO UPDATE SET
               updated_at = strftime('%s','now')",
        )
        .bind(params.reply_thread_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("insert_reply_and_fanout: ensure_thread failed: {}", e))?;

        // Insert reply message.
        let reply_row: (i64,) = sqlx::query_as(
            "INSERT INTO messages (thread_id, from_alias, to_alias, intent, body)
             VALUES (?, ?, ?, ?, ?)
             RETURNING id",
        )
        .bind(params.reply_thread_id)
        .bind(params.reply_from)
        .bind(params.reply_to)
        .bind(params.reply_intent)
        .bind(params.reply_body)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| format!("insert_reply_and_fanout: insert reply failed: {}", e))?;

        // Create fan-out threads + handoff messages.
        let fanout_results = insert_fanout_threads_in_tx(
            &mut tx,
            params.source_thread_id,
            params.batch_id,
            params.targets,
            params.handoff_from,
            params.handoff_body,
            "insert_reply_and_fanout",
        )
        .await?;

        tx.commit()
            .await
            .map_err(|e| format!("insert_reply_and_fanout: commit failed: {}", e))?;

        Ok((reply_row.0, fanout_results))
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
                    e.attempt_number, e.retry_after, e.error_category,
                    e.original_dispatch_message_id,
                    e.pid
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
    #[allow(clippy::too_many_arguments)]
    pub async fn complete_execution(
        &self,
        id: &str,
        exit_code: Option<i32>,
        output_preview: Option<&str>,
        parsed_intent: Option<&str>,
        duration_ms: i64,
        cost_usd: Option<f64>,
        tokens_in: Option<i64>,
        tokens_out: Option<i64>,
        num_turns: Option<i32>,
    ) -> Result<u64, sqlx::Error> {
        let result = sqlx::query(
            "UPDATE executions
             SET status = 'completed',
                 finished_at = strftime('%s','now'),
                 exit_code = ?,
                 output_preview = ?,
                 parsed_intent = ?,
                 duration_ms = ?,
                 cost_usd = ?,
                 tokens_in = ?,
                 tokens_out = ?,
                 num_turns = ?
             WHERE id = ?
               AND status IN ('picked_up', 'executing')",
        )
        .bind(exit_code)
        .bind(output_preview)
        .bind(parsed_intent)
        .bind(duration_ms)
        .bind(cost_usd)
        .bind(tokens_in)
        .bind(tokens_out)
        .bind(num_turns)
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
    #[allow(clippy::too_many_arguments)]
    pub async fn fail_execution(
        &self,
        id: &str,
        error_detail: &str,
        exit_code: Option<i32>,
        duration_ms: i64,
        status: ExecutionStatus,
        cost_usd: Option<f64>,
        tokens_in: Option<i64>,
        tokens_out: Option<i64>,
        num_turns: Option<i32>,
    ) -> Result<u64, sqlx::Error> {
        let result = sqlx::query(
            "UPDATE executions
             SET status = ?,
                 finished_at = strftime('%s','now'),
                 error_detail = ?,
                 exit_code = ?,
                 duration_ms = ?,
                 cost_usd = ?,
                 tokens_in = ?,
                 tokens_out = ?,
                 num_turns = ?
             WHERE id = ?
               AND status IN ('picked_up', 'executing')",
        )
        .bind(status.as_str())
        .bind(error_detail)
        .bind(exit_code)
        .bind(duration_ms)
        .bind(cost_usd)
        .bind(tokens_in)
        .bind(tokens_out)
        .bind(num_turns)
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

    /// Persist the OS PID for a running execution.
    pub async fn set_execution_pid(&self, execution_id: &str, pid: u32) -> Result<(), String> {
        sqlx::query("UPDATE executions SET pid = ? WHERE id = ?")
            .bind(pid as i64)
            .bind(execution_id)
            .execute(&self.pool)
            .await
            .map_err(|e| format!("set_execution_pid failed: {}", e))?;
        Ok(())
    }

    /// Get orphaned executions (picked_up/executing) that have a recorded PID.
    ///
    /// Used at startup to kill still-alive backend processes before marking
    /// executions as crashed.
    pub async fn get_orphaned_executions_with_pid(
        &self,
    ) -> Result<Vec<(String, u32)>, sqlx::Error> {
        let rows: Vec<(String, i64)> = sqlx::query_as(
            "SELECT id, pid FROM executions
             WHERE status IN ('picked_up', 'executing') AND pid IS NOT NULL",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|(id, pid)| (id, pid as u32)).collect())
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
                    e.attempt_number, e.retry_after, e.error_category,
                    e.original_dispatch_message_id,
                    e.pid
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
                    e.attempt_number, e.retry_after, e.error_category,
                    e.original_dispatch_message_id,
                    e.pid
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
                    e.attempt_number, e.retry_after, e.error_category,
                    e.original_dispatch_message_id,
                    e.pid
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
                    e.attempt_number, e.retry_after, e.error_category,
                    e.original_dispatch_message_id,
                    e.pid
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
            "SELECT t.thread_id, t.batch_id, t.summary, t.status, t.created_at, t.updated_at,
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
            String,         // t.thread_id
            Option<String>, // t.batch_id
            Option<String>, // t.summary
            String,         // t.status
            i64,            // t.created_at
            i64,            // t.updated_at
            Option<String>, // e.id
            Option<String>, // agent_alias
            Option<String>, // e.status
            Option<i64>,    // e.queued_at
            Option<i64>,    // e.started_at
            Option<i64>,    // e.finished_at
            Option<i64>,    // e.duration_ms
            Option<String>, // e.error_detail
            Option<String>, // e.parsed_intent
            Option<String>, // e.prompt_hash
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
                summary: r.2,
                thread_status: r.3,
                thread_created_at: r.4,
                thread_updated_at: r.5,
                execution_id: r.6,
                agent_alias: r.7,
                execution_status: r.8,
                queued_at: r.9,
                started_at: r.10,
                finished_at: r.11,
                duration_ms: r.12,
                error_detail: r.13,
                parsed_intent: r.14,
                prompt_hash: r.15,
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
                    e.attempt_number, e.retry_after, e.error_category,
                    e.original_dispatch_message_id,
                    e.pid
             FROM executions e
             LEFT JOIN threads t ON t.thread_id = e.thread_id
             WHERE e.id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(row_to_execution))
    }

    /// Retrieve the backend-specific session ID from the most recent execution
    /// (any status) for a given thread+agent pair.
    ///
    /// Used by the executor to resume a prior CLI session rather than starting
    /// a fresh one on every dispatch. Session IDs are now persisted mid-stream
    /// (within milliseconds of backend startup), so crashed and failed
    /// executions also have valid session IDs for resumption.
    pub async fn get_last_backend_session_id(
        &self,
        thread_id: &str,
        agent_alias: &str,
    ) -> Result<Option<String>, sqlx::Error> {
        let row: Option<(String, String)> = sqlx::query_as(
            "SELECT backend_session_id, status FROM executions
             WHERE thread_id = ? AND agent_alias = ? AND backend_session_id IS NOT NULL
             ORDER BY COALESCE(finished_at, started_at, queued_at) DESC, id DESC LIMIT 1",
        )
        .bind(thread_id)
        .bind(agent_alias)
        .fetch_optional(&self.pool)
        .await?;
        if let Some((sid, status)) = row {
            if status != "completed" {
                tracing::debug!(
                    thread_id = %thread_id,
                    agent_alias = %agent_alias,
                    status = %status,
                    "resuming session from non-completed execution"
                );
            }
            Ok(Some(sid))
        } else {
            Ok(None)
        }
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

    /// Insert a retry execution for a failed execution.
    ///
    /// Creates a new queued execution with an incremented attempt number
    /// and a `retry_after` timestamp for exponential backoff.
    ///
    /// The `dispatch_message_id` is intentionally NOT set on retry executions
    /// to avoid violating the partial UNIQUE index. Instead,
    /// `original_dispatch_message_id` (non-unique) links back to the
    /// originating dispatch so the loop runner can resolve the instruction.
    ///
    /// Returns the new execution ID.
    pub async fn insert_retry_execution(
        &self,
        thread_id: &str,
        agent_alias: &str,
        original_dispatch_message_id: Option<i64>,
        prompt_hash: Option<&str>,
        attempt_number: i32,
        retry_after: i64,
    ) -> Result<String, sqlx::Error> {
        let id = ulid::Ulid::new().to_string();
        sqlx::query(
            "INSERT INTO executions (id, thread_id, agent_alias, status, prompt_hash, attempt_number, retry_after, original_dispatch_message_id)
             VALUES (?, ?, ?, 'queued', ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(thread_id)
        .bind(agent_alias)
        .bind(prompt_hash)
        .bind(attempt_number)
        .bind(retry_after)
        .bind(original_dispatch_message_id)
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
                "INSERT INTO execution_events (execution_id, event_type, summary, detail, timestamp_ms, event_index, tool_name)
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(execution_id)
            .bind(&event.event_type)
            .bind(&event.summary)
            .bind(&event.detail)
            .bind(event.timestamp_ms)
            .bind(event.event_index)
            .bind(&event.tool_name)
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

        let rows: Vec<(
            i64,
            String,
            String,
            String,
            Option<String>,
            i64,
            i32,
            Option<String>,
        )> = sqlx::query_as(
            "SELECT id, execution_id, event_type, summary, detail, timestamp_ms, event_index, tool_name
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
                tool_name: r.7,
            })
            .collect())
    }

    /// Fetch the single most recent execution event for an execution (by event_index DESC).
    pub async fn get_latest_execution_event(
        &self,
        execution_id: &str,
    ) -> Result<Option<ExecutionEventRow>, sqlx::Error> {
        let row: Option<(
            i64,
            String,
            String,
            String,
            Option<String>,
            i64,
            i32,
            Option<String>,
        )> = sqlx::query_as(
            "SELECT id, execution_id, event_type, summary, detail, timestamp_ms, event_index, tool_name
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
            tool_name: r.7,
        }))
    }

    /// Fetch the most recent execution event that represents meaningful progress,
    /// excluding noisy event types (`tool_result`, `turn_complete`).
    pub async fn get_latest_progress_event(
        &self,
        execution_id: &str,
    ) -> Result<Option<ExecutionEventRow>, sqlx::Error> {
        let row: Option<(
            i64,
            String,
            String,
            String,
            Option<String>,
            i64,
            i32,
            Option<String>,
        )> = sqlx::query_as(
            "SELECT id, execution_id, event_type, summary, detail, timestamp_ms, event_index, tool_name
                 FROM execution_events
                 WHERE execution_id = ? AND event_type NOT IN ('tool_result', 'turn_complete')
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
            tool_name: r.7,
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

    // ── Observability aggregates ──────────────────────────────────────────

    /// Count tool_call events grouped by tool_name.
    /// Optional agent_alias filter via join to executions.
    ///
    /// Note: `error_count` and `error_rate` in returned `ToolCallStat` are always 0.
    /// Use [`Store::tool_error_rates`] to get per-tool error rates.
    pub async fn tool_call_counts(
        &self,
        agent_alias: Option<&str>,
    ) -> Result<Vec<ToolCallStat>, sqlx::Error> {
        let rows: Vec<(String, i64)> = if let Some(alias) = agent_alias {
            sqlx::query_as(
                "SELECT ee.tool_name, COUNT(*) as call_count
                 FROM execution_events ee
                 JOIN executions e ON ee.execution_id = e.id
                 WHERE ee.event_type = 'tool_call' AND ee.tool_name IS NOT NULL
                   AND e.agent_alias = ?
                 GROUP BY ee.tool_name
                 ORDER BY call_count DESC",
            )
            .bind(alias)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query_as(
                "SELECT tool_name, COUNT(*) as call_count
                 FROM execution_events
                 WHERE event_type = 'tool_call' AND tool_name IS NOT NULL
                 GROUP BY tool_name
                 ORDER BY call_count DESC",
            )
            .fetch_all(&self.pool)
            .await?
        };

        Ok(rows
            .into_iter()
            .map(|(tool_name, call_count)| ToolCallStat {
                tool_name,
                call_count,
                error_count: 0,
                error_rate: 0.0,
            })
            .collect())
    }

    /// Per-tool error rate from tool_result events.
    /// Degrades gracefully to zero error counts when tool_result events have no tool_name.
    pub async fn tool_error_rates(
        &self,
        agent_alias: Option<&str>,
    ) -> Result<Vec<ToolCallStat>, sqlx::Error> {
        let rows: Vec<(String, i64, i64)> = if let Some(alias) = agent_alias {
            sqlx::query_as(
                "SELECT
                    tc.tool_name,
                    tc.call_count,
                    COALESCE(tr.error_count, 0) as error_count
                 FROM (
                     SELECT ee.tool_name, COUNT(*) as call_count
                     FROM execution_events ee
                     JOIN executions e ON ee.execution_id = e.id
                     WHERE ee.event_type = 'tool_call' AND ee.tool_name IS NOT NULL
                       AND e.agent_alias = ?
                     GROUP BY ee.tool_name
                 ) tc
                 LEFT JOIN (
                     SELECT ee.tool_name, COUNT(*) as error_count
                     FROM execution_events ee
                     JOIN executions e ON ee.execution_id = e.id
                     WHERE ee.event_type = 'tool_result' AND ee.tool_name IS NOT NULL
                       AND e.agent_alias = ?
                       AND (lower(ee.summary) LIKE '%error%' OR lower(ee.summary) LIKE '%fail%')
                     GROUP BY ee.tool_name
                 ) tr ON tc.tool_name = tr.tool_name
                 ORDER BY tc.call_count DESC",
            )
            .bind(alias)
            .bind(alias)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query_as(
                "SELECT
                    tc.tool_name,
                    tc.call_count,
                    COALESCE(tr.error_count, 0) as error_count
                 FROM (
                     SELECT tool_name, COUNT(*) as call_count
                     FROM execution_events
                     WHERE event_type = 'tool_call' AND tool_name IS NOT NULL
                     GROUP BY tool_name
                 ) tc
                 LEFT JOIN (
                     SELECT tool_name, COUNT(*) as error_count
                     FROM execution_events
                     WHERE event_type = 'tool_result' AND tool_name IS NOT NULL
                       AND (lower(summary) LIKE '%error%' OR lower(summary) LIKE '%fail%')
                     GROUP BY tool_name
                 ) tr ON tc.tool_name = tr.tool_name
                 ORDER BY tc.call_count DESC",
            )
            .fetch_all(&self.pool)
            .await?
        };

        Ok(rows
            .into_iter()
            .map(|(tool_name, call_count, error_count)| ToolCallStat {
                error_rate: if call_count > 0 {
                    error_count as f64 / call_count as f64
                } else {
                    0.0
                },
                tool_name,
                call_count,
                error_count,
            })
            .collect())
    }

    /// Tool usage breakdown by agent: (agent_alias, tool_name, count).
    pub async fn tool_usage_by_agent(&self) -> Result<Vec<(String, String, i64)>, sqlx::Error> {
        sqlx::query_as(
            "SELECT e.agent_alias, ee.tool_name, COUNT(*) as count
             FROM execution_events ee
             JOIN executions e ON ee.execution_id = e.id
             WHERE ee.event_type = 'tool_call' AND ee.tool_name IS NOT NULL
             GROUP BY e.agent_alias, ee.tool_name
             ORDER BY e.agent_alias ASC, count DESC",
        )
        .fetch_all(&self.pool)
        .await
    }

    /// Aggregate cost and token summary across executions.
    /// NULL cost_usd (Codex/Gemini) is treated as 0 for sum; excluded from avg.
    ///
    /// `execution_count` includes all lifecycle states (queued, executing, completed,
    /// failed, etc.) — not just completed executions. Consumers should not use it as
    /// a "successful run count".
    pub async fn cost_summary(
        &self,
        agent_alias: Option<&str>,
    ) -> Result<CostSummary, sqlx::Error> {
        let row: (Option<f64>, Option<f64>, Option<i64>, Option<i64>, i64, i64) =
            if let Some(alias) = agent_alias {
                sqlx::query_as(
                    "SELECT
                        SUM(cost_usd),
                        AVG(cost_usd),
                        SUM(tokens_in),
                        SUM(tokens_out),
                        COUNT(*),
                        COUNT(cost_usd)
                     FROM executions
                     WHERE agent_alias = ?",
                )
                .bind(alias)
                .fetch_one(&self.pool)
                .await?
            } else {
                sqlx::query_as(
                    "SELECT
                        SUM(cost_usd),
                        AVG(cost_usd),
                        SUM(tokens_in),
                        SUM(tokens_out),
                        COUNT(*),
                        COUNT(cost_usd)
                     FROM executions",
                )
                .fetch_one(&self.pool)
                .await?
            };

        Ok(CostSummary {
            total_cost_usd: row.0.unwrap_or(0.0),
            avg_cost_usd: row.1.unwrap_or(0.0),
            total_tokens_in: row.2.unwrap_or(0),
            total_tokens_out: row.3.unwrap_or(0),
            execution_count: row.4,
            executions_with_cost: row.5,
        })
    }

    /// Cost and token breakdown per agent.
    ///
    /// `execution_count` includes all lifecycle states, not just completed executions.
    /// Ordered by `SUM(cost_usd) DESC`; agents with NULL cost (Codex/Gemini) sort last
    /// because SQLite treats NULL as less than any non-NULL value in DESC ordering.
    pub async fn cost_by_agent(&self) -> Result<Vec<AgentCostSummary>, sqlx::Error> {
        let rows: Vec<(String, Option<f64>, Option<i64>, Option<i64>, i64)> = sqlx::query_as(
            "SELECT
                agent_alias,
                SUM(cost_usd),
                SUM(tokens_in),
                SUM(tokens_out),
                COUNT(*)
             FROM executions
             GROUP BY agent_alias
             ORDER BY SUM(cost_usd) DESC",
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(
                |(
                    agent_alias,
                    total_cost_usd,
                    total_tokens_in,
                    total_tokens_out,
                    execution_count,
                )| {
                    AgentCostSummary {
                        agent_alias,
                        total_cost_usd: total_cost_usd.unwrap_or(0.0),
                        total_tokens_in: total_tokens_in.unwrap_or(0),
                        total_tokens_out: total_tokens_out.unwrap_or(0),
                        execution_count,
                    }
                },
            )
            .collect())
    }

    // ── Merge operation methods ─────────────────────────────────────────

    /// Insert a new merge operation.
    pub async fn insert_merge_op(&self, op: &MergeOperation) -> Result<(), String> {
        sqlx::query(
            "INSERT INTO merge_operations
             (id, thread_id, source_branch, target_branch, merge_strategy,
              requested_by, status, push_requested, queued_at,
              claimed_at, started_at, finished_at, duration_ms,
              result_summary, error_detail, conflict_files)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&op.id)
        .bind(&op.thread_id)
        .bind(&op.source_branch)
        .bind(&op.target_branch)
        .bind(&op.merge_strategy)
        .bind(&op.requested_by)
        .bind(&op.status)
        .bind(op.push_requested)
        .bind(op.queued_at)
        .bind(op.claimed_at)
        .bind(op.started_at)
        .bind(op.finished_at)
        .bind(op.duration_ms)
        .bind(&op.result_summary)
        .bind(&op.error_detail)
        .bind(&op.conflict_files)
        .execute(&self.pool)
        .await
        .map_err(|e| format!("insert_merge_op failed: {}", e))?;
        Ok(())
    }

    /// Atomically claim the next queued merge operation, respecting per-target serialization.
    ///
    /// Only claims an op when no other op for the same `target_branch` is `claimed` or
    /// `executing`. Returns the claimed op, or `None` if no work is available.
    pub async fn claim_next_merge_op(&self) -> Result<Option<MergeOperation>, String> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| format!("claim_next_merge_op: begin tx failed: {}", e))?;

        let candidate: Option<(String,)> = sqlx::query_as(
            "SELECT id FROM merge_operations
             WHERE status = 'queued'
               AND target_branch NOT IN (
                   SELECT target_branch FROM merge_operations
                   WHERE status IN ('claimed', 'executing')
               )
             ORDER BY queued_at ASC
             LIMIT 1",
        )
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| format!("claim_next_merge_op: select failed: {}", e))?;

        let Some(candidate) = candidate else {
            tx.commit()
                .await
                .map_err(|e| format!("claim_next_merge_op: commit failed: {}", e))?;
            return Ok(None);
        };
        let op_id = candidate.0;

        let result = sqlx::query(
            "UPDATE merge_operations
             SET status = 'claimed', claimed_at = strftime('%s','now')
             WHERE id = ? AND status = 'queued'",
        )
        .bind(&op_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("claim_next_merge_op: update failed: {}", e))?;

        if result.rows_affected() == 0 {
            tx.commit()
                .await
                .map_err(|e| format!("claim_next_merge_op: commit failed: {}", e))?;
            return Ok(None);
        }

        let row: Option<MergeOperation> = sqlx::query_as(
            "SELECT id, thread_id, source_branch, target_branch, merge_strategy,
                    requested_by, status, push_requested, queued_at,
                    claimed_at, started_at, finished_at, duration_ms,
                    result_summary, error_detail, conflict_files
             FROM merge_operations
             WHERE id = ?",
        )
        .bind(&op_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| format!("claim_next_merge_op: fetch claimed row failed: {}", e))?;

        tx.commit()
            .await
            .map_err(|e| format!("claim_next_merge_op: commit failed: {}", e))?;
        Ok(row)
    }

    /// Update merge operation status and optional result fields.
    ///
    /// Sets `started_at` when transitioning to `Executing`, and `finished_at` +
    /// `duration_ms` when transitioning to terminal states.
    pub async fn update_merge_op_status(
        &self,
        id: &str,
        status: MergeOperationStatus,
        result_summary: Option<&str>,
        error_detail: Option<&str>,
        conflict_files: Option<&str>,
    ) -> Result<(), String> {
        let status_str = status.as_str();

        match status {
            MergeOperationStatus::Executing => {
                sqlx::query(
                    "UPDATE merge_operations
                     SET status = ?, started_at = strftime('%s','now'),
                         result_summary = COALESCE(?, result_summary),
                         error_detail = COALESCE(?, error_detail),
                         conflict_files = COALESCE(?, conflict_files)
                     WHERE id = ?",
                )
                .bind(status_str)
                .bind(result_summary)
                .bind(error_detail)
                .bind(conflict_files)
                .bind(id)
                .execute(&self.pool)
                .await
                .map_err(|e| format!("update_merge_op_status failed: {}", e))?;
            }
            MergeOperationStatus::Completed
            | MergeOperationStatus::Failed
            | MergeOperationStatus::Cancelled => {
                // Terminal states: set finished_at and compute duration_ms from started_at
                // (or claimed_at/queued_at as fallback).
                sqlx::query(
                    "UPDATE merge_operations
                     SET status = ?,
                         finished_at = strftime('%s','now'),
                         duration_ms = (strftime('%s','now') - COALESCE(started_at, claimed_at, queued_at)) * 1000,
                         result_summary = COALESCE(?, result_summary),
                         error_detail = COALESCE(?, error_detail),
                         conflict_files = COALESCE(?, conflict_files)
                     WHERE id = ?",
                )
                .bind(status_str)
                .bind(result_summary)
                .bind(error_detail)
                .bind(conflict_files)
                .bind(id)
                .execute(&self.pool)
                .await
                .map_err(|e| format!("update_merge_op_status failed: {}", e))?;
            }
            _ => {
                sqlx::query(
                    "UPDATE merge_operations
                     SET status = ?,
                         result_summary = COALESCE(?, result_summary),
                         error_detail = COALESCE(?, error_detail),
                         conflict_files = COALESCE(?, conflict_files)
                     WHERE id = ?",
                )
                .bind(status_str)
                .bind(result_summary)
                .bind(error_detail)
                .bind(conflict_files)
                .bind(id)
                .execute(&self.pool)
                .await
                .map_err(|e| format!("update_merge_op_status failed: {}", e))?;
            }
        }
        Ok(())
    }

    /// Get a single merge operation by ID.
    pub async fn get_merge_op(&self, id: &str) -> Result<Option<MergeOperation>, String> {
        let row: Option<MergeOperation> = sqlx::query_as(
            "SELECT id, thread_id, source_branch, target_branch, merge_strategy,
                    requested_by, status, push_requested, queued_at,
                    claimed_at, started_at, finished_at, duration_ms,
                    result_summary, error_detail, conflict_files
             FROM merge_operations
             WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| format!("get_merge_op failed: {}", e))?;
        Ok(row)
    }

    /// List merge operations with optional filters.
    pub async fn list_merge_ops(
        &self,
        target_branch: Option<&str>,
        status: Option<MergeOperationStatus>,
        thread_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<MergeOperation>, String> {
        let mut conditions = Vec::new();
        if target_branch.is_some() {
            conditions.push("target_branch = ?");
        }
        if status.is_some() {
            conditions.push("status = ?");
        }
        if thread_id.is_some() {
            conditions.push("thread_id = ?");
        }

        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conditions.join(" AND "))
        };

        let sql = format!(
            "SELECT id, thread_id, source_branch, target_branch, merge_strategy,
                    requested_by, status, push_requested, queued_at,
                    claimed_at, started_at, finished_at, duration_ms,
                    result_summary, error_detail, conflict_files
             FROM merge_operations
             {}
             ORDER BY queued_at DESC
             LIMIT ?",
            where_clause
        );

        let mut query = sqlx::query_as::<_, MergeOperation>(&sql);
        if let Some(tb) = target_branch {
            query = query.bind(tb);
        }
        if let Some(ref st) = status {
            query = query.bind(st.as_str());
        }
        if let Some(tid) = thread_id {
            query = query.bind(tid);
        }
        query = query.bind(limit);

        query
            .fetch_all(&self.pool)
            .await
            .map_err(|e| format!("list_merge_ops failed: {}", e))
    }

    /// Mark merge operations in `claimed`/`executing` status older than `timeout_secs` as `failed`.
    ///
    /// Returns count of affected rows.
    pub async fn mark_stale_merge_ops_failed(&self, timeout_secs: u64) -> Result<u64, String> {
        let result = sqlx::query(
            "UPDATE merge_operations
             SET status = 'failed',
                 finished_at = strftime('%s','now'),
                 duration_ms = (strftime('%s','now') - COALESCE(started_at, claimed_at, queued_at)) * 1000,
                 error_detail = 'merge operation timed out'
             WHERE status IN ('claimed', 'executing')
               AND COALESCE(started_at, claimed_at, queued_at)
                   <= strftime('%s','now') - ?",
        )
        .bind(timeout_secs.min(i64::MAX as u64) as i64)
        .execute(&self.pool)
        .await
        .map_err(|e| format!("mark_stale_merge_ops_failed failed: {}", e))?;
        Ok(result.rows_affected())
    }

    /// Cancel a merge operation. Only succeeds if status is `queued`.
    ///
    /// Returns `true` if cancelled, `false` if not found or wrong status.
    pub async fn cancel_merge_op(&self, id: &str) -> Result<bool, String> {
        let result = sqlx::query(
            "UPDATE merge_operations
             SET status = 'cancelled',
                 finished_at = strftime('%s','now'),
                 duration_ms = (strftime('%s','now') - queued_at) * 1000
             WHERE id = ? AND status = 'queued'",
        )
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(|e| format!("cancel_merge_op failed: {}", e))?;
        Ok(result.rows_affected() > 0)
    }

    /// Check if there's an existing queued/claimed/executing merge for the given thread+target.
    ///
    /// Used for idempotency guard in preflight.
    pub async fn has_pending_merge_for_thread(
        &self,
        thread_id: &str,
        target_branch: &str,
    ) -> Result<bool, String> {
        let row: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM merge_operations
             WHERE thread_id = ? AND target_branch = ?
               AND status IN ('queued', 'claimed', 'executing')",
        )
        .bind(thread_id)
        .bind(target_branch)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| format!("has_pending_merge_for_thread failed: {}", e))?;
        Ok(row.0 > 0)
    }
}

/// Combined thread + execution view.
#[derive(Debug, Clone)]
pub struct ThreadStatusView {
    pub thread_id: String,
    pub batch_id: Option<String>,
    pub summary: Option<String>,
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

/// Create fan-out threads + handoff messages inside an existing transaction.
///
/// Shared by `insert_fanout_handoffs` and `insert_reply_and_fanout` to avoid
/// duplicating the per-target insert loop.
async fn insert_fanout_threads_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    source_thread_id: &str,
    batch_id: &str,
    targets: &[String],
    from_alias: &str,
    body: &str,
    caller: &str,
) -> Result<Vec<(String, i64)>, String> {
    let mut results = Vec::with_capacity(targets.len());

    for target_alias in targets {
        let thread_id = ulid::Ulid::new().to_string();

        sqlx::query(
            "INSERT INTO threads (thread_id, batch_id, source_thread_id)
             VALUES (?, ?, ?)",
        )
        .bind(&thread_id)
        .bind(batch_id)
        .bind(source_thread_id)
        .execute(&mut **tx)
        .await
        .map_err(|e| format!("{}: create fanout thread failed: {}", caller, e))?;

        let row: (i64,) = sqlx::query_as(
            "INSERT INTO messages (thread_id, from_alias, to_alias, intent, body, batch_id)
             VALUES (?, ?, ?, 'handoff', ?, ?)
             RETURNING id",
        )
        .bind(&thread_id)
        .bind(from_alias)
        .bind(target_alias)
        .bind(body)
        .bind(batch_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(|e| format!("{}: insert fanout message failed: {}", caller, e))?;

        results.push((thread_id, row.0));
    }

    Ok(results)
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
    original_dispatch_message_id: Option<i64>,
    pid: Option<i64>,
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
        original_dispatch_message_id: r.original_dispatch_message_id,
        pid: r.pid,
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
        store
            .ensure_thread("t-1", Some("batch-1"), None)
            .await
            .unwrap();
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
        store.ensure_thread("t-1", None, None).await.unwrap();
        let thread = store.get_thread("t-1").await.unwrap().unwrap();
        assert_eq!(thread.batch_id, None);

        store
            .ensure_thread("t-1", Some("batch-1"), None)
            .await
            .unwrap();
        let thread = store.get_thread("t-1").await.unwrap().unwrap();
        assert_eq!(thread.batch_id.as_deref(), Some("batch-1"));
    }

    #[tokio::test]
    async fn test_message_insert_and_query() {
        let store = test_store().await;
        let id = store
            .insert_message(
                "t-1", "operator", "focused", "dispatch", "do work", None, None,
            )
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
        store.ensure_thread("t-1", None, None).await.unwrap();

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
                None,
                None,
                None,
                None,
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
        store.ensure_thread("t-1", None, None).await.unwrap();
        let dispatch_id = store
            .insert_message(
                "t-1",
                "operator",
                "focused",
                "dispatch",
                "linked input",
                None,
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
        store.ensure_thread("t-1", None, None).await.unwrap();

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
        store.ensure_thread("t-1", None, None).await.unwrap();

        let exec_id = store.insert_execution("t-1", "focused").await.unwrap();

        let exec = store.get_execution(&exec_id).await.unwrap().unwrap();
        assert_eq!(exec.prompt_hash, None);
    }

    #[tokio::test]
    async fn test_per_agent_concurrency() {
        let store = test_store().await;
        store.ensure_thread("t-1", None, None).await.unwrap();
        store.ensure_thread("t-2", None, None).await.unwrap();

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
        store.ensure_thread("t-1", None, None).await.unwrap();
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
        store.ensure_thread("t-1", None, None).await.unwrap();
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
        store.ensure_thread("t-1", None, None).await.unwrap();
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
        store.ensure_thread("t-1", None, None).await.unwrap();
        let exec_id = store.insert_execution("t-1", "focused").await.unwrap();
        let _ = store.claim_next_execution(2).await.unwrap();
        store.mark_execution_executing(&exec_id).await.unwrap();
        store
            .complete_execution(
                &exec_id,
                Some(0),
                Some("ok"),
                None,
                5000,
                None,
                None,
                None,
                None,
            )
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
        store.ensure_thread("t-1", None, None).await.unwrap();
        store.ensure_thread("t-2", None, None).await.unwrap();
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
                None,
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
        store.ensure_thread("t-1", None, None).await.unwrap();

        let exec_id = store.insert_execution("t-1", "focused").await.unwrap();
        let _ = store.claim_next_execution(2).await.unwrap();
        store.mark_execution_executing(&exec_id).await.unwrap();
        store
            .complete_execution(
                &exec_id,
                Some(0),
                Some("ok"),
                None,
                1000,
                None,
                None,
                None,
                None,
            )
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
        store.ensure_thread("t-1", None, None).await.unwrap();

        // First execution for thread t-1, agent focused.
        let exec_id_1 = store.insert_execution("t-1", "focused").await.unwrap();
        let _ = store.claim_next_execution(2).await.unwrap();
        store.mark_execution_executing(&exec_id_1).await.unwrap();
        store
            .complete_execution(
                &exec_id_1,
                Some(0),
                Some("ok"),
                None,
                1000,
                None,
                None,
                None,
                None,
            )
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
            .complete_execution(
                &exec_id_2,
                Some(0),
                Some("ok"),
                None,
                1000,
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();
        store
            .set_backend_session_id(&exec_id_2, "claude-sid-latest")
            .await
            .unwrap();

        // get_last_backend_session_id must return the most recent session ID (any status).
        let sid = store
            .get_last_backend_session_id("t-1", "focused")
            .await
            .unwrap();
        assert_eq!(sid.as_deref(), Some("claude-sid-latest"));
    }

    #[tokio::test]
    async fn test_insert_retry_execution_and_attempt_number() {
        let store = test_store().await;
        store.ensure_thread("t-1", None, None).await.unwrap();

        let retry_after = chrono::Utc::now().timestamp() - 10; // in the past
        let exec_id = store
            .insert_retry_execution("t-1", "focused", None, Some("hash123"), 2, retry_after)
            .await
            .unwrap();

        let exec = store.get_execution(&exec_id).await.unwrap().unwrap();
        assert_eq!(exec.attempt_number, 2);
    }

    #[tokio::test]
    async fn test_claim_respects_retry_after_future() {
        let store = test_store().await;
        store.ensure_thread("t-1", None, None).await.unwrap();

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
        store.ensure_thread("t-1", None, None).await.unwrap();

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
        store.ensure_thread("t-1", None, None).await.unwrap();

        let exec_id = store.insert_execution("t-1", "focused").await.unwrap();
        store
            .set_error_category(&exec_id, "transient")
            .await
            .unwrap();

        let exec = store.get_execution(&exec_id).await.unwrap().unwrap();
        assert_eq!(exec.error_category.as_deref(), Some("transient"));
    }

    #[tokio::test]
    async fn test_get_last_backend_session_id_returns_from_failed_executions() {
        let store = test_store().await;
        store.ensure_thread("t-1", None, None).await.unwrap();

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
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();
        store
            .set_backend_session_id(&exec_id, "claude-sid-failed")
            .await
            .unwrap();

        // Session IDs from failed executions ARE now returned for resume.
        let sid = store
            .get_last_backend_session_id("t-1", "focused")
            .await
            .unwrap();
        assert_eq!(sid.as_deref(), Some("claude-sid-failed"));
    }

    #[tokio::test]
    async fn test_get_last_backend_session_id_crashed_most_recent_wins() {
        let store = test_store().await;
        store.ensure_thread("t-1", None, None).await.unwrap();

        // First crashed execution
        let exec_id_1 = store.insert_execution("t-1", "focused").await.unwrap();
        let _ = store.claim_next_execution(2).await.unwrap();
        store.mark_execution_executing(&exec_id_1).await.unwrap();
        store
            .set_backend_session_id(&exec_id_1, "sid-first-crash")
            .await
            .unwrap();
        store
            .fail_execution(
                &exec_id_1,
                "crash 1",
                None,
                100,
                ExecutionStatus::Crashed,
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();

        // Second crashed execution (more recent)
        let exec_id_2 = store.insert_execution("t-1", "focused").await.unwrap();
        let _ = store.claim_next_execution(2).await.unwrap();
        store.mark_execution_executing(&exec_id_2).await.unwrap();
        store
            .set_backend_session_id(&exec_id_2, "sid-second-crash")
            .await
            .unwrap();
        store
            .fail_execution(
                &exec_id_2,
                "crash 2",
                None,
                200,
                ExecutionStatus::Crashed,
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();

        // Most recent session ID should win
        let sid = store
            .get_last_backend_session_id("t-1", "focused")
            .await
            .unwrap();
        assert_eq!(sid.as_deref(), Some("sid-second-crash"));
    }

    #[tokio::test]
    async fn test_get_last_backend_session_id_from_executing_row() {
        let store = test_store().await;
        store.ensure_thread("t-1", None, None).await.unwrap();

        // A still-executing row with an early-persisted session ID
        let exec_id = store.insert_execution("t-1", "focused").await.unwrap();
        let _ = store.claim_next_execution(2).await.unwrap();
        store.mark_execution_executing(&exec_id).await.unwrap();
        store
            .set_backend_session_id(&exec_id, "sid-early-persist")
            .await
            .unwrap();

        // Should return the early-persisted session ID even though status is "executing"
        let sid = store
            .get_last_backend_session_id("t-1", "focused")
            .await
            .unwrap();
        assert_eq!(sid.as_deref(), Some("sid-early-persist"));
    }

    #[tokio::test]
    async fn test_execution_events_tool_name_roundtrip() {
        let store = test_store().await;
        store.ensure_thread("t-ev", None, None).await.unwrap();
        let exec_id = store.insert_execution("t-ev", "agent-ev").await.unwrap();

        let events = vec![
            crate::backend::ExecutionEvent {
                event_type: "tool_call".to_string(),
                summary: "Write to src/main.rs".to_string(),
                detail: None,
                timestamp_ms: 1000,
                event_index: 0,
                tool_name: Some("Write".to_string()),
            },
            crate::backend::ExecutionEvent {
                event_type: "tool_result".to_string(),
                summary: "tu_01: completed".to_string(),
                detail: None,
                timestamp_ms: 1001,
                event_index: 1,
                tool_name: None,
            },
            crate::backend::ExecutionEvent {
                event_type: "turn_complete".to_string(),
                summary: "completed".to_string(),
                detail: None,
                timestamp_ms: 1002,
                event_index: 2,
                tool_name: None,
            },
        ];

        store
            .insert_execution_events(&exec_id, &events)
            .await
            .unwrap();

        let rows = store
            .get_execution_events(&exec_id, None, None, None)
            .await
            .unwrap();
        assert_eq!(rows.len(), 3);

        // tool_call has tool_name populated
        assert_eq!(rows[0].event_type, "tool_call");
        assert_eq!(rows[0].tool_name.as_deref(), Some("Write"));

        // tool_result has tool_name=None (Claude limitation)
        assert_eq!(rows[1].event_type, "tool_result");
        assert!(rows[1].tool_name.is_none());

        // turn_complete has tool_name=None
        assert_eq!(rows[2].event_type, "turn_complete");
        assert!(rows[2].tool_name.is_none());

        // Also verify get_latest_execution_event returns the last one
        let latest = store
            .get_latest_execution_event(&exec_id)
            .await
            .unwrap()
            .expect("should have a latest event");
        assert_eq!(latest.event_type, "turn_complete");
        assert!(latest.tool_name.is_none());
    }

    #[tokio::test]
    async fn test_get_latest_progress_event_excludes_noisy_events() {
        let store = test_store().await;
        store.ensure_thread("t-prog", None, None).await.unwrap();
        let exec_id = store
            .insert_execution("t-prog", "agent-prog")
            .await
            .unwrap();

        // Insert: tool_call (0), tool_result (1), turn_complete (2)
        let events = vec![
            crate::backend::ExecutionEvent {
                event_type: "tool_call".to_string(),
                summary: "Read src/lib.rs".to_string(),
                detail: None,
                timestamp_ms: 1000,
                event_index: 0,
                tool_name: Some("Read".to_string()),
            },
            crate::backend::ExecutionEvent {
                event_type: "tool_result".to_string(),
                summary: "tu_01: completed".to_string(),
                detail: None,
                timestamp_ms: 1001,
                event_index: 1,
                tool_name: None,
            },
            crate::backend::ExecutionEvent {
                event_type: "turn_complete".to_string(),
                summary: "completed".to_string(),
                detail: None,
                timestamp_ms: 1002,
                event_index: 2,
                tool_name: None,
            },
        ];
        store
            .insert_execution_events(&exec_id, &events)
            .await
            .unwrap();

        // get_latest_progress_event should skip tool_result and turn_complete,
        // returning the tool_call at index 0.
        let progress = store
            .get_latest_progress_event(&exec_id)
            .await
            .unwrap()
            .expect("should find the tool_call event");
        assert_eq!(progress.event_type, "tool_call");
        assert_eq!(progress.summary, "Read src/lib.rs");
        assert_eq!(progress.event_index, 0);

        // get_latest_execution_event (unfiltered) still returns the last event.
        let latest = store
            .get_latest_execution_event(&exec_id)
            .await
            .unwrap()
            .expect("should find the turn_complete event");
        assert_eq!(latest.event_type, "turn_complete");
    }

    #[tokio::test]
    async fn test_get_latest_progress_event_returns_none_when_only_noisy() {
        let store = test_store().await;
        store.ensure_thread("t-noisy", None, None).await.unwrap();
        let exec_id = store
            .insert_execution("t-noisy", "agent-noisy")
            .await
            .unwrap();

        // Insert only tool_result and turn_complete — no meaningful progress events.
        let events = vec![
            crate::backend::ExecutionEvent {
                event_type: "tool_result".to_string(),
                summary: "tu_01: completed".to_string(),
                detail: None,
                timestamp_ms: 1000,
                event_index: 0,
                tool_name: None,
            },
            crate::backend::ExecutionEvent {
                event_type: "turn_complete".to_string(),
                summary: "completed".to_string(),
                detail: None,
                timestamp_ms: 1001,
                event_index: 1,
                tool_name: None,
            },
        ];
        store
            .insert_execution_events(&exec_id, &events)
            .await
            .unwrap();

        // Should return None — all events are excluded.
        let result = store.get_latest_progress_event(&exec_id).await.unwrap();
        assert!(
            result.is_none(),
            "should return None when only noisy events exist"
        );
    }

    // ── OBS-02: observability aggregate tests ────────────────────────────

    #[tokio::test]
    async fn test_tool_call_counts_empty() {
        let store = test_store().await;
        let stats = store.tool_call_counts(None).await.unwrap();
        assert!(stats.is_empty(), "empty table should return empty vec");
    }

    #[tokio::test]
    async fn test_tool_call_counts_basic() {
        let store = test_store().await;
        store.ensure_thread("t-tc-1", None, None).await.unwrap();
        let exec_id = store.insert_execution("t-tc-1", "focused").await.unwrap();

        let events = vec![
            crate::backend::ExecutionEvent {
                event_type: "tool_call".to_string(),
                summary: "Write file".to_string(),
                detail: None,
                timestamp_ms: 1000,
                event_index: 0,
                tool_name: Some("Write".to_string()),
            },
            crate::backend::ExecutionEvent {
                event_type: "tool_call".to_string(),
                summary: "Read file".to_string(),
                detail: None,
                timestamp_ms: 1001,
                event_index: 1,
                tool_name: Some("Read".to_string()),
            },
            crate::backend::ExecutionEvent {
                event_type: "tool_call".to_string(),
                summary: "Write file again".to_string(),
                detail: None,
                timestamp_ms: 1002,
                event_index: 2,
                tool_name: Some("Write".to_string()),
            },
        ];
        store
            .insert_execution_events(&exec_id, &events)
            .await
            .unwrap();

        let stats = store.tool_call_counts(None).await.unwrap();
        assert_eq!(stats.len(), 2);

        // Write has 2 calls, Read has 1; ordered by count DESC
        let write_stat = stats.iter().find(|s| s.tool_name == "Write").unwrap();
        assert_eq!(write_stat.call_count, 2);
        assert_eq!(write_stat.error_count, 0);
        assert_eq!(write_stat.error_rate, 0.0);

        let read_stat = stats.iter().find(|s| s.tool_name == "Read").unwrap();
        assert_eq!(read_stat.call_count, 1);
    }

    #[tokio::test]
    async fn test_tool_call_counts_agent_filter() {
        let store = test_store().await;
        store.ensure_thread("t-tc-2", None, None).await.unwrap();
        store.ensure_thread("t-tc-3", None, None).await.unwrap();

        let exec_focused = store.insert_execution("t-tc-2", "focused").await.unwrap();
        let exec_spark = store.insert_execution("t-tc-3", "spark").await.unwrap();

        let events_focused = vec![crate::backend::ExecutionEvent {
            event_type: "tool_call".to_string(),
            summary: "Grep".to_string(),
            detail: None,
            timestamp_ms: 2000,
            event_index: 0,
            tool_name: Some("Grep".to_string()),
        }];
        let events_spark = vec![crate::backend::ExecutionEvent {
            event_type: "tool_call".to_string(),
            summary: "Read".to_string(),
            detail: None,
            timestamp_ms: 2001,
            event_index: 0,
            tool_name: Some("Read".to_string()),
        }];

        store
            .insert_execution_events(&exec_focused, &events_focused)
            .await
            .unwrap();
        store
            .insert_execution_events(&exec_spark, &events_spark)
            .await
            .unwrap();

        // Filter for "focused" only
        let stats = store.tool_call_counts(Some("focused")).await.unwrap();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].tool_name, "Grep");

        // Filter for "spark" only
        let stats = store.tool_call_counts(Some("spark")).await.unwrap();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].tool_name, "Read");
    }

    #[tokio::test]
    async fn test_tool_error_rates_empty() {
        let store = test_store().await;
        let stats = store.tool_error_rates(None).await.unwrap();
        assert!(stats.is_empty(), "empty table should return empty vec");
    }

    #[tokio::test]
    async fn test_tool_error_rates_no_tool_result_name() {
        // When tool_result events have NULL tool_name (Claude behavior),
        // error_count should degrade gracefully to 0.
        let store = test_store().await;
        store.ensure_thread("t-er-1", None, None).await.unwrap();
        let exec_id = store.insert_execution("t-er-1", "focused").await.unwrap();

        let events = vec![
            crate::backend::ExecutionEvent {
                event_type: "tool_call".to_string(),
                summary: "Write file".to_string(),
                detail: None,
                timestamp_ms: 3000,
                event_index: 0,
                tool_name: Some("Write".to_string()),
            },
            crate::backend::ExecutionEvent {
                event_type: "tool_result".to_string(),
                summary: "error: permission denied".to_string(),
                detail: None,
                timestamp_ms: 3001,
                event_index: 1,
                tool_name: None, // Claude doesn't populate tool_name on tool_result
            },
        ];
        store
            .insert_execution_events(&exec_id, &events)
            .await
            .unwrap();

        let stats = store.tool_error_rates(None).await.unwrap();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].tool_name, "Write");
        assert_eq!(stats[0].call_count, 1);
        // error_count is 0 because tool_result has NULL tool_name
        assert_eq!(stats[0].error_count, 0);
        assert_eq!(stats[0].error_rate, 0.0);
    }

    #[tokio::test]
    async fn test_tool_error_rates_with_tool_name_on_result() {
        // When tool_result events DO have tool_name and contain error keywords,
        // error_count is populated and error_rate is computed.
        let store = test_store().await;
        store.ensure_thread("t-er-2", None, None).await.unwrap();
        let exec_id = store.insert_execution("t-er-2", "focused").await.unwrap();

        let events = vec![
            crate::backend::ExecutionEvent {
                event_type: "tool_call".to_string(),
                summary: "Bash cmd".to_string(),
                detail: None,
                timestamp_ms: 4000,
                event_index: 0,
                tool_name: Some("Bash".to_string()),
            },
            crate::backend::ExecutionEvent {
                event_type: "tool_call".to_string(),
                summary: "Bash cmd 2".to_string(),
                detail: None,
                timestamp_ms: 4001,
                event_index: 1,
                tool_name: Some("Bash".to_string()),
            },
            crate::backend::ExecutionEvent {
                event_type: "tool_result".to_string(),
                summary: "error: command not found".to_string(),
                detail: None,
                timestamp_ms: 4002,
                event_index: 2,
                tool_name: Some("Bash".to_string()),
            },
        ];
        store
            .insert_execution_events(&exec_id, &events)
            .await
            .unwrap();

        let stats = store.tool_error_rates(None).await.unwrap();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].tool_name, "Bash");
        assert_eq!(stats[0].call_count, 2);
        assert_eq!(stats[0].error_count, 1);
        // 1 error out of 2 calls = 0.5
        assert!((stats[0].error_rate - 0.5).abs() < 1e-9);
    }

    #[tokio::test]
    async fn test_tool_usage_by_agent_empty() {
        let store = test_store().await;
        let rows = store.tool_usage_by_agent().await.unwrap();
        assert!(rows.is_empty());
    }

    #[tokio::test]
    async fn test_tool_usage_by_agent_basic() {
        let store = test_store().await;
        store.ensure_thread("t-ua-1", None, None).await.unwrap();
        store.ensure_thread("t-ua-2", None, None).await.unwrap();

        let exec_focused = store.insert_execution("t-ua-1", "focused").await.unwrap();
        let exec_spark = store.insert_execution("t-ua-2", "spark").await.unwrap();

        let events_focused = vec![
            crate::backend::ExecutionEvent {
                event_type: "tool_call".to_string(),
                summary: "Write".to_string(),
                detail: None,
                timestamp_ms: 5000,
                event_index: 0,
                tool_name: Some("Write".to_string()),
            },
            crate::backend::ExecutionEvent {
                event_type: "tool_call".to_string(),
                summary: "Write again".to_string(),
                detail: None,
                timestamp_ms: 5001,
                event_index: 1,
                tool_name: Some("Write".to_string()),
            },
        ];
        let events_spark = vec![crate::backend::ExecutionEvent {
            event_type: "tool_call".to_string(),
            summary: "Read".to_string(),
            detail: None,
            timestamp_ms: 5002,
            event_index: 0,
            tool_name: Some("Read".to_string()),
        }];

        store
            .insert_execution_events(&exec_focused, &events_focused)
            .await
            .unwrap();
        store
            .insert_execution_events(&exec_spark, &events_spark)
            .await
            .unwrap();

        let rows = store.tool_usage_by_agent().await.unwrap();
        assert_eq!(rows.len(), 2);

        // focused/Write: 2 calls
        let focused_row = rows
            .iter()
            .find(|r| r.0 == "focused" && r.1 == "Write")
            .unwrap();
        assert_eq!(focused_row.2, 2);

        // spark/Read: 1 call
        let spark_row = rows
            .iter()
            .find(|r| r.0 == "spark" && r.1 == "Read")
            .unwrap();
        assert_eq!(spark_row.2, 1);
    }

    #[tokio::test]
    async fn test_cost_summary_empty() {
        let store = test_store().await;
        let summary = store.cost_summary(None).await.unwrap();
        assert_eq!(summary.total_cost_usd, 0.0);
        assert_eq!(summary.avg_cost_usd, 0.0);
        assert_eq!(summary.total_tokens_in, 0);
        assert_eq!(summary.total_tokens_out, 0);
        assert_eq!(summary.execution_count, 0);
    }

    #[tokio::test]
    async fn test_cost_summary_basic() {
        let store = test_store().await;
        store.ensure_thread("t-cs-1", None, None).await.unwrap();
        store.ensure_thread("t-cs-2", None, None).await.unwrap();

        // Execution 1: focused, cost=0.10, tokens_in=1000, tokens_out=500
        let exec1 = store.insert_execution("t-cs-1", "focused").await.unwrap();
        let _ = store.claim_next_execution(10).await.unwrap();
        store.mark_execution_executing(&exec1).await.unwrap();
        store
            .complete_execution(
                &exec1,
                Some(0),
                None,
                None,
                1000,
                Some(0.10),
                Some(1000),
                Some(500),
                None,
            )
            .await
            .unwrap();

        // Execution 2: focused, cost=0.20, tokens_in=2000, tokens_out=1000
        let exec2 = store.insert_execution("t-cs-2", "focused").await.unwrap();
        let _ = store.claim_next_execution(10).await.unwrap();
        store.mark_execution_executing(&exec2).await.unwrap();
        store
            .complete_execution(
                &exec2,
                Some(0),
                None,
                None,
                2000,
                Some(0.20),
                Some(2000),
                Some(1000),
                None,
            )
            .await
            .unwrap();

        let summary = store.cost_summary(None).await.unwrap();
        assert_eq!(summary.execution_count, 2);
        assert!((summary.total_cost_usd - 0.30).abs() < 1e-9);
        assert!((summary.avg_cost_usd - 0.15).abs() < 1e-9);
        assert_eq!(summary.total_tokens_in, 3000);
        assert_eq!(summary.total_tokens_out, 1500);
    }

    #[tokio::test]
    async fn test_cost_summary_null_cost_excluded_from_avg() {
        // Codex/Gemini executions have NULL cost_usd; SUM ignores NULL,
        // AVG also ignores NULL rows — only non-NULL costs are averaged.
        let store = test_store().await;
        store
            .ensure_thread("t-cs-null-1", None, None)
            .await
            .unwrap();
        store
            .ensure_thread("t-cs-null-2", None, None)
            .await
            .unwrap();

        // Execution 1: has cost
        let exec1 = store
            .insert_execution("t-cs-null-1", "focused")
            .await
            .unwrap();
        let _ = store.claim_next_execution(10).await.unwrap();
        store.mark_execution_executing(&exec1).await.unwrap();
        store
            .complete_execution(
                &exec1,
                Some(0),
                None,
                None,
                1000,
                Some(0.10),
                Some(500),
                Some(200),
                None,
            )
            .await
            .unwrap();

        // Execution 2: NULL cost (Codex-style)
        let exec2 = store
            .insert_execution("t-cs-null-2", "spark")
            .await
            .unwrap();
        let _ = store.claim_next_execution(10).await.unwrap();
        store.mark_execution_executing(&exec2).await.unwrap();
        store
            .complete_execution(
                &exec2,
                Some(0),
                None,
                None,
                500,
                None,
                Some(300),
                Some(100),
                None,
            )
            .await
            .unwrap();

        let summary = store.cost_summary(None).await.unwrap();
        assert_eq!(summary.execution_count, 2);
        // total_cost_usd: SUM ignores NULL → 0.10
        assert!((summary.total_cost_usd - 0.10).abs() < 1e-9);
        // avg_cost_usd: only 1 non-NULL row → 0.10
        assert!((summary.avg_cost_usd - 0.10).abs() < 1e-9);
        // tokens sum across all (both rows have token data)
        assert_eq!(summary.total_tokens_in, 800);
        assert_eq!(summary.total_tokens_out, 300);
    }

    #[tokio::test]
    async fn test_cost_summary_agent_filter() {
        let store = test_store().await;
        store.ensure_thread("t-cs-f-1", None, None).await.unwrap();
        store.ensure_thread("t-cs-f-2", None, None).await.unwrap();

        let exec1 = store.insert_execution("t-cs-f-1", "focused").await.unwrap();
        let _ = store.claim_next_execution(10).await.unwrap();
        store.mark_execution_executing(&exec1).await.unwrap();
        store
            .complete_execution(
                &exec1,
                Some(0),
                None,
                None,
                1000,
                Some(0.50),
                Some(5000),
                Some(2000),
                None,
            )
            .await
            .unwrap();

        let exec2 = store.insert_execution("t-cs-f-2", "spark").await.unwrap();
        let _ = store.claim_next_execution(10).await.unwrap();
        store.mark_execution_executing(&exec2).await.unwrap();
        store
            .complete_execution(
                &exec2,
                Some(0),
                None,
                None,
                1000,
                Some(0.10),
                Some(1000),
                Some(500),
                None,
            )
            .await
            .unwrap();

        // Only focused agent
        let summary = store.cost_summary(Some("focused")).await.unwrap();
        assert_eq!(summary.execution_count, 1);
        assert!((summary.total_cost_usd - 0.50).abs() < 1e-9);
        assert_eq!(summary.total_tokens_in, 5000);

        // Only spark agent
        let summary = store.cost_summary(Some("spark")).await.unwrap();
        assert_eq!(summary.execution_count, 1);
        assert!((summary.total_cost_usd - 0.10).abs() < 1e-9);
    }

    #[tokio::test]
    async fn test_cost_by_agent_empty() {
        let store = test_store().await;
        let rows = store.cost_by_agent().await.unwrap();
        assert!(rows.is_empty());
    }

    #[tokio::test]
    async fn test_cost_by_agent_basic() {
        let store = test_store().await;
        store.ensure_thread("t-cba-1", None, None).await.unwrap();
        store.ensure_thread("t-cba-2", None, None).await.unwrap();
        store.ensure_thread("t-cba-3", None, None).await.unwrap();

        // focused: 2 executions
        let exec1 = store.insert_execution("t-cba-1", "focused").await.unwrap();
        let _ = store.claim_next_execution(10).await.unwrap();
        store.mark_execution_executing(&exec1).await.unwrap();
        store
            .complete_execution(
                &exec1,
                Some(0),
                None,
                None,
                1000,
                Some(0.10),
                Some(1000),
                Some(500),
                None,
            )
            .await
            .unwrap();

        let exec2 = store.insert_execution("t-cba-2", "focused").await.unwrap();
        let _ = store.claim_next_execution(10).await.unwrap();
        store.mark_execution_executing(&exec2).await.unwrap();
        store
            .complete_execution(
                &exec2,
                Some(0),
                None,
                None,
                2000,
                Some(0.20),
                Some(2000),
                Some(1000),
                None,
            )
            .await
            .unwrap();

        // spark: 1 execution with NULL cost
        let exec3 = store.insert_execution("t-cba-3", "spark").await.unwrap();
        let _ = store.claim_next_execution(10).await.unwrap();
        store.mark_execution_executing(&exec3).await.unwrap();
        store
            .complete_execution(
                &exec3,
                Some(0),
                None,
                None,
                500,
                None,
                Some(300),
                Some(150),
                None,
            )
            .await
            .unwrap();

        let rows = store.cost_by_agent().await.unwrap();
        assert_eq!(rows.len(), 2);

        let focused = rows.iter().find(|r| r.agent_alias == "focused").unwrap();
        assert_eq!(focused.execution_count, 2);
        assert!((focused.total_cost_usd - 0.30).abs() < 1e-9);
        assert_eq!(focused.total_tokens_in, 3000);
        assert_eq!(focused.total_tokens_out, 1500);

        let spark = rows.iter().find(|r| r.agent_alias == "spark").unwrap();
        assert_eq!(spark.execution_count, 1);
        assert_eq!(spark.total_cost_usd, 0.0); // NULL cost → 0.0
        assert_eq!(spark.total_tokens_in, 300);
        assert_eq!(spark.total_tokens_out, 150);
    }

    #[tokio::test]
    async fn test_tool_error_rates_agent_filter() {
        let store = test_store().await;
        store.ensure_thread("t-er-af-1", None, None).await.unwrap();
        store.ensure_thread("t-er-af-2", None, None).await.unwrap();

        let exec_a = store
            .insert_execution("t-er-af-1", "agent_a")
            .await
            .unwrap();
        let exec_b = store
            .insert_execution("t-er-af-2", "agent_b")
            .await
            .unwrap();

        // agent_a: 2 Bash calls, 1 error result (tool_name populated)
        let events_a = vec![
            crate::backend::ExecutionEvent {
                event_type: "tool_call".to_string(),
                summary: "run cmd 1".to_string(),
                detail: None,
                timestamp_ms: 6000,
                event_index: 0,
                tool_name: Some("Bash".to_string()),
            },
            crate::backend::ExecutionEvent {
                event_type: "tool_call".to_string(),
                summary: "run cmd 2".to_string(),
                detail: None,
                timestamp_ms: 6001,
                event_index: 1,
                tool_name: Some("Bash".to_string()),
            },
            crate::backend::ExecutionEvent {
                event_type: "tool_result".to_string(),
                summary: "error: exit 1".to_string(),
                detail: None,
                timestamp_ms: 6002,
                event_index: 2,
                tool_name: Some("Bash".to_string()),
            },
        ];

        // agent_b: 1 Write call, no errors
        let events_b = vec![crate::backend::ExecutionEvent {
            event_type: "tool_call".to_string(),
            summary: "write file".to_string(),
            detail: None,
            timestamp_ms: 6003,
            event_index: 0,
            tool_name: Some("Write".to_string()),
        }];

        store
            .insert_execution_events(&exec_a, &events_a)
            .await
            .unwrap();
        store
            .insert_execution_events(&exec_b, &events_b)
            .await
            .unwrap();

        // Filter to agent_a only: should see Bash with 2 calls and 1 error
        let stats = store.tool_error_rates(Some("agent_a")).await.unwrap();
        assert_eq!(stats.len(), 1, "only agent_a's tools should appear");
        assert_eq!(stats[0].tool_name, "Bash");
        assert_eq!(stats[0].call_count, 2);
        assert_eq!(stats[0].error_count, 1);
        assert!((stats[0].error_rate - 0.5).abs() < 1e-9);

        // Filter to agent_b only: should see Write with 0 errors
        let stats = store.tool_error_rates(Some("agent_b")).await.unwrap();
        assert_eq!(stats.len(), 1, "only agent_b's tools should appear");
        assert_eq!(stats[0].tool_name, "Write");
        assert_eq!(stats[0].call_count, 1);
        assert_eq!(stats[0].error_count, 0);
        assert_eq!(stats[0].error_rate, 0.0);
    }

    #[tokio::test]
    async fn test_ensure_thread_with_summary() {
        let store = test_store().await;

        // Create with summary
        store
            .ensure_thread("t-sum", None, Some("Fix login bug"))
            .await
            .unwrap();
        let thread = store.get_thread("t-sum").await.unwrap().unwrap();
        assert_eq!(thread.summary.as_deref(), Some("Fix login bug"));

        // Update without summary should preserve existing
        store
            .ensure_thread("t-sum", Some("batch-1"), None)
            .await
            .unwrap();
        let thread = store.get_thread("t-sum").await.unwrap().unwrap();
        assert_eq!(thread.summary.as_deref(), Some("Fix login bug"));
        assert_eq!(thread.batch_id.as_deref(), Some("batch-1"));

        // Update with new summary should overwrite
        store
            .ensure_thread("t-sum", None, Some("Fix signup bug"))
            .await
            .unwrap();
        let thread = store.get_thread("t-sum").await.unwrap().unwrap();
        assert_eq!(thread.summary.as_deref(), Some("Fix signup bug"));
    }

    // ── Merge operation tests ────────────────────────────────────────────

    fn make_merge_op(id: &str, thread_id: &str, target: &str) -> MergeOperation {
        MergeOperation {
            id: id.to_string(),
            thread_id: thread_id.to_string(),
            source_branch: format!("feature/{}", id),
            target_branch: target.to_string(),
            merge_strategy: "merge".to_string(),
            requested_by: "operator".to_string(),
            status: "queued".to_string(),
            push_requested: false,
            queued_at: 1000,
            claimed_at: None,
            started_at: None,
            finished_at: None,
            duration_ms: None,
            result_summary: None,
            error_detail: None,
            conflict_files: None,
        }
    }

    #[tokio::test]
    async fn test_merge_op_insert_and_get() {
        let store = test_store().await;
        let op = make_merge_op("m-1", "t-1", "main");

        store.insert_merge_op(&op).await.unwrap();

        let fetched = store.get_merge_op("m-1").await.unwrap().unwrap();
        assert_eq!(fetched.id, "m-1");
        assert_eq!(fetched.thread_id, "t-1");
        assert_eq!(fetched.source_branch, "feature/m-1");
        assert_eq!(fetched.target_branch, "main");
        assert_eq!(fetched.merge_strategy, "merge");
        assert_eq!(fetched.requested_by, "operator");
        assert_eq!(fetched.status, "queued");
        assert!(!fetched.push_requested);
        assert!(fetched.claimed_at.is_none());
        assert!(fetched.result_summary.is_none());

        // Verify not-found returns None
        let missing = store.get_merge_op("nonexistent").await.unwrap();
        assert!(missing.is_none());
    }

    #[tokio::test]
    async fn test_merge_op_claim_serialization() {
        let store = test_store().await;

        // Insert two ops for the same target_branch
        let op1 = make_merge_op("m-1", "t-1", "main");
        let op2 = make_merge_op("m-2", "t-2", "main");
        store.insert_merge_op(&op1).await.unwrap();
        store.insert_merge_op(&op2).await.unwrap();

        // First claim should succeed — gets the older one
        let claimed = store.claim_next_merge_op().await.unwrap();
        assert!(claimed.is_some());
        let claimed = claimed.unwrap();
        assert_eq!(claimed.id, "m-1");
        assert_eq!(claimed.status, "claimed");
        assert!(claimed.claimed_at.is_some());

        // Second claim should return None — same target_branch is blocked
        let second = store.claim_next_merge_op().await.unwrap();
        assert!(second.is_none());

        // After completing the first, the second should be claimable
        store
            .update_merge_op_status(
                "m-1",
                MergeOperationStatus::Completed,
                Some("merged"),
                None,
                None,
            )
            .await
            .unwrap();

        let third = store.claim_next_merge_op().await.unwrap();
        assert!(third.is_some());
        assert_eq!(third.unwrap().id, "m-2");
    }

    #[tokio::test]
    async fn test_merge_op_claim_different_targets() {
        let store = test_store().await;

        // Insert ops for different target branches
        let op1 = make_merge_op("m-1", "t-1", "main");
        let op2 = make_merge_op("m-2", "t-2", "develop");
        store.insert_merge_op(&op1).await.unwrap();
        store.insert_merge_op(&op2).await.unwrap();

        // Both should be claimable since they target different branches
        let first = store.claim_next_merge_op().await.unwrap();
        assert!(first.is_some());

        let second = store.claim_next_merge_op().await.unwrap();
        assert!(second.is_some());

        // Verify they are different ops
        let ids: Vec<String> = vec![first.unwrap().id, second.unwrap().id];
        assert!(ids.contains(&"m-1".to_string()));
        assert!(ids.contains(&"m-2".to_string()));
    }

    #[tokio::test]
    async fn test_merge_op_cancel_only_queued() {
        let store = test_store().await;

        let op = make_merge_op("m-1", "t-1", "main");
        store.insert_merge_op(&op).await.unwrap();

        // Cancel queued op should succeed
        let cancelled = store.cancel_merge_op("m-1").await.unwrap();
        assert!(cancelled);

        let fetched = store.get_merge_op("m-1").await.unwrap().unwrap();
        assert_eq!(fetched.status, "cancelled");
        assert!(fetched.finished_at.is_some());

        // Cancel again should fail (already cancelled)
        let again = store.cancel_merge_op("m-1").await.unwrap();
        assert!(!again);

        // Cancel a claimed op should fail
        let op2 = make_merge_op("m-2", "t-2", "main");
        store.insert_merge_op(&op2).await.unwrap();
        let _ = store.claim_next_merge_op().await.unwrap();
        let cancel_claimed = store.cancel_merge_op("m-2").await.unwrap();
        assert!(!cancel_claimed);

        // Cancel nonexistent should return false
        let cancel_missing = store.cancel_merge_op("nonexistent").await.unwrap();
        assert!(!cancel_missing);
    }

    #[tokio::test]
    async fn test_merge_op_stale_detection() {
        let store = test_store().await;

        // Insert an op with very old queued_at
        let mut op = make_merge_op("m-1", "t-1", "main");
        op.queued_at = 100; // very old timestamp
        store.insert_merge_op(&op).await.unwrap();

        // Claim it (this sets claimed_at to now)
        let claimed = store.claim_next_merge_op().await.unwrap().unwrap();
        assert_eq!(claimed.status, "claimed");

        // With timeout=0, any op claimed at or before "now" is stale — should catch it.
        let count = store.mark_stale_merge_ops_failed(0).await.unwrap();
        assert_eq!(count, 1);

        let fetched = store.get_merge_op("m-1").await.unwrap().unwrap();
        assert_eq!(fetched.status, "failed");
        assert_eq!(
            fetched.error_detail.as_deref(),
            Some("merge operation timed out")
        );
    }

    #[tokio::test]
    async fn test_merge_op_has_pending() {
        let store = test_store().await;

        // No pending ops
        let has = store
            .has_pending_merge_for_thread("t-1", "main")
            .await
            .unwrap();
        assert!(!has);

        // Insert a queued op
        let op = make_merge_op("m-1", "t-1", "main");
        store.insert_merge_op(&op).await.unwrap();

        let has = store
            .has_pending_merge_for_thread("t-1", "main")
            .await
            .unwrap();
        assert!(has);

        // Different thread, same target — no pending
        let has = store
            .has_pending_merge_for_thread("t-2", "main")
            .await
            .unwrap();
        assert!(!has);

        // Same thread, different target — no pending
        let has = store
            .has_pending_merge_for_thread("t-1", "develop")
            .await
            .unwrap();
        assert!(!has);

        // Cancel the op — no longer pending
        store.cancel_merge_op("m-1").await.unwrap();
        let has = store
            .has_pending_merge_for_thread("t-1", "main")
            .await
            .unwrap();
        assert!(!has);
    }

    #[tokio::test]
    async fn test_stale_worktrees_excludes_pending_merges() {
        let store = test_store().await;

        // Create a completed thread with a worktree
        store.ensure_thread("t-1", None, None).await.unwrap();
        store
            .set_thread_worktree_path(
                "t-1",
                std::path::Path::new("/tmp/wt1"),
                std::path::Path::new("/tmp/repo"),
            )
            .await
            .unwrap();
        store
            .update_thread_status("t-1", ThreadStatus::Completed)
            .await
            .unwrap();

        // Without a merge op, thread should appear in stale worktrees
        let stale = store.threads_with_stale_worktrees().await.unwrap();
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].0, "t-1");

        // Add a pending merge op for the thread
        let op = make_merge_op("m-1", "t-1", "main");
        store.insert_merge_op(&op).await.unwrap();

        // Now thread should be excluded from stale worktrees
        let stale = store.threads_with_stale_worktrees().await.unwrap();
        assert!(stale.is_empty());

        // Complete the merge — thread should reappear
        store
            .update_merge_op_status(
                "m-1",
                MergeOperationStatus::Completed,
                Some("merged"),
                None,
                None,
            )
            .await
            .unwrap();

        let stale = store.threads_with_stale_worktrees().await.unwrap();
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].0, "t-1");
    }

    #[tokio::test]
    async fn test_merge_op_list_ops() {
        let store = test_store().await;

        let op1 = make_merge_op("m-1", "t-1", "main");
        let op2 = make_merge_op("m-2", "t-2", "develop");
        let mut op3 = make_merge_op("m-3", "t-3", "main");
        op3.queued_at = 2000; // newer
        store.insert_merge_op(&op1).await.unwrap();
        store.insert_merge_op(&op2).await.unwrap();
        store.insert_merge_op(&op3).await.unwrap();

        // No filters — all ops returned
        let all = store.list_merge_ops(None, None, None, 100).await.unwrap();
        assert_eq!(all.len(), 3);

        // Filter by target_branch
        let main_ops = store
            .list_merge_ops(Some("main"), None, None, 100)
            .await
            .unwrap();
        assert_eq!(main_ops.len(), 2);
        assert!(main_ops.iter().all(|op| op.target_branch == "main"));

        let dev_ops = store
            .list_merge_ops(Some("develop"), None, None, 100)
            .await
            .unwrap();
        assert_eq!(dev_ops.len(), 1);
        assert_eq!(dev_ops[0].id, "m-2");

        // Filter by status — all are queued
        let queued = store
            .list_merge_ops(None, Some(MergeOperationStatus::Queued), None, 100)
            .await
            .unwrap();
        assert_eq!(queued.len(), 3);

        // Cancel one, then filter by cancelled
        store.cancel_merge_op("m-1").await.unwrap();
        let cancelled = store
            .list_merge_ops(None, Some(MergeOperationStatus::Cancelled), None, 100)
            .await
            .unwrap();
        assert_eq!(cancelled.len(), 1);
        assert_eq!(cancelled[0].id, "m-1");

        // Queued should now be 2
        let queued = store
            .list_merge_ops(None, Some(MergeOperationStatus::Queued), None, 100)
            .await
            .unwrap();
        assert_eq!(queued.len(), 2);
    }

    #[tokio::test]
    async fn test_merge_op_update_executing_sets_started_at() {
        let store = test_store().await;

        let op = make_merge_op("m-1", "t-1", "main");
        store.insert_merge_op(&op).await.unwrap();

        // Claim it first
        let claimed = store.claim_next_merge_op().await.unwrap().unwrap();
        assert_eq!(claimed.status, "claimed");
        assert!(claimed.started_at.is_none());

        // Transition to executing
        store
            .update_merge_op_status("m-1", MergeOperationStatus::Executing, None, None, None)
            .await
            .unwrap();

        let fetched = store.get_merge_op("m-1").await.unwrap().unwrap();
        assert_eq!(fetched.status, "executing");
        assert!(fetched.started_at.is_some());
        assert!(fetched.finished_at.is_none());
        assert!(fetched.duration_ms.is_none());
    }
}
