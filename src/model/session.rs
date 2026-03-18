use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// A backend agent session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub agent_alias: String,
    pub backend: String,
    pub started_at: DateTime<Utc>,
    /// Backend-specific session ID for resuming a prior CLI session.
    /// When `Some`, the backend should pass the appropriate resume flag
    /// (e.g. `-r` for Claude, `resume <id>` for Codex, `-s` for OpenCode).
    #[serde(default)]
    pub resume_session_id: Option<String>,
    /// Optional channel for streaming stdout lines to a telemetry consumer.
    /// Set by the executor before calling `trigger()`. Backends pass this
    /// through to `wait_with_timeout()` for real-time event extraction.
    #[serde(skip)]
    pub stdout_tx: Option<Arc<std::sync::mpsc::SyncSender<String>>>,
    /// Optional channel for reporting the backend process PID immediately
    /// after `spawn_cli()`. Used by the executor to persist the PID in the DB
    /// while the process is still running, enabling orphan detection on crash.
    #[serde(skip)]
    pub pid_tx: Option<std::sync::mpsc::SyncSender<u32>>,
}

/// Status of an agent session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SessionStatus {
    Running,
    Idle,
    Stopped,
    Crashed,
}

/// Legacy result of triggering a backend agent.
///
/// Superseded by `crate::backend::BackendOutput` which provides richer fields
/// (parsed intent, raw output, structured session ID). Kept temporarily for
/// reference; will be removed in a follow-up cleanup.
#[deprecated(since = "0.2.0", note = "Use crate::backend::BackendOutput instead")]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggerResult {
    pub session_id: String,
    pub success: bool,
    pub output: Option<String>,
}
