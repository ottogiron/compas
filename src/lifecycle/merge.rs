//! Shared merge service for queueing merge operations.
//!
//! Extracted from MCP handler so dashboard actions and other surfaces
//! can queue merges without coupling to MCP types.

use thiserror::Error;

use crate::config::ConfigHandle;
use crate::merge::MergeExecutor;
use crate::store::{MergeOperation, Store};

#[derive(Debug, Error)]
pub enum MergeError {
    #[error("invalid merge strategy '{0}' — must be one of: merge, rebase, squash")]
    InvalidStrategy(String),
    #[error("preflight failed: {0}")]
    PreflightFailed(String),
    #[error("store error: {0}")]
    StoreFailed(String),
}

#[derive(Debug, Clone)]
pub struct QueueMergeOutcome {
    pub op_id: String,
    pub thread_id: String,
    pub source_branch: String,
    pub target_branch: String,
    pub strategy: String,
    pub queue_depth: i64,
}

#[derive(Clone)]
pub struct MergeService {
    store: Store,
    config: ConfigHandle,
}

impl MergeService {
    pub fn new(store: Store, config: ConfigHandle) -> Self {
        Self { store, config }
    }

    pub async fn queue_merge(
        &self,
        thread_id: &str,
        requested_by: &str,
        target_branch: Option<&str>,
        strategy: Option<&str>,
    ) -> Result<QueueMergeOutcome, MergeError> {
        let config = self.config.load();

        let target_branch = target_branch.unwrap_or("main").to_string();
        let strategy = strategy
            .map(|s| s.to_string())
            .unwrap_or_else(|| config.orchestration.default_merge_strategy.clone());

        // Validate strategy
        if !["merge", "rebase", "squash"].contains(&strategy.as_str()) {
            return Err(MergeError::InvalidStrategy(strategy));
        }

        // Resolve repo_root from thread's worktree_repo_root (per-agent workdir),
        // falling back to config.default_workdir for shared-workspace or legacy threads.
        let repo_root = match self.store.get_thread_worktree_info(thread_id).await {
            Ok(Some((_, root))) => root,
            Ok(None) => config.default_workdir.clone(),
            Err(e) => {
                tracing::warn!(thread_id = %thread_id, error = %e,
                    "get_thread_worktree_info failed, falling back to default_workdir");
                config.default_workdir.clone()
            }
        };

        // Run preflight checks
        let preflight =
            MergeExecutor::preflight_check(&self.store, thread_id, &target_branch, &repo_root)
                .await
                .map_err(MergeError::PreflightFailed)?;

        // Generate ULID and build operation
        let op_id = ulid::Ulid::new().to_string();
        let now = chrono::Utc::now().timestamp();

        // Look up the thread summary for use as the merge commit message
        let commit_message = match self.store.get_thread(thread_id).await {
            Ok(Some(thread)) => thread.summary,
            _ => None,
        };

        let op = MergeOperation {
            id: op_id.clone(),
            thread_id: thread_id.to_string(),
            source_branch: preflight.source_branch.clone(),
            target_branch: target_branch.clone(),
            merge_strategy: strategy.clone(),
            requested_by: requested_by.to_string(),
            status: "queued".to_string(),
            push_requested: false,
            queued_at: now,
            claimed_at: None,
            started_at: None,
            finished_at: None,
            duration_ms: None,
            result_summary: None,
            error_detail: None,
            conflict_files: None,
            commit_message,
        };

        self.store
            .insert_merge_op(&op)
            .await
            .map_err(|e| MergeError::StoreFailed(format!("failed to queue merge: {}", e)))?;

        let queue_depth = match self.store.count_queued_merge_ops(&target_branch).await {
            Ok(depth) => depth,
            Err(e) => {
                tracing::warn!(target_branch = %target_branch, error = %e,
                    "count_queued_merge_ops failed, defaulting to 1");
                1
            }
        };

        Ok(QueueMergeOutcome {
            op_id,
            thread_id: thread_id.to_string(),
            source_branch: preflight.source_branch,
            target_branch,
            strategy,
            queue_depth,
        })
    }
}

#[cfg(test)]
mod tests {
    use sqlx::SqlitePool;

    use super::*;
    use crate::config::ConfigHandle;

    async fn test_store() -> Store {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        let store = Store::new(pool);
        store.setup().await.unwrap();
        store
    }

    fn test_config() -> ConfigHandle {
        use crate::config::load_config_from_str;
        let config = load_config_from_str(
            "default_workdir: /tmp\nstate_dir: /tmp/test\nagents:\n  - alias: test\n    backend: stub\n",
        )
        .unwrap();
        ConfigHandle::new(config)
    }

    #[tokio::test]
    async fn test_queue_merge_invalid_strategy() {
        let store = test_store().await;
        store.ensure_thread("t-1", None, None).await.unwrap();
        let config = test_config();
        let svc = MergeService::new(store, config);

        let err = svc
            .queue_merge("t-1", "operator", Some("main"), Some("yolo"))
            .await
            .unwrap_err();

        assert!(matches!(err, MergeError::InvalidStrategy(ref s) if s == "yolo"));
        assert!(err.to_string().contains("yolo"));
    }
}
