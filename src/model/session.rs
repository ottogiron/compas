use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

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

/// Result of triggering a backend agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggerResult {
    pub session_id: String,
    pub success: bool,
    pub output: Option<String>,
}
