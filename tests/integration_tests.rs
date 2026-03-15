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
            },
        ],
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

        let state_dir = tempfile::tempdir().unwrap();
        let worktree_manager = std::sync::Arc::new(WorktreeManager::new(state_dir.path()));

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
        )
        .await;

        assert!(output.success, "execution should succeed");

        // Verify worktree was created
        let wt_path = state_dir.path().join("worktrees").join("t-wt-1");
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
            .remove_worktree(repo_path, "t-wt-1")
            .unwrap();
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
        }];

        let mut registry = BackendRegistry::new();
        registry.register("stub", Arc::new(StubBackend { ping_alive: true }));
        let registry = Arc::new(registry);

        let state_dir = tempfile::tempdir().unwrap();
        let worktree_manager = std::sync::Arc::new(WorktreeManager::new(state_dir.path()));

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

        let worktree_manager = aster_orch::worktree::WorktreeManager::new(&config.state_dir);
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
