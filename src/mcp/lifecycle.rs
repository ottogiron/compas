//! orch_approve, orch_reject, orch_complete, orch_abandon, orch_reopen implementations.

use rmcp::model::CallToolResult;

use super::params::*;
use super::server::{err_text, json_text, OrchestratorMcpServer};
use crate::lifecycle::LifecycleService;

impl OrchestratorMcpServer {
    fn lifecycle_service(&self) -> LifecycleService {
        LifecycleService::new(self.store.clone(), self.config.agents.as_slice())
    }

    // ── orch_approve ─────────────────────────────────────────────────────

    pub async fn approve_impl(&self, params: ApproveParams) -> Result<CallToolResult, rmcp::Error> {
        match self
            .lifecycle_service()
            .approve(&params.thread_id, &params.from, &params.to)
            .await
        {
            Ok(out) => Ok(json_text(&out)),
            Err(e) => Ok(err_text(e)),
        }
    }

    // ── orch_reject ──────────────────────────────────────────────────────

    pub async fn reject_impl(&self, params: RejectParams) -> Result<CallToolResult, rmcp::Error> {
        match self
            .lifecycle_service()
            .reject(
                &params.thread_id,
                &params.from,
                &params.to,
                &params.feedback,
            )
            .await
        {
            Ok(out) => Ok(json_text(&out)),
            Err(e) => Ok(err_text(e)),
        }
    }

    // ── orch_complete ────────────────────────────────────────────────────

    pub async fn complete_impl(
        &self,
        params: CompleteParams,
    ) -> Result<CallToolResult, rmcp::Error> {
        match self
            .lifecycle_service()
            .complete(&params.thread_id, &params.from, &params.token)
            .await
        {
            Ok(out) => Ok(json_text(&out)),
            Err(e) => Ok(err_text(e)),
        }
    }

    // ── orch_abandon ─────────────────────────────────────────────────────

    pub async fn abandon_impl(&self, params: AbandonParams) -> Result<CallToolResult, rmcp::Error> {
        match self.lifecycle_service().abandon(&params.thread_id).await {
            Ok(out) => Ok(json_text(&out)),
            Err(e) => Ok(err_text(e)),
        }
    }

    // ── orch_reopen ──────────────────────────────────────────────────────

    pub async fn reopen_impl(&self, params: ReopenParams) -> Result<CallToolResult, rmcp::Error> {
        match self.lifecycle_service().reopen(&params.thread_id).await {
            Ok(out) => Ok(json_text(&out)),
            Err(e) => Ok(err_text(e)),
        }
    }
}
