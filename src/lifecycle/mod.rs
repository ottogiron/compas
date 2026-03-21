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

        let cancelled = self
            .store
            .cancel_thread_executions(thread_id)
            .await
            .unwrap_or(0);

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
}
