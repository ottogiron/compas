//! MCP server hub — struct definition, thin `#[tool]` stubs, and `ServerHandler` impl.
//!
//! All tool logic lives in sibling modules (`dispatch`, `query`, `lifecycle`, etc.).
//! Each `#[tool]` method here delegates to a `*_impl` method defined on
//! `OrchestratorMcpServer` in its respective module.

use std::sync::Arc;

use rmcp::handler::server::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::*;
use rmcp::{tool, tool_handler, tool_router, ServerHandler};
use serde::Serialize;

use crate::backend::registry::BackendRegistry;
use crate::config::ConfigHandle;
use crate::store::Store;

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
        description = "Send a message to an agent. Creates or continues a thread."
    )]
    async fn orch_dispatch(
        &self,
        Parameters(params): Parameters<DispatchParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.dispatch_impl(params).await
    }

    #[tool(
        name = "orch_status",
        description = "Query message status by agent and/or thread."
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
        description = "Close a thread with a terminal status (completed or failed)."
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

    // orch_wait removed from MCP surface — stdio transport timeouts make it
    // unreliable. Use `aster_orch wait` CLI subcommand instead.
    // The wait_impl method is preserved for potential future use.

    #[tool(
        name = "orch_poll",
        description = "Non-blocking check of thread state. Returns current status, matching messages, and recent events immediately without waiting. When neither intent nor since_reference is provided, trigger intents are auto-excluded."
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
        description = "Check agent health: backend readiness, CLI availability, environment, runtime state."
    )]
    async fn orch_health(
        &self,
        Parameters(params): Parameters<HealthParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        self.health_impl(params).await
    }

    #[tool(
        name = "orch_tasks",
        description = "List active and recent trigger executions. Shows which agents are running, start time, duration, result status, and batch/ticket linkage."
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
}

// ---------------------------------------------------------------------------
// ServerHandler trait
// ---------------------------------------------------------------------------

#[tool_handler]
impl ServerHandler for OrchestratorMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("aster-orch", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "Aster orchestrator MCP server. Exposes dispatch, status, \
                 metrics, and diagnostic tools for multi-agent coordination."
                    .to_string(),
            )
    }
}
