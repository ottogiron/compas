use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Lenient deserialization helpers
// ---------------------------------------------------------------------------

/// Deserialize an `Option<u64>` that accepts both numeric (`120`) and
/// string-encoded (`"120"`) representations.  Some MCP transports serialize
/// JSON-RPC number parameters as strings; this helper tolerates either form.
fn deserialize_option_u64_lenient<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de;

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrU64 {
        U64(u64),
        String(String),
    }

    Option::<StringOrU64>::deserialize(deserializer).and_then(|opt| match opt {
        None => Ok(None),
        Some(StringOrU64::U64(v)) => Ok(Some(v)),
        Some(StringOrU64::String(s)) => s
            .parse::<u64>()
            .map(Some)
            .map_err(|_| de::Error::custom(format!("invalid u64 string: {s}"))),
    })
}

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
    /// When true, the agent's response will NOT trigger auto-handoff even if
    /// the agent has handoff.on_response configured.
    pub skip_handoff: Option<bool>,
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
    /// Timeout in seconds (defaults to derived ceiling from execution_timeout_secs; clamped to ceiling)
    #[serde(default, deserialize_with = "deserialize_option_u64_lenient")]
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
    #[serde(default, deserialize_with = "deserialize_option_u64_lenient")]
    pub offset: Option<u64>,
    /// Max lines to return (default 200, max 1000)
    #[serde(default, deserialize_with = "deserialize_option_u64_lenient")]
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

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct WaitMergeParams {
    /// Merge operation ULID to wait on
    pub op_id: String,
    /// Timeout in seconds (default 120)
    #[serde(default, deserialize_with = "deserialize_option_u64_lenient")]
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct CommitParams {
    /// Thread ID whose worktree to commit in
    pub thread_id: String,
    /// Commit message
    pub message: String,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_wait_merge_params_timeout_as_integer() {
        let params: WaitMergeParams =
            serde_json::from_value(json!({"op_id": "abc", "timeout_secs": 120})).unwrap();
        assert_eq!(params.timeout_secs, Some(120));
    }

    #[test]
    fn test_wait_merge_params_timeout_as_string() {
        let params: WaitMergeParams =
            serde_json::from_value(json!({"op_id": "abc", "timeout_secs": "120"})).unwrap();
        assert_eq!(params.timeout_secs, Some(120));
    }

    #[test]
    fn test_wait_merge_params_timeout_null() {
        let params: WaitMergeParams =
            serde_json::from_value(json!({"op_id": "abc", "timeout_secs": null})).unwrap();
        assert_eq!(params.timeout_secs, None);
    }

    #[test]
    fn test_wait_merge_params_timeout_missing() {
        let params: WaitMergeParams = serde_json::from_value(json!({"op_id": "abc"})).unwrap();
        assert_eq!(params.timeout_secs, None);
    }

    #[test]
    fn test_wait_merge_params_timeout_invalid_string() {
        let result = serde_json::from_value::<WaitMergeParams>(
            json!({"op_id": "abc", "timeout_secs": "not_a_number"}),
        );
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("invalid u64 string"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_wait_params_timeout_as_string() {
        let params: WaitParams = serde_json::from_value(json!({
            "thread_id": "t1",
            "timeout_secs": "300"
        }))
        .unwrap();
        assert_eq!(params.timeout_secs, Some(300));
    }

    #[test]
    fn test_wait_params_timeout_as_integer() {
        let params: WaitParams = serde_json::from_value(json!({
            "thread_id": "t1",
            "timeout_secs": 300
        }))
        .unwrap();
        assert_eq!(params.timeout_secs, Some(300));
    }

    #[test]
    fn test_read_log_params_u64_fields_as_strings() {
        let params: ReadLogParams = serde_json::from_value(json!({
            "execution_id": "e1",
            "offset": "10",
            "limit": "500"
        }))
        .unwrap();
        assert_eq!(params.offset, Some(10));
        assert_eq!(params.limit, Some(500));
    }

    #[test]
    fn test_read_log_params_u64_fields_as_integers() {
        let params: ReadLogParams = serde_json::from_value(json!({
            "execution_id": "e1",
            "offset": 10,
            "limit": 500
        }))
        .unwrap();
        assert_eq!(params.offset, Some(10));
        assert_eq!(params.limit, Some(500));
    }
}
