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

        // Resolve merge intent:
        // 1. Explicit `merge` param → use provided target/strategy with defaults
        // 2. Completed status + worktree thread → auto-merge with config defaults
        // 3. Failed/non-worktree → no merge
        let merge_intent = if let Some(m) = params.merge {
            // Explicit override — use provided target/strategy
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
            let target_branch = m
                .target_branch
                .unwrap_or_else(|| config.orchestration.default_merge_target.clone());
            let strategy = m
                .strategy
                .unwrap_or_else(|| config.orchestration.default_merge_strategy.clone());
            Some(MergeIntent {
                target_branch,
                strategy,
                repo_root: thread_repo_root,
            })
        } else if matches!(status, crate::lifecycle::CloseStatus::Completed) {
            // Auto-merge for Completed worktree threads
            match self.store.get_thread_worktree_info(&params.thread_id).await {
                Ok(Some((_, repo_root))) => Some(MergeIntent {
                    target_branch: config.orchestration.default_merge_target.clone(),
                    strategy: config.orchestration.default_merge_strategy.clone(),
                    repo_root,
                }),
                Ok(None) => None, // Shared workspace — nothing to merge
                Err(e) => {
                    tracing::warn!(thread_id = %params.thread_id, error = %e,
                        "get_thread_worktree_info failed, skipping auto-merge");
                    None
                }
            }
        } else {
            None // Failed — no auto-merge
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
