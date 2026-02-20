//! orch_approve, orch_reject, orch_complete, orch_abandon, orch_reopen implementations.

use rmcp::model::CallToolResult;
use serde::Serialize;

use super::params::*;
use super::server::{err_text, json_text, OrchestratorMcpServer};
use crate::config::types::AgentRole;
use crate::store::ThreadStatus;

impl OrchestratorMcpServer {
    // ── orch_approve ─────────────────────────────────────────────────────

    pub async fn approve_impl(&self, params: ApproveParams) -> Result<CallToolResult, rmcp::Error> {
        // Verify thread exists
        let thread = match self.store.get_thread(&params.thread_id).await {
            Ok(Some(t)) => t,
            Ok(None) => return Ok(err_text(format!("thread not found: {}", params.thread_id))),
            Err(e) => return Ok(err_text(format!("lookup failed: {}", e))),
        };

        // Generate review token
        let token = ulid::Ulid::new().to_string();

        // Insert approval message
        if let Err(e) = self
            .store
            .insert_message(
                &params.thread_id,
                &params.from,
                &params.to,
                "approved",
                &format!("Approved. Review token: {}", token),
                None,
            )
            .await
        {
            return Ok(err_text(format!("failed to insert approval: {}", e)));
        }

        #[derive(Serialize)]
        struct ApproveResult {
            thread_id: String,
            token: String,
            status: String,
        }

        Ok(json_text(&ApproveResult {
            thread_id: params.thread_id,
            token,
            status: thread.status,
        }))
    }

    // ── orch_reject ──────────────────────────────────────────────────────

    pub async fn reject_impl(&self, params: RejectParams) -> Result<CallToolResult, rmcp::Error> {
        // Verify thread exists
        if let Ok(None) | Err(_) = self.store.get_thread(&params.thread_id).await {
            return Ok(err_text(format!("thread not found: {}", params.thread_id)));
        }

        // Insert rejection message with feedback
        if let Err(e) = self
            .store
            .insert_message(
                &params.thread_id,
                &params.from,
                &params.to,
                "changes-requested",
                &params.feedback,
                None,
            )
            .await
        {
            return Ok(err_text(format!("failed to insert rejection: {}", e)));
        }

        // Set thread back to Active
        if let Err(e) = self
            .store
            .update_thread_status(&params.thread_id, ThreadStatus::Active)
            .await
        {
            tracing::error!(error = %e, "failed to update thread status on reject");
        }

        // Check if target agent is a worker — if so, create a new execution
        let target_is_worker = self
            .config
            .agents
            .iter()
            .find(|a| a.alias == params.to)
            .map(|a| a.role == AgentRole::Worker)
            .unwrap_or(false);

        let execution_id = if target_is_worker {
            match self
                .store
                .insert_execution(&params.thread_id, &params.to)
                .await
            {
                Ok(id) => Some(id),
                Err(e) => {
                    tracing::error!(error = %e, "failed to queue re-trigger on reject");
                    None
                }
            }
        } else {
            None
        };

        #[derive(Serialize)]
        struct RejectResult {
            thread_id: String,
            re_triggered: bool,
            execution_id: Option<String>,
        }

        Ok(json_text(&RejectResult {
            thread_id: params.thread_id,
            re_triggered: execution_id.is_some(),
            execution_id,
        }))
    }

    // ── orch_complete ────────────────────────────────────────────────────

    pub async fn complete_impl(
        &self,
        params: CompleteParams,
    ) -> Result<CallToolResult, rmcp::Error> {
        // Verify thread exists
        match self.store.get_thread(&params.thread_id).await {
            Ok(Some(_)) => {}
            Ok(None) => return Ok(err_text(format!("thread not found: {}", params.thread_id))),
            Err(e) => return Ok(err_text(format!("lookup failed: {}", e))),
        }

        // Mark thread as completed
        if let Err(e) = self
            .store
            .update_thread_status(&params.thread_id, ThreadStatus::Completed)
            .await
        {
            return Ok(err_text(format!("failed to complete thread: {}", e)));
        }

        // Insert completion message
        if let Err(e) = self
            .store
            .insert_message(
                &params.thread_id,
                &params.from,
                "operator",
                "completion",
                &format!("Thread completed with token: {}", params.token),
                None,
            )
            .await
        {
            tracing::error!(error = %e, "failed to insert completion message");
        }

        #[derive(Serialize)]
        struct CompleteResult {
            thread_id: String,
            status: String,
        }

        Ok(json_text(&CompleteResult {
            thread_id: params.thread_id,
            status: "Completed".to_string(),
        }))
    }

    // ── orch_abandon ─────────────────────────────────────────────────────

    pub async fn abandon_impl(&self, params: AbandonParams) -> Result<CallToolResult, rmcp::Error> {
        // Verify thread exists
        match self.store.get_thread(&params.thread_id).await {
            Ok(Some(_)) => {}
            Ok(None) => return Ok(err_text(format!("thread not found: {}", params.thread_id))),
            Err(e) => return Ok(err_text(format!("lookup failed: {}", e))),
        }

        // Cancel any active executions
        let cancelled = self
            .store
            .cancel_thread_executions(&params.thread_id)
            .await
            .unwrap_or(0);

        // Mark thread as abandoned
        if let Err(e) = self
            .store
            .update_thread_status(&params.thread_id, ThreadStatus::Abandoned)
            .await
        {
            return Ok(err_text(format!("failed to abandon thread: {}", e)));
        }

        #[derive(Serialize)]
        struct AbandonResult {
            thread_id: String,
            status: String,
            executions_cancelled: u64,
        }

        Ok(json_text(&AbandonResult {
            thread_id: params.thread_id,
            status: "Abandoned".to_string(),
            executions_cancelled: cancelled,
        }))
    }

    // ── orch_reopen ──────────────────────────────────────────────────────

    pub async fn reopen_impl(&self, params: ReopenParams) -> Result<CallToolResult, rmcp::Error> {
        let thread = match self.store.get_thread(&params.thread_id).await {
            Ok(Some(t)) => t,
            Ok(None) => return Ok(err_text(format!("thread not found: {}", params.thread_id))),
            Err(e) => return Ok(err_text(format!("lookup failed: {}", e))),
        };

        let status: ThreadStatus = match thread.status.parse() {
            Ok(s) => s,
            Err(e) => return Ok(err_text(e)),
        };

        if !status.is_terminal() {
            return Ok(err_text(format!(
                "thread {} is already {} — only terminal threads can be reopened",
                params.thread_id, thread.status
            )));
        }

        if let Err(e) = self
            .store
            .update_thread_status(&params.thread_id, ThreadStatus::Active)
            .await
        {
            return Ok(err_text(format!("failed to reopen thread: {}", e)));
        }

        #[derive(Serialize)]
        struct ReopenResult {
            thread_id: String,
            previous_status: String,
            new_status: String,
        }

        Ok(json_text(&ReopenResult {
            thread_id: params.thread_id,
            previous_status: thread.status,
            new_status: "Active".to_string(),
        }))
    }
}
