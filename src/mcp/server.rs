//! MCP server hub — struct definition, thin `#[tool]` stubs, and `ServerHandler` impl.
//!
//! All tool logic lives in sibling modules (`dispatch`, `query`, `lifecycle`, etc.).
//! Each `#[tool]` method here delegates to a `*_impl` method defined on
//! `OrchestratorMcpServer` in its respective module.

use std::sync::Arc;

use rmcp::model::*;
use rmcp::{tool, ServerHandler};
use serde::Serialize;

use crate::backend::registry::BackendRegistry;
use crate::config::types::OrchestratorConfig;
use crate::model::message::Intent;
use crate::store::Store;

pub use super::params::*;
pub use super::wait::WaitRegistry;

// ---------------------------------------------------------------------------
// MCP Server struct
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct OrchestratorMcpServer {
    pub(crate) config: Arc<OrchestratorConfig>,
    pub(crate) store: Store,
    /// Backend registry — used by orch_health for backend pings.
    #[allow(dead_code)]
    pub(crate) backend_registry: Arc<BackendRegistry>,
    pub(crate) wait_registry: WaitRegistry,
}

// ---------------------------------------------------------------------------
// Shared helpers (used across multiple modules)
// ---------------------------------------------------------------------------

pub(crate) fn parse_intent(s: &str) -> Result<Intent, String> {
    s.parse()
}

pub(crate) fn json_text<T: Serialize>(val: &T) -> CallToolResult {
    match serde_json::to_string_pretty(val) {
        Ok(json) => CallToolResult::success(vec![Content::text(json)]),
        Err(e) => CallToolResult::error(vec![Content::text(format!("serialization error: {}", e))]),
    }
}

pub(crate) fn err_text(msg: impl std::fmt::Display) -> CallToolResult {
    CallToolResult::error(vec![Content::text(msg.to_string())])
}

impl OrchestratorMcpServer {
    pub fn new(config: OrchestratorConfig, store: Store, backend_registry: BackendRegistry) -> Self {
        Self {
            config: Arc::new(config),
            store,
            backend_registry: Arc::new(backend_registry),
            wait_registry: WaitRegistry::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// #[tool(tool_box)] — thin delegation stubs
// ---------------------------------------------------------------------------

#[tool(tool_box)]
impl OrchestratorMcpServer {
    #[tool(
        name = "orch_session_info",
        description = "Get current MCP session namespace and binding metadata."
    )]
    fn orch_session_info(&self) -> Result<CallToolResult, rmcp::Error> {
        self.session_info_impl()
    }

    #[tool(
        name = "orch_dispatch",
        description = "Send a message to an agent. Creates or continues a thread."
    )]
    async fn orch_dispatch(
        &self,
        #[tool(aggr)] params: DispatchParams,
    ) -> Result<CallToolResult, rmcp::Error> {
        self.dispatch_impl(params).await
    }

    #[tool(
        name = "orch_status",
        description = "Query message status by agent and/or thread."
    )]
    async fn orch_status(
        &self,
        #[tool(aggr)] params: StatusParams,
    ) -> Result<CallToolResult, rmcp::Error> {
        self.status_impl(params).await
    }

    #[tool(
        name = "orch_transcript",
        description = "Get the full conversation transcript for a thread."
    )]
    async fn orch_transcript(
        &self,
        #[tool(aggr)] params: TranscriptParams,
    ) -> Result<CallToolResult, rmcp::Error> {
        self.transcript_impl(params).await
    }

    #[tool(
        name = "orch_approve",
        description = "Approve a review, issuing a review token for the thread."
    )]
    async fn orch_approve(
        &self,
        #[tool(aggr)] params: ApproveParams,
    ) -> Result<CallToolResult, rmcp::Error> {
        self.approve_impl(params).await
    }

    #[tool(
        name = "orch_reject",
        description = "Reject a review with feedback, requesting changes."
    )]
    async fn orch_reject(
        &self,
        #[tool(aggr)] params: RejectParams,
    ) -> Result<CallToolResult, rmcp::Error> {
        self.reject_impl(params).await
    }

    #[tool(
        name = "orch_complete",
        description = "Complete a thread using the review token from approval."
    )]
    async fn orch_complete(
        &self,
        #[tool(aggr)] params: CompleteParams,
    ) -> Result<CallToolResult, rmcp::Error> {
        self.complete_impl(params).await
    }

    #[tool(
        name = "orch_read",
        description = "Read a single message by reference (db:<id> or numeric ID)."
    )]
    async fn orch_read(
        &self,
        #[tool(aggr)] params: ReadParams,
    ) -> Result<CallToolResult, rmcp::Error> {
        self.read_impl(params).await
    }

    #[tool(
        name = "orch_metrics",
        description = "Get aggregate orchestrator metrics (active/blocked/completed threads, queue depth)."
    )]
    async fn orch_metrics(
        &self,
        #[tool(aggr)] params: MetricsParams,
    ) -> Result<CallToolResult, rmcp::Error> {
        self.metrics_impl(params).await
    }

    #[tool(
        name = "orch_wait",
        description = "Poll for a message on a thread, optionally filtering by intent. Blocks up to timeout_secs."
    )]
    async fn orch_wait(
        &self,
        #[tool(aggr)] params: WaitParams,
    ) -> Result<CallToolResult, rmcp::Error> {
        self.wait_impl(params).await
    }

    #[tool(
        name = "orch_poll",
        description = "Non-blocking check of thread state. Returns current status, matching messages, and recent events immediately without waiting."
    )]
    async fn orch_poll(
        &self,
        #[tool(aggr)] params: PollParams,
    ) -> Result<CallToolResult, rmcp::Error> {
        self.poll_impl(params).await
    }

    #[tool(
        name = "orch_batch_status",
        description = "Get batch-level status with per-thread breakdown and intent counts."
    )]
    async fn orch_batch_status(
        &self,
        #[tool(aggr)] params: BatchStatusParams,
    ) -> Result<CallToolResult, rmcp::Error> {
        self.batch_status_impl(params).await
    }

    #[tool(
        name = "orch_abandon",
        description = "Abandon a thread, removing it from processing. Use for stale or stuck threads."
    )]
    async fn orch_abandon(
        &self,
        #[tool(aggr)] params: AbandonParams,
    ) -> Result<CallToolResult, rmcp::Error> {
        self.abandon_impl(params).await
    }

    #[tool(
        name = "orch_reopen",
        description = "Reopen a terminal thread (Completed/Failed/Abandoned) and set it Active."
    )]
    async fn orch_reopen(
        &self,
        #[tool(aggr)] params: ReopenParams,
    ) -> Result<CallToolResult, rmcp::Error> {
        self.reopen_impl(params).await
    }

    #[tool(
        name = "orch_diagnose",
        description = "Diagnose a thread: status + message count + blockers + suggested next actions."
    )]
    async fn orch_diagnose(
        &self,
        #[tool(aggr)] params: DiagnoseParams,
    ) -> Result<CallToolResult, rmcp::Error> {
        self.diagnose_impl(params).await
    }

    #[tool(
        name = "orch_list_agents",
        description = "List all configured agents with their alias, identity, backend, model, and other settings."
    )]
    fn orch_list_agents(&self) -> Result<CallToolResult, rmcp::Error> {
        self.list_agents_impl()
    }

    #[tool(
        name = "orch_health",
        description = "Check agent health: backend readiness, CLI availability, environment, runtime state."
    )]
    async fn orch_health(
        &self,
        #[tool(aggr)] params: HealthParams,
    ) -> Result<CallToolResult, rmcp::Error> {
        self.health_impl(params).await
    }

    #[tool(
        name = "orch_tasks",
        description = "List active and recent trigger executions. Shows which agents are running, start time, duration, result status, and batch/ticket linkage."
    )]
    async fn orch_tasks(
        &self,
        #[tool(aggr)] params: TasksParams,
    ) -> Result<CallToolResult, rmcp::Error> {
        self.tasks_impl(params).await
    }
}

// ---------------------------------------------------------------------------
// ServerHandler trait
// ---------------------------------------------------------------------------

#[tool(tool_box)]
impl ServerHandler for OrchestratorMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::default(),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            server_info: Implementation {
                name: "aster-orch".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
            instructions: Some(
                "Aster orchestrator MCP server. Exposes dispatch, status, review, \
                 metrics, and diagnostic tools for multi-agent coordination."
                    .into(),
            ),
        }
    }
}
