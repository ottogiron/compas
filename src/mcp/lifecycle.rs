//! orch_close, orch_abandon, orch_reopen implementations.

use rmcp::model::CallToolResult;

use super::params::*;
use super::server::{err_text, json_text, OrchestratorMcpServer};
use crate::lifecycle::{LifecycleService, MergeIntent};

impl OrchestratorMcpServer {
    fn lifecycle_service(&self) -> LifecycleService {
        LifecycleService::new(self.store.clone())
    }

    // ── orch_close ───────────────────────────────────────────────────────

    pub async fn close_impl(&self, params: CloseParams) -> Result<CallToolResult, rmcp::ErrorData> {
        let config = self.config.load();
        let status = match params.status {
            super::params::CloseStatus::Completed => crate::lifecycle::CloseStatus::Completed,
            super::params::CloseStatus::Failed => crate::lifecycle::CloseStatus::Failed,
        };

        // Resolve repo_root from thread's worktree_repo_root (per-agent workdir),
        // falling back to config.default_workdir for shared-workspace or legacy threads.
        let merge_intent = if let Some(m) = params.merge {
            let thread_repo_root =
                match self.store.get_thread_worktree_info(&params.thread_id).await {
                    Ok(Some((_, root))) => root,
                    Ok(None) => config.default_workdir.clone(),
                    Err(e) => {
                        tracing::warn!(thread_id = %params.thread_id, error = %e,
                            "get_thread_worktree_info failed, falling back to default_workdir");
                        config.default_workdir.clone()
                    }
                };
            let target_branch = m.target_branch.unwrap_or_else(|| "main".to_string());
            let strategy = m
                .strategy
                .unwrap_or_else(|| config.orchestration.default_merge_strategy.clone());
            Some(MergeIntent {
                target_branch,
                strategy,
                repo_root: thread_repo_root,
            })
        } else {
            None
        };

        match self
            .lifecycle_service()
            .close(
                &params.thread_id,
                &params.from,
                status,
                params.note.as_deref(),
                merge_intent,
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
