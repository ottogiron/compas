pub mod logger;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Types of audit events.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum AuditEvent {
    Dispatch {
        from: String,
        to: String,
        thread_id: String,
        batch: Option<String>,
        timestamp: DateTime<Utc>,
    },
    Handoff {
        from: String,
        to: String,
        thread_id: String,
        timestamp: DateTime<Utc>,
    },
    ReviewRequest {
        from: String,
        to: String,
        thread_id: String,
        timestamp: DateTime<Utc>,
    },
    Approval {
        from: String,
        to: String,
        thread_id: String,
        token: String,
        timestamp: DateTime<Utc>,
    },
    Rejection {
        from: String,
        to: String,
        thread_id: String,
        feedback: String,
        timestamp: DateTime<Utc>,
    },
    Completion {
        from: String,
        thread_id: String,
        token: String,
        timestamp: DateTime<Utc>,
    },
    TriggerAttempt {
        alias: String,
        thread_id: Option<String>,
        timestamp: DateTime<Utc>,
    },
    TriggerSuccess {
        alias: String,
        thread_id: Option<String>,
        session_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        output: Option<String>,
        timestamp: DateTime<Utc>,
    },
    TriggerFailure {
        alias: String,
        thread_id: Option<String>,
        error: String,
        timestamp: DateTime<Utc>,
    },
    StatusUpdate {
        thread_id: Option<String>,
        status: String,
        now: String,
        impact: String,
        decision_needed: bool,
        next: String,
        timestamp: DateTime<Utc>,
    },

    ThreadAbandoned {
        thread_id: String,
        timestamp: DateTime<Utc>,
    },
    ModelSwap {
        alias: String,
        old_model: Option<String>,
        new_model: String,
        forced: bool,
        timestamp: DateTime<Utc>,
    },
    Error {
        context: String,
        error: String,
        timestamp: DateTime<Utc>,
    },
}
