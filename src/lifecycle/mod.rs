//! Shared lifecycle service for thread state transitions.
//!
//! This module centralizes lifecycle mutations so MCP handlers and other
//! surfaces (for example, dashboard actions) can share exactly the same
//! transition behavior and error contracts.

use serde::Serialize;
use thiserror::Error;

use crate::store::{Store, ThreadStatus};

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

#[derive(Debug, Clone, Serialize)]
pub struct CloseOutcome {
    pub thread_id: String,
    pub status: String,
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
    pub processes_killed: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree_branch: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AbandonBatchOutcome {
    pub batch_id: String,
    pub threads_abandoned: u64,
    pub threads_already_terminal: u64,
    pub total_executions_cancelled: u64,
    pub total_processes_killed: u64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
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

        // Merge-before-close gate: completed worktree threads require a
        // completed merge operation before they can be closed.
        if matches!(status, CloseStatus::Completed) {
            let has_worktree = self
                .store
                .get_thread_worktree_info(thread_id)
                .await
                .map_err(|e| LifecycleError::StorageFailure {
                    context: "failed to check worktree info",
                    message: e,
                })?
                .is_some();

            if has_worktree {
                let has_completed_merge = self
                    .store
                    .has_completed_merge_for_thread(thread_id)
                    .await
                    .map_err(|e| LifecycleError::StorageFailure {
                        context: "failed to check merge status",
                        message: e,
                    })?;

                if !has_completed_merge {
                    return Err(LifecycleError::InvalidTransition {
                        message: format!(
                            "thread '{}' is a worktree thread with no completed merge \
                             — call orch_merge first, then close after the merge completes",
                            thread_id
                        ),
                    });
                }
            }
        }

        // Close the thread
        self.store
            .update_thread_status(thread_id, thread_status.clone())
            .await
            .map_err(|e| LifecycleError::StorageFailure {
                context: "failed to close thread",
                message: e.to_string(),
            })?;

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
        })
    }

    pub async fn abandon(&self, thread_id: &str) -> Result<AbandonOutcome, LifecycleError> {
        self.ensure_thread(thread_id).await?;

        // SEC-6: Query executing PIDs BEFORE cancelling — cancel_thread_executions
        // flips DB status first, then we kill processes. When the executor's
        // wait_with_timeout sees the child exit, it tries to finalize — sees
        // rows_affected=0 (already cancelled) — logs warning and moves on.
        let executing_pids = match self.store.get_executing_pids_for_thread(thread_id).await {
            Ok(pids) => pids,
            Err(e) => {
                tracing::warn!(thread_id = %thread_id, error = %e,
                    "failed to query executing PIDs for abandon");
                vec![]
            }
        };

        let cancelled = self
            .store
            .cancel_thread_executions(thread_id)
            .await
            .unwrap_or(0);

        // SEC-6: Kill running subprocesses for this thread.
        let mut processes_killed: u64 = 0;
        for (exec_id, pid) in executing_pids {
            tracing::info!(thread_id = %thread_id, exec_id = %exec_id, pid = pid,
                "killing subprocess for abandoned thread");
            let kill_result =
                tokio::task::spawn_blocking(move || crate::backend::process::kill_process(pid))
                    .await;
            match kill_result {
                Ok(Ok(())) => {
                    tracing::info!(exec_id = %exec_id, pid = pid, "subprocess killed");
                    processes_killed += 1;
                }
                Ok(Err(e)) => {
                    tracing::warn!(exec_id = %exec_id, pid = pid, error = %e,
                        "failed to kill subprocess (may have already exited)");
                }
                Err(e) => {
                    tracing::warn!(exec_id = %exec_id, pid = pid, error = %e,
                        "spawn_blocking panicked during subprocess kill");
                }
            }
        }

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
            processes_killed,
            worktree_branch,
        })
    }

    pub async fn abandon_batch(
        &self,
        batch_id: &str,
    ) -> Result<AbandonBatchOutcome, LifecycleError> {
        let threads = self
            .store
            .list_threads(Some(batch_id), None, 500)
            .await
            .map_err(|e| LifecycleError::LookupFailed {
                message: format!("failed to list threads for batch '{}': {}", batch_id, e),
            })?;

        let mut threads_abandoned: u64 = 0;
        let mut threads_already_terminal: u64 = 0;
        let mut total_executions_cancelled: u64 = 0;
        let mut total_processes_killed: u64 = 0;
        let mut errors: Vec<String> = Vec::new();

        for thread in &threads {
            let status = match thread.status.parse::<ThreadStatus>() {
                Ok(s) => s,
                Err(e) => {
                    errors.push(format!(
                        "{}: unrecognized status '{}': {}",
                        thread.thread_id, thread.status, e
                    ));
                    continue;
                }
            };
            if status.is_terminal() {
                threads_already_terminal += 1;
                continue;
            }

            match self.abandon(&thread.thread_id).await {
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

        Ok(AbandonBatchOutcome {
            batch_id: batch_id.to_string(),
            threads_abandoned,
            threads_already_terminal,
            total_executions_cancelled,
            total_processes_killed,
            errors,
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

        // Non-worktree thread closes without merge
        let out = svc
            .close("t-1", "operator", CloseStatus::Completed, Some("done"))
            .await
            .unwrap();
        assert_eq!(out.status, "Completed");

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
            .close("missing", "operator", CloseStatus::Failed, None)
            .await
            .unwrap_err();
        assert!(matches!(err, LifecycleError::ThreadNotFound { .. }));
    }

    #[tokio::test]
    async fn test_close_completed_worktree_requires_merge() {
        let store = test_store().await;
        store.ensure_thread("t-wt-1", None, None).await.unwrap();
        store
            .set_thread_worktree_path(
                "t-wt-1",
                std::path::Path::new("/tmp/wt"),
                std::path::Path::new("/tmp/repo"),
            )
            .await
            .unwrap();
        let svc = LifecycleService::new(store.clone());

        let err = svc
            .close("t-wt-1", "operator", CloseStatus::Completed, None)
            .await
            .unwrap_err();

        assert!(err.to_string().contains("no completed merge"));
        assert!(err.to_string().contains("orch_merge"));

        // Thread should still be Active
        let status = store.get_thread_status("t-wt-1").await.unwrap().unwrap();
        assert_eq!(status, "Active");
    }

    #[tokio::test]
    async fn test_close_completed_worktree_with_completed_merge_succeeds() {
        let store = test_store().await;
        store.ensure_thread("t-wt-2", None, None).await.unwrap();
        store
            .set_thread_worktree_path(
                "t-wt-2",
                std::path::Path::new("/tmp/wt"),
                std::path::Path::new("/tmp/repo"),
            )
            .await
            .unwrap();

        // Insert a completed merge op
        let op = crate::store::MergeOperation {
            id: "m-ok".to_string(),
            thread_id: "t-wt-2".to_string(),
            source_branch: "compas/t-wt-2".to_string(),
            target_branch: "main".to_string(),
            merge_strategy: "merge".to_string(),
            requested_by: "operator".to_string(),
            status: "completed".to_string(),
            push_requested: false,
            queued_at: 1000,
            claimed_at: None,
            started_at: None,
            finished_at: None,
            duration_ms: None,
            result_summary: None,
            error_detail: None,
            conflict_files: None,
            commit_message: None,
        };
        store.insert_merge_op(&op).await.unwrap();

        let svc = LifecycleService::new(store.clone());
        let out = svc
            .close(
                "t-wt-2",
                "operator",
                CloseStatus::Completed,
                Some("merged and done"),
            )
            .await
            .unwrap();

        assert_eq!(out.status, "Completed");
        let status = store.get_thread_status("t-wt-2").await.unwrap().unwrap();
        assert_eq!(status, "Completed");
    }

    #[tokio::test]
    async fn test_close_completed_worktree_with_failed_merge_refuses() {
        let store = test_store().await;
        store.ensure_thread("t-wt-3", None, None).await.unwrap();
        store
            .set_thread_worktree_path(
                "t-wt-3",
                std::path::Path::new("/tmp/wt"),
                std::path::Path::new("/tmp/repo"),
            )
            .await
            .unwrap();

        // Insert a failed merge op (not completed)
        let op = crate::store::MergeOperation {
            id: "m-fail".to_string(),
            thread_id: "t-wt-3".to_string(),
            source_branch: "compas/t-wt-3".to_string(),
            target_branch: "main".to_string(),
            merge_strategy: "merge".to_string(),
            requested_by: "operator".to_string(),
            status: "failed".to_string(),
            push_requested: false,
            queued_at: 1000,
            claimed_at: None,
            started_at: None,
            finished_at: None,
            duration_ms: None,
            result_summary: None,
            error_detail: None,
            conflict_files: None,
            commit_message: None,
        };
        store.insert_merge_op(&op).await.unwrap();

        let svc = LifecycleService::new(store.clone());
        let err = svc
            .close("t-wt-3", "operator", CloseStatus::Completed, None)
            .await
            .unwrap_err();

        assert!(err.to_string().contains("no completed merge"));
    }

    #[tokio::test]
    async fn test_close_completed_worktree_with_pending_merge_refuses() {
        let store = test_store().await;
        store.ensure_thread("t-wt-4", None, None).await.unwrap();
        store
            .set_thread_worktree_path(
                "t-wt-4",
                std::path::Path::new("/tmp/wt"),
                std::path::Path::new("/tmp/repo"),
            )
            .await
            .unwrap();

        // Insert a queued merge op (still pending, not completed)
        let op = crate::store::MergeOperation {
            id: "m-pending".to_string(),
            thread_id: "t-wt-4".to_string(),
            source_branch: "compas/t-wt-4".to_string(),
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
            commit_message: None,
        };
        store.insert_merge_op(&op).await.unwrap();

        let svc = LifecycleService::new(store.clone());
        let err = svc
            .close("t-wt-4", "operator", CloseStatus::Completed, None)
            .await
            .unwrap_err();

        assert!(err.to_string().contains("no completed merge"));
    }

    #[tokio::test]
    async fn test_close_failed_worktree_no_merge_required() {
        let store = test_store().await;
        store.ensure_thread("t-wt-5", None, None).await.unwrap();
        store
            .set_thread_worktree_path(
                "t-wt-5",
                std::path::Path::new("/tmp/wt"),
                std::path::Path::new("/tmp/repo"),
            )
            .await
            .unwrap();
        let svc = LifecycleService::new(store.clone());

        // Failed close on worktree thread — no merge required
        let out = svc
            .close(
                "t-wt-5",
                "operator",
                CloseStatus::Failed,
                Some("failed work"),
            )
            .await
            .unwrap();
        assert_eq!(out.status, "Failed");
    }

    #[tokio::test]
    async fn test_close_completed_non_worktree_no_merge_required() {
        let store = test_store().await;
        store.ensure_thread("t-shared", None, None).await.unwrap();
        let svc = LifecycleService::new(store.clone());

        // Non-worktree thread — no merge gate
        let out = svc
            .close("t-shared", "operator", CloseStatus::Completed, Some("done"))
            .await
            .unwrap();
        assert_eq!(out.status, "Completed");
    }

    // ── abandon_batch tests ─────────────────────────────────────────────

    #[tokio::test]
    async fn test_abandon_batch_all_active() {
        let store = test_store().await;
        store.ensure_thread("t-1", Some("b-1"), None).await.unwrap();
        store.ensure_thread("t-2", Some("b-1"), None).await.unwrap();
        store.ensure_thread("t-3", Some("b-1"), None).await.unwrap();
        store.insert_execution("t-1", "focused").await.unwrap();
        store.insert_execution("t-2", "focused").await.unwrap();
        store.insert_execution("t-3", "focused").await.unwrap();
        let svc = LifecycleService::new(store.clone());

        let out = svc.abandon_batch("b-1").await.unwrap();
        assert_eq!(out.batch_id, "b-1");
        assert_eq!(out.threads_abandoned, 3);
        assert_eq!(out.threads_already_terminal, 0);
        assert!(out.errors.is_empty());

        // All threads should now be Abandoned
        for tid in &["t-1", "t-2", "t-3"] {
            let status = store.get_thread_status(tid).await.unwrap().unwrap();
            assert_eq!(status, "Abandoned");
        }
    }

    #[tokio::test]
    async fn test_abandon_batch_mixed_states() {
        let store = test_store().await;
        store.ensure_thread("t-1", Some("b-2"), None).await.unwrap();
        store.ensure_thread("t-2", Some("b-2"), None).await.unwrap();
        store.ensure_thread("t-3", Some("b-2"), None).await.unwrap();
        // Set one thread to Completed (terminal)
        store
            .update_thread_status("t-2", ThreadStatus::Completed)
            .await
            .unwrap();
        let svc = LifecycleService::new(store.clone());

        let out = svc.abandon_batch("b-2").await.unwrap();
        assert_eq!(out.threads_abandoned, 2);
        assert_eq!(out.threads_already_terminal, 1);
        assert!(out.errors.is_empty());
    }

    #[tokio::test]
    async fn test_abandon_batch_empty() {
        let store = test_store().await;
        let svc = LifecycleService::new(store);

        let out = svc.abandon_batch("nonexistent-batch").await.unwrap();
        assert_eq!(out.threads_abandoned, 0);
        assert_eq!(out.threads_already_terminal, 0);
        assert!(out.errors.is_empty());
    }

    #[tokio::test]
    async fn test_abandon_batch_unrecognized_status_collects_error() {
        let store = test_store().await;
        store
            .ensure_thread("t-1", Some("b-err"), None)
            .await
            .unwrap();
        // Force an unrecognized status via raw SQL
        sqlx::query("UPDATE threads SET status = 'Bogus' WHERE thread_id = 't-1'")
            .execute(store.pool())
            .await
            .unwrap();
        let svc = LifecycleService::new(store);

        let out = svc.abandon_batch("b-err").await.unwrap();
        assert_eq!(out.threads_abandoned, 0);
        assert_eq!(out.threads_already_terminal, 0);
        assert_eq!(out.errors.len(), 1);
        assert!(out.errors[0].contains("unrecognized status"));
    }

    #[tokio::test]
    async fn test_abandon_batch_all_terminal() {
        let store = test_store().await;
        store.ensure_thread("t-1", Some("b-3"), None).await.unwrap();
        store.ensure_thread("t-2", Some("b-3"), None).await.unwrap();
        store
            .update_thread_status("t-1", ThreadStatus::Completed)
            .await
            .unwrap();
        store
            .update_thread_status("t-2", ThreadStatus::Completed)
            .await
            .unwrap();
        let svc = LifecycleService::new(store);

        let out = svc.abandon_batch("b-3").await.unwrap();
        assert_eq!(out.threads_abandoned, 0);
        assert_eq!(out.threads_already_terminal, 2);
        assert!(out.errors.is_empty());
    }
}
