//! Shared lifecycle service for thread state transitions.
//!
//! This module centralizes lifecycle mutations so MCP handlers and other
//! surfaces (for example, dashboard actions) can share exactly the same
//! transition behavior and error contracts.

use std::collections::HashMap;

use serde::Serialize;
use thiserror::Error;

use crate::config::types::{AgentConfig, AgentRole};
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
pub struct ApproveOutcome {
    pub thread_id: String,
    pub token: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RejectOutcome {
    pub thread_id: String,
    pub re_triggered: bool,
    pub execution_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CompleteOutcome {
    pub thread_id: String,
    pub status: String,
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
    agent_roles: HashMap<String, AgentRole>,
}

impl LifecycleService {
    pub fn new(store: Store, agents: &[AgentConfig]) -> Self {
        let agent_roles = agents
            .iter()
            .map(|a| (a.alias.clone(), a.role.clone()))
            .collect();
        Self { store, agent_roles }
    }

    pub async fn approve(
        &self,
        thread_id: &str,
        from: &str,
        to: &str,
    ) -> Result<ApproveOutcome, LifecycleError> {
        let thread = self.ensure_thread(thread_id).await?;

        let token = ulid::Ulid::new().to_string();
        self.store
            .insert_message(
                thread_id,
                from,
                to,
                "approved",
                &format!("Approved. Review token: {}", token),
                None,
            )
            .await
            .map_err(|e| LifecycleError::StorageFailure {
                context: "failed to insert approval",
                message: e.to_string(),
            })?;

        Ok(ApproveOutcome {
            thread_id: thread_id.to_string(),
            token,
            status: thread.status,
        })
    }

    pub async fn reject(
        &self,
        thread_id: &str,
        from: &str,
        to: &str,
        feedback: &str,
    ) -> Result<RejectOutcome, LifecycleError> {
        self.ensure_thread(thread_id).await?;

        self.store
            .insert_message(thread_id, from, to, "changes-requested", feedback, None)
            .await
            .map_err(|e| LifecycleError::StorageFailure {
                context: "failed to insert rejection",
                message: e.to_string(),
            })?;

        // Preserve current behavior: log status-update failures but do not fail
        // the reject operation if the rejection message was persisted.
        if let Err(e) = self
            .store
            .update_thread_status(thread_id, ThreadStatus::Active)
            .await
        {
            tracing::error!(error = %e, "failed to update thread status on reject");
        }

        let target_is_worker = self
            .agent_roles
            .get(to)
            .map(|r| r == &AgentRole::Worker)
            .unwrap_or(false);

        let execution_id = if target_is_worker {
            match self.store.insert_execution(thread_id, to).await {
                Ok(id) => Some(id),
                Err(e) => {
                    tracing::error!(error = %e, "failed to queue re-trigger on reject");
                    None
                }
            }
        } else {
            None
        };

        Ok(RejectOutcome {
            thread_id: thread_id.to_string(),
            re_triggered: execution_id.is_some(),
            execution_id,
        })
    }

    pub async fn complete(
        &self,
        thread_id: &str,
        from: &str,
        token: &str,
    ) -> Result<CompleteOutcome, LifecycleError> {
        self.ensure_thread(thread_id).await?;

        self.store
            .update_thread_status(thread_id, ThreadStatus::Completed)
            .await
            .map_err(|e| LifecycleError::StorageFailure {
                context: "failed to complete thread",
                message: e.to_string(),
            })?;

        // Preserve current behavior: completion status is primary; message write
        // failure is logged but does not fail completion.
        if let Err(e) = self
            .store
            .insert_message(
                thread_id,
                from,
                "operator",
                "completion",
                &format!("Thread completed with token: {}", token),
                None,
            )
            .await
        {
            tracing::error!(error = %e, "failed to insert completion message");
        }

        Ok(CompleteOutcome {
            thread_id: thread_id.to_string(),
            status: "Completed".to_string(),
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

    fn test_agents() -> Vec<AgentConfig> {
        vec![
            AgentConfig {
                alias: "focused".to_string(),
                identity: "focused".to_string(),
                backend: "stub".to_string(),
                role: AgentRole::Worker,
                model: None,
                models: None,
                preferred_models: None,
                prompt: None,
                prompt_file: None,
                timeout_secs: None,
                backend_args: None,
                env: None,
            },
            AgentConfig {
                alias: "operator".to_string(),
                identity: "operator".to_string(),
                backend: "stub".to_string(),
                role: AgentRole::Operator,
                model: None,
                models: None,
                preferred_models: None,
                prompt: None,
                prompt_file: None,
                timeout_secs: None,
                backend_args: None,
                env: None,
            },
        ]
    }

    async fn test_store() -> Store {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        let store = Store::new(pool);
        store.setup().await.unwrap();
        store
    }

    #[tokio::test]
    async fn test_service_reject_worker_retriggers_execution() {
        let store = test_store().await;
        store.ensure_thread("t-1", None).await.unwrap();
        let svc = LifecycleService::new(store.clone(), &test_agents());

        let out = svc
            .reject("t-1", "operator", "focused", "please revise")
            .await
            .unwrap();
        assert!(out.re_triggered);
        assert!(out.execution_id.is_some());

        let status = store.get_thread_status("t-1").await.unwrap().unwrap();
        assert_eq!(status, "Active");
    }

    #[tokio::test]
    async fn test_service_abandon_sets_terminal_and_cancels() {
        let store = test_store().await;
        store.ensure_thread("t-1", None).await.unwrap();
        store.insert_execution("t-1", "focused").await.unwrap();
        let svc = LifecycleService::new(store.clone(), &test_agents());

        let out = svc.abandon("t-1").await.unwrap();
        assert_eq!(out.status, "Abandoned");
        assert!(out.executions_cancelled >= 1);

        let status = store.get_thread_status("t-1").await.unwrap().unwrap();
        assert_eq!(status, "Abandoned");
    }

    #[tokio::test]
    async fn test_service_reopen_non_terminal_errors() {
        let store = test_store().await;
        store.ensure_thread("t-1", None).await.unwrap();
        let svc = LifecycleService::new(store, &test_agents());

        let err = svc.reopen("t-1").await.unwrap_err();
        assert!(err
            .to_string()
            .contains("only terminal threads can be reopened"));
    }

    #[tokio::test]
    async fn test_service_approve_nonexistent_thread_errors() {
        let store = test_store().await;
        let svc = LifecycleService::new(store, &test_agents());

        let err = svc
            .approve("missing", "operator", "focused")
            .await
            .unwrap_err();
        assert!(matches!(err, LifecycleError::ThreadNotFound { .. }));
    }
}
