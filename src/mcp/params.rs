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
    /// Intent label (e.g. dispatch, handoff, response)
    pub intent: String,
    /// Optional thread ID (auto-generated if omitted)
    pub thread_id: Option<String>,
    /// Short one-line summary (~80 chars) describing the thread's purpose
    pub summary: Option<String>,
    /// Optional ISO 8601 timestamp (e.g. "2026-03-21T20:00:00Z") for delayed execution.
    /// When set, the execution will not be eligible for worker pickup until this time.
    pub scheduled_for: Option<String>,
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
pub struct CloseParams {
    /// Thread ID to close
    pub thread_id: String,
    /// Agent alias closing the thread
    pub from: String,
    /// Final status for the thread
    pub status: CloseStatus,
    /// Optional close note
    pub note: Option<String>,
    /// Optional: override the auto-merge target branch or strategy.
    /// Completed worktree threads are auto-merged using config defaults;
    /// pass this to override target_branch or strategy.
    pub merge: Option<CloseMergeParams>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema, Clone)]
pub struct CloseMergeParams {
    /// Target branch (default: "main")
    pub target_branch: Option<String>,
    /// Merge strategy: "merge", "rebase", or "squash" (default from config)
    pub strategy: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum CloseStatus {
    Completed,
    Failed,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ReadParams {
    /// Message reference (db:<id> or numeric ID)
    pub reference: String,
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
    /// Optional filter: `"scheduled"` returns only queued executions with a future `eligible_at`.
    pub filter: Option<String>,
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
    /// Optional intent to wait for (e.g. "response", "review-request")
    pub intent: Option<String>,
    /// Optional message cursor (`db:<id>` or numeric ID). Only newer messages are considered.
    pub since_reference: Option<String>,
    /// If true, only consider messages newer than the cursor/call start.
    pub strict_new: Option<bool>,
    /// Timeout in seconds (default 60, clamped to config max)
    pub timeout_secs: Option<u64>,
    /// If true, wait until entire handoff/fan-out chain settles (default false).
    pub await_chain: Option<bool>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct PollParams {
    /// Thread ID to check
    pub thread_id: String,
    /// Optional intent to look for (e.g. "response", "review-request")
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
pub struct WorktreesParams {
    /// Optional thread ID filter
    pub thread_id: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ExecutionEventsParams {
    /// Execution ID to fetch events for
    pub execution_id: String,
    /// Filter events after this timestamp (unix epoch milliseconds)
    pub since_timestamp: Option<i64>,
    /// Return only events with event_index strictly greater than this value.
    /// Preferred cursor for polling (timestamps can collide; event_index is monotonic).
    pub since_event_index: Option<i32>,
    /// Max events to return (default 100)
    pub limit: Option<i64>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ReadLogParams {
    /// Execution ID whose log to read
    pub execution_id: String,
    /// Line offset (0-based, default 0). Ignored when tail=true.
    pub offset: Option<u64>,
    /// Max lines to return (default 200, max 1000)
    pub limit: Option<u64>,
    /// When true, return the last `limit` lines instead of starting from offset
    pub tail: Option<bool>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ToolStatsParams {
    /// Filter to a specific agent alias (omit for all agents)
    pub agent_alias: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct MergeParams {
    /// Thread ID whose branch to merge
    pub thread_id: String,
    /// Target branch (default "main")
    pub target_branch: Option<String>,
    /// Merge strategy: "merge", "rebase", or "squash" (default from config)
    pub strategy: Option<String>,
    /// Alias of the requesting agent
    pub from: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct MergeStatusParams {
    /// Specific merge operation ID (for detail view)
    pub op_id: Option<String>,
    /// Filter by target branch (for overview)
    pub target_branch: Option<String>,
    /// Filter by thread ID (for overview)
    pub thread_id: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct MergeCancelParams {
    /// Merge operation ID to cancel
    pub op_id: String,
}
