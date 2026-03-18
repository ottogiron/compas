//! Integration tests for aster-orch: store, MCP tools, backend registry.
//!
//! These tests use in-memory SQLite and a stub backend to exercise the full
//! MCP tool surface without requiring external processes or real agents.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use sqlx::SqlitePool;

use aster_orch::backend::registry::BackendRegistry;
use aster_orch::backend::{Backend, BackendOutput, PingResult};
use aster_orch::config::types::*;
use aster_orch::config::ConfigHandle;
use aster_orch::error::Result as OrchResult;
use aster_orch::mcp::params::*;
use aster_orch::mcp::server::OrchestratorMcpServer;
use aster_orch::model::agent::Agent;
use aster_orch::model::session::{Session, SessionStatus};
use aster_orch::store::{ExecutionStatus, Store, ThreadStatus};

// ═══════════════════════════════════════════════════════════════════════════
// Test Harness
// ═══════════════════════════════════════════════════════════════════════════

/// Stub backend that always succeeds with a fixed response.
#[derive(Debug)]
struct StubBackend {
    ping_alive: bool,
}

#[async_trait]
impl Backend for StubBackend {
    fn name(&self) -> &str {
        "stub"
    }

    async fn start_session(&self, agent: &Agent) -> OrchResult<Session> {
        Ok(Session {
            id: format!("stub-session-{}", agent.alias),
            agent_alias: agent.alias.clone(),
            backend: "stub".to_string(),
            started_at: chrono::Utc::now(),
            resume_session_id: None,
            stdout_tx: None,
            pid_tx: None,
        })
    }

    async fn trigger(
        &self,
        _agent: &Agent,
        session: &Session,
        instruction: Option<&str>,
    ) -> OrchResult<BackendOutput> {
        let result_text = format!("stub response to: {}", instruction.unwrap_or("(none)"));
        Ok(BackendOutput {
            success: true,
            result_text: result_text.clone(),
            parsed_intent: None,
            session_id: Some(session.id.clone()),
            raw_output: result_text,
            error_category: None,
            pid: None,
        })
    }

    async fn session_status(&self, _agent: &Agent) -> OrchResult<Option<SessionStatus>> {
        Ok(Some(SessionStatus::Running))
    }

    async fn kill_session(
        &self,
        _agent: &Agent,
        _session: &Session,
        _reason: &str,
    ) -> OrchResult<()> {
        Ok(())
    }

    async fn ping(&self, _agent: &Agent, _timeout_secs: u64) -> PingResult {
        PingResult {
            alive: self.ping_alive,
            latency_ms: 1,
            detail: Some("stub ping".into()),
        }
    }
}

/// Create a minimal valid `OrchestratorConfig` with two worker agents.
fn test_config() -> OrchestratorConfig {
    OrchestratorConfig {
        target_repo_root: PathBuf::from("/tmp"),
        state_dir: PathBuf::from("/tmp/aster-orch-test"),
        poll_interval_secs: 1,
        models: None,
        agents: vec![
            AgentConfig {
                alias: "focused".to_string(),
                backend: "stub".to_string(),
                role: AgentRole::Worker,
                model: Some("test-model".to_string()),
                prompt: Some("You are a test agent.".to_string()),
                prompt_file: None,
                timeout_secs: Some(30),
                backend_args: None,
                env: None,
                workdir: None,
                workspace: None,
                max_retries: 0,
                retry_backoff_secs: 30,
                handoff: None,
            },
            AgentConfig {
                alias: "spark".to_string(),
                backend: "stub".to_string(),
                role: AgentRole::Worker,
                model: Some("test-model".to_string()),
                prompt: None,
                prompt_file: None,
                timeout_secs: None,
                backend_args: None,
                env: None,
                workdir: None,
                workspace: None,
                max_retries: 0,
                retry_backoff_secs: 30,
                handoff: None,
            },
        ],
        worktree_dir: None,
        orchestration: OrchestrationConfig::default(),
        database: DatabaseConfig::default(),
        notifications: Default::default(),
    }
}

/// Create an in-memory Store with schema setup.
async fn test_store() -> Store {
    let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
    let store = Store::new(pool);
    store.setup().await.unwrap();
    store
}

/// Build a complete `OrchestratorMcpServer` with in-memory DB and stub backend.
async fn test_server() -> OrchestratorMcpServer {
    let store = test_store().await;
    let config = test_config();
    let mut registry = BackendRegistry::new();
    registry.register("stub", Arc::new(StubBackend { ping_alive: true }));
    OrchestratorMcpServer::new(ConfigHandle::new(config), store, registry)
}

/// Helper: extract JSON string from CallToolResult's first content block.
fn extract_json(result: &rmcp::model::CallToolResult) -> serde_json::Value {
    let text = result
        .content
        .first()
        .and_then(|c| match &c.raw {
            rmcp::model::RawContent::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .expect("expected text content");
    serde_json::from_str(text).expect("expected valid JSON")
}

/// Helper: check if CallToolResult is an error.
fn is_error(result: &rmcp::model::CallToolResult) -> bool {
    result.is_error.unwrap_or(false)
}

// ═══════════════════════════════════════════════════════════════════════════
// Store Integration Tests — uncovered methods
// ═══════════════════════════════════════════════════════════════════════════

mod store_tests {
    use super::*;

    #[tokio::test]
    async fn test_list_threads_no_filter() {
        let store = test_store().await;
        store.ensure_thread("t-1", Some("batch-A")).await.unwrap();
        store.ensure_thread("t-2", Some("batch-A")).await.unwrap();
        store.ensure_thread("t-3", Some("batch-B")).await.unwrap();

        let all = store.list_threads(None, None, 100).await.unwrap();
        assert_eq!(all.len(), 3);
    }

    #[tokio::test]
    async fn test_list_threads_filter_by_batch() {
        let store = test_store().await;
        store.ensure_thread("t-1", Some("batch-A")).await.unwrap();
        store.ensure_thread("t-2", Some("batch-A")).await.unwrap();
        store.ensure_thread("t-3", Some("batch-B")).await.unwrap();

        let batch_a = store
            .list_threads(Some("batch-A"), None, 100)
            .await
            .unwrap();
        assert_eq!(batch_a.len(), 2);
        assert!(batch_a
            .iter()
            .all(|t| t.batch_id.as_deref() == Some("batch-A")));
    }

    #[tokio::test]
    async fn test_list_threads_filter_by_status() {
        let store = test_store().await;
        store.ensure_thread("t-1", None).await.unwrap();
        store.ensure_thread("t-2", None).await.unwrap();
        store
            .update_thread_status("t-2", ThreadStatus::Completed)
            .await
            .unwrap();

        let active = store.list_threads(None, Some("Active"), 100).await.unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].thread_id, "t-1");

        let completed = store
            .list_threads(None, Some("Completed"), 100)
            .await
            .unwrap();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].thread_id, "t-2");
    }

    #[tokio::test]
    async fn test_list_threads_with_limit() {
        let store = test_store().await;
        for i in 0..10 {
            store
                .ensure_thread(&format!("t-{}", i), None)
                .await
                .unwrap();
        }
        let limited = store.list_threads(None, None, 3).await.unwrap();
        assert_eq!(limited.len(), 3);
    }

    #[tokio::test]
    async fn test_get_messages_since() {
        let store = test_store().await;
        let id1 = store
            .insert_message("t-1", "operator", "focused", "dispatch", "msg 1", None)
            .await
            .unwrap();
        let id2 = store
            .insert_message("t-1", "focused", "operator", "status-update", "msg 2", None)
            .await
            .unwrap();
        let _id3 = store
            .insert_message("t-1", "focused", "operator", "status-update", "msg 3", None)
            .await
            .unwrap();

        // Messages since id1 should not include id1
        let since1 = store.get_messages_since("t-1", id1).await.unwrap();
        assert_eq!(since1.len(), 2);
        assert_eq!(since1[0].id, id2);

        // Messages since id2 should only include id3
        let since2 = store.get_messages_since("t-1", id2).await.unwrap();
        assert_eq!(since2.len(), 1);
        assert_eq!(since2[0].intent, "status-update");
    }

    #[tokio::test]
    async fn test_get_message_by_id() {
        let store = test_store().await;
        let id = store
            .insert_message("t-1", "operator", "focused", "dispatch", "hello", None)
            .await
            .unwrap();

        let msg = store.get_message(id).await.unwrap().unwrap();
        assert_eq!(msg.from_alias, "operator");
        assert_eq!(msg.body, "hello");

        // Non-existent message
        let none = store.get_message(9999).await.unwrap();
        assert!(none.is_none());
    }

    #[tokio::test]
    async fn test_latest_message_id() {
        let store = test_store().await;

        // No messages yet
        let none = store.latest_message_id("t-1").await.unwrap();
        assert!(none.is_none());

        store
            .insert_message("t-1", "op", "a", "dispatch", "m1", None)
            .await
            .unwrap();
        let id2 = store
            .insert_message("t-1", "a", "op", "status-update", "m2", None)
            .await
            .unwrap();

        let latest = store.latest_message_id("t-1").await.unwrap().unwrap();
        assert_eq!(latest, id2);
    }

    #[tokio::test]
    async fn test_fail_execution() {
        let store = test_store().await;
        store.ensure_thread("t-1", None).await.unwrap();
        let exec_id = store.insert_execution("t-1", "focused").await.unwrap();

        let _ = store.claim_next_execution(2).await.unwrap();
        store.mark_execution_executing(&exec_id).await.unwrap();

        store
            .fail_execution(
                &exec_id,
                "timeout reached",
                Some(124),
                60000,
                ExecutionStatus::TimedOut,
            )
            .await
            .unwrap();

        let exec = store.latest_execution("t-1").await.unwrap().unwrap();
        assert_eq!(exec.status, "timed_out");
        assert_eq!(exec.error_detail.as_deref(), Some("timeout reached"));
        assert_eq!(exec.exit_code, Some(124));
        assert_eq!(exec.duration_ms, Some(60000));
    }

    #[tokio::test]
    async fn test_fail_execution_with_failed_status() {
        let store = test_store().await;
        store.ensure_thread("t-1", None).await.unwrap();
        let exec_id = store.insert_execution("t-1", "focused").await.unwrap();

        let _ = store.claim_next_execution(2).await.unwrap();
        store.mark_execution_executing(&exec_id).await.unwrap();

        store
            .fail_execution(
                &exec_id,
                "non-zero exit",
                Some(1),
                5000,
                ExecutionStatus::Failed,
            )
            .await
            .unwrap();

        let exec = store.latest_execution("t-1").await.unwrap().unwrap();
        assert_eq!(exec.status, "failed");
    }

    #[tokio::test]
    async fn test_cancel_thread_executions() {
        let store = test_store().await;
        store.ensure_thread("t-1", None).await.unwrap();

        // Create two queued executions
        let exec1 = store.insert_execution("t-1", "focused").await.unwrap();
        let _exec2 = store.insert_execution("t-1", "spark").await.unwrap();

        // Claim and start executing one
        let _ = store.claim_next_execution(2).await.unwrap();
        store.mark_execution_executing(&exec1).await.unwrap();

        // Cancel all active executions for the thread
        let cancelled = store.cancel_thread_executions("t-1").await.unwrap();
        assert_eq!(cancelled, 2); // one executing + one queued

        let execs = store.get_thread_executions("t-1").await.unwrap();
        assert!(execs.iter().all(|e| e.status == "cancelled"));
    }

    #[tokio::test]
    async fn test_get_thread_executions() {
        let store = test_store().await;
        store.ensure_thread("t-1", None).await.unwrap();
        store.insert_execution("t-1", "focused").await.unwrap();
        store.insert_execution("t-1", "spark").await.unwrap();

        let execs = store.get_thread_executions("t-1").await.unwrap();
        assert_eq!(execs.len(), 2);
        assert_eq!(execs[0].agent_alias, "focused");
        assert_eq!(execs[1].agent_alias, "spark");
    }

    #[tokio::test]
    async fn test_active_executions_by_agent() {
        let store = test_store().await;
        store.ensure_thread("t-1", None).await.unwrap();
        store.ensure_thread("t-2", None).await.unwrap();

        store.insert_execution("t-1", "focused").await.unwrap();
        store.insert_execution("t-2", "focused").await.unwrap();

        // Claim both (max_per_agent=2)
        let _ = store.claim_next_execution(2).await.unwrap();
        let _ = store.claim_next_execution(2).await.unwrap();

        let active = store.active_executions_by_agent().await.unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].0, "focused");
        assert_eq!(active[0].1, 2);
    }

    #[tokio::test]
    async fn test_status_view_basic() {
        let store = test_store().await;
        store.ensure_thread("t-1", Some("batch-1")).await.unwrap();
        store
            .insert_message(
                "t-1",
                "operator",
                "focused",
                "dispatch",
                "work",
                Some("batch-1"),
            )
            .await
            .unwrap();
        store.insert_execution("t-1", "focused").await.unwrap();

        let views = store.status_view(None, None, None, 50).await.unwrap();
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].thread_id, "t-1");
        assert_eq!(views[0].batch_id.as_deref(), Some("batch-1"));
        assert_eq!(views[0].agent_alias.as_deref(), Some("focused"));
    }

    #[tokio::test]
    async fn test_status_view_filter_by_thread() {
        let store = test_store().await;
        store.ensure_thread("t-1", None).await.unwrap();
        store.ensure_thread("t-2", None).await.unwrap();
        store.insert_execution("t-1", "focused").await.unwrap();
        store.insert_execution("t-2", "spark").await.unwrap();

        let views = store
            .status_view(Some("t-1"), None, None, 50)
            .await
            .unwrap();
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].thread_id, "t-1");
    }

    #[tokio::test]
    async fn test_status_view_filter_by_agent() {
        let store = test_store().await;
        store.ensure_thread("t-1", None).await.unwrap();
        store.ensure_thread("t-2", None).await.unwrap();
        store.insert_execution("t-1", "focused").await.unwrap();
        store.insert_execution("t-2", "spark").await.unwrap();

        let views = store
            .status_view(None, Some("spark"), None, 50)
            .await
            .unwrap();
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].agent_alias.as_deref(), Some("spark"));
    }

    #[tokio::test]
    async fn test_thread_counts() {
        let store = test_store().await;
        store.ensure_thread("t-1", None).await.unwrap();
        store.ensure_thread("t-2", None).await.unwrap();
        store.ensure_thread("t-3", None).await.unwrap();
        store
            .update_thread_status("t-3", ThreadStatus::Completed)
            .await
            .unwrap();

        let counts = store.thread_counts().await.unwrap();
        let active_count = counts
            .iter()
            .find(|(s, _)| s == "Active")
            .map(|(_, c)| *c)
            .unwrap_or(0);
        let completed_count = counts
            .iter()
            .find(|(s, _)| s == "Completed")
            .map(|(_, c)| *c)
            .unwrap_or(0);

        assert_eq!(active_count, 2);
        assert_eq!(completed_count, 1);
    }

    #[tokio::test]
    async fn test_message_count() {
        let store = test_store().await;
        assert_eq!(store.message_count().await.unwrap(), 0);

        store
            .insert_message("t-1", "op", "a", "dispatch", "m1", None)
            .await
            .unwrap();
        store
            .insert_message("t-1", "a", "op", "status-update", "m2", None)
            .await
            .unwrap();

        assert_eq!(store.message_count().await.unwrap(), 2);
    }

    #[tokio::test]
    async fn test_message_ref_and_parse() {
        use aster_orch::store::{message_ref, parse_message_ref};

        assert_eq!(message_ref(42), "db:42");
        assert_eq!(parse_message_ref("db:42").unwrap(), 42);
        assert_eq!(parse_message_ref("42").unwrap(), 42);
        assert!(parse_message_ref("invalid").is_err());
        assert!(parse_message_ref("db:abc").is_err());
    }

    #[tokio::test]
    async fn test_thread_status_enum() {
        assert_eq!(ThreadStatus::Active.as_str(), "Active");
        assert!(!ThreadStatus::Active.is_terminal());
        assert!(ThreadStatus::Completed.is_terminal());
        assert!(ThreadStatus::Failed.is_terminal());
        assert!(ThreadStatus::Abandoned.is_terminal());

        assert_eq!(
            "Completed".parse::<ThreadStatus>().unwrap(),
            ThreadStatus::Completed
        );
        assert!("invalid".parse::<ThreadStatus>().is_err());
    }

    #[tokio::test]
    async fn test_execution_status_enum() {
        assert_eq!(ExecutionStatus::Queued.as_str(), "queued");
        assert!(!ExecutionStatus::Queued.is_terminal());
        assert!(!ExecutionStatus::Queued.is_active());

        assert!(ExecutionStatus::PickedUp.is_active());
        assert!(ExecutionStatus::Executing.is_active());
        assert!(!ExecutionStatus::Completed.is_active());

        assert!(ExecutionStatus::Completed.is_terminal());
        assert!(ExecutionStatus::Failed.is_terminal());
        assert!(ExecutionStatus::TimedOut.is_terminal());
        assert!(ExecutionStatus::Crashed.is_terminal());
        assert!(ExecutionStatus::Cancelled.is_terminal());

        assert_eq!(
            "executing".parse::<ExecutionStatus>().unwrap(),
            ExecutionStatus::Executing
        );
        assert!("unknown".parse::<ExecutionStatus>().is_err());
    }

    #[tokio::test]
    async fn test_per_agent_concurrency_multiple_agents() {
        let store = test_store().await;
        store.ensure_thread("t-1", None).await.unwrap();
        store.ensure_thread("t-2", None).await.unwrap();

        // One execution per agent
        store.insert_execution("t-1", "focused").await.unwrap();
        store.insert_execution("t-2", "spark").await.unwrap();

        // max_per_agent=1: both agents can each claim one
        let first = store.claim_next_execution(1).await.unwrap();
        assert!(first.is_some());
        let second = store.claim_next_execution(1).await.unwrap();
        assert!(second.is_some());

        // Different agents
        assert_ne!(first.unwrap().agent_alias, second.unwrap().agent_alias);

        // No more work
        let third = store.claim_next_execution(1).await.unwrap();
        assert!(third.is_none());
    }

    #[tokio::test]
    async fn test_heartbeat_upsert() {
        let store = test_store().await;
        store.write_heartbeat("w-1", "0.1.0").await.unwrap();

        let hb = store.latest_heartbeat().await.unwrap().unwrap();
        assert_eq!(hb.0, "w-1");
        assert_eq!(hb.3.as_deref(), Some("0.1.0"));

        // Update version
        store.write_heartbeat("w-1", "0.2.0").await.unwrap();
        let hb = store.latest_heartbeat().await.unwrap().unwrap();
        assert_eq!(hb.3.as_deref(), Some("0.2.0"));
    }

    // ── find_untriggered_messages tests ──────────────────────────────────

    #[tokio::test]
    async fn test_find_untriggered_messages_basic() {
        let store = test_store().await;
        let msg_id = store
            .insert_message("t-1", "operator", "focused", "dispatch", "work", None)
            .await
            .unwrap();

        let trigger_intents = vec!["dispatch".to_string(), "handoff".to_string()];
        let worker_aliases = vec!["focused".to_string(), "spark".to_string()];

        let untriggered = store
            .find_untriggered_messages(&trigger_intents, &worker_aliases)
            .await
            .unwrap();

        assert_eq!(untriggered.len(), 1);
        assert_eq!(untriggered[0].0, msg_id);
        assert_eq!(untriggered[0].1, "t-1");
        assert_eq!(untriggered[0].2, "focused");
    }

    #[tokio::test]
    async fn test_find_untriggered_messages_skips_already_triggered() {
        let store = test_store().await;
        let msg_id = store
            .insert_message("t-1", "operator", "focused", "dispatch", "work", None)
            .await
            .unwrap();

        // Create execution linked to this message — marks it as triggered.
        store
            .insert_execution_with_dispatch("t-1", "focused", Some(msg_id), None)
            .await
            .unwrap();

        let trigger_intents = vec!["dispatch".to_string()];
        let worker_aliases = vec!["focused".to_string()];

        let untriggered = store
            .find_untriggered_messages(&trigger_intents, &worker_aliases)
            .await
            .unwrap();

        assert!(untriggered.is_empty());
    }

    #[tokio::test]
    async fn test_find_untriggered_messages_skips_non_trigger_intents() {
        let store = test_store().await;
        // status-update is not a trigger intent
        store
            .insert_message("t-1", "focused", "operator", "status-update", "done", None)
            .await
            .unwrap();

        let trigger_intents = vec!["dispatch".to_string(), "handoff".to_string()];
        let worker_aliases = vec!["focused".to_string()];

        let untriggered = store
            .find_untriggered_messages(&trigger_intents, &worker_aliases)
            .await
            .unwrap();

        assert!(untriggered.is_empty());
    }

    #[tokio::test]
    async fn test_find_untriggered_messages_skips_non_worker_aliases() {
        let store = test_store().await;
        // Message addressed to "reviewer" who is not in worker_aliases
        store
            .insert_message("t-1", "operator", "reviewer", "dispatch", "review", None)
            .await
            .unwrap();

        let trigger_intents = vec!["dispatch".to_string()];
        let worker_aliases = vec!["focused".to_string(), "spark".to_string()];

        let untriggered = store
            .find_untriggered_messages(&trigger_intents, &worker_aliases)
            .await
            .unwrap();

        assert!(untriggered.is_empty());
    }

    #[tokio::test]
    async fn test_find_untriggered_messages_empty_inputs() {
        let store = test_store().await;
        store
            .insert_message("t-1", "operator", "focused", "dispatch", "work", None)
            .await
            .unwrap();

        // Empty trigger_intents → no results
        let empty: Vec<String> = vec![];
        let aliases = vec!["focused".to_string()];
        let result = store
            .find_untriggered_messages(&empty, &aliases)
            .await
            .unwrap();
        assert!(result.is_empty());

        // Empty worker_aliases → no results
        let intents = vec!["dispatch".to_string()];
        let result = store
            .find_untriggered_messages(&intents, &empty)
            .await
            .unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn test_insert_execution_dedup_on_dispatch_message_id() {
        let store = test_store().await;
        let msg_id = store
            .insert_message("t-1", "operator", "focused", "dispatch", "work", None)
            .await
            .unwrap();

        // First insert succeeds
        let first = store
            .insert_execution_with_dispatch("t-1", "focused", Some(msg_id), None)
            .await
            .unwrap();
        assert!(first.is_some());

        // Second insert with same dispatch_message_id is silently ignored
        let second = store
            .insert_execution_with_dispatch("t-1", "focused", Some(msg_id), None)
            .await
            .unwrap();
        assert!(second.is_none());

        // Verify only one execution exists
        let execs = store.get_thread_executions("t-1").await.unwrap();
        assert_eq!(execs.len(), 1);
    }

    #[tokio::test]
    async fn test_insert_execution_without_dispatch_id_no_dedup() {
        let store = test_store().await;
        store.ensure_thread("t-1", None).await.unwrap();

        // Multiple inserts without dispatch_message_id all succeed (no dedup).
        let first = store.insert_execution("t-1", "focused").await.unwrap();
        let second = store.insert_execution("t-1", "focused").await.unwrap();
        assert_ne!(first, second);

        let execs = store.get_thread_executions("t-1").await.unwrap();
        assert_eq!(execs.len(), 2);
    }

    #[tokio::test]
    async fn test_find_untriggered_messages_skips_terminal_threads() {
        let store = test_store().await;

        // Insert dispatch messages on threads with different statuses.
        store
            .insert_message("t-active", "operator", "focused", "dispatch", "work", None)
            .await
            .unwrap();
        store
            .insert_message(
                "t-completed",
                "operator",
                "focused",
                "dispatch",
                "work",
                None,
            )
            .await
            .unwrap();
        store
            .insert_message("t-failed", "operator", "focused", "dispatch", "work", None)
            .await
            .unwrap();
        store
            .insert_message(
                "t-abandoned",
                "operator",
                "focused",
                "dispatch",
                "work",
                None,
            )
            .await
            .unwrap();

        // Move threads to terminal states.
        store
            .update_thread_status("t-completed", ThreadStatus::Completed)
            .await
            .unwrap();
        store
            .update_thread_status("t-failed", ThreadStatus::Failed)
            .await
            .unwrap();
        store
            .update_thread_status("t-abandoned", ThreadStatus::Abandoned)
            .await
            .unwrap();

        let trigger_intents = vec!["dispatch".to_string()];
        let worker_aliases = vec!["focused".to_string()];

        let untriggered = store
            .find_untriggered_messages(&trigger_intents, &worker_aliases)
            .await
            .unwrap();

        // Only the Active thread's message should be returned.
        assert_eq!(untriggered.len(), 1);
        assert_eq!(untriggered[0].1, "t-active");
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Backend Registry Tests
// ═══════════════════════════════════════════════════════════════════════════

mod registry_tests {
    use super::*;

    #[test]
    fn test_register_and_get() {
        let mut registry = BackendRegistry::new();
        registry.register("stub", Arc::new(StubBackend { ping_alive: true }));

        let agent_cfg = AgentConfig {
            alias: "test".to_string(),
            backend: "stub".to_string(),
            role: AgentRole::Worker,
            model: None,
            prompt: None,
            prompt_file: None,
            timeout_secs: None,
            backend_args: None,
            env: None,
            workdir: None,
            workspace: None,
            max_retries: 0,
            retry_backoff_secs: 30,
            handoff: None,
        };

        let backend = registry.get(&agent_cfg);
        assert!(backend.is_ok());
        assert_eq!(backend.unwrap().name(), "stub");
    }

    #[test]
    fn test_get_missing_backend() {
        let registry = BackendRegistry::new();

        let agent_cfg = AgentConfig {
            alias: "test".to_string(),
            backend: "nonexistent".to_string(),
            role: AgentRole::Worker,
            model: None,
            prompt: None,
            prompt_file: None,
            timeout_secs: None,
            backend_args: None,
            env: None,
            workdir: None,
            workspace: None,
            max_retries: 0,
            retry_backoff_secs: 30,
            handoff: None,
        };

        let result = registry.get(&agent_cfg);
        assert!(result.is_err());
    }

    #[test]
    fn test_get_by_name() {
        let mut registry = BackendRegistry::new();
        registry.register("stub", Arc::new(StubBackend { ping_alive: true }));

        assert!(registry.get_by_name("stub").is_ok());
        assert!(registry.get_by_name("nonexistent").is_err());
    }

    #[test]
    fn test_multiple_backends() {
        let mut registry = BackendRegistry::new();
        registry.register("backend-a", Arc::new(StubBackend { ping_alive: true }));
        registry.register("backend-b", Arc::new(StubBackend { ping_alive: false }));

        assert_eq!(registry.get_by_name("backend-a").unwrap().name(), "stub");
        assert_eq!(registry.get_by_name("backend-b").unwrap().name(), "stub");
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// MCP Tool Integration Tests — Dispatch
// ═══════════════════════════════════════════════════════════════════════════

mod dispatch_tests {
    use super::*;

    #[tokio::test]
    async fn test_dispatch_creates_thread_and_message() {
        let server = test_server().await;
        let result = server
            .dispatch_impl(DispatchParams {
                from: "operator".to_string(),
                to: "focused".to_string(),
                body: "Do some work".to_string(),
                batch: None,
                intent: "dispatch".to_string(),
                thread_id: Some("t-dispatch-1".to_string()),
            })
            .await
            .unwrap();

        assert!(!is_error(&result));
        let json = extract_json(&result);
        assert_eq!(json["thread_id"], "t-dispatch-1");
        let message_id = json["message_id"].as_i64().unwrap();
        assert!(message_id > 0);

        // Dispatch is now insert-only; no triggered/execution_id in response.
        assert!(json.get("triggered").is_none());
        assert!(json.get("execution_id").is_none());

        // Verify the message was stored correctly.
        let msg = server.store.get_message(message_id).await.unwrap().unwrap();
        assert_eq!(msg.from_alias, "operator");
        assert_eq!(msg.to_alias, "focused");
        assert_eq!(msg.intent, "dispatch");
    }

    #[tokio::test]
    async fn test_dispatch_auto_generates_thread_id() {
        let server = test_server().await;
        let result = server
            .dispatch_impl(DispatchParams {
                from: "operator".to_string(),
                to: "focused".to_string(),
                body: "Work".to_string(),
                batch: None,
                intent: "dispatch".to_string(),
                thread_id: None,
            })
            .await
            .unwrap();

        let json = extract_json(&result);
        let thread_id = json["thread_id"].as_str().unwrap();
        assert!(!thread_id.is_empty());
    }

    #[tokio::test]
    async fn test_dispatch_unknown_agent() {
        let server = test_server().await;
        let result = server
            .dispatch_impl(DispatchParams {
                from: "operator".to_string(),
                to: "nonexistent".to_string(),
                body: "Work".to_string(),
                batch: None,
                intent: "dispatch".to_string(),
                thread_id: None,
            })
            .await
            .unwrap();

        assert!(is_error(&result));
    }

    #[tokio::test]
    async fn test_dispatch_non_trigger_intent_does_not_queue() {
        let server = test_server().await;
        let result = server
            .dispatch_impl(DispatchParams {
                from: "operator".to_string(),
                to: "focused".to_string(),
                body: "Just info".to_string(),
                batch: None,
                intent: "status-update".to_string(), // not a trigger intent
                thread_id: Some("t-info".to_string()),
            })
            .await
            .unwrap();

        let json = extract_json(&result);
        // Dispatch is insert-only; no triggered/execution_id regardless of intent.
        assert!(json.get("triggered").is_none());
        assert!(json.get("execution_id").is_none());
        assert!(json["message_id"].as_i64().unwrap() > 0);
    }

    #[tokio::test]
    async fn test_dispatch_with_batch_id() {
        let server = test_server().await;
        let result = server
            .dispatch_impl(DispatchParams {
                from: "operator".to_string(),
                to: "focused".to_string(),
                body: "Batch work".to_string(),
                batch: Some("TICKET-123".to_string()),
                intent: "dispatch".to_string(),
                thread_id: Some("t-batch".to_string()),
            })
            .await
            .unwrap();

        let json = extract_json(&result);
        assert_eq!(json["thread_id"], "t-batch");

        // Verify batch was set on the thread
        let thread = server.store.get_thread("t-batch").await.unwrap().unwrap();
        assert_eq!(thread.batch_id.as_deref(), Some("TICKET-123"));
    }

    #[tokio::test]
    async fn test_dispatch_handoff_intent_triggers() {
        let server = test_server().await;
        let result = server
            .dispatch_impl(DispatchParams {
                from: "operator".to_string(),
                to: "spark".to_string(),
                body: "Handoff task".to_string(),
                batch: None,
                intent: "handoff".to_string(),
                thread_id: Some("t-handoff".to_string()),
            })
            .await
            .unwrap();

        let json = extract_json(&result);
        // Dispatch is insert-only; trigger eligibility is determined by the worker.
        assert!(json.get("triggered").is_none());
        assert!(json["message_id"].as_i64().unwrap() > 0);
    }

    #[tokio::test]
    async fn test_dispatch_continues_existing_thread() {
        let server = test_server().await;

        // First dispatch
        server
            .dispatch_impl(DispatchParams {
                from: "operator".to_string(),
                to: "focused".to_string(),
                body: "First message".to_string(),
                batch: None,
                intent: "dispatch".to_string(),
                thread_id: Some("t-continue".to_string()),
            })
            .await
            .unwrap();

        // Second dispatch on same thread
        let result = server
            .dispatch_impl(DispatchParams {
                from: "operator".to_string(),
                to: "focused".to_string(),
                body: "Second message".to_string(),
                batch: None,
                intent: "dispatch".to_string(),
                thread_id: Some("t-continue".to_string()),
            })
            .await
            .unwrap();

        let json = extract_json(&result);
        assert_eq!(json["thread_id"], "t-continue");

        // Verify both messages are on the thread
        let msgs = server
            .store
            .get_thread_messages("t-continue")
            .await
            .unwrap();
        assert_eq!(msgs.len(), 2);
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// MCP Tool Integration Tests — Lifecycle (close, abandon, reopen)
// ═══════════════════════════════════════════════════════════════════════════

mod lifecycle_tests {
    use super::*;

    /// Helper: dispatch a message and return the thread_id.
    async fn setup_thread(server: &OrchestratorMcpServer, thread_id: &str) {
        server
            .dispatch_impl(DispatchParams {
                from: "operator".to_string(),
                to: "focused".to_string(),
                body: "Do work".to_string(),
                batch: None,
                intent: "dispatch".to_string(),
                thread_id: Some(thread_id.to_string()),
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_close_marks_thread_completed() {
        let server = test_server().await;
        setup_thread(&server, "t-close-ok").await;

        let result = server
            .close_impl(CloseParams {
                thread_id: "t-close-ok".to_string(),
                from: "operator".to_string(),
                status: CloseStatus::Completed,
                note: Some("done".to_string()),
            })
            .await
            .unwrap();

        assert!(!is_error(&result));
        let json = extract_json(&result);
        assert_eq!(json["thread_id"], "t-close-ok");
        assert_eq!(json["status"], "Completed");
    }

    #[tokio::test]
    async fn test_close_marks_thread_failed() {
        let server = test_server().await;
        setup_thread(&server, "t-close-failed").await;

        let result = server
            .close_impl(CloseParams {
                thread_id: "t-close-failed".to_string(),
                from: "operator".to_string(),
                status: CloseStatus::Failed,
                note: Some("failed by operator".to_string()),
            })
            .await
            .unwrap();

        assert!(!is_error(&result));
        let status = server
            .store
            .get_thread_status("t-close-failed")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(status, "Failed");
    }

    #[tokio::test]
    async fn test_close_nonexistent_thread() {
        let server = test_server().await;
        let result = server
            .close_impl(CloseParams {
                thread_id: "nonexistent".to_string(),
                from: "operator".to_string(),
                status: CloseStatus::Completed,
                note: None,
            })
            .await
            .unwrap();

        assert!(is_error(&result));
    }

    #[tokio::test]
    async fn test_abandon_cancels_executions() {
        let server = test_server().await;
        setup_thread(&server, "t-abandon").await;

        // Manually create execution (dispatch is now insert-only).
        let msg_id = server
            .store
            .latest_message_id("t-abandon")
            .await
            .unwrap()
            .unwrap();
        server
            .store
            .insert_execution_with_dispatch("t-abandon", "focused", Some(msg_id), None)
            .await
            .unwrap();

        let result = server
            .abandon_impl(AbandonParams {
                thread_id: "t-abandon".to_string(),
            })
            .await
            .unwrap();

        assert!(!is_error(&result));
        let json = extract_json(&result);
        assert_eq!(json["status"], "Abandoned");
        assert!(json["executions_cancelled"].as_u64().unwrap() >= 1);

        let status = server
            .store
            .get_thread_status("t-abandon")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(status, "Abandoned");
    }

    #[tokio::test]
    async fn test_abandon_nonexistent_thread() {
        let server = test_server().await;

        let result = server
            .abandon_impl(AbandonParams {
                thread_id: "nonexistent".to_string(),
            })
            .await
            .unwrap();

        assert!(is_error(&result));
    }

    #[tokio::test]
    async fn test_reopen_terminal_thread() {
        let server = test_server().await;
        setup_thread(&server, "t-reopen").await;

        // Mark as completed first
        server
            .store
            .update_thread_status("t-reopen", ThreadStatus::Completed)
            .await
            .unwrap();

        let result = server
            .reopen_impl(ReopenParams {
                thread_id: "t-reopen".to_string(),
            })
            .await
            .unwrap();

        assert!(!is_error(&result));
        let json = extract_json(&result);
        assert_eq!(json["previous_status"], "Completed");
        assert_eq!(json["new_status"], "Active");
    }

    #[tokio::test]
    async fn test_reopen_already_active_thread() {
        let server = test_server().await;
        setup_thread(&server, "t-reopen-active").await;

        let result = server
            .reopen_impl(ReopenParams {
                thread_id: "t-reopen-active".to_string(),
            })
            .await
            .unwrap();

        // Should error because thread is already Active (non-terminal)
        assert!(is_error(&result));
    }

    #[tokio::test]
    async fn test_reopen_nonexistent_thread() {
        let server = test_server().await;

        let result = server
            .reopen_impl(ReopenParams {
                thread_id: "nonexistent".to_string(),
            })
            .await
            .unwrap();

        assert!(is_error(&result));
    }

    #[tokio::test]
    async fn test_full_lifecycle_dispatch_close_completed() {
        let server = test_server().await;

        // 1. Dispatch
        let dispatch_result = server
            .dispatch_impl(DispatchParams {
                from: "operator".to_string(),
                to: "focused".to_string(),
                body: "Implement feature X".to_string(),
                batch: None,
                intent: "dispatch".to_string(),
                thread_id: Some("t-full-lifecycle".to_string()),
            })
            .await
            .unwrap();
        let dispatch_json = extract_json(&dispatch_result);
        let dispatch_msg_id = dispatch_json["message_id"].as_i64().unwrap();

        // Manually create execution (dispatch is now insert-only).
        server
            .store
            .insert_execution_with_dispatch(
                "t-full-lifecycle",
                "focused",
                Some(dispatch_msg_id),
                None,
            )
            .await
            .unwrap();

        // 2. Simulate agent response (insert message as if agent replied)
        server
            .store
            .insert_message(
                "t-full-lifecycle",
                "focused",
                "operator",
                "status-update",
                "Done",
                None,
            )
            .await
            .unwrap();

        // 3. Close
        let close_result = server
            .close_impl(CloseParams {
                thread_id: "t-full-lifecycle".to_string(),
                from: "operator".to_string(),
                status: CloseStatus::Completed,
                note: Some("accepted".to_string()),
            })
            .await
            .unwrap();
        let close_json = extract_json(&close_result);
        assert_eq!(close_json["status"], "Completed");

        // Verify final state
        let status = server
            .store
            .get_thread_status("t-full-lifecycle")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(status, "Completed");

        let msgs = server
            .store
            .get_thread_messages("t-full-lifecycle")
            .await
            .unwrap();
        // dispatch + status-update + completion = 3 messages
        assert_eq!(msgs.len(), 3);
    }

    #[tokio::test]
    async fn test_full_lifecycle_dispatch_close_failed() {
        let server = test_server().await;

        // 1. Dispatch
        server
            .dispatch_impl(DispatchParams {
                from: "operator".to_string(),
                to: "focused".to_string(),
                body: "Implement feature Y".to_string(),
                batch: None,
                intent: "dispatch".to_string(),
                thread_id: Some("t-close-failed-cycle".to_string()),
            })
            .await
            .unwrap();

        // 2. Agent sends status update
        server
            .store
            .insert_message(
                "t-close-failed-cycle",
                "focused",
                "operator",
                "status-update",
                "cannot finish task",
                None,
            )
            .await
            .unwrap();

        // 3. Close as failed
        let close_result = server
            .close_impl(CloseParams {
                thread_id: "t-close-failed-cycle".to_string(),
                from: "operator".to_string(),
                status: CloseStatus::Failed,
                note: Some("operator marked failed".to_string()),
            })
            .await
            .unwrap();
        let close_json = extract_json(&close_result);
        assert_eq!(close_json["status"], "Failed");

        let msgs = server
            .store
            .get_thread_messages("t-close-failed-cycle")
            .await
            .unwrap();
        // dispatch + status-update + failure = 3
        assert_eq!(msgs.len(), 3);
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// MCP Tool Integration Tests — Query (status, transcript, read, metrics, poll, batch_status, tasks)
// ═══════════════════════════════════════════════════════════════════════════

mod query_tests {
    use super::*;

    async fn setup_data(server: &OrchestratorMcpServer) {
        server
            .dispatch_impl(DispatchParams {
                from: "operator".to_string(),
                to: "focused".to_string(),
                body: "Work on task A".to_string(),
                batch: Some("BATCH-1".to_string()),
                intent: "dispatch".to_string(),
                thread_id: Some("t-q-1".to_string()),
            })
            .await
            .unwrap();

        server
            .dispatch_impl(DispatchParams {
                from: "operator".to_string(),
                to: "spark".to_string(),
                body: "Work on task B".to_string(),
                batch: Some("BATCH-1".to_string()),
                intent: "dispatch".to_string(),
                thread_id: Some("t-q-2".to_string()),
            })
            .await
            .unwrap();

        // Manually create executions (dispatch is now insert-only).
        let msg1 = server
            .store
            .latest_message_id("t-q-1")
            .await
            .unwrap()
            .unwrap();
        server
            .store
            .insert_execution_with_dispatch("t-q-1", "focused", Some(msg1), None)
            .await
            .unwrap();
        let msg2 = server
            .store
            .latest_message_id("t-q-2")
            .await
            .unwrap()
            .unwrap();
        server
            .store
            .insert_execution_with_dispatch("t-q-2", "spark", Some(msg2), None)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_status_all_threads() {
        let server = test_server().await;
        setup_data(&server).await;

        let result = server
            .status_impl(StatusParams {
                agent: None,
                thread_id: None,
            })
            .await
            .unwrap();

        assert!(!is_error(&result));
        let json = extract_json(&result);
        assert!(json.as_array().unwrap().len() >= 2);
    }

    #[tokio::test]
    async fn test_status_filter_by_thread() {
        let server = test_server().await;
        setup_data(&server).await;

        let result = server
            .status_impl(StatusParams {
                agent: None,
                thread_id: Some("t-q-1".to_string()),
            })
            .await
            .unwrap();

        let json = extract_json(&result);
        let arr = json.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["thread_id"], "t-q-1");
    }

    #[tokio::test]
    async fn test_status_filter_by_agent() {
        let server = test_server().await;
        setup_data(&server).await;

        let result = server
            .status_impl(StatusParams {
                agent: Some("spark".to_string()),
                thread_id: None,
            })
            .await
            .unwrap();

        let json = extract_json(&result);
        let arr = json.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["agent"], "spark");
    }

    #[tokio::test]
    async fn test_transcript() {
        let server = test_server().await;
        setup_data(&server).await;

        // Add a reply
        server
            .store
            .insert_message(
                "t-q-1",
                "focused",
                "operator",
                "status-update",
                "Done",
                None,
            )
            .await
            .unwrap();

        let result = server
            .transcript_impl(TranscriptParams {
                thread_id: "t-q-1".to_string(),
            })
            .await
            .unwrap();

        assert!(!is_error(&result));
        let json = extract_json(&result);
        assert_eq!(json["thread_id"], "t-q-1");
        assert_eq!(json["messages"].as_array().unwrap().len(), 2);
        assert!(!json["executions"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_read_message() {
        let server = test_server().await;
        let id = server
            .store
            .insert_message(
                "t-read",
                "operator",
                "focused",
                "dispatch",
                "Read this",
                None,
            )
            .await
            .unwrap();

        let result = server
            .read_impl(ReadParams {
                reference: format!("db:{}", id),
            })
            .await
            .unwrap();

        assert!(!is_error(&result));
        let json = extract_json(&result);
        assert_eq!(json["body"], "Read this");
        assert_eq!(json["from"], "operator");
    }

    #[tokio::test]
    async fn test_read_message_numeric_ref() {
        let server = test_server().await;
        let id = server
            .store
            .insert_message("t-read2", "op", "a", "dispatch", "msg", None)
            .await
            .unwrap();

        let result = server
            .read_impl(ReadParams {
                reference: id.to_string(),
            })
            .await
            .unwrap();

        assert!(!is_error(&result));
    }

    #[tokio::test]
    async fn test_read_nonexistent_message() {
        let server = test_server().await;

        let result = server
            .read_impl(ReadParams {
                reference: "db:99999".to_string(),
            })
            .await
            .unwrap();

        assert!(is_error(&result));
    }

    #[tokio::test]
    async fn test_read_invalid_reference() {
        let server = test_server().await;

        let result = server
            .read_impl(ReadParams {
                reference: "invalid-ref".to_string(),
            })
            .await
            .unwrap();

        assert!(is_error(&result));
    }

    #[tokio::test]
    async fn test_metrics() {
        let server = test_server().await;
        setup_data(&server).await;

        let result = server.metrics_impl().await.unwrap();

        assert!(!is_error(&result));
        let json = extract_json(&result);
        assert!(json["total_messages"].as_i64().unwrap() >= 2);
        assert!(json["queue_depth"].as_i64().unwrap() >= 0);
    }

    #[tokio::test]
    async fn test_batch_status() {
        let server = test_server().await;
        setup_data(&server).await;

        let result = server
            .batch_status_impl(BatchStatusParams {
                batch_id: "BATCH-1".to_string(),
            })
            .await
            .unwrap();

        assert!(!is_error(&result));
        let json = extract_json(&result);
        assert_eq!(json["batch_id"], "BATCH-1");
        assert_eq!(json["thread_count"], 2);
    }

    #[tokio::test]
    async fn test_batch_status_empty_batch() {
        let server = test_server().await;

        let result = server
            .batch_status_impl(BatchStatusParams {
                batch_id: "NONEXISTENT".to_string(),
            })
            .await
            .unwrap();

        assert!(!is_error(&result));
        let json = extract_json(&result);
        assert_eq!(json["thread_count"], 0);
    }

    #[tokio::test]
    async fn test_tasks() {
        let server = test_server().await;
        setup_data(&server).await;

        let result = server
            .tasks_impl(TasksParams {
                alias: None,
                batch_id: None,
                limit: Some(10),
            })
            .await
            .unwrap();

        assert!(!is_error(&result));
        let json = extract_json(&result);
        assert!(json.as_array().unwrap().len() >= 2);
    }

    #[tokio::test]
    async fn test_tasks_filter_by_agent() {
        let server = test_server().await;
        setup_data(&server).await;

        let result = server
            .tasks_impl(TasksParams {
                alias: Some("focused".to_string()),
                batch_id: None,
                limit: None,
            })
            .await
            .unwrap();

        let json = extract_json(&result);
        let arr = json.as_array().unwrap();
        assert!(arr.iter().all(|e| e["agent"] == "focused"));
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// MCP Tool Integration Tests — Poll
// ═══════════════════════════════════════════════════════════════════════════

mod poll_tests {
    use super::*;

    #[tokio::test]
    async fn test_poll_auto_excludes_trigger_intents() {
        let server = test_server().await;

        // Dispatch (trigger intent)
        server
            .dispatch_impl(DispatchParams {
                from: "operator".to_string(),
                to: "focused".to_string(),
                body: "Do work".to_string(),
                batch: None,
                intent: "dispatch".to_string(),
                thread_id: Some("t-poll-auto".to_string()),
            })
            .await
            .unwrap();

        // Poll without intent or since_reference — should auto-exclude dispatch
        let result = server
            .poll_impl(PollParams {
                thread_id: "t-poll-auto".to_string(),
                intent: None,
                since_reference: None,
            })
            .await
            .unwrap();

        let json = extract_json(&result);
        assert_eq!(json["matched_messages"], 0);
    }

    #[tokio::test]
    async fn test_poll_returns_non_trigger_messages() {
        let server = test_server().await;

        // Dispatch
        server
            .dispatch_impl(DispatchParams {
                from: "operator".to_string(),
                to: "focused".to_string(),
                body: "Do work".to_string(),
                batch: None,
                intent: "dispatch".to_string(),
                thread_id: Some("t-poll-resp".to_string()),
            })
            .await
            .unwrap();

        // Simulate agent response
        server
            .store
            .insert_message(
                "t-poll-resp",
                "focused",
                "operator",
                "status-update",
                "Done!",
                None,
            )
            .await
            .unwrap();

        // Poll without filters — should return the status-update but not the dispatch
        let result = server
            .poll_impl(PollParams {
                thread_id: "t-poll-resp".to_string(),
                intent: None,
                since_reference: None,
            })
            .await
            .unwrap();

        let json = extract_json(&result);
        assert_eq!(json["matched_messages"], 1);
        assert_eq!(json["latest_intent"], "status-update");
    }

    #[tokio::test]
    async fn test_poll_with_intent_filter() {
        let server = test_server().await;

        server
            .store
            .insert_message("t-poll-f", "op", "focused", "dispatch", "work", None)
            .await
            .unwrap();
        server
            .store
            .insert_message("t-poll-f", "focused", "op", "progress", "progress", None)
            .await
            .unwrap();
        server
            .store
            .insert_message("t-poll-f", "focused", "op", "status-update", "done", None)
            .await
            .unwrap();

        let result = server
            .poll_impl(PollParams {
                thread_id: "t-poll-f".to_string(),
                intent: Some("status-update".to_string()),
                since_reference: None,
            })
            .await
            .unwrap();

        let json = extract_json(&result);
        assert_eq!(json["matched_messages"], 1);
        assert_eq!(json["latest_intent"], "status-update");
    }

    #[tokio::test]
    async fn test_poll_with_since_reference() {
        let server = test_server().await;

        let id1 = server
            .store
            .insert_message("t-poll-s", "op", "focused", "dispatch", "m1", None)
            .await
            .unwrap();
        server
            .store
            .insert_message("t-poll-s", "focused", "op", "status-update", "m2", None)
            .await
            .unwrap();

        // Since id1 + since_reference provided → no auto-exclude
        let result = server
            .poll_impl(PollParams {
                thread_id: "t-poll-s".to_string(),
                intent: None,
                since_reference: Some(format!("db:{}", id1)),
            })
            .await
            .unwrap();

        let json = extract_json(&result);
        // m2 is after id1, and since_reference is provided → all intents included
        assert_eq!(json["matched_messages"], 1);
        assert_eq!(json["latest_intent"], "status-update");
    }

    #[tokio::test]
    async fn test_poll_nonexistent_thread() {
        let server = test_server().await;

        let result = server
            .poll_impl(PollParams {
                thread_id: "nonexistent".to_string(),
                intent: None,
                since_reference: None,
            })
            .await
            .unwrap();

        assert!(is_error(&result));
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// MCP Tool Integration Tests — Wait
// ═══════════════════════════════════════════════════════════════════════════

mod wait_tests {
    use super::*;

    #[tokio::test]
    async fn test_wait_finds_existing_message() {
        let server = test_server().await;

        // Pre-insert a non-trigger message
        server
            .store
            .insert_message(
                "t-wait-1",
                "focused",
                "operator",
                "status-update",
                "Done",
                None,
            )
            .await
            .unwrap();

        let result = server
            .wait_impl(
                WaitParams {
                    thread_id: "t-wait-1".to_string(),
                    intent: None,
                    since_reference: None,
                    strict_new: None,
                    timeout_secs: Some(1),
                },
                None,
                None,
            )
            .await
            .unwrap();

        let json = extract_json(&result);
        assert_eq!(json["found"], true);
        assert_eq!(json["intent"], "status-update");
        assert_eq!(json["body"], "Done");
    }

    #[tokio::test]
    async fn test_wait_auto_excludes_trigger_intents() {
        let server = test_server().await;

        // Only a dispatch message — trigger intent
        server
            .store
            .insert_message("t-wait-exc", "op", "focused", "dispatch", "work", None)
            .await
            .unwrap();

        // Wait without intent/since_reference → auto-exclude dispatch → timeout
        let result = server
            .wait_impl(
                WaitParams {
                    thread_id: "t-wait-exc".to_string(),
                    intent: None,
                    since_reference: None,
                    strict_new: None,
                    timeout_secs: Some(1),
                },
                None,
                None,
            )
            .await
            .unwrap();

        let json = extract_json(&result);
        assert_eq!(json["found"], false);
    }

    #[tokio::test]
    async fn test_wait_with_intent_filter() {
        let server = test_server().await;

        // Insert multiple messages
        server
            .store
            .insert_message("t-wait-i", "op", "focused", "dispatch", "work", None)
            .await
            .unwrap();
        server
            .store
            .insert_message(
                "t-wait-i",
                "focused",
                "op",
                "status-update",
                "progress",
                None,
            )
            .await
            .unwrap();
        server
            .store
            .insert_message("t-wait-i", "focused", "op", "status-update", "ready", None)
            .await
            .unwrap();

        let result = server
            .wait_impl(
                WaitParams {
                    thread_id: "t-wait-i".to_string(),
                    intent: Some("status-update".to_string()),
                    since_reference: None,
                    strict_new: None,
                    timeout_secs: Some(1),
                },
                None,
                None,
            )
            .await
            .unwrap();

        let json = extract_json(&result);
        assert_eq!(json["found"], true);
        assert_eq!(json["intent"], "status-update");
    }

    #[tokio::test]
    async fn test_wait_times_out() {
        let server = test_server().await;

        // Create thread with no messages matching
        server
            .store
            .insert_message("t-wait-to", "op", "focused", "dispatch", "work", None)
            .await
            .unwrap();

        let start = std::time::Instant::now();
        let result = server
            .wait_impl(
                WaitParams {
                    thread_id: "t-wait-to".to_string(),
                    intent: Some("status-update".to_string()),
                    since_reference: None,
                    strict_new: None,
                    timeout_secs: Some(1),
                },
                None,
                None,
            )
            .await
            .unwrap();
        let elapsed = start.elapsed();

        let json = extract_json(&result);
        assert_eq!(json["found"], false);
        assert_eq!(json["timeout_secs"], 1);
        // Should have waited approximately 1 second
        assert!(elapsed.as_millis() >= 800);
    }

    #[tokio::test]
    async fn test_wait_with_since_reference() {
        let server = test_server().await;

        let id1 = server
            .store
            .insert_message("t-wait-sr", "op", "focused", "dispatch", "work", None)
            .await
            .unwrap();
        server
            .store
            .insert_message("t-wait-sr", "focused", "op", "status-update", "done", None)
            .await
            .unwrap();

        let result = server
            .wait_impl(
                WaitParams {
                    thread_id: "t-wait-sr".to_string(),
                    intent: None,
                    since_reference: Some(format!("db:{}", id1)),
                    strict_new: None,
                    timeout_secs: Some(1),
                },
                None,
                None,
            )
            .await
            .unwrap();

        let json = extract_json(&result);
        assert_eq!(json["found"], true);
        // Since reference provided → all messages after id1 included (no auto-exclude)
        assert_eq!(json["intent"], "status-update");
    }

    #[tokio::test]
    async fn test_wait_concurrent_message_arrival() {
        let server = test_server().await;
        let store_clone = server.store.clone();

        // Create thread
        server
            .store
            .insert_message("t-wait-conc", "op", "focused", "dispatch", "work", None)
            .await
            .unwrap();

        // Spawn a task that inserts a message after a short delay
        let inserter = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            store_clone
                .insert_message(
                    "t-wait-conc",
                    "focused",
                    "operator",
                    "status-update",
                    "Completed",
                    None,
                )
                .await
                .unwrap();
        });

        // Wait should pick up the message once it arrives
        let result = server
            .wait_impl(
                WaitParams {
                    thread_id: "t-wait-conc".to_string(),
                    intent: Some("status-update".to_string()),
                    since_reference: None,
                    strict_new: None,
                    timeout_secs: Some(5),
                },
                None,
                None,
            )
            .await
            .unwrap();

        inserter.await.unwrap();

        let json = extract_json(&result);
        assert_eq!(json["found"], true);
        assert_eq!(json["intent"], "status-update");
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// MCP Tool Integration Tests — Session & Health
// ═══════════════════════════════════════════════════════════════════════════

mod session_health_tests {
    use super::*;

    #[tokio::test]
    async fn test_session_info() {
        let server = test_server().await;

        let result = server.session_info_impl().unwrap();

        assert!(!is_error(&result));
        let json = extract_json(&result);
        assert_eq!(json["server"], "aster-orch");
        assert_eq!(json["agent_count"], 2);
    }

    #[tokio::test]
    async fn test_list_agents() {
        let server = test_server().await;

        let result = server.list_agents_impl().unwrap();

        assert!(!is_error(&result));
        let json = extract_json(&result);
        let agents = json.as_array().unwrap();
        assert_eq!(agents.len(), 2);

        let aliases: Vec<&str> = agents
            .iter()
            .map(|a| a["alias"].as_str().unwrap())
            .collect();
        assert!(aliases.contains(&"focused"));
        assert!(aliases.contains(&"spark"));
    }

    #[tokio::test]
    async fn test_health_all_agents() {
        let server = test_server().await;

        let result = server
            .health_impl(HealthParams { alias: None })
            .await
            .unwrap();

        assert!(!is_error(&result));
        let json = extract_json(&result);
        let agents = json["agents"].as_array().unwrap();
        assert_eq!(agents.len(), 2);
        assert!(agents.iter().all(|a| a["ping_alive"] == true));
    }

    #[tokio::test]
    async fn test_health_specific_agent() {
        let server = test_server().await;

        let result = server
            .health_impl(HealthParams {
                alias: Some("focused".to_string()),
            })
            .await
            .unwrap();

        let json = extract_json(&result);
        let agents = json["agents"].as_array().unwrap();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0]["alias"], "focused");
    }

    #[tokio::test]
    async fn test_health_with_heartbeat() {
        let server = test_server().await;
        server
            .store
            .write_heartbeat("worker-test", "0.2.0")
            .await
            .unwrap();

        let result = server
            .health_impl(HealthParams { alias: None })
            .await
            .unwrap();

        let json = extract_json(&result);
        let hb = &json["worker_heartbeat"];
        assert_eq!(hb["worker_id"], "worker-test");
        assert_eq!(hb["version"], "0.2.0");
    }

    #[tokio::test]
    async fn test_health_no_heartbeat() {
        let server = test_server().await;

        let result = server
            .health_impl(HealthParams { alias: None })
            .await
            .unwrap();

        let json = extract_json(&result);
        assert!(json["worker_heartbeat"].is_null());
    }

    #[tokio::test]
    async fn test_health_with_dead_backend() {
        let store = test_store().await;
        let config = test_config();
        let mut registry = BackendRegistry::new();
        registry.register("stub", Arc::new(StubBackend { ping_alive: false }));
        let server = OrchestratorMcpServer::new(ConfigHandle::new(config), store, registry);

        let result = server
            .health_impl(HealthParams { alias: None })
            .await
            .unwrap();

        let json = extract_json(&result);
        let agents = json["agents"].as_array().unwrap();
        assert!(agents.iter().all(|a| a["ping_alive"] == false));
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// MCP Tool Integration Tests — Diagnose
// ═══════════════════════════════════════════════════════════════════════════

mod diagnose_tests {
    use super::*;

    #[tokio::test]
    async fn test_diagnose_active_with_queued_execution_no_heartbeat() {
        let server = test_server().await;

        server
            .dispatch_impl(DispatchParams {
                from: "operator".to_string(),
                to: "focused".to_string(),
                body: "Work".to_string(),
                batch: None,
                intent: "dispatch".to_string(),
                thread_id: Some("t-diag-q".to_string()),
            })
            .await
            .unwrap();

        // Manually create execution (dispatch is now insert-only).
        let msg_id = server
            .store
            .latest_message_id("t-diag-q")
            .await
            .unwrap()
            .unwrap();
        server
            .store
            .insert_execution_with_dispatch("t-diag-q", "focused", Some(msg_id), None)
            .await
            .unwrap();

        let result = server
            .diagnose_impl(DiagnoseParams {
                thread_id: "t-diag-q".to_string(),
            })
            .await
            .unwrap();

        let json = extract_json(&result);
        assert_eq!(json["thread_status"], "Active");
        assert!(json["message_count"].as_u64().unwrap() >= 1);
        assert!(json["execution_count"].as_u64().unwrap() >= 1);

        // Should have blocker about no heartbeat
        let blockers = json["blockers"].as_array().unwrap();
        assert!(blockers
            .iter()
            .any(|b| b.as_str().unwrap().contains("heartbeat")));
    }

    #[tokio::test]
    async fn test_diagnose_active_with_queued_execution_with_heartbeat() {
        let server = test_server().await;

        server
            .dispatch_impl(DispatchParams {
                from: "operator".to_string(),
                to: "focused".to_string(),
                body: "Work".to_string(),
                batch: None,
                intent: "dispatch".to_string(),
                thread_id: Some("t-diag-hb".to_string()),
            })
            .await
            .unwrap();

        // Manually create execution (dispatch is now insert-only).
        let msg_id = server
            .store
            .latest_message_id("t-diag-hb")
            .await
            .unwrap()
            .unwrap();
        server
            .store
            .insert_execution_with_dispatch("t-diag-hb", "focused", Some(msg_id), None)
            .await
            .unwrap();

        // Write heartbeat
        server.store.write_heartbeat("w-1", "0.2.0").await.unwrap();

        let result = server
            .diagnose_impl(DiagnoseParams {
                thread_id: "t-diag-hb".to_string(),
            })
            .await
            .unwrap();

        let json = extract_json(&result);
        // No blocker about heartbeat
        let blockers = json["blockers"].as_array().unwrap();
        assert!(!blockers
            .iter()
            .any(|b| b.as_str().unwrap().contains("heartbeat")));
    }

    #[tokio::test]
    async fn test_diagnose_completed_thread() {
        let server = test_server().await;
        server.store.ensure_thread("t-diag-c", None).await.unwrap();
        server
            .store
            .update_thread_status("t-diag-c", ThreadStatus::Completed)
            .await
            .unwrap();

        let result = server
            .diagnose_impl(DiagnoseParams {
                thread_id: "t-diag-c".to_string(),
            })
            .await
            .unwrap();

        let json = extract_json(&result);
        assert_eq!(json["thread_status"], "Completed");
        let suggestions = json["suggestions"].as_array().unwrap();
        assert!(suggestions
            .iter()
            .any(|s| s.as_str().unwrap().contains("no action needed")));
    }

    #[tokio::test]
    async fn test_diagnose_abandoned_thread() {
        let server = test_server().await;
        server.store.ensure_thread("t-diag-a", None).await.unwrap();
        server
            .store
            .update_thread_status("t-diag-a", ThreadStatus::Abandoned)
            .await
            .unwrap();

        let result = server
            .diagnose_impl(DiagnoseParams {
                thread_id: "t-diag-a".to_string(),
            })
            .await
            .unwrap();

        let json = extract_json(&result);
        let suggestions = json["suggestions"].as_array().unwrap();
        assert!(suggestions
            .iter()
            .any(|s| s.as_str().unwrap().contains("reopen")));
    }

    #[tokio::test]
    async fn test_diagnose_failed_execution() {
        let server = test_server().await;
        server.store.ensure_thread("t-diag-f", None).await.unwrap();

        let exec_id = server
            .store
            .insert_execution("t-diag-f", "focused")
            .await
            .unwrap();
        let _ = server.store.claim_next_execution(2).await.unwrap();
        server
            .store
            .mark_execution_executing(&exec_id)
            .await
            .unwrap();
        server
            .store
            .fail_execution(
                &exec_id,
                "process crashed",
                Some(1),
                5000,
                ExecutionStatus::Failed,
            )
            .await
            .unwrap();

        let result = server
            .diagnose_impl(DiagnoseParams {
                thread_id: "t-diag-f".to_string(),
            })
            .await
            .unwrap();

        let json = extract_json(&result);
        let blockers = json["blockers"].as_array().unwrap();
        assert!(blockers
            .iter()
            .any(|b| b.as_str().unwrap().contains("failed")));
    }

    #[tokio::test]
    async fn test_diagnose_nonexistent_thread() {
        let server = test_server().await;

        let result = server
            .diagnose_impl(DiagnoseParams {
                thread_id: "nonexistent".to_string(),
            })
            .await
            .unwrap();

        assert!(is_error(&result));
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// ORCH-EVO-2: Event Broadcast Channel Tests
// ═══════════════════════════════════════════════════════════════════════════

mod worktree_tests {
    use super::*;
    use aster_orch::worktree::WorktreeManager;
    use std::process::Command;

    #[tokio::test]
    async fn test_execute_trigger_with_worktree_mode() {
        let store = test_store().await;

        // Create a real git repo for the worktree source
        let repo_dir = tempfile::tempdir().unwrap();
        let repo_path = repo_dir.path();
        let init = Command::new("git")
            .args(["init", &repo_path.to_string_lossy()])
            .output()
            .unwrap();
        assert!(init.status.success(), "git init failed");

        // Need at least one commit for HEAD to exist
        let commit = Command::new("git")
            .args([
                "-C",
                &repo_path.to_string_lossy(),
                "-c",
                "user.email=test@test.com",
                "-c",
                "user.name=Test",
                "commit",
                "--allow-empty",
                "-m",
                "initial",
            ])
            .output()
            .unwrap();
        assert!(commit.status.success(), "git commit failed");

        let worktree_manager = std::sync::Arc::new(WorktreeManager::new());

        // Configure agent with worktree mode
        let agent_configs = vec![AgentConfig {
            alias: "focused".to_string(),
            backend: "stub".to_string(),
            role: AgentRole::Worker,
            model: Some("test-model".to_string()),
            prompt: Some("You are a test agent.".to_string()),
            prompt_file: None,
            timeout_secs: Some(30),
            backend_args: None,
            env: None,
            workdir: None,
            workspace: Some("worktree".to_string()),
            max_retries: 0,
            retry_backoff_secs: 30,
            handoff: None,
        }];

        let mut registry = BackendRegistry::new();
        registry.register("stub", Arc::new(StubBackend { ping_alive: true }));
        let registry = Arc::new(registry);

        // Create a thread and execution
        store.ensure_thread("t-wt-1", None).await.unwrap();
        let msg_id = store
            .insert_message("t-wt-1", "operator", "focused", "dispatch", "do work", None)
            .await
            .unwrap();
        let exec_id = store
            .insert_execution_with_dispatch("t-wt-1", "focused", Some(msg_id), None)
            .await
            .unwrap()
            .unwrap();

        // Claim the execution
        let execution = store.claim_next_execution(1).await.unwrap().unwrap();
        assert_eq!(execution.id, exec_id);

        // Execute with worktree mode
        let output = aster_orch::worker::execute_trigger(
            &execution,
            &store,
            &registry,
            &agent_configs,
            "do work",
            30,
            None,
            None,
            &worktree_manager,
            repo_path,
            None,
        )
        .await;

        assert!(output.success, "execution should succeed");

        // Verify worktree was created at the new default location
        let wt_path = repo_path
            .parent()
            .unwrap()
            .join(".aster-worktrees")
            .join("t-wt-1");
        assert!(wt_path.exists(), "worktree directory should exist");

        // Verify worktree path was stored in DB
        let stored_path = store.get_thread_worktree_path("t-wt-1").await.unwrap();
        assert!(
            stored_path.is_some(),
            "worktree path should be stored in DB"
        );
        assert_eq!(stored_path.unwrap(), wt_path);

        // Cleanup
        worktree_manager
            .remove_worktree(repo_path, "t-wt-1", None)
            .unwrap();
        // Also clean up the .aster-worktrees directory
        let wt_root = repo_path.parent().unwrap().join(".aster-worktrees");
        let _ = std::fs::remove_dir_all(&wt_root);
    }

    #[tokio::test]
    async fn test_execute_trigger_shared_mode_no_worktree() {
        let store = test_store().await;

        let agent_configs = vec![AgentConfig {
            alias: "focused".to_string(),
            backend: "stub".to_string(),
            role: AgentRole::Worker,
            model: Some("test-model".to_string()),
            prompt: Some("You are a test agent.".to_string()),
            prompt_file: None,
            timeout_secs: Some(30),
            backend_args: None,
            env: None,
            workdir: None,
            workspace: None, // shared (default)
            max_retries: 0,
            retry_backoff_secs: 30,
            handoff: None,
        }];

        let mut registry = BackendRegistry::new();
        registry.register("stub", Arc::new(StubBackend { ping_alive: true }));
        let registry = Arc::new(registry);

        let worktree_manager = std::sync::Arc::new(WorktreeManager::new());

        // Create a thread and execution
        store.ensure_thread("t-shared-1", None).await.unwrap();
        let msg_id = store
            .insert_message(
                "t-shared-1",
                "operator",
                "focused",
                "dispatch",
                "do work",
                None,
            )
            .await
            .unwrap();
        let exec_id = store
            .insert_execution_with_dispatch("t-shared-1", "focused", Some(msg_id), None)
            .await
            .unwrap()
            .unwrap();

        let execution = store.claim_next_execution(1).await.unwrap().unwrap();
        assert_eq!(execution.id, exec_id);

        let output = aster_orch::worker::execute_trigger(
            &execution,
            &store,
            &registry,
            &agent_configs,
            "do work",
            30,
            None,
            None,
            &worktree_manager,
            std::path::Path::new("/tmp"),
            None,
        )
        .await;

        assert!(output.success, "execution should succeed");

        // No worktree should be created in shared mode
        let stored_path = store.get_thread_worktree_path("t-shared-1").await.unwrap();
        assert!(
            stored_path.is_none(),
            "no worktree path should be stored for shared mode"
        );
    }
}

mod evo2_event_bus_tests {
    use super::*;
    use aster_orch::events::{EventBus, OrchestratorEvent};
    use aster_orch::worker::WorkerRunner;
    use tokio::sync::Semaphore;

    // NOTE: Basic emit/subscribe tests live in src/events.rs as unit tests.
    // This module only contains integration-level tests that exercise the
    // full worker → event bus → subscriber flow.

    #[tokio::test]
    async fn test_worker_emits_events_on_dispatch_cycle() {
        // Set up in-memory DB and store.
        let store = test_store().await;

        // Register stub backend.
        let mut registry = BackendRegistry::new();
        registry.register("stub", Arc::new(StubBackend { ping_alive: true }));

        // Build config and handle.
        let config = test_config();
        let config_handle = ConfigHandle::new(config.clone());

        // Create event bus and subscribe before the runner is created.
        let event_bus = EventBus::new();
        let mut rx = event_bus.subscribe();

        let worktree_manager = aster_orch::worktree::WorktreeManager::new();
        let runner = WorkerRunner::new(
            config_handle,
            store.clone(),
            registry,
            event_bus,
            worktree_manager,
        );

        // Seed: insert a dispatch message and a queued execution.
        let agent_alias = config.agents[0].alias.clone();
        let thread_id = "evo2-test-thread";

        let msg_id = store
            .insert_message(
                thread_id,
                "operator",
                &agent_alias,
                "dispatch",
                "do something",
                None,
            )
            .await
            .unwrap();

        store
            .insert_execution_with_dispatch(thread_id, &agent_alias, Some(msg_id), None)
            .await
            .unwrap();

        // Run poll_once to claim and spawn the execution.
        let semaphore = Arc::new(Semaphore::new(4));
        runner.poll_once(&semaphore).await;

        // Wait for the spawned task to complete by polling for MessageReceived
        // (the last event emitted in the dispatch cycle) with a timeout.
        let mut events = Vec::new();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut saw_message = false;
        loop {
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Ok(event)) => {
                    if matches!(event, OrchestratorEvent::MessageReceived { .. }) {
                        saw_message = true;
                    }
                    events.push(event);
                    if saw_message {
                        // Drain any remaining buffered events.
                        while let Ok(event) = rx.try_recv() {
                            events.push(event);
                        }
                        break;
                    }
                }
                Ok(Err(_)) => break, // channel closed
                Err(_) => break,     // deadline exceeded
            }
        }

        let has_started = events
            .iter()
            .any(|e| matches!(e, OrchestratorEvent::ExecutionStarted { .. }));
        let has_completed = events.iter().any(|e| {
            matches!(
                e,
                OrchestratorEvent::ExecutionCompleted { success: true, .. }
            )
        });
        let has_message = events
            .iter()
            .any(|e| matches!(e, OrchestratorEvent::MessageReceived { .. }));

        let event_names: Vec<String> = events.iter().map(|e| format!("{:?}", e)).collect();
        assert!(
            has_started,
            "expected ExecutionStarted event, got: {:?}",
            event_names
        );
        assert!(
            has_completed,
            "expected ExecutionCompleted(success=true), got: {:?}",
            event_names
        );
        assert!(
            has_message,
            "expected MessageReceived event, got: {:?}",
            event_names
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// ORCH-EVO-13 — Prompt Hash Round-Trip Integration Test
// ═══════════════════════════════════════════════════════════════════════════

mod prompt_hash_tests {
    use super::*;
    use aster_orch::events::EventBus;
    use aster_orch::worker::WorkerRunner;
    use sha2::{Digest, Sha256};
    use tokio::sync::Semaphore;

    fn expected_hash(prompt: &str) -> String {
        let mut h = Sha256::new();
        h.update(prompt.as_bytes());
        format!("{:x}", h.finalize())
    }

    /// Verify that `scan_and_enqueue_triggers` stores the SHA-256 hash of the
    /// agent prompt on the execution row and that it round-trips through both
    /// `orch_tasks` and `orch_transcript`.
    #[tokio::test]
    async fn test_prompt_hash_round_trip_through_worker_and_mcp() {
        let store = test_store().await;
        let config = test_config(); // "focused" has prompt "You are a test agent."
        let config_handle = ConfigHandle::new(config.clone());

        let mut registry = BackendRegistry::new();
        registry.register("stub", Arc::new(StubBackend { ping_alive: true }));

        let event_bus = EventBus::new();
        let worktree_manager = aster_orch::worktree::WorktreeManager::new();
        let runner = WorkerRunner::new(
            config_handle.clone(),
            store.clone(),
            registry,
            event_bus,
            worktree_manager,
        );

        // Insert a trigger message addressed to the "focused" agent (which has
        // prompt = "You are a test agent." in test_config()).
        let thread_id = "t-hash-rt";
        store.ensure_thread(thread_id, None).await.unwrap();
        store
            .insert_message(
                thread_id,
                "operator",
                "focused",
                "dispatch",
                "do something",
                None,
            )
            .await
            .unwrap();

        // scan_and_enqueue_triggers runs inside poll_once before the claim loop.
        let semaphore = Arc::new(Semaphore::new(4));
        runner.poll_once(&semaphore).await;

        // The hash must be stored at enqueue time, so it's readable immediately.
        let views = store
            .status_view(Some(thread_id), None, None, 10)
            .await
            .unwrap();
        let view = views
            .iter()
            .find(|v| v.execution_id.is_some())
            .expect("expected at least one execution");

        let known_prompt = "You are a test agent.";
        assert_eq!(
            view.prompt_hash.as_deref(),
            Some(expected_hash(known_prompt).as_str()),
            "prompt_hash in store should match sha256 of the known agent prompt"
        );

        // Build an MCP server backed by the same store and verify the hash
        // appears in orch_tasks output.
        let server = OrchestratorMcpServer::new(config_handle, store, BackendRegistry::new());
        let tasks_result = server
            .tasks_impl(TasksParams {
                alias: Some("focused".to_string()),
                batch_id: None,
                limit: Some(10),
            })
            .await
            .unwrap();
        assert!(!is_error(&tasks_result));
        let json = extract_json(&tasks_result);
        let entry = json
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["thread_id"] == thread_id)
            .expect("expected task entry for the thread");
        assert_eq!(
            entry["prompt_hash"].as_str(),
            Some(expected_hash(known_prompt).as_str()),
            "orch_tasks must surface prompt_hash"
        );

        // Also verify prompt_hash appears in orch_transcript executions.
        let transcript_result = server
            .transcript_impl(TranscriptParams {
                thread_id: thread_id.to_string(),
            })
            .await
            .unwrap();
        assert!(!is_error(&transcript_result));
        let tjson = extract_json(&transcript_result);
        let exec_entry = tjson["executions"]
            .as_array()
            .unwrap()
            .first()
            .expect("expected at least one execution in transcript");
        assert_eq!(
            exec_entry["prompt_hash"].as_str(),
            Some(expected_hash(known_prompt).as_str()),
            "orch_transcript must surface prompt_hash"
        );
    }

    /// Agents without a prompt field must have prompt_hash = null (not a hash
    /// of the empty string).
    #[tokio::test]
    async fn test_prompt_hash_null_for_prompt_less_agent() {
        let store = test_store().await;
        let config = test_config(); // "spark" has prompt: None
        let config_handle = ConfigHandle::new(config.clone());

        let mut registry = BackendRegistry::new();
        registry.register("stub", Arc::new(StubBackend { ping_alive: true }));

        let event_bus = EventBus::new();
        let worktree_manager = aster_orch::worktree::WorktreeManager::new();
        let runner = WorkerRunner::new(
            config_handle,
            store.clone(),
            registry,
            event_bus,
            worktree_manager,
        );

        let thread_id = "t-hash-null";
        store.ensure_thread(thread_id, None).await.unwrap();
        store
            .insert_message(
                thread_id,
                "operator",
                "spark", // prompt: None in test_config
                "dispatch",
                "do something",
                None,
            )
            .await
            .unwrap();

        let semaphore = Arc::new(Semaphore::new(4));
        runner.poll_once(&semaphore).await;

        let views = store
            .status_view(Some(thread_id), None, None, 10)
            .await
            .unwrap();
        let view = views
            .iter()
            .find(|v| v.execution_id.is_some())
            .expect("expected at least one execution");

        assert_eq!(
            view.prompt_hash, None,
            "prompt_hash must be None when the agent has no prompt field"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Handoff Chain Tests (ORCH-CHAIN-1)
// ═══════════════════════════════════════════════════════════════════════════

mod handoff_chain_tests {
    use super::*;
    use aster_orch::config::types::{HandoffConfig, HandoffTarget};
    use aster_orch::events::{EventBus, OrchestratorEvent};
    use aster_orch::worker::WorkerRunner;
    use tokio::sync::Semaphore;

    /// Config with agent A handing off `response` to agent B.
    fn chain_config() -> OrchestratorConfig {
        OrchestratorConfig {
            target_repo_root: PathBuf::from("/tmp"),
            state_dir: PathBuf::from("/tmp/aster-orch-test"),
            poll_interval_secs: 1,
            models: None,
            agents: vec![
                AgentConfig {
                    alias: "agent-a".to_string(),
                    backend: "stub".to_string(),
                    role: AgentRole::Worker,
                    model: None,
                    prompt: None,
                    prompt_file: None,
                    timeout_secs: None,
                    backend_args: None,
                    env: None,
                    workdir: None,
                    workspace: None,
                    max_retries: 0,
                    retry_backoff_secs: 30,
                    handoff: Some(HandoffConfig {
                        on_response: Some(HandoffTarget::Single("agent-b".to_string())),
                        handoff_prompt: None,
                        max_chain_depth: Some(3),
                    }),
                },
                AgentConfig {
                    alias: "agent-b".to_string(),
                    backend: "stub".to_string(),
                    role: AgentRole::Worker,
                    model: None,
                    prompt: None,
                    prompt_file: None,
                    timeout_secs: None,
                    backend_args: None,
                    env: None,
                    workdir: None,
                    workspace: None,
                    max_retries: 0,
                    retry_backoff_secs: 30,
                    handoff: None, // agent-b does NOT chain further
                },
            ],
            worktree_dir: None,
            orchestration: OrchestrationConfig::default(),
            database: DatabaseConfig::default(),
            notifications: Default::default(),
        }
    }

    #[tokio::test]
    async fn test_basic_auto_handoff_chain() {
        // Agent A has on_response: agent-b.
        // Dispatch to A → A completes with "response" → auto-handoff to B.
        let store = test_store().await;
        let config = chain_config();
        let config_handle = ConfigHandle::new(config.clone());

        let mut registry = BackendRegistry::new();
        registry.register("stub", Arc::new(StubBackend { ping_alive: true }));

        let event_bus = EventBus::new();
        let mut rx = event_bus.subscribe();

        let worktree_manager = aster_orch::worktree::WorktreeManager::new();
        let runner = WorkerRunner::new(
            config_handle,
            store.clone(),
            registry,
            event_bus,
            worktree_manager,
        );

        // Seed: dispatch to agent-a.
        let thread_id = "chain-test-1";
        let msg_id = store
            .insert_message(
                thread_id,
                "operator",
                "agent-a",
                "dispatch",
                "implement feature X",
                None,
            )
            .await
            .unwrap();
        store
            .insert_execution_with_dispatch(thread_id, "agent-a", Some(msg_id), None)
            .await
            .unwrap();

        // Run poll_once → agent-a executes.
        let semaphore = Arc::new(Semaphore::new(4));
        runner.poll_once(&semaphore).await;

        // Wait for TWO MessageReceived events: the reply + the handoff.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut message_count = 0;
        while let Ok(Ok(event)) = tokio::time::timeout_at(deadline, rx.recv()).await {
            if matches!(event, OrchestratorEvent::MessageReceived { .. }) {
                message_count += 1;
                if message_count >= 2 {
                    while rx.try_recv().is_ok() {}
                    break;
                }
            }
        }
        assert!(
            message_count >= 2,
            "expected 2 MessageReceived events (reply + handoff), got {}",
            message_count
        );

        // Verify: a handoff message was inserted to agent-b.
        let messages = store.get_thread_messages(thread_id).await.unwrap();
        let handoff_msg = messages
            .iter()
            .find(|m| m.intent == "handoff" && m.to_alias == "agent-b");
        assert!(
            handoff_msg.is_some(),
            "expected handoff message to agent-b; messages: {:?}",
            messages
                .iter()
                .map(|m| format!("{}→{} ({})", m.from_alias, m.to_alias, m.intent))
                .collect::<Vec<_>>()
        );

        // The handoff message body should contain the original dispatch context.
        let hm = handoff_msg.unwrap();
        assert!(
            hm.body.contains("implement feature X"),
            "handoff body should include original dispatch context"
        );
    }

    #[tokio::test]
    async fn test_chain_depth_limit_interrupts() {
        // Set max_chain_depth=1 on agent-a. After 1 handoff, subsequent should
        // be interrupted with a review-request to operator.
        let store = test_store().await;

        let mut config = chain_config();
        // Set max_chain_depth to 1.
        config.agents[0].handoff.as_mut().unwrap().max_chain_depth = Some(1);

        let config_handle = ConfigHandle::new(config.clone());

        let mut registry = BackendRegistry::new();
        registry.register("stub", Arc::new(StubBackend { ping_alive: true }));

        let event_bus = EventBus::new();
        let mut rx = event_bus.subscribe();

        let worktree_manager = aster_orch::worktree::WorktreeManager::new();
        let runner = WorkerRunner::new(
            config_handle,
            store.clone(),
            registry,
            event_bus,
            worktree_manager,
        );

        let thread_id = "chain-depth-test";

        // Pre-seed: insert an existing handoff message so depth = 1 already.
        let _msg0 = store
            .insert_message(
                thread_id,
                "operator",
                "agent-a",
                "dispatch",
                "original task",
                None,
            )
            .await
            .unwrap();
        let _handoff_msg = store
            .insert_message(
                thread_id,
                "agent-b",
                "agent-a",
                "handoff",
                "previous handoff",
                None,
            )
            .await
            .unwrap();

        // Now dispatch to agent-a again.
        let dispatch_msg = store
            .insert_message(
                thread_id,
                "operator",
                "agent-a",
                "dispatch",
                "follow-up work",
                None,
            )
            .await
            .unwrap();
        store
            .insert_execution_with_dispatch(thread_id, "agent-a", Some(dispatch_msg), None)
            .await
            .unwrap();

        let semaphore = Arc::new(Semaphore::new(4));
        runner.poll_once(&semaphore).await;

        // Wait for TWO MessageReceived events: reply + chain-interrupt review-request.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut message_count = 0;
        while let Ok(Ok(event)) = tokio::time::timeout_at(deadline, rx.recv()).await {
            if matches!(event, OrchestratorEvent::MessageReceived { .. }) {
                message_count += 1;
                if message_count >= 2 {
                    while rx.try_recv().is_ok() {}
                    break;
                }
            }
        }

        // Verify: chain should be interrupted (no new handoff, but a review-request).
        let messages = store.get_thread_messages(thread_id).await.unwrap();
        let new_handoff_count = messages
            .iter()
            .filter(|m| m.intent == "handoff" && m.from_alias == "agent-a")
            .count();

        // Should still be 1 (the pre-seeded one), not 2.
        assert_eq!(
            new_handoff_count, 0,
            "no new handoff should be created from agent-a when depth limit is hit"
        );

        // There should be a review-request message about chain interruption.
        let interrupt = messages
            .iter()
            .find(|m| m.intent == "review-request" && m.body.contains("chain interrupted"));
        assert!(
            interrupt.is_some(),
            "expected chain-interrupt review-request; messages: {:?}",
            messages
                .iter()
                .map(|m| format!(
                    "{}→{} ({}: {})",
                    m.from_alias,
                    m.to_alias,
                    m.intent,
                    &m.body[..m.body.len().min(50)]
                ))
                .collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn test_no_handoff_config_preserves_behavior() {
        // Agent without handoff config should not auto-dispatch.
        let store = test_store().await;
        let config = test_config(); // default config, no handoff
        let config_handle = ConfigHandle::new(config.clone());

        let mut registry = BackendRegistry::new();
        registry.register("stub", Arc::new(StubBackend { ping_alive: true }));

        let event_bus = EventBus::new();
        let mut rx = event_bus.subscribe();

        let worktree_manager = aster_orch::worktree::WorktreeManager::new();
        let runner = WorkerRunner::new(
            config_handle,
            store.clone(),
            registry,
            event_bus,
            worktree_manager,
        );

        let thread_id = "no-handoff-test";
        let msg_id = store
            .insert_message(
                thread_id, "operator", "focused", "dispatch", "do work", None,
            )
            .await
            .unwrap();
        store
            .insert_execution_with_dispatch(thread_id, "focused", Some(msg_id), None)
            .await
            .unwrap();

        let semaphore = Arc::new(Semaphore::new(4));
        runner.poll_once(&semaphore).await;

        // Wait for completion.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Ok(OrchestratorEvent::MessageReceived { .. })) => {
                    while rx.try_recv().is_ok() {}
                    break;
                }
                Ok(Ok(_)) => continue,
                Ok(Err(_)) | Err(_) => break,
            }
        }

        // Verify: no handoff message was inserted.
        let messages = store.get_thread_messages(thread_id).await.unwrap();
        let handoff_count = messages.iter().filter(|m| m.intent == "handoff").count();
        assert_eq!(
            handoff_count, 0,
            "no handoff messages should exist for agent without handoff config"
        );
    }

    #[tokio::test]
    async fn test_count_handoff_messages_store() {
        let store = test_store().await;
        let thread_id = "count-handoff-test";

        store.ensure_thread(thread_id, None).await.unwrap();

        // Initially zero.
        let count = store.count_handoff_messages(thread_id).await.unwrap();
        assert_eq!(count, 0);

        // Insert a non-handoff message.
        store
            .insert_message(thread_id, "operator", "agent-a", "dispatch", "task", None)
            .await
            .unwrap();
        let count = store.count_handoff_messages(thread_id).await.unwrap();
        assert_eq!(count, 0);

        // Insert handoff messages.
        store
            .insert_message(
                thread_id,
                "agent-a",
                "agent-b",
                "handoff",
                "pass along",
                None,
            )
            .await
            .unwrap();
        let count = store.count_handoff_messages(thread_id).await.unwrap();
        assert_eq!(count, 1);

        store
            .insert_message(
                thread_id,
                "agent-b",
                "agent-a",
                "handoff",
                "back to you",
                None,
            )
            .await
            .unwrap();
        let count = store.count_handoff_messages(thread_id).await.unwrap();
        assert_eq!(count, 2);
    }

    #[tokio::test]
    async fn test_handoff_config_yaml_roundtrip() {
        let yaml = r#"
target_repo_root: /tmp
state_dir: /tmp/test
agents:
  - alias: coder
    backend: stub
    handoff:
      on_response: reviewer
      max_chain_depth: 5
  - alias: reviewer
    backend: stub
    handoff:
      on_response: operator
"#;
        let config = aster_orch::config::load_config_from_str(yaml).unwrap();

        let coder = &config.agents[0];
        let handoff = coder.handoff.as_ref().unwrap();
        assert!(matches!(
            handoff.on_response,
            Some(HandoffTarget::Single(ref s)) if s == "reviewer"
        ));
        assert_eq!(handoff.max_chain_depth, Some(5));

        let reviewer = &config.agents[1];
        let handoff = reviewer.handoff.as_ref().unwrap();
        assert!(matches!(
            handoff.on_response,
            Some(HandoffTarget::Single(ref s)) if s == "operator"
        ));
    }

    #[tokio::test]
    async fn test_handoff_custom_prompt_prepended() {
        // Agent A has on_response: agent-b with a custom handoff_prompt.
        // Dispatch to A → A completes → handoff body should start with custom prompt.
        let store = test_store().await;
        let mut config = chain_config();
        config.agents[0].handoff.as_mut().unwrap().handoff_prompt =
            Some("Review for correctness and test coverage.".to_string());
        let config_handle = ConfigHandle::new(config.clone());

        let mut registry = BackendRegistry::new();
        registry.register("stub", Arc::new(StubBackend { ping_alive: true }));

        let event_bus = EventBus::new();
        let mut rx = event_bus.subscribe();

        let worktree_manager = aster_orch::worktree::WorktreeManager::new();
        let runner = WorkerRunner::new(
            config_handle,
            store.clone(),
            registry,
            event_bus,
            worktree_manager,
        );

        let thread_id = "handoff-prompt-test";
        let msg_id = store
            .insert_message(
                thread_id,
                "operator",
                "agent-a",
                "dispatch",
                "implement feature Y",
                None,
            )
            .await
            .unwrap();
        store
            .insert_execution_with_dispatch(thread_id, "agent-a", Some(msg_id), None)
            .await
            .unwrap();

        let semaphore = Arc::new(Semaphore::new(4));
        runner.poll_once(&semaphore).await;

        // Wait for reply + handoff events.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut message_count = 0;
        while let Ok(Ok(event)) = tokio::time::timeout_at(deadline, rx.recv()).await {
            if matches!(event, OrchestratorEvent::MessageReceived { .. }) {
                message_count += 1;
                if message_count >= 2 {
                    while rx.try_recv().is_ok() {}
                    break;
                }
            }
        }
        assert!(message_count >= 2, "expected reply + handoff events");

        let messages = store.get_thread_messages(thread_id).await.unwrap();
        let handoff_msg = messages
            .iter()
            .find(|m| m.intent == "handoff" && m.to_alias == "agent-b")
            .expect("expected handoff message to agent-b");

        // Body should start with the custom prompt.
        assert!(
            handoff_msg
                .body
                .starts_with("Review for correctness and test coverage."),
            "handoff body should start with custom prompt, got: {}",
            &handoff_msg.body[..handoff_msg.body.len().min(200)]
        );
        // And still contain the auto-generated context.
        assert!(
            handoff_msg.body.contains("## Original dispatch"),
            "handoff body should contain auto-generated context"
        );
        assert!(
            handoff_msg.body.contains("implement feature Y"),
            "handoff body should contain original dispatch"
        );
    }

    #[tokio::test]
    async fn test_handoff_without_custom_prompt_unchanged() {
        // Agent A has on_response: agent-b but NO handoff_prompt.
        // Handoff body should start directly with "## Original dispatch".
        let store = test_store().await;
        let config = chain_config(); // default: no handoff_prompt
        let config_handle = ConfigHandle::new(config.clone());

        let mut registry = BackendRegistry::new();
        registry.register("stub", Arc::new(StubBackend { ping_alive: true }));

        let event_bus = EventBus::new();
        let mut rx = event_bus.subscribe();

        let worktree_manager = aster_orch::worktree::WorktreeManager::new();
        let runner = WorkerRunner::new(
            config_handle,
            store.clone(),
            registry,
            event_bus,
            worktree_manager,
        );

        let thread_id = "handoff-no-prompt-test";
        let msg_id = store
            .insert_message(
                thread_id,
                "operator",
                "agent-a",
                "dispatch",
                "implement feature Z",
                None,
            )
            .await
            .unwrap();
        store
            .insert_execution_with_dispatch(thread_id, "agent-a", Some(msg_id), None)
            .await
            .unwrap();

        let semaphore = Arc::new(Semaphore::new(4));
        runner.poll_once(&semaphore).await;

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut message_count = 0;
        while let Ok(Ok(event)) = tokio::time::timeout_at(deadline, rx.recv()).await {
            if matches!(event, OrchestratorEvent::MessageReceived { .. }) {
                message_count += 1;
                if message_count >= 2 {
                    while rx.try_recv().is_ok() {}
                    break;
                }
            }
        }
        assert!(message_count >= 2, "expected reply + handoff events");

        let messages = store.get_thread_messages(thread_id).await.unwrap();
        let handoff_msg = messages
            .iter()
            .find(|m| m.intent == "handoff" && m.to_alias == "agent-b")
            .expect("expected handoff message to agent-b");

        // Body should start with auto-generated context (no custom prompt prefix).
        assert!(
            handoff_msg.body.starts_with("## Original dispatch"),
            "handoff body without custom prompt should start with '## Original dispatch', got: {}",
            &handoff_msg.body[..handoff_msg.body.len().min(200)]
        );
    }

    #[tokio::test]
    async fn test_handoff_prompt_yaml_roundtrip() {
        let yaml = r#"
target_repo_root: /tmp
state_dir: /tmp/test
agents:
  - alias: coder
    backend: stub
    handoff:
      on_response: reviewer
      handoff_prompt: |
        Review for correctness, test coverage, and AGENTS.md compliance.
      max_chain_depth: 3
  - alias: reviewer
    backend: stub
"#;
        let config = aster_orch::config::load_config_from_str(yaml).unwrap();

        let coder = &config.agents[0];
        let handoff = coder.handoff.as_ref().unwrap();
        assert!(matches!(
            handoff.on_response,
            Some(HandoffTarget::Single(ref s)) if s == "reviewer"
        ));
        assert!(
            handoff
                .handoff_prompt
                .as_ref()
                .unwrap()
                .contains("Review for correctness"),
            "handoff_prompt should be parsed from YAML"
        );
        assert_eq!(handoff.max_chain_depth, Some(3));

        // Agent without handoff_prompt should have None.
        let reviewer = &config.agents[1];
        assert!(reviewer.handoff.is_none());
    }
}

mod pending_chain_work_tests {
    use super::*;

    #[tokio::test]
    async fn test_count_pending_chain_work_no_work() {
        let store = test_store().await;
        store.ensure_thread("t-chain", None).await.unwrap();

        let count = store.count_pending_chain_work("t-chain").await.unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn test_count_pending_chain_work_active_execution() {
        let store = test_store().await;
        store.ensure_thread("t-chain", None).await.unwrap();

        // Insert a queued execution — counts as pending
        store.insert_execution("t-chain", "focused").await.unwrap();

        let count = store.count_pending_chain_work("t-chain").await.unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn test_count_pending_chain_work_untriggered_handoff() {
        let store = test_store().await;
        store.ensure_thread("t-chain", None).await.unwrap();

        // Insert a handoff message with no linked execution — counts as pending
        store
            .insert_message(
                "t-chain",
                "agent-a",
                "agent-b",
                "handoff",
                "take over",
                None,
            )
            .await
            .unwrap();

        let count = store.count_pending_chain_work("t-chain").await.unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn test_count_pending_chain_work_triggered_handoff() {
        let store = test_store().await;
        store.ensure_thread("t-chain", None).await.unwrap();

        // Insert a handoff message
        let msg_id = store
            .insert_message(
                "t-chain",
                "agent-a",
                "agent-b",
                "handoff",
                "take over",
                None,
            )
            .await
            .unwrap();

        // Link an execution to that handoff message — it's now "triggered"
        store
            .insert_execution_with_dispatch("t-chain", "agent-b", Some(msg_id), None)
            .await
            .unwrap();

        // Triggered handoff does not count as pending (but the queued execution does)
        // Total: 0 untriggered handoffs + 1 queued execution = 1
        let count = store.count_pending_chain_work("t-chain").await.unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn test_count_pending_chain_work_completed_execution_not_counted() {
        let store = test_store().await;
        store.ensure_thread("t-chain", None).await.unwrap();

        // Insert and complete an execution
        let exec_id = store.insert_execution("t-chain", "focused").await.unwrap();
        store.claim_next_execution(10).await.unwrap();
        store.mark_execution_executing(&exec_id).await.unwrap();
        store
            .complete_execution(&exec_id, Some(0), None, None, 100)
            .await
            .unwrap();

        let count = store.count_pending_chain_work("t-chain").await.unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn test_count_pending_chain_work_nonexistent_thread() {
        let store = test_store().await;

        // Non-existent thread returns 0 (no rows match)
        let count = store
            .count_pending_chain_work("no-such-thread")
            .await
            .unwrap();
        assert_eq!(count, 0);
    }
}

mod await_chain_wait_tests {
    use super::*;
    use aster_orch::wait::{self, WaitOutcome, WaitRequest};
    use std::time::Duration;

    #[tokio::test]
    async fn test_await_chain_returns_immediately_when_no_pending_work() {
        let store = test_store().await;
        store.ensure_thread("t-chain", None).await.unwrap();

        // Insert a response message — no pending chain work
        store
            .insert_message(
                "t-chain",
                "reviewer",
                "operator",
                "response",
                "review done",
                None,
            )
            .await
            .unwrap();

        let req = WaitRequest {
            thread_id: "t-chain".to_string(),
            intent: Some("response".to_string()),
            since_reference: None,
            strict_new: false,
            timeout: Duration::from_secs(5),
            trigger_intents: vec![],
            await_chain: true,
        };

        let outcome = wait::wait_for_message(&store, &req).await.unwrap();
        match outcome {
            WaitOutcome::Found(msg) => {
                assert_eq!(msg.from_alias, "reviewer");
                assert_eq!(msg.body, "review done");
            }
            WaitOutcome::Timeout { .. } => panic!("should not timeout"),
        }
    }

    #[tokio::test]
    async fn test_await_chain_waits_through_handoff() {
        let store = test_store().await;
        store.ensure_thread("t-chain", None).await.unwrap();

        // Insert implementer's response message (found first during polling)
        store
            .insert_message(
                "t-chain",
                "implementer",
                "operator",
                "response",
                "impl done",
                None,
            )
            .await
            .unwrap();

        // Insert handoff message (simulates auto-handoff from implementer to reviewer)
        let handoff_msg_id = store
            .insert_message(
                "t-chain",
                "implementer",
                "reviewer",
                "handoff",
                "handoff context",
                None,
            )
            .await
            .unwrap();

        // Insert a queued execution linked to the handoff message
        let exec_id = store
            .insert_execution_with_dispatch("t-chain", "reviewer", Some(handoff_msg_id), None)
            .await
            .unwrap()
            .expect("execution should be created");

        // Background task: execute and insert reviewer's response, then mark complete.
        // Note: in production, the worker inserts the reply message BEFORE marking
        // the execution complete. The test must follow the same order to avoid the
        // race window where the execution is completed but the reply isn't yet visible.
        let store2 = store.clone();
        let exec_id2 = exec_id.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(400)).await;
            store2.claim_next_execution(10).await.unwrap();
            store2.mark_execution_executing(&exec_id2).await.unwrap();
            store2
                .insert_message(
                    "t-chain",
                    "reviewer",
                    "operator",
                    "response",
                    "review done",
                    None,
                )
                .await
                .unwrap();
            store2
                .complete_execution(&exec_id2, Some(0), None, None, 200)
                .await
                .unwrap();
        });

        let req = WaitRequest {
            thread_id: "t-chain".to_string(),
            intent: Some("response".to_string()),
            since_reference: None,
            strict_new: false,
            timeout: Duration::from_secs(10),
            trigger_intents: vec![],
            await_chain: true,
        };

        let outcome = wait::wait_for_message(&store, &req).await.unwrap();
        match outcome {
            WaitOutcome::Found(msg) => {
                // Should return the reviewer's reply, not the implementer's
                assert_eq!(msg.from_alias, "reviewer");
                assert_eq!(msg.body, "review done");
            }
            WaitOutcome::Timeout { .. } => panic!("should not timeout"),
        }
    }

    #[tokio::test]
    async fn test_await_chain_returns_on_depth_limit() {
        let store = test_store().await;
        store.ensure_thread("t-chain", None).await.unwrap();

        // Simulate a depth-limit escalation: an escalation message is posted
        // with no pending chain work (no active executions, no untriggered handoffs)
        store
            .insert_message(
                "t-chain",
                "orchestrator",
                "operator",
                "response",
                "chain depth limit reached; escalating to operator",
                None,
            )
            .await
            .unwrap();

        let req = WaitRequest {
            thread_id: "t-chain".to_string(),
            intent: None,
            since_reference: None,
            strict_new: false,
            timeout: Duration::from_secs(5),
            trigger_intents: vec![],
            await_chain: true,
        };

        let outcome = wait::wait_for_message(&store, &req).await.unwrap();
        match outcome {
            WaitOutcome::Found(msg) => {
                assert_eq!(msg.from_alias, "orchestrator");
                assert!(msg.body.contains("depth limit"));
            }
            WaitOutcome::Timeout { .. } => panic!("should not timeout"),
        }
    }

    #[test]
    fn test_await_chain_false_is_default_behavior() {
        // Without await_chain, the request struct still works as before.
        // This is a compile-time check via construction.
        let _req = WaitRequest {
            thread_id: "t-1".to_string(),
            intent: None,
            since_reference: None,
            strict_new: false,
            timeout: Duration::from_secs(1),
            trigger_intents: vec![],
            await_chain: false,
        };
    }

    // ── Fan-out settlement tests (ADR-014 Phase 2) ──

    #[tokio::test]
    async fn test_await_chain_waits_for_fanout_threads() {
        let store = test_store().await;
        let source_thread = "t-fanout-source";
        store.ensure_thread(source_thread, None).await.unwrap();

        // Insert the implementer's response on the source thread
        store
            .insert_message(
                source_thread,
                "implementer",
                "operator",
                "response",
                "impl done",
                None,
            )
            .await
            .unwrap();

        // Create two fan-out threads linked via source_thread_id
        // (thread IDs are auto-generated by insert_fanout_handoffs)
        let fanout_results = store
            .insert_fanout_handoffs(
                source_thread,
                "batch-fanout",
                &["reviewer-1".to_string(), "reviewer-2".to_string()],
                "implementer",
                "review this",
            )
            .await
            .unwrap();
        let (child_1_id, child_1_msg_id) = &fanout_results[0];
        let (child_2_id, child_2_msg_id) = &fanout_results[1];

        // Insert queued executions linked to the handoff messages
        let exec_1_id = store
            .insert_execution_with_dispatch(child_1_id, "reviewer-1", Some(*child_1_msg_id), None)
            .await
            .unwrap()
            .expect("exec 1 should be created");
        let exec_2_id = store
            .insert_execution_with_dispatch(child_2_id, "reviewer-2", Some(*child_2_msg_id), None)
            .await
            .unwrap()
            .expect("exec 2 should be created");

        // Background: complete fan-out executions after a delay
        let store2 = store.clone();
        let exec_1 = exec_1_id.clone();
        let exec_2 = exec_2_id.clone();
        let child_1 = child_1_id.clone();
        let child_2 = child_2_id.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(400)).await;
            store2.claim_next_execution(10).await.unwrap();
            store2.mark_execution_executing(&exec_1).await.unwrap();
            store2
                .insert_message(&child_1, "reviewer-1", "operator", "response", "lgtm", None)
                .await
                .unwrap();
            store2
                .complete_execution(&exec_1, Some(0), None, None, 100)
                .await
                .unwrap();

            store2.claim_next_execution(10).await.unwrap();
            store2.mark_execution_executing(&exec_2).await.unwrap();
            store2
                .insert_message(&child_2, "reviewer-2", "operator", "response", "lgtm", None)
                .await
                .unwrap();
            store2
                .complete_execution(&exec_2, Some(0), None, None, 100)
                .await
                .unwrap();
        });

        let req = WaitRequest {
            thread_id: source_thread.to_string(),
            intent: Some("response".to_string()),
            since_reference: None,
            strict_new: false,
            timeout: Duration::from_secs(10),
            trigger_intents: vec![],
            await_chain: true,
        };

        let outcome = wait::wait_for_message(&store, &req).await.unwrap();
        match outcome {
            WaitOutcome::Found(msg) => {
                assert_eq!(msg.from_alias, "implementer");
                assert_eq!(msg.body, "impl done");
            }
            WaitOutcome::Timeout { .. } => panic!("should not timeout — fan-out should settle"),
        }

        // Verify: no pending work on source or children
        let pending = store
            .count_pending_chain_and_fanout_work(source_thread)
            .await
            .unwrap();
        assert_eq!(pending, 0);
    }

    #[tokio::test]
    async fn test_count_pending_chain_and_fanout_work_zero_when_no_fanout() {
        let store = test_store().await;
        store.ensure_thread("t-no-fanout", None).await.unwrap();

        // No children, no pending work
        let count = store
            .count_pending_chain_and_fanout_work("t-no-fanout")
            .await
            .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn test_count_pending_chain_and_fanout_work_counts_fanout_executions() {
        let store = test_store().await;
        let source = "t-count-source";
        store.ensure_thread(source, None).await.unwrap();

        // Create fan-out child with queued execution
        let results = store
            .insert_fanout_handoffs(
                source,
                "batch-count",
                &["reviewer".to_string()],
                "agent",
                "review",
            )
            .await
            .unwrap();
        let (child_id, msg_id) = &results[0];

        // Execution linked to the handoff message
        store
            .insert_execution_with_dispatch(child_id, "reviewer", Some(*msg_id), None)
            .await
            .unwrap();

        // Source thread itself has no pending work, but fan-out child does
        // (queued execution counts as pending)
        let pending = store
            .count_pending_chain_and_fanout_work(source)
            .await
            .unwrap();
        // The handoff message has an execution linked (dispatch_message_id),
        // so only the queued execution counts.
        assert_eq!(
            pending, 1,
            "fan-out child queued execution should be counted"
        );

        // The old method should NOT see the fan-out child
        let old_pending = store.count_pending_chain_work(source).await.unwrap();
        assert_eq!(
            old_pending, 0,
            "old method should not count fan-out children"
        );
    }

    #[tokio::test]
    async fn test_fanout_settlement_ignores_batch_siblings() {
        let store = test_store().await;

        // Thread A: source, part of batch "shared-batch"
        let source_a = "t-sibling-a";
        store
            .ensure_thread(source_a, Some("shared-batch"))
            .await
            .unwrap();

        // Thread B: sibling in same batch but different source
        let source_b = "t-sibling-b";
        store
            .ensure_thread(source_b, Some("shared-batch"))
            .await
            .unwrap();

        // Create fan-out child for thread B (source_thread_id = source_b)
        let results = store
            .insert_fanout_handoffs(
                source_b,
                "shared-batch",
                &["reviewer".to_string()],
                "agent",
                "review for B",
            )
            .await
            .unwrap();
        let (child_id, msg_id) = &results[0];

        // Verify child's source_thread_id points to B, not A
        let child_thread = store.get_thread(child_id).await.unwrap().unwrap();
        assert_eq!(child_thread.source_thread_id.as_deref(), Some(source_b));

        // Add a queued execution on the child
        store
            .insert_execution_with_dispatch(child_id, "reviewer", Some(*msg_id), None)
            .await
            .unwrap();

        // Thread A should NOT see thread B's fan-out child as pending work,
        // even though they share the same batch_id.
        let pending_a = store
            .count_pending_chain_and_fanout_work(source_a)
            .await
            .unwrap();
        assert_eq!(
            pending_a, 0,
            "thread A should not count thread B's fan-out children"
        );

        // Thread B SHOULD see its fan-out child's pending work.
        let pending_b = store
            .count_pending_chain_and_fanout_work(source_b)
            .await
            .unwrap();
        assert!(
            pending_b > 0,
            "thread B should count its own fan-out children"
        );
    }

    #[tokio::test]
    async fn test_fanout_with_chain_on_child() {
        let store = test_store().await;
        let source = "t-child-chain";
        store.ensure_thread(source, None).await.unwrap();

        // Create fan-out child
        let results = store
            .insert_fanout_handoffs(
                source,
                "batch-child-chain",
                &["reviewer".to_string()],
                "agent",
                "review",
            )
            .await
            .unwrap();
        let (child_id, msg_id) = &results[0];

        // Execution on child completes (linked to handoff, so handoff is not untriggered)
        store
            .insert_execution_with_dispatch(child_id, "reviewer", Some(*msg_id), None)
            .await
            .unwrap();

        // But an untriggered handoff remains on the child thread
        store
            .insert_message(
                child_id,
                "reviewer",
                "sub-reviewer",
                "handoff",
                "sub-review needed",
                None,
            )
            .await
            .unwrap();

        // The untriggered handoff on the child should be counted
        let pending = store
            .count_pending_chain_and_fanout_work(source)
            .await
            .unwrap();
        // queued execution on child (1) + untriggered handoff on child (1) = 2
        assert_eq!(
            pending, 2,
            "expected queued execution + untriggered handoff on fan-out child, got {}",
            pending
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Fan-Out Handoff Tests (ORCH-HANDOFF-2)
// ═══════════════════════════════════════════════════════════════════════════

mod fanout_tests {
    use super::*;
    use aster_orch::config::types::{HandoffConfig, HandoffTarget};
    use aster_orch::events::{EventBus, OrchestratorEvent};
    use aster_orch::worker::WorkerRunner;
    use tokio::sync::Semaphore;

    fn fanout_config() -> OrchestratorConfig {
        OrchestratorConfig {
            target_repo_root: PathBuf::from("/tmp"),
            state_dir: PathBuf::from("/tmp/aster-orch-test"),
            poll_interval_secs: 1,
            models: None,
            agents: vec![
                AgentConfig {
                    alias: "agent-a".to_string(),
                    backend: "stub".to_string(),
                    role: AgentRole::Worker,
                    model: None,
                    prompt: None,
                    prompt_file: None,
                    timeout_secs: None,
                    backend_args: None,
                    env: None,
                    workdir: None,
                    workspace: None,
                    max_retries: 0,
                    retry_backoff_secs: 30,
                    handoff: Some(HandoffConfig {
                        on_response: Some(HandoffTarget::FanOut(vec![
                            "reviewer".to_string(),
                            "reviewer-2".to_string(),
                        ])),
                        handoff_prompt: None,
                        max_chain_depth: Some(3),
                    }),
                },
                AgentConfig {
                    alias: "reviewer".to_string(),
                    backend: "stub".to_string(),
                    role: AgentRole::Worker,
                    model: None,
                    prompt: None,
                    prompt_file: None,
                    timeout_secs: None,
                    backend_args: None,
                    env: None,
                    workdir: None,
                    workspace: None,
                    max_retries: 0,
                    retry_backoff_secs: 30,
                    handoff: None,
                },
                AgentConfig {
                    alias: "reviewer-2".to_string(),
                    backend: "stub".to_string(),
                    role: AgentRole::Worker,
                    model: None,
                    prompt: None,
                    prompt_file: None,
                    timeout_secs: None,
                    backend_args: None,
                    env: None,
                    workdir: None,
                    workspace: None,
                    max_retries: 0,
                    retry_backoff_secs: 30,
                    handoff: None,
                },
            ],
            worktree_dir: None,
            orchestration: OrchestrationConfig::default(),
            database: DatabaseConfig::default(),
            notifications: Default::default(),
        }
    }

    #[tokio::test]
    async fn test_fanout_creates_separate_threads() {
        let store = test_store().await;
        let config = fanout_config();
        let config_handle = ConfigHandle::new(config.clone());

        let mut registry = BackendRegistry::new();
        registry.register("stub", Arc::new(StubBackend { ping_alive: true }));

        let event_bus = EventBus::new();
        let mut rx = event_bus.subscribe();

        let worktree_manager = aster_orch::worktree::WorktreeManager::new();
        let runner = WorkerRunner::new(
            config_handle,
            store.clone(),
            registry,
            event_bus,
            worktree_manager,
        );

        let thread_id = "fanout-test-1";
        let msg_id = store
            .insert_message(
                thread_id,
                "operator",
                "agent-a",
                "dispatch",
                "implement feature X",
                None,
            )
            .await
            .unwrap();
        store
            .insert_execution_with_dispatch(thread_id, "agent-a", Some(msg_id), None)
            .await
            .unwrap();

        let semaphore = Arc::new(Semaphore::new(4));
        runner.poll_once(&semaphore).await;

        // Wait for MessageReceived events: reply + 2 fan-out handoffs
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut message_count = 0;
        while let Ok(Ok(event)) = tokio::time::timeout_at(deadline, rx.recv()).await {
            if matches!(event, OrchestratorEvent::MessageReceived { .. }) {
                message_count += 1;
                if message_count >= 3 {
                    while rx.try_recv().is_ok() {}
                    break;
                }
            }
        }
        assert!(
            message_count >= 3,
            "expected 3 MessageReceived events (reply + 2 fan-out), got {}",
            message_count
        );

        // Find the fan-out threads by batch_id
        let batch_id = format!("fanout-{}", thread_id);
        let threads = store.list_threads(Some(&batch_id), None, 10).await.unwrap();
        assert_eq!(
            threads.len(),
            2,
            "expected 2 fan-out threads, got {}",
            threads.len()
        );

        // Each thread should have a handoff message
        for thread in &threads {
            let msgs = store.get_thread_messages(&thread.thread_id).await.unwrap();
            let handoff = msgs.iter().find(|m| m.intent == "handoff");
            assert!(
                handoff.is_some(),
                "expected handoff message in thread {}",
                thread.thread_id
            );
            assert_eq!(thread.batch_id.as_deref(), Some(batch_id.as_str()));
        }
    }

    #[tokio::test]
    async fn test_fanout_single_element_degrades_to_single() {
        let store = test_store().await;
        let mut config = fanout_config();
        // Single-element FanOut
        config.agents[0].handoff = Some(HandoffConfig {
            on_response: Some(HandoffTarget::FanOut(vec!["reviewer".to_string()])),
            handoff_prompt: None,
            max_chain_depth: Some(3),
        });
        let config_handle = ConfigHandle::new(config.clone());

        let mut registry = BackendRegistry::new();
        registry.register("stub", Arc::new(StubBackend { ping_alive: true }));

        let event_bus = EventBus::new();
        let mut rx = event_bus.subscribe();

        let worktree_manager = aster_orch::worktree::WorktreeManager::new();
        let runner = WorkerRunner::new(
            config_handle,
            store.clone(),
            registry,
            event_bus,
            worktree_manager,
        );

        let thread_id = "fanout-single-test";
        let msg_id = store
            .insert_message(
                thread_id,
                "operator",
                "agent-a",
                "dispatch",
                "implement feature",
                None,
            )
            .await
            .unwrap();
        store
            .insert_execution_with_dispatch(thread_id, "agent-a", Some(msg_id), None)
            .await
            .unwrap();

        let semaphore = Arc::new(Semaphore::new(4));
        runner.poll_once(&semaphore).await;

        // Wait for reply + handoff (on same thread, like Single behavior)
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut message_count = 0;
        while let Ok(Ok(event)) = tokio::time::timeout_at(deadline, rx.recv()).await {
            if matches!(event, OrchestratorEvent::MessageReceived { .. }) {
                message_count += 1;
                if message_count >= 2 {
                    while rx.try_recv().is_ok() {}
                    break;
                }
            }
        }
        assert!(
            message_count >= 2,
            "expected reply + handoff, got {}",
            message_count
        );

        // Handoff should be on the SAME thread (not a new one)
        let messages = store.get_thread_messages(thread_id).await.unwrap();
        let handoff = messages
            .iter()
            .find(|m| m.intent == "handoff" && m.to_alias == "reviewer");
        assert!(handoff.is_some(), "expected handoff on same thread");
    }

    #[tokio::test]
    async fn test_fanout_inherits_batch_id() {
        let store = test_store().await;

        let source_thread = "fanout-inherit-test";
        let batch_id = "existing-batch-123";

        // Create thread with existing batch_id
        store
            .ensure_thread(source_thread, Some(batch_id))
            .await
            .unwrap();

        // Use store method directly
        let results = store
            .insert_fanout_handoffs(
                source_thread,
                batch_id,
                &["reviewer".to_string(), "reviewer-2".to_string()],
                "agent-a",
                "handoff body",
            )
            .await
            .unwrap();

        assert_eq!(results.len(), 2);

        for (thread_id, _msg_id) in &results {
            let thread = store.get_thread(thread_id).await.unwrap().unwrap();
            assert_eq!(
                thread.batch_id.as_deref(),
                Some(batch_id),
                "fan-out thread should inherit batch_id"
            );
        }
    }

    #[tokio::test]
    async fn test_fanout_generates_batch_id() {
        let store = test_store().await;
        let config = fanout_config();
        let config_handle = ConfigHandle::new(config.clone());

        let mut registry = BackendRegistry::new();
        registry.register("stub", Arc::new(StubBackend { ping_alive: true }));

        let event_bus = EventBus::new();
        let mut rx = event_bus.subscribe();

        let worktree_manager = aster_orch::worktree::WorktreeManager::new();
        let runner = WorkerRunner::new(
            config_handle,
            store.clone(),
            registry,
            event_bus,
            worktree_manager,
        );

        // Thread without batch_id
        let thread_id = "fanout-gen-batch-test";
        let msg_id = store
            .insert_message(
                thread_id, "operator", "agent-a", "dispatch", "do work", None,
            )
            .await
            .unwrap();
        store
            .insert_execution_with_dispatch(thread_id, "agent-a", Some(msg_id), None)
            .await
            .unwrap();

        let semaphore = Arc::new(Semaphore::new(4));
        runner.poll_once(&semaphore).await;

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut message_count = 0;
        while let Ok(Ok(event)) = tokio::time::timeout_at(deadline, rx.recv()).await {
            if matches!(event, OrchestratorEvent::MessageReceived { .. }) {
                message_count += 1;
                if message_count >= 3 {
                    while rx.try_recv().is_ok() {}
                    break;
                }
            }
        }

        let expected_batch = format!("fanout-{}", thread_id);
        let threads = store
            .list_threads(Some(&expected_batch), None, 10)
            .await
            .unwrap();
        assert_eq!(
            threads.len(),
            2,
            "expected 2 fan-out threads with generated batch_id"
        );
        for t in &threads {
            assert_eq!(t.batch_id.as_deref(), Some(expected_batch.as_str()));
        }
    }

    #[tokio::test]
    async fn test_fanout_handoff_prompt_applied() {
        let store = test_store().await;
        let mut config = fanout_config();
        config.agents[0].handoff.as_mut().unwrap().handoff_prompt =
            Some("Review for correctness.".to_string());
        let config_handle = ConfigHandle::new(config.clone());

        let mut registry = BackendRegistry::new();
        registry.register("stub", Arc::new(StubBackend { ping_alive: true }));

        let event_bus = EventBus::new();
        let mut rx = event_bus.subscribe();

        let worktree_manager = aster_orch::worktree::WorktreeManager::new();
        let runner = WorkerRunner::new(
            config_handle,
            store.clone(),
            registry,
            event_bus,
            worktree_manager,
        );

        let thread_id = "fanout-prompt-test";
        let msg_id = store
            .insert_message(
                thread_id,
                "operator",
                "agent-a",
                "dispatch",
                "implement feature",
                None,
            )
            .await
            .unwrap();
        store
            .insert_execution_with_dispatch(thread_id, "agent-a", Some(msg_id), None)
            .await
            .unwrap();

        let semaphore = Arc::new(Semaphore::new(4));
        runner.poll_once(&semaphore).await;

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut message_count = 0;
        while let Ok(Ok(event)) = tokio::time::timeout_at(deadline, rx.recv()).await {
            if matches!(event, OrchestratorEvent::MessageReceived { .. }) {
                message_count += 1;
                if message_count >= 3 {
                    while rx.try_recv().is_ok() {}
                    break;
                }
            }
        }

        let batch_id = format!("fanout-{}", thread_id);
        let threads = store.list_threads(Some(&batch_id), None, 10).await.unwrap();

        for thread in &threads {
            let msgs = store.get_thread_messages(&thread.thread_id).await.unwrap();
            let handoff = msgs.iter().find(|m| m.intent == "handoff").unwrap();
            assert!(
                handoff.body.starts_with("Review for correctness."),
                "handoff body should start with custom prompt, got: {}",
                &handoff.body[..handoff.body.len().min(100)]
            );
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Orphan PID Detection Tests (P0-1)
// ═══════════════════════════════════════════════════════════════════════════

mod orphan_pid_tests {
    use super::*;

    #[tokio::test]
    async fn test_set_execution_pid() {
        let store = test_store().await;
        store.ensure_thread("t-pid-1", None).await.unwrap();
        let exec_id = store.insert_execution("t-pid-1", "focused").await.unwrap();
        store.mark_execution_executing(&exec_id).await.unwrap();

        store.set_execution_pid(&exec_id, 12345).await.unwrap();

        let exec = store.get_execution(&exec_id).await.unwrap().unwrap();
        assert_eq!(exec.pid, Some(12345));
    }

    #[tokio::test]
    async fn test_get_orphaned_executions_with_pid() {
        let store = test_store().await;
        store.ensure_thread("t-pid-2", None).await.unwrap();
        let exec_id = store.insert_execution("t-pid-2", "focused").await.unwrap();
        let _ = store.claim_next_execution(10).await.unwrap();
        store.mark_execution_executing(&exec_id).await.unwrap();
        store.set_execution_pid(&exec_id, 54321).await.unwrap();

        let orphans = store.get_orphaned_executions_with_pid().await.unwrap();
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0].0, exec_id);
        assert_eq!(orphans[0].1, 54321);
    }

    #[tokio::test]
    async fn test_orphaned_without_pid_excluded() {
        let store = test_store().await;
        store.ensure_thread("t-pid-3", None).await.unwrap();
        let exec_id = store.insert_execution("t-pid-3", "focused").await.unwrap();
        let _ = store.claim_next_execution(10).await.unwrap();
        store.mark_execution_executing(&exec_id).await.unwrap();
        // No set_execution_pid — PID is NULL

        let orphans = store.get_orphaned_executions_with_pid().await.unwrap();
        assert!(
            orphans.is_empty(),
            "execution without PID should not appear in orphan query"
        );
    }

    #[tokio::test]
    async fn test_completed_execution_not_orphaned() {
        let store = test_store().await;
        store.ensure_thread("t-pid-4", None).await.unwrap();
        let exec_id = store.insert_execution("t-pid-4", "focused").await.unwrap();
        let _ = store.claim_next_execution(10).await.unwrap();
        store.mark_execution_executing(&exec_id).await.unwrap();
        store.set_execution_pid(&exec_id, 99999).await.unwrap();
        store
            .complete_execution(&exec_id, Some(0), None, None, 100)
            .await
            .unwrap();

        let orphans = store.get_orphaned_executions_with_pid().await.unwrap();
        assert!(
            orphans.is_empty(),
            "completed execution should not appear in orphan query"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// orch_read_log Tests (P0-2)
// ═══════════════════════════════════════════════════════════════════════════

mod read_log_tests {
    use super::*;

    fn config_with_state_dir(state_dir: PathBuf) -> OrchestratorConfig {
        OrchestratorConfig {
            target_repo_root: PathBuf::from("/tmp"),
            state_dir,
            poll_interval_secs: 1,
            models: None,
            agents: vec![AgentConfig {
                alias: "focused".to_string(),
                backend: "stub".to_string(),
                role: AgentRole::Worker,
                model: None,
                prompt: None,
                prompt_file: None,
                timeout_secs: None,
                backend_args: None,
                env: None,
                workdir: None,
                workspace: None,
                max_retries: 0,
                retry_backoff_secs: 30,
                handoff: None,
            }],
            worktree_dir: None,
            orchestration: OrchestrationConfig::default(),
            database: DatabaseConfig::default(),
            notifications: Default::default(),
        }
    }

    async fn server_with_state_dir(state_dir: PathBuf) -> (OrchestratorMcpServer, Store) {
        let store = test_store().await;
        let config = config_with_state_dir(state_dir);
        let mut registry = BackendRegistry::new();
        registry.register("stub", Arc::new(StubBackend { ping_alive: true }));
        let server = OrchestratorMcpServer::new(ConfigHandle::new(config), store.clone(), registry);
        (server, store)
    }

    #[tokio::test]
    async fn test_read_log_execution_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let (server, _store) = server_with_state_dir(tmp.path().to_path_buf()).await;

        let result = server
            .read_log_impl(ReadLogParams {
                execution_id: "nonexistent-exec".to_string(),
                offset: None,
                limit: None,
                tail: None,
            })
            .await
            .unwrap();

        assert!(is_error(&result), "expected error result");
    }

    #[tokio::test]
    async fn test_read_log_fallback_to_output_preview() {
        let tmp = tempfile::tempdir().unwrap();
        let (server, store) = server_with_state_dir(tmp.path().to_path_buf()).await;

        store.ensure_thread("t-log-1", None).await.unwrap();
        let exec_id = store.insert_execution("t-log-1", "focused").await.unwrap();
        store.mark_execution_executing(&exec_id).await.unwrap();
        store
            .complete_execution(&exec_id, Some(0), Some("line1\nline2\nline3"), None, 100)
            .await
            .unwrap();

        let result = server
            .read_log_impl(ReadLogParams {
                execution_id: exec_id.clone(),
                offset: None,
                limit: None,
                tail: None,
            })
            .await
            .unwrap();

        let v = extract_json(&result);
        assert_eq!(v["source"], "output_preview");
        assert_eq!(v["total_lines"], 3);
        assert_eq!(v["lines"][0], "line1");
        assert_eq!(v["lines"][2], "line3");
    }

    #[tokio::test]
    async fn test_read_log_from_file() {
        let tmp = tempfile::tempdir().unwrap();
        let log_dir = tmp.path().join("logs");
        std::fs::create_dir_all(&log_dir).unwrap();

        let (server, store) = server_with_state_dir(tmp.path().to_path_buf()).await;

        store.ensure_thread("t-log-2", None).await.unwrap();
        let exec_id = store.insert_execution("t-log-2", "focused").await.unwrap();

        let log_content = "hello world\nfoo bar\nbaz qux\nfinal line\n";
        std::fs::write(log_dir.join(format!("{}.log", exec_id)), log_content).unwrap();

        let result = server
            .read_log_impl(ReadLogParams {
                execution_id: exec_id.clone(),
                offset: None,
                limit: None,
                tail: None,
            })
            .await
            .unwrap();

        let v = extract_json(&result);
        assert_eq!(v["source"], "log_file");
        assert_eq!(v["total_lines"], 4);
        assert_eq!(v["returned_lines"], 4);
        assert_eq!(v["offset"], 0);
        assert_eq!(v["has_more"], false);
        assert_eq!(v["lines"][0], "hello world");
        assert_eq!(v["lines"][3], "final line");
    }

    #[tokio::test]
    async fn test_read_log_offset_limit() {
        let tmp = tempfile::tempdir().unwrap();
        let log_dir = tmp.path().join("logs");
        std::fs::create_dir_all(&log_dir).unwrap();

        let (server, store) = server_with_state_dir(tmp.path().to_path_buf()).await;

        store.ensure_thread("t-log-3", None).await.unwrap();
        let exec_id = store.insert_execution("t-log-3", "focused").await.unwrap();

        let lines: Vec<String> = (0..10).map(|i| format!("line-{}", i)).collect();
        std::fs::write(
            log_dir.join(format!("{}.log", exec_id)),
            lines.join("\n") + "\n",
        )
        .unwrap();

        let result = server
            .read_log_impl(ReadLogParams {
                execution_id: exec_id.clone(),
                offset: Some(3),
                limit: Some(4),
                tail: None,
            })
            .await
            .unwrap();

        let v = extract_json(&result);
        assert_eq!(v["total_lines"], 10);
        assert_eq!(v["returned_lines"], 4);
        assert_eq!(v["offset"], 3);
        assert_eq!(v["has_more"], true);
        assert_eq!(v["lines"][0], "line-3");
        assert_eq!(v["lines"][3], "line-6");
    }

    #[tokio::test]
    async fn test_read_log_tail_mode() {
        let tmp = tempfile::tempdir().unwrap();
        let log_dir = tmp.path().join("logs");
        std::fs::create_dir_all(&log_dir).unwrap();

        let (server, store) = server_with_state_dir(tmp.path().to_path_buf()).await;

        store.ensure_thread("t-log-4", None).await.unwrap();
        let exec_id = store.insert_execution("t-log-4", "focused").await.unwrap();

        let lines: Vec<String> = (0..10).map(|i| format!("line-{}", i)).collect();
        std::fs::write(
            log_dir.join(format!("{}.log", exec_id)),
            lines.join("\n") + "\n",
        )
        .unwrap();

        let result = server
            .read_log_impl(ReadLogParams {
                execution_id: exec_id.clone(),
                offset: None,
                limit: Some(3),
                tail: Some(true),
            })
            .await
            .unwrap();

        let v = extract_json(&result);
        assert_eq!(v["returned_lines"], 3);
        assert_eq!(v["offset"], 7);
        assert_eq!(v["has_more"], false);
        assert_eq!(v["lines"][0], "line-7");
        assert_eq!(v["lines"][2], "line-9");
    }

    #[tokio::test]
    async fn test_read_log_limit_clamping() {
        let tmp = tempfile::tempdir().unwrap();
        let log_dir = tmp.path().join("logs");
        std::fs::create_dir_all(&log_dir).unwrap();

        let (server, store) = server_with_state_dir(tmp.path().to_path_buf()).await;

        store.ensure_thread("t-log-5", None).await.unwrap();
        let exec_id = store.insert_execution("t-log-5", "focused").await.unwrap();

        let lines: Vec<String> = (0..1500).map(|i| format!("line-{}", i)).collect();
        std::fs::write(
            log_dir.join(format!("{}.log", exec_id)),
            lines.join("\n") + "\n",
        )
        .unwrap();

        let result = server
            .read_log_impl(ReadLogParams {
                execution_id: exec_id.clone(),
                offset: None,
                limit: Some(5000),
                tail: None,
            })
            .await
            .unwrap();

        let v = extract_json(&result);
        assert_eq!(v["returned_lines"], 1000);
        assert_eq!(v["total_lines"], 1500);
        assert_eq!(v["has_more"], true);
    }

    #[tokio::test]
    async fn test_read_log_empty_file() {
        let tmp = tempfile::tempdir().unwrap();
        let log_dir = tmp.path().join("logs");
        std::fs::create_dir_all(&log_dir).unwrap();

        let (server, store) = server_with_state_dir(tmp.path().to_path_buf()).await;

        store.ensure_thread("t-log-empty", None).await.unwrap();
        let exec_id = store
            .insert_execution("t-log-empty", "focused")
            .await
            .unwrap();

        std::fs::write(log_dir.join(format!("{}.log", exec_id)), "").unwrap();

        let result = server
            .read_log_impl(ReadLogParams {
                execution_id: exec_id.clone(),
                offset: None,
                limit: None,
                tail: None,
            })
            .await
            .unwrap();

        let v = extract_json(&result);
        assert_eq!(v["total_lines"], 0);
        assert_eq!(v["returned_lines"], 0);
        assert_eq!(v["has_more"], false);
        assert_eq!(v["lines"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn test_read_log_offset_beyond_eof() {
        let tmp = tempfile::tempdir().unwrap();
        let log_dir = tmp.path().join("logs");
        std::fs::create_dir_all(&log_dir).unwrap();

        let (server, store) = server_with_state_dir(tmp.path().to_path_buf()).await;

        store.ensure_thread("t-log-eof", None).await.unwrap();
        let exec_id = store
            .insert_execution("t-log-eof", "focused")
            .await
            .unwrap();

        std::fs::write(log_dir.join(format!("{}.log", exec_id)), "line-0\nline-1\n").unwrap();

        let result = server
            .read_log_impl(ReadLogParams {
                execution_id: exec_id.clone(),
                offset: Some(999),
                limit: None,
                tail: None,
            })
            .await
            .unwrap();

        let v = extract_json(&result);
        assert_eq!(v["total_lines"], 2);
        assert_eq!(v["returned_lines"], 0);
        assert_eq!(v["has_more"], false);
        assert_eq!(v["lines"], serde_json::json!([]));
    }
}
