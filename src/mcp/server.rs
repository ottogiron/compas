//! MCP server hub — struct definition, thin `#[tool]` stubs, and `ServerHandler` impl.
//!
//! All tool logic lives in sibling modules (`dispatch`, `query`, `lifecycle`, etc.).
//! Each `#[tool]` method here delegates to a `*_impl` method defined on
//! `OrchestratorMcpServer` in its respective module.

use std::sync::Arc;

use rmcp::handler::server::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::*;
use rmcp::{tool, tool_handler, tool_router, Peer, RoleServer, ServerHandler};
use serde::Serialize;

use crate::backend::registry::BackendRegistry;
use crate::config::ConfigHandle;
use crate::store::Store;

use super::health::PingCache;

pub use super::params::*;

// ---------------------------------------------------------------------------
// MCP Server struct
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct OrchestratorMcpServer {
    pub config: ConfigHandle,
    pub store: Store,
    /// Backend registry — used by orch_health for backend pings.
    pub backend_registry: Arc<BackendRegistry>,
    /// Cache for backend ping results (shared across orch_health calls).
    pub ping_cache: Arc<PingCache>,
    tool_router: ToolRouter<Self>,
}

// ---------------------------------------------------------------------------
// Shared helpers (used across multiple modules)
// ---------------------------------------------------------------------------

pub fn json_text<T: Serialize>(val: &T) -> CallToolResult {
    match serde_json::to_string_pretty(val) {
        Ok(json) => CallToolResult::success(vec![Content::text(json)]),
        Err(e) => CallToolResult::error(vec![Content::text(format!("serialization error: {}", e))]),
    }
}

pub fn err_text(msg: impl std::fmt::Display) -> CallToolResult {
    CallToolResult::error(vec![Content::text(msg.to_string())])
}

impl OrchestratorMcpServer {
    pub fn new(config: ConfigHandle, store: Store, backend_registry: BackendRegistry) -> Self {
        Self {
            config,
            store,
            backend_registry: Arc::new(backend_registry),
            ping_cache: Arc::new(PingCache::new()),
            tool_router: Self::tool_router(),
        }
    }
}

// ---------------------------------------------------------------------------
// #[tool_router] — thin delegation stubs
// ---------------------------------------------------------------------------

#[tool_router]
impl OrchestratorMcpServer {
    #[tool(
        name = "orch_session_info",
        description = "Get current MCP session namespace and binding metadata."
    )]
    fn orch_session_info(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        self.session_info_impl()
    }

    #[tool(
        name = "orch_dispatch",
        description = "Send a message to an agent. Creates or continues a thread. Supports delayed execution via `scheduled_for` (ISO 8601 timestamp, e.g. '2026-03-21T20:00:00Z') — the agent won't be triggered until that time. After dispatch, use orch_wait to block for the response (sends progress notifications to prevent transport timeouts). Use await_chain=true if the agent uses auto-handoff. Do not poll in a loop — use orch_poll only for non-blocking status checks."
    )]
    async fn orch_dispatch(
        &self,
        Parameters(params): Parameters<DispatchParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.dispatch_impl(params).await
    }

    #[tool(
        name = "orch_status",
        description = "Query thread and execution status by agent and/or thread. Response includes scheduled_count (queued executions with a future eligible_at)."
    )]
    async fn orch_status(
        &self,
        Parameters(params): Parameters<StatusParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.status_impl(params).await
    }

    #[tool(
        name = "orch_transcript",
        description = "Get the full conversation transcript for a thread."
    )]
    async fn orch_transcript(
        &self,
        Parameters(params): Parameters<TranscriptParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.transcript_impl(params).await
    }

    #[tool(
        name = "orch_close",
        description = "Close a thread with a terminal status (completed or failed). For completed worktree threads, a merge must be completed first via orch_merge — close will refuse if no completed merge exists. Failed and non-worktree threads close without merge requirements."
    )]
    async fn orch_close(
        &self,
        Parameters(params): Parameters<CloseParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.close_impl(params).await
    }

    #[tool(
        name = "orch_read",
        description = "Read a single message by reference (db:<id> or numeric ID)."
    )]
    async fn orch_read(
        &self,
        Parameters(params): Parameters<ReadParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.read_impl(params).await
    }

    #[tool(
        name = "orch_metrics",
        description = "Get aggregate orchestrator metrics (active/blocked/completed threads, queue depth)."
    )]
    async fn orch_metrics(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        self.metrics_impl().await
    }

    #[tool(
        name = "orch_tool_stats",
        description = "Per-tool call counts, error rates, and cost breakdown across executions. Use to understand agent tool usage patterns and identify problematic tools."
    )]
    async fn orch_tool_stats(
        &self,
        Parameters(params): Parameters<ToolStatsParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.tool_stats_impl(params).await
    }

    #[tool(
        name = "orch_wait",
        description = "Block until a matching message arrives on a thread, or timeout. \
            Returns the full message body on success. Sends progress notifications every 10s \
            to prevent transport timeouts. Use await_chain=true to wait for entire handoff/fan-out \
            chain to settle. For non-blocking checks use orch_poll instead."
    )]
    async fn orch_wait(
        &self,
        Parameters(params): Parameters<WaitParams>,
        peer: Peer<RoleServer>,
        meta: Meta,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let progress_token = meta.get_progress_token();
        self.wait_impl(params, Some(peer), progress_token).await
    }

    #[tool(
        name = "orch_poll",
        description = "Non-blocking check of thread state. Returns current status, matching messages, and recent events immediately without waiting. For blocking waits on agent responses, use orch_wait instead — poll is for quick status checks or diagnosing timeouts. When neither intent nor since_reference is provided, trigger intents are auto-excluded."
    )]
    async fn orch_poll(
        &self,
        Parameters(params): Parameters<PollParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.poll_impl(params).await
    }

    #[tool(
        name = "orch_batch_status",
        description = "Get batch-level status with per-thread breakdown and intent counts."
    )]
    async fn orch_batch_status(
        &self,
        Parameters(params): Parameters<BatchStatusParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.batch_status_impl(params).await
    }

    #[tool(
        name = "orch_abandon",
        description = "Abandon a thread, removing it from processing. Use for stale or stuck threads."
    )]
    async fn orch_abandon(
        &self,
        Parameters(params): Parameters<AbandonParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.abandon_impl(params).await
    }

    #[tool(
        name = "orch_reopen",
        description = "Reopen a terminal thread (Completed/Failed/Abandoned) and set it Active."
    )]
    async fn orch_reopen(
        &self,
        Parameters(params): Parameters<ReopenParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.reopen_impl(params).await
    }

    #[tool(
        name = "orch_diagnose",
        description = "Diagnose a thread: status + message count + blockers + suggested next actions."
    )]
    async fn orch_diagnose(
        &self,
        Parameters(params): Parameters<DiagnoseParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.diagnose_impl(params).await
    }

    #[tool(
        name = "orch_list_agents",
        description = "List all configured agents with their alias, backend, model, and other settings."
    )]
    fn orch_list_agents(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        self.list_agents_impl()
    }

    #[tool(
        name = "orch_health",
        description = "Check agent health: backend readiness, CLI availability, environment, runtime state. Results are cached per agent for `ping_cache_ttl_secs` (default 60s). Each agent entry includes a `cached` boolean indicating whether the result was served from cache. Use `alias` to check a single agent."
    )]
    async fn orch_health(
        &self,
        Parameters(params): Parameters<HealthParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.health_impl(params).await
    }

    #[tool(
        name = "orch_tasks",
        description = "List active and recent trigger executions. Shows which agents are running, start time, duration, result status, and batch/ticket linkage. Set filter='scheduled' to list only queued executions with a future eligible_at."
    )]
    async fn orch_tasks(
        &self,
        Parameters(params): Parameters<TasksParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.tasks_impl(params).await
    }

    #[tool(
        name = "orch_worktrees",
        description = "List active git worktrees for agent isolation."
    )]
    async fn orch_worktrees(
        &self,
        Parameters(params): Parameters<WorktreesParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.worktrees_impl(params).await
    }

    #[tool(
        name = "orch_execution_events",
        description = "Get real-time execution telemetry events (tool calls, messages, turn completions) for a running or completed execution."
    )]
    async fn orch_execution_events(
        &self,
        Parameters(params): Parameters<ExecutionEventsParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.execution_events_impl(params).await
    }

    #[tool(
        name = "orch_read_log",
        description = "Read execution log file with pagination. Returns log lines for a given execution ID with offset/limit support. Falls back to output_preview from DB if log file is unavailable (pruned or not yet written)."
    )]
    async fn orch_read_log(
        &self,
        Parameters(params): Parameters<ReadLogParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.read_log_impl(params).await
    }

    #[tool(
        name = "orch_merge",
        description = "Queue a merge operation for a thread's branch. Accepts Active, Completed, or Failed threads (rejects Abandoned). Runs preflight validation (thread status, branch existence, clean worktree, no duplicate). After queuing, wait for completion using CLI: `compas wait-merge --op-id <id> --timeout 120`."
    )]
    async fn orch_merge(
        &self,
        Parameters(params): Parameters<MergeParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.merge_impl(params).await
    }

    #[tool(
        name = "orch_merge_status",
        description = "Query merge operation status. With op_id: returns full detail including conflict files and suggested actions on failure. Without op_id: returns aggregate counts by status and a preview of recent operations."
    )]
    async fn orch_merge_status(
        &self,
        Parameters(params): Parameters<MergeStatusParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.merge_status_impl(params).await
    }

    #[tool(
        name = "orch_merge_cancel",
        description = "Cancel a queued merge operation. Only operations in 'queued' status can be cancelled."
    )]
    async fn orch_merge_cancel(
        &self,
        Parameters(params): Parameters<MergeCancelParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.merge_cancel_impl(params).await
    }
}

// ---------------------------------------------------------------------------
// ServerHandler trait
// ---------------------------------------------------------------------------

#[tool_handler]
impl ServerHandler for OrchestratorMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("compas", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "Compas MCP server. Exposes dispatch, status, \
                 metrics, and diagnostic tools for multi-agent coordination. \
                 After dispatching work via orch_dispatch, use orch_wait to block \
                 for the response (sends progress notifications to prevent transport \
                 timeouts). If orch_wait returns found=false, re-issue with the same \
                 parameters. Use await_chain=true when the agent uses auto-handoff \
                 and you want the terminal result. Use orch_poll for instant status \
                 checks only, not for waiting."
                    .to_string(),
            )
    }
}
