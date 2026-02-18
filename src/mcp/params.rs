use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// MCP tool parameter structs
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct DispatchParams {
    /// Alias of the sending agent
    pub from: String,
    /// Alias of the receiving agent
    pub to: String,
    /// Message body (Markdown)
    pub body: String,
    /// Optional batch/ticket ID
    pub batch: Option<String>,
    /// Intent: dispatch, handoff, review-request, status-update, decision-needed
    pub intent: String,
    /// Optional thread ID (auto-generated if omitted)
    pub thread_id: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct StatusParams {
    /// Agent alias to filter (omit for all agents)
    pub agent: Option<String>,
    /// Thread ID to filter (omit for all threads)
    pub thread_id: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct TranscriptParams {
    /// Thread ID to retrieve conversation for
    pub thread_id: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct TimelineParams {
    /// Thread ID for state-machine timeline
    pub thread_id: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ApproveParams {
    /// Thread ID to approve
    pub thread_id: String,
    /// Reviewer alias
    pub from: String,
    /// Author alias
    pub to: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct RejectParams {
    /// Thread ID to reject
    pub thread_id: String,
    /// Reviewer alias
    pub from: String,
    /// Author alias
    pub to: String,
    /// Feedback for the rejection
    pub feedback: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct CompleteParams {
    /// Thread ID to complete
    pub thread_id: String,
    /// Agent alias completing the thread
    pub from: String,
    /// Review token issued during approval
    pub token: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ReadParams {
    /// Message reference (db:<id> or numeric ID)
    pub reference: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct LogParams {
    /// Number of recent events to return (default 20)
    pub n: Option<usize>,
    /// Filter by thread ID
    pub thread_id: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct MetricsParams {
    /// Time window: "last_1h" or "last_24h" (default)
    pub window: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct HealthParams {
    /// Agent alias (omit for all agents)
    pub alias: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct TasksParams {
    /// Filter by agent alias (omit for all agents)
    pub alias: Option<String>,
    /// Filter by batch/ticket ID
    pub batch_id: Option<String>,
    /// Maximum number of recent historical tasks to return (default 20)
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct DiagnoseParams {
    /// Thread ID to diagnose
    pub thread_id: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct WaitParams {
    /// Thread ID to poll
    pub thread_id: String,
    /// Optional intent to wait for (e.g. "approved", "completion")
    pub intent: Option<String>,
    /// Optional message cursor (`db:<id>` or numeric ID). Only newer messages are considered.
    pub since_reference: Option<String>,
    /// If true, only consider messages newer than the cursor/call start.
    pub strict_new: Option<bool>,
    /// Timeout in seconds (default 15)
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct PollParams {
    /// Thread ID to check
    pub thread_id: String,
    /// Optional intent to look for (e.g. "review-request", "completion")
    pub intent: Option<String>,
    /// Optional message cursor (`db:<id>` or numeric ID). Only newer messages are considered.
    pub since_reference: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct BatchStatusParams {
    /// Batch/ticket ID to query
    pub batch_id: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct AbandonParams {
    /// Thread ID to abandon
    pub thread_id: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ReopenParams {
    /// Thread ID to reopen
    pub thread_id: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct BindSessionParams {
    pub session_namespace_id: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct SwapModelParams {
    /// Agent alias
    pub alias: String,
    /// Model ID to activate
    pub model_id: String,
    /// If true, inject model into pool even if not already present
    #[serde(default)]
    pub force: Option<bool>,
}
