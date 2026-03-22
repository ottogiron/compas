//! Shared lifecycle service for thread state transitions.
//!
//! This module centralizes lifecycle mutations so MCP handlers and other
//! surfaces (for example, dashboard actions) can share exactly the same
//! transition behavior and error contracts.

use std::path::PathBuf;

use serde::Serialize;
use thiserror::Error;

use crate::store::{MergeOperation, Store, ThreadStatus};

#[derive(Debug, Error)]
pub enum LifecycleError {
    #[error("thread not found: {thread_id}")]
    ThreadNotFound { thread_id: String },
    #[error("lookup failed: {message}")]
    LookupFailed { message: String },
    #[error("{message}")]
    InvalidTransition { message: String },
    #[error("{context}: {message}")]
    StorageFailure {
        context: &'static str,
        message: String,
    },
}

/// Merge intent bundled with a close operation.
#[derive(Debug, Clone)]
pub struct MergeIntent {
    pub target_branch: String,
    pub strategy: String,
    pub repo_root: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
pub struct CloseOutcome {
    pub thread_id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merge_op_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CloseStatus {
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
pub struct AbandonOutcome {
    pub thread_id: String,
    pub status: String,
    pub executions_cancelled: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree_branch: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReopenOutcome {
    pub thread_id: String,
    pub previous_status: String,
    pub new_status: String,
}

#[derive(Clone, Debug)]
pub struct LifecycleService {
    store: Store,
}

impl LifecycleService {
    pub fn new(store: Store) -> Self {
        Self { store }
    }

    pub async fn close(
        &self,
        thread_id: &str,
        from: &str,
        status: CloseStatus,
        note: Option<&str>,
        merge_intent: Option<MergeIntent>,
    ) -> Result<CloseOutcome, LifecycleError> {
        self.ensure_thread(thread_id).await?;

        let (thread_status, intent, fallback_note) = match status {
            CloseStatus::Completed => (
                ThreadStatus::Completed,
                "completion",
                "thread closed as completed",
            ),
            CloseStatus::Failed => (ThreadStatus::Failed, "failure", "thread closed as failed"),
        };

        // If merge is requested, validate strategy and check branch exists
        // BEFORE closing. This way we fail early without changing thread state.
        let merge_op = if let Some(ref mi) = merge_intent {
            if !["merge", "rebase", "squash"].contains(&mi.strategy.as_str()) {
                return Err(LifecycleError::InvalidTransition {
                    message: format!(
                        "invalid merge strategy '{}' — must be one of: merge, rebase, squash",
                        mi.strategy
                    ),
                });
            }

            let source_branch = format!("compas/{}", thread_id);

            // Check no pending merge for same thread+target (cheap DB query first)
            if self
                .store
                .has_pending_merge_for_thread(thread_id, &mi.target_branch)
                .await
                .map_err(|e| LifecycleError::StorageFailure {
                    context: "failed to check pending merges",
                    message: e,
                })?
            {
                return Err(LifecycleError::InvalidTransition {
                    message: format!(
                        "a merge operation for thread {} → {} is already queued or executing",
                        thread_id, mi.target_branch
                    ),
                });
            }

            // Check branch exists (spawns git subprocess)
            let repo_root = mi.repo_root.clone();
            let branch_check = source_branch.clone();
            let branch_exists = tokio::task::spawn_blocking(move || {
                std::process::Command::new("git")
                    .args(["rev-parse", "--verify", &branch_check])
                    .current_dir(&repo_root)
                    .output()
                    .map(|o| o.status.success())
                    .unwrap_or(false)
            })
            .await
            .map_err(|e| LifecycleError::StorageFailure {
                context: "branch check task failed",
                message: e.to_string(),
            })?;

            if !branch_exists {
                return Err(LifecycleError::InvalidTransition {
                    message: format!(
                        "source branch '{}' does not exist in repository",
                        source_branch
                    ),
                });
            }

            let op_id = ulid::Ulid::new().to_string();
            let now = chrono::Utc::now().timestamp();
            Some(MergeOperation {
                id: op_id,
                thread_id: thread_id.to_string(),
                source_branch,
                target_branch: mi.target_branch.clone(),
                merge_strategy: mi.strategy.clone(),
                requested_by: from.to_string(),
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
            })
        } else {
            None
        };

        // Close the thread
        self.store
            .update_thread_status(thread_id, thread_status.clone())
            .await
            .map_err(|e| LifecycleError::StorageFailure {
                context: "failed to close thread",
                message: e.to_string(),
            })?;

        // Insert the merge op immediately after close — cleanup guard is now active
        let merge_op_id = if let Some(op) = merge_op {
            let id = op.id.clone();
            self.store
                .insert_merge_op(&op)
                .await
                .map_err(|e| LifecycleError::StorageFailure {
                    context: "thread closed but merge op insert failed — call orch_merge manually",
                    message: e.to_string(),
                })?;
            Some(id)
        } else {
            None
        };

        let body = note.unwrap_or(fallback_note);

        if let Err(e) = self
            .store
            .insert_message(thread_id, from, "operator", intent, body, None, None)
            .await
        {
            tracing::error!(error = %e, "failed to insert close message");
        }

        Ok(CloseOutcome {
            thread_id: thread_id.to_string(),
            status: thread_status.as_str().to_string(),
            merge_op_id,
        })
    }

    pub async fn abandon(&self, thread_id: &str) -> Result<AbandonOutcome, LifecycleError> {
        self.ensure_thread(thread_id).await?;

        let cancelled = self
            .store
            .cancel_thread_executions(thread_id)
            .await
            .unwrap_or(0);

        // Look up worktree info before status transition
        let worktree_branch = match self.store.get_thread_worktree_info(thread_id).await {
            Ok(Some(_)) => Some(format!("compas/{}", thread_id)),
            Ok(None) => None,
            Err(e) => {
                tracing::warn!(thread_id = %thread_id, error = %e,
                    "get_thread_worktree_info failed during abandon");
                None
            }
        };

        self.store
            .update_thread_status(thread_id, ThreadStatus::Abandoned)
            .await
            .map_err(|e| LifecycleError::StorageFailure {
                context: "failed to abandon thread",
                message: e.to_string(),
            })?;

        Ok(AbandonOutcome {
            thread_id: thread_id.to_string(),
            status: "Abandoned".to_string(),
            executions_cancelled: cancelled,
            worktree_branch,
        })
    }

    pub async fn reopen(&self, thread_id: &str) -> Result<ReopenOutcome, LifecycleError> {
        let thread = self.ensure_thread(thread_id).await?;
        let status: ThreadStatus = thread
            .status
            .parse()
            .map_err(|e: String| LifecycleError::InvalidTransition { message: e })?;

        if !status.is_terminal() {
            return Err(LifecycleError::InvalidTransition {
                message: format!(
                    "thread {} is already {} — only terminal threads can be reopened",
                    thread_id, thread.status
                ),
            });
        }

        self.store
            .update_thread_status(thread_id, ThreadStatus::Active)
            .await
            .map_err(|e| LifecycleError::StorageFailure {
                context: "failed to reopen thread",
                message: e.to_string(),
            })?;

        Ok(ReopenOutcome {
            thread_id: thread_id.to_string(),
            previous_status: thread.status,
            new_status: "Active".to_string(),
        })
    }

    async fn ensure_thread(
        &self,
        thread_id: &str,
    ) -> Result<crate::store::ThreadRow, LifecycleError> {
        match self.store.get_thread(thread_id).await {
            Ok(Some(t)) => Ok(t),
            Ok(None) => Err(LifecycleError::ThreadNotFound {
                thread_id: thread_id.to_string(),
            }),
            Err(e) => Err(LifecycleError::LookupFailed {
                message: e.to_string(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use sqlx::SqlitePool;

    use super::*;

    async fn test_store() -> Store {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        let store = Store::new(pool);
        store.setup().await.unwrap();
        store
    }

    #[tokio::test]
    async fn test_service_close_completed_sets_terminal() {
        let store = test_store().await;
        store.ensure_thread("t-1", None, None).await.unwrap();
        let svc = LifecycleService::new(store.clone());

        let out = svc
            .close(
                "t-1",
                "operator",
                CloseStatus::Completed,
                Some("done"),
                None,
            )
            .await
            .unwrap();
        assert_eq!(out.status, "Completed");
        assert!(out.merge_op_id.is_none());

        let status = store.get_thread_status("t-1").await.unwrap().unwrap();
        assert_eq!(status, "Completed");
    }

    #[tokio::test]
    async fn test_service_abandon_sets_terminal_and_cancels() {
        let store = test_store().await;
        store.ensure_thread("t-1", None, None).await.unwrap();
        store.insert_execution("t-1", "focused").await.unwrap();
        let svc = LifecycleService::new(store.clone());

        let out = svc.abandon("t-1").await.unwrap();
        assert_eq!(out.status, "Abandoned");
        assert!(out.executions_cancelled >= 1);

        let status = store.get_thread_status("t-1").await.unwrap().unwrap();
        assert_eq!(status, "Abandoned");
    }

    #[tokio::test]
    async fn test_service_reopen_non_terminal_errors() {
        let store = test_store().await;
        store.ensure_thread("t-1", None, None).await.unwrap();
        let svc = LifecycleService::new(store);

        let err = svc.reopen("t-1").await.unwrap_err();
        assert!(err
            .to_string()
            .contains("only terminal threads can be reopened"));
    }

    #[tokio::test]
    async fn test_service_close_nonexistent_thread_errors() {
        let store = test_store().await;
        let svc = LifecycleService::new(store);

        let err = svc
            .close("missing", "operator", CloseStatus::Failed, None, None)
            .await
            .unwrap_err();
        assert!(matches!(err, LifecycleError::ThreadNotFound { .. }));
    }

    #[tokio::test]
    async fn test_service_close_with_merge_invalid_strategy_rejects_before_close() {
        let store = test_store().await;
        store.ensure_thread("t-merge-1", None, None).await.unwrap();
        let svc = LifecycleService::new(store.clone());

        let err = svc
            .close(
                "t-merge-1",
                "operator",
                CloseStatus::Completed,
                None,
                Some(MergeIntent {
                    target_branch: "main".to_string(),
                    strategy: "cherry-pick".to_string(),
                    repo_root: std::path::PathBuf::from("/nonexistent"),
                }),
            )
            .await
            .unwrap_err();

        // Strategy validation fires before close
        assert!(err.to_string().contains("invalid merge strategy"));

        // Thread should still be Active (close was not applied)
        let status = store.get_thread_status("t-merge-1").await.unwrap().unwrap();
        assert_eq!(status, "Active");
    }

    #[tokio::test]
    async fn test_service_close_with_merge_missing_branch_rejects_before_close() {
        let store = test_store().await;
        store.ensure_thread("t-merge-2", None, None).await.unwrap();
        let svc = LifecycleService::new(store.clone());

        let err = svc
            .close(
                "t-merge-2",
                "operator",
                CloseStatus::Completed,
                None,
                Some(MergeIntent {
                    target_branch: "main".to_string(),
                    strategy: "merge".to_string(),
                    repo_root: std::path::PathBuf::from("/nonexistent"),
                }),
            )
            .await
            .unwrap_err();

        // Branch check fires before close
        assert!(err.to_string().contains("does not exist"));

        // Thread should still be Active
        let status = store.get_thread_status("t-merge-2").await.unwrap().unwrap();
        assert_eq!(status, "Active");
    }

    #[tokio::test]
    async fn test_service_close_with_merge_duplicate_rejects_before_close() {
        let store = test_store().await;
        store.ensure_thread("t-merge-3", None, None).await.unwrap();

        // Insert a pending merge op for this thread
        let op = crate::store::MergeOperation {
            id: "existing-op".to_string(),
            thread_id: "t-merge-3".to_string(),
            source_branch: "compas/t-merge-3".to_string(),
            target_branch: "main".to_string(),
            merge_strategy: "merge".to_string(),
            requested_by: "operator".to_string(),
            status: "queued".to_string(),
            push_requested: false,
            queued_at: 1000,
            claimed_at: None,
            started_at: None,
            finished_at: None,
            duration_ms: None,
            result_summary: None,
            error_detail: None,
            conflict_files: None,
        };
        store.insert_merge_op(&op).await.unwrap();

        let svc = LifecycleService::new(store.clone());
        let err = svc
            .close(
                "t-merge-3",
                "operator",
                CloseStatus::Completed,
                None,
                Some(MergeIntent {
                    target_branch: "main".to_string(),
                    strategy: "merge".to_string(),
                    repo_root: std::path::PathBuf::from("/nonexistent"),
                }),
            )
            .await
            .unwrap_err();

        assert!(err.to_string().contains("already queued or executing"));

        // Thread should still be Active
        let status = store.get_thread_status("t-merge-3").await.unwrap().unwrap();
        assert_eq!(status, "Active");
    }

    #[tokio::test]
    async fn test_service_close_with_merge_success_path() {
        let store = test_store().await;
        let thread_id = "t-merge-ok";
        store.ensure_thread(thread_id, None, None).await.unwrap();

        // Create a temp git repo with the expected branch
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(repo)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args([
                "-c",
                "user.email=test@test",
                "-c",
                "user.name=test",
                "commit",
                "--allow-empty",
                "-m",
                "init",
            ])
            .current_dir(repo)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["branch", &format!("compas/{}", thread_id)])
            .current_dir(repo)
            .output()
            .unwrap();

        let svc = LifecycleService::new(store.clone());
        let out = svc
            .close(
                thread_id,
                "operator",
                CloseStatus::Completed,
                Some("done with merge"),
                Some(MergeIntent {
                    target_branch: "main".to_string(),
                    strategy: "merge".to_string(),
                    repo_root: repo.to_path_buf(),
                }),
            )
            .await
            .unwrap();

        // Thread is Completed
        assert_eq!(out.status, "Completed");

        // Merge op was created
        assert!(out.merge_op_id.is_some());

        // Thread status in store
        let status = store.get_thread_status(thread_id).await.unwrap().unwrap();
        assert_eq!(status, "Completed");

        // Merge op exists in store
        assert!(store
            .has_pending_merge_for_thread(thread_id, "main")
            .await
            .unwrap());

        // Merge op has correct fields
        let op = store
            .get_merge_op(out.merge_op_id.as_ref().unwrap())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(op.thread_id, thread_id);
        assert_eq!(op.target_branch, "main");
        assert_eq!(op.merge_strategy, "merge");
        assert_eq!(op.status, "queued");
        assert_eq!(op.source_branch, format!("compas/{}", thread_id));
    }
}
