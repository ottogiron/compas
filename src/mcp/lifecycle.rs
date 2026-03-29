//! orch_close, orch_abandon, orch_abandon_batch, orch_reopen implementations.

use rmcp::model::CallToolResult;

use super::params::*;
use super::server::{err_text, json_text, OrchestratorMcpServer};
use crate::lifecycle::LifecycleService;

impl OrchestratorMcpServer {
    fn lifecycle_service(&self) -> LifecycleService {
        LifecycleService::new(self.store.clone())
    }

    // ── orch_close ───────────────────────────────────────────────────────

    pub async fn close_impl(&self, params: CloseParams) -> Result<CallToolResult, rmcp::ErrorData> {
        let status = match params.status {
            super::params::CloseStatus::Completed => crate::lifecycle::CloseStatus::Completed,
            super::params::CloseStatus::Failed => crate::lifecycle::CloseStatus::Failed,
        };

        match self
            .lifecycle_service()
            .close(
                &params.thread_id,
                &params.from,
                status,
                params.note.as_deref(),
            )
            .await
        {
            Ok(out) => Ok(json_text(&out)),
            Err(e) => Ok(err_text(e)),
        }
    }

    // ── orch_abandon ─────────────────────────────────────────────────────

    pub async fn abandon_impl(
        &self,
        params: AbandonParams,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        match self.lifecycle_service().abandon(&params.thread_id).await {
            Ok(out) => Ok(json_text(&out)),
            Err(e) => Ok(err_text(e)),
        }
    }

    // ── orch_abandon_batch (SEC-6) ─────────────────────────────────────

    pub async fn abandon_batch_impl(
        &self,
        params: AbandonBatchParams,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let outcome = match self
            .lifecycle_service()
            .abandon_batch(&params.batch_id)
            .await
        {
            Ok(o) => o,
            Err(e) => return Ok(err_text(e)),
        };

        // Preserve existing MCP behavior: empty batch is an error response.
        // Only trigger when there are truly no threads — not when all abandons failed.
        if outcome.threads_abandoned == 0
            && outcome.threads_already_terminal == 0
            && outcome.errors.is_empty()
        {
            return Ok(err_text(format!(
                "No threads found for batch '{}'. Check the batch_id with orch_batch_status.",
                params.batch_id
            )));
        }

        Ok(json_text(&outcome))
    }

    // ── orch_reopen ──────────────────────────────────────────────────────

    pub async fn reopen_impl(
        &self,
        params: ReopenParams,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        match self.lifecycle_service().reopen(&params.thread_id).await {
            Ok(out) => Ok(json_text(&out)),
            Err(e) => Ok(err_text(e)),
        }
    }
}
