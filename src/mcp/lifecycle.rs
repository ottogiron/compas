//! orch_close, orch_abandon, orch_abandon_batch, orch_reopen implementations.

use rmcp::model::CallToolResult;
use serde::Serialize;

use super::params::*;
use super::server::{err_text, json_text, OrchestratorMcpServer};
use crate::lifecycle::LifecycleService;
use crate::store::ThreadStatus;

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
        let threads = match self
            .store
            .list_threads(Some(&params.batch_id), None, 500)
            .await
        {
            Ok(t) => t,
            Err(e) => {
                return Ok(err_text(format!(
                    "failed to list threads for batch '{}': {}",
                    params.batch_id, e
                )));
            }
        };

        if threads.is_empty() {
            return Ok(err_text(format!(
                "No threads found for batch '{}'. Check the batch_id with orch_batch_status.",
                params.batch_id
            )));
        }

        let lifecycle = self.lifecycle_service();
        let mut threads_abandoned: u64 = 0;
        let mut threads_already_terminal: u64 = 0;
        let mut total_executions_cancelled: u64 = 0;
        let mut total_processes_killed: u64 = 0;
        let mut errors: Vec<String> = Vec::new();

        for thread in &threads {
            let status: ThreadStatus = thread.status.parse().unwrap_or(ThreadStatus::Active);
            if status.is_terminal() {
                threads_already_terminal += 1;
                continue;
            }

            match lifecycle.abandon(&thread.thread_id).await {
                Ok(outcome) => {
                    threads_abandoned += 1;
                    total_executions_cancelled += outcome.executions_cancelled;
                    total_processes_killed += outcome.processes_killed;
                }
                Err(e) => {
                    errors.push(format!("{}: {}", thread.thread_id, e));
                }
            }
        }

        #[derive(Serialize)]
        struct BatchAbandonResult {
            batch_id: String,
            threads_abandoned: u64,
            threads_already_terminal: u64,
            total_executions_cancelled: u64,
            total_processes_killed: u64,
            #[serde(skip_serializing_if = "Vec::is_empty")]
            errors: Vec<String>,
        }

        Ok(json_text(&BatchAbandonResult {
            batch_id: params.batch_id,
            threads_abandoned,
            threads_already_terminal,
            total_executions_cancelled,
            total_processes_killed,
            errors,
        }))
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
