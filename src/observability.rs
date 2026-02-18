use crate::audit::AuditEvent;
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use std::collections::HashMap;

pub const OBS_SCHEMA_VERSION: &str = "v1";

#[derive(Debug, Clone, Serialize)]
pub struct TimelineEvent {
    pub at: DateTime<Utc>,
    pub event_type: String,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ThreadTimeline {
    pub schema_version: String,
    pub thread_id: String,
    pub current_state: String,
    pub last_event_at: Option<DateTime<Utc>>,
    pub waiting_on: String,
    pub next_action: String,
    pub blocker: Option<String>,
    pub events: Vec<TimelineEvent>,
    /// Raw agent output when trigger succeeded but auto-reply parsing failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_trigger_output: Option<String>,
    /// Actionable suggestion for the operator when diagnosis detects a known issue.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MetricsReport {
    pub schema_version: String,
    pub window: String,
    pub active_threads: usize,
    pub blocked_threads: usize,
    pub completed_threads: usize,
    pub queue_depth_by_alias: HashMap<String, usize>,
    pub retries_total: u64,

    pub completions_total: u64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub batch_summary: Vec<BatchMetricsSummary>,
}

/// Per-batch summary included in metrics output.
#[derive(Debug, Clone, Serialize)]
pub struct BatchMetricsSummary {
    pub batch_id: String,
    pub thread_count: usize,
    pub message_count: usize,
    pub completed_threads: usize,
}

/// A single trigger execution record (persisted in SQLite).
#[derive(Debug, Clone, Serialize)]
pub struct TriggerExecution {
    pub id: i64,
    pub alias: String,
    pub thread_id: Option<String>,
    pub batch_id: Option<String>,
    pub status: String,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub duration_ms: Option<i64>,
    pub error_summary: Option<String>,
    pub output_preview: Option<String>,
}

/// Response for the `orch_tasks` MCP tool.
#[derive(Debug, Clone, Serialize)]
pub struct TasksReport {
    pub schema_version: String,
    pub active: Vec<TriggerExecution>,
    pub recent: Vec<TriggerExecution>,
    pub summary: TasksSummary,
}

#[derive(Debug, Clone, Serialize)]
pub struct TasksSummary {
    pub active_count: usize,
    pub recent_success: usize,
    pub recent_failure: usize,
    pub agents_busy: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WatchEvent {
    pub schema_version: String,
    pub at: DateTime<Utc>,
    pub event_type: String,
    pub thread_id: Option<String>,
    pub severity: String,
    pub now: String,
    pub impact: String,
    pub decision_needed: bool,
    pub next: String,
}

pub fn event_timestamp(event: &AuditEvent) -> DateTime<Utc> {
    match event {
        AuditEvent::Dispatch { timestamp, .. }
        | AuditEvent::Handoff { timestamp, .. }
        | AuditEvent::ReviewRequest { timestamp, .. }
        | AuditEvent::Approval { timestamp, .. }
        | AuditEvent::Rejection { timestamp, .. }
        | AuditEvent::Completion { timestamp, .. }
        | AuditEvent::TriggerAttempt { timestamp, .. }
        | AuditEvent::TriggerSuccess { timestamp, .. }
        | AuditEvent::TriggerFailure { timestamp, .. }
        | AuditEvent::StatusUpdate { timestamp, .. }
        | AuditEvent::ThreadAbandoned { timestamp, .. }
        | AuditEvent::ModelSwap { timestamp, .. }
        | AuditEvent::Error { timestamp, .. } => *timestamp,
    }
}

pub fn event_thread_id(event: &AuditEvent) -> Option<String> {
    match event {
        AuditEvent::Dispatch { thread_id, .. }
        | AuditEvent::Handoff { thread_id, .. }
        | AuditEvent::ReviewRequest { thread_id, .. }
        | AuditEvent::Approval { thread_id, .. }
        | AuditEvent::Rejection { thread_id, .. }
        | AuditEvent::Completion { thread_id, .. }
        | AuditEvent::ThreadAbandoned { thread_id, .. } => Some(thread_id.clone()),
        AuditEvent::TriggerAttempt { thread_id, .. }
        | AuditEvent::TriggerSuccess { thread_id, .. }
        | AuditEvent::TriggerFailure { thread_id, .. }
        | AuditEvent::StatusUpdate { thread_id, .. } => thread_id.clone(),
        AuditEvent::ModelSwap { .. } | AuditEvent::Error { .. } => None,
    }
}

pub fn to_watch_event(event: &AuditEvent) -> WatchEvent {
    match event {
        AuditEvent::StatusUpdate {
            thread_id,
            status,
            now,
            impact,
            decision_needed,
            next,
            timestamp,
        } => WatchEvent {
            schema_version: OBS_SCHEMA_VERSION.to_string(),
            at: *timestamp,
            event_type: "status-update".to_string(),
            thread_id: thread_id.clone(),
            severity: status.clone(),
            now: now.clone(),
            impact: impact.clone(),
            decision_needed: *decision_needed,
            next: next.clone(),
        },
        AuditEvent::TriggerFailure {
            alias,
            error,
            thread_id,
            timestamp,
        } => WatchEvent {
            schema_version: OBS_SCHEMA_VERSION.to_string(),
            at: *timestamp,
            event_type: "trigger-failure".to_string(),
            thread_id: thread_id.clone(),
            severity: "red".to_string(),
            now: format!("Trigger failed for {}: {}", alias, error),
            impact: "Automated execution could not progress".to_string(),
            decision_needed: true,
            next: "Inspect diagnostics and decide: retry, reroute, or close".to_string(),
        },
        AuditEvent::Error {
            context,
            error,
            timestamp,
        } => WatchEvent {
            schema_version: OBS_SCHEMA_VERSION.to_string(),
            at: *timestamp,
            event_type: "error".to_string(),
            thread_id: None,
            severity: "red".to_string(),
            now: format!("Error in {}: {}", context, error),
            impact: "Control-plane error requires investigation".to_string(),
            decision_needed: true,
            next: "Inspect logs and run backend check".to_string(),
        },
        other => WatchEvent {
            schema_version: OBS_SCHEMA_VERSION.to_string(),
            at: event_timestamp(other),
            event_type: audit_event_type(other).to_string(),
            thread_id: event_thread_id(other),
            severity: "green".to_string(),
            now: audit_event_summary(other),
            impact: "Workflow state updated".to_string(),
            decision_needed: false,
            next: "Continue orchestration".to_string(),
        },
    }
}

pub fn audit_event_type(event: &AuditEvent) -> &'static str {
    match event {
        AuditEvent::Dispatch { .. } => "dispatch",
        AuditEvent::Handoff { .. } => "handoff",
        AuditEvent::ReviewRequest { .. } => "review-request",
        AuditEvent::Approval { .. } => "approved",
        AuditEvent::Rejection { .. } => "changes-requested",
        AuditEvent::Completion { .. } => "completion",
        AuditEvent::TriggerAttempt { .. } => "trigger-attempt",
        AuditEvent::TriggerSuccess { .. } => "trigger-success",
        AuditEvent::TriggerFailure { .. } => "trigger-failure",
        AuditEvent::StatusUpdate { .. } => "status-update",
        AuditEvent::ThreadAbandoned { .. } => "thread-abandoned",
        AuditEvent::ModelSwap { .. } => "model-swap",
        AuditEvent::Error { .. } => "error",
    }
}

pub fn audit_event_summary(event: &AuditEvent) -> String {
    match event {
        AuditEvent::Dispatch { from, to, .. } => format!("{} dispatched work to {}", from, to),
        AuditEvent::Handoff { from, to, .. } => format!("{} handed off work to {}", from, to),
        AuditEvent::ReviewRequest { from, to, .. } => {
            format!("{} requested review from {}", from, to)
        }
        AuditEvent::Approval { from, to, .. } => format!("{} approved review for {}", from, to),
        AuditEvent::Rejection { from, to, .. } => {
            format!("{} requested changes from {}", from, to)
        }
        AuditEvent::Completion { from, .. } => format!("{} marked thread complete", from),
        AuditEvent::TriggerAttempt { alias, .. } => {
            format!("Trigger attempt for {}", alias)
        }
        AuditEvent::TriggerSuccess { alias, .. } => format!("Triggered {}", alias),
        AuditEvent::TriggerFailure { alias, error, .. } => {
            format!("Trigger failed for {}: {}", alias, error)
        }
        AuditEvent::StatusUpdate { now, .. } => now.clone(),
        AuditEvent::ThreadAbandoned { thread_id, .. } => {
            format!("Thread {} abandoned", thread_id)
        }
        AuditEvent::ModelSwap {
            alias,
            old_model,
            new_model,
            forced,
            ..
        } => {
            let old = old_model.as_deref().unwrap_or("none");
            let force_tag = if *forced { " (forced)" } else { "" };
            format!(
                "Model swap for {}: {} -> {}{}",
                alias, old, new_model, force_tag
            )
        }
        AuditEvent::Error { context, error, .. } => format!("Error in {}: {}", context, error),
    }
}

pub fn within_window(event_time: DateTime<Utc>, window: &str) -> bool {
    let cutoff = match window {
        "last_1h" => Utc::now() - Duration::hours(1),
        _ => Utc::now() - Duration::hours(24),
    };
    event_time >= cutoff
}
