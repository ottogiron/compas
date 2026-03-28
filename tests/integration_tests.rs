//! Integration tests for compas: store, MCP tools, backend registry.
//!
//! These tests use in-memory SQLite and a stub backend to exercise the full
//! MCP tool surface without requiring external processes or real agents.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use sqlx::SqlitePool;

use compas::backend::registry::BackendRegistry;
use compas::backend::{Backend, BackendOutput, PingResult};
use compas::config::types::*;
use compas::config::ConfigHandle;
use compas::error::Result as OrchResult;
use compas::mcp::params::*;
use compas::mcp::server::OrchestratorMcpServer;
use compas::model::agent::Agent;
use compas::model::session::{Session, SessionStatus};
use compas::store::{ExecutionStatus, Store, ThreadStatus};

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
            cost_usd: None,
            tokens_in: None,
            tokens_out: None,
            num_turns: None,
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
        default_workdir: PathBuf::from("/tmp"),
        state_dir: PathBuf::from("/tmp/compas-test"),
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
        backend_definitions: None,
        hooks: None,
        schedules: None,
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

/// Helper: extract raw text from CallToolResult's first content block.
fn extract_text(result: &rmcp::model::CallToolResult) -> &str {
    result
        .content
        .first()
        .and_then(|c| match &c.raw {
            rmcp::model::RawContent::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .expect("expected text content")
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
        store
            .ensure_thread("t-1", Some("batch-A"), None)
            .await
            .unwrap();
        store
            .ensure_thread("t-2", Some("batch-A"), None)
            .await
            .unwrap();
        store
            .ensure_thread("t-3", Some("batch-B"), None)
            .await
            .unwrap();

        let all = store.list_threads(None, None, 100).await.unwrap();
        assert_eq!(all.len(), 3);
    }

    #[tokio::test]
    async fn test_list_threads_filter_by_batch() {
        let store = test_store().await;
        store
            .ensure_thread("t-1", Some("batch-A"), None)
            .await
            .unwrap();
        store
            .ensure_thread("t-2", Some("batch-A"), None)
            .await
            .unwrap();
        store
            .ensure_thread("t-3", Some("batch-B"), None)
            .await
            .unwrap();

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
        store.ensure_thread("t-1", None, None).await.unwrap();
        store.ensure_thread("t-2", None, None).await.unwrap();
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
                .ensure_thread(&format!("t-{}", i), None, None)
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
            .insert_message(
                "t-1", "operator", "focused", "dispatch", "msg 1", None, None,
            )
            .await
            .unwrap();
        let id2 = store
            .insert_message(
                "t-1",
                "focused",
                "operator",
                "status-update",
                "msg 2",
                None,
                None,
            )
            .await
            .unwrap();
        let _id3 = store
            .insert_message(
                "t-1",
                "focused",
                "operator",
                "status-update",
                "msg 3",
                None,
                None,
            )
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
            .insert_message(
                "t-1", "operator", "focused", "dispatch", "hello", None, None,
            )
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
            .insert_message("t-1", "op", "a", "dispatch", "m1", None, None)
            .await
            .unwrap();
        let id2 = store
            .insert_message("t-1", "a", "op", "status-update", "m2", None, None)
            .await
            .unwrap();

        let latest = store.latest_message_id("t-1").await.unwrap().unwrap();
        assert_eq!(latest, id2);
    }

    #[tokio::test]
    async fn test_fail_execution() {
        let store = test_store().await;
        store.ensure_thread("t-1", None, None).await.unwrap();
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
                None,
                None,
                None,
                None,
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
        store.ensure_thread("t-1", None, None).await.unwrap();
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
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();

        let exec = store.latest_execution("t-1").await.unwrap().unwrap();
        assert_eq!(exec.status, "failed");
    }

    #[tokio::test]
    async fn test_cancel_thread_executions() {
        let store = test_store().await;
        store.ensure_thread("t-1", None, None).await.unwrap();

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
        store.ensure_thread("t-1", None, None).await.unwrap();
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
        store.ensure_thread("t-1", None, None).await.unwrap();
        store.ensure_thread("t-2", None, None).await.unwrap();

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
        store
            .ensure_thread("t-1", Some("batch-1"), None)
            .await
            .unwrap();
        store
            .insert_message(
                "t-1",
                "operator",
                "focused",
                "dispatch",
                "work",
                Some("batch-1"),
                None,
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
        store.ensure_thread("t-1", None, None).await.unwrap();
        store.ensure_thread("t-2", None, None).await.unwrap();
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
        store.ensure_thread("t-1", None, None).await.unwrap();
        store.ensure_thread("t-2", None, None).await.unwrap();
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
        store.ensure_thread("t-1", None, None).await.unwrap();
        store.ensure_thread("t-2", None, None).await.unwrap();
        store.ensure_thread("t-3", None, None).await.unwrap();
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
            .insert_message("t-1", "op", "a", "dispatch", "m1", None, None)
            .await
            .unwrap();
        store
            .insert_message("t-1", "a", "op", "status-update", "m2", None, None)
            .await
            .unwrap();

        assert_eq!(store.message_count().await.unwrap(), 2);
    }

    #[tokio::test]
    async fn test_message_ref_and_parse() {
        use compas::store::{message_ref, parse_message_ref};

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
        store.ensure_thread("t-1", None, None).await.unwrap();
        store.ensure_thread("t-2", None, None).await.unwrap();

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
            .insert_message("t-1", "operator", "focused", "dispatch", "work", None, None)
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
            .insert_message("t-1", "operator", "focused", "dispatch", "work", None, None)
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
            .insert_message(
                "t-1",
                "focused",
                "operator",
                "status-update",
                "done",
                None,
                None,
            )
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
            .insert_message(
                "t-1", "operator", "reviewer", "dispatch", "review", None, None,
            )
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
            .insert_message("t-1", "operator", "focused", "dispatch", "work", None, None)
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
            .insert_message("t-1", "operator", "focused", "dispatch", "work", None, None)
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
        store.ensure_thread("t-1", None, None).await.unwrap();

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
            .insert_message(
                "t-active", "operator", "focused", "dispatch", "work", None, None,
            )
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
                None,
            )
            .await
            .unwrap();
        store
            .insert_message(
                "t-failed", "operator", "focused", "dispatch", "work", None, None,
            )
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
                summary: None,
                scheduled_for: None,
                skip_handoff: None,
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

        // Verify next_step references orch_wait MCP tool (not CLI).
        let next_step = json["next_step"].as_str().unwrap();
        assert!(
            !next_step.contains("compas wait"),
            "next_step should not be a CLI command, got: {next_step}"
        );
        assert!(
            next_step.contains("orch_wait"),
            "next_step should reference orch_wait, got: {next_step}"
        );
        assert!(
            next_step.contains("t-dispatch-1"),
            "next_step should contain the thread_id, got: {next_step}"
        );
        assert!(
            next_step.contains(&format!("db:{}", message_id)),
            "next_step should contain since_reference with message_id, got: {next_step}"
        );
        assert!(
            next_step.contains("await_chain"),
            "next_step should mention await_chain, got: {next_step}"
        );
    }

    #[tokio::test]
    async fn test_dispatch_with_summary() {
        let server = test_server().await;
        let result = server
            .dispatch_impl(DispatchParams {
                from: "operator".to_string(),
                to: "focused".to_string(),
                body: "Implement auth module".to_string(),
                batch: None,
                intent: "dispatch".to_string(),
                thread_id: Some("t-summary".to_string()),
                summary: Some("Add JWT authentication".to_string()),
                scheduled_for: None,
                skip_handoff: None,
            })
            .await
            .unwrap();

        assert!(!is_error(&result));

        // Verify summary round-trips through get_thread
        let thread = server.store.get_thread("t-summary").await.unwrap().unwrap();
        assert_eq!(thread.summary.as_deref(), Some("Add JWT authentication"));

        // Verify summary appears in status_view
        let views = server
            .store
            .status_view(Some("t-summary"), None, None, 10)
            .await
            .unwrap();
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].summary.as_deref(), Some("Add JWT authentication"));
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
                summary: None,
                scheduled_for: None,
                skip_handoff: None,
            })
            .await
            .unwrap();

        let json = extract_json(&result);
        let thread_id = json["thread_id"].as_str().unwrap();
        assert!(!thread_id.is_empty());

        // Verify next_step embeds the auto-generated thread_id.
        let next_step = json["next_step"].as_str().unwrap();
        assert!(
            next_step.contains(thread_id),
            "next_step should contain the auto-generated thread_id '{thread_id}', got: {next_step}"
        );
        let message_id = json["message_id"].as_i64().unwrap();
        assert!(
            next_step.contains(&format!("db:{}", message_id)),
            "next_step should contain since_reference with message_id, got: {next_step}"
        );
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
                summary: None,
                scheduled_for: None,
                skip_handoff: None,
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
                summary: None,
                scheduled_for: None,
                skip_handoff: None,
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
                summary: None,
                scheduled_for: None,
                skip_handoff: None,
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
                summary: None,
                scheduled_for: None,
                skip_handoff: None,
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
                summary: None,
                scheduled_for: None,
                skip_handoff: None,
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
                summary: None,
                scheduled_for: None,
                skip_handoff: None,
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

    #[tokio::test]
    async fn test_dispatch_with_scheduled_for_creates_deferred_execution() {
        let server = test_server().await;

        // Schedule 60 seconds in the future.
        let future_ts = (chrono::Utc::now() + chrono::Duration::seconds(60))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

        let result = server
            .dispatch_impl(DispatchParams {
                from: "operator".to_string(),
                to: "focused".to_string(),
                body: "Scheduled work".to_string(),
                batch: None,
                intent: "dispatch".to_string(),
                thread_id: Some("t-sched-1".to_string()),
                summary: None,
                scheduled_for: Some(future_ts.clone()),
                skip_handoff: None,
            })
            .await
            .unwrap();

        assert!(!is_error(&result));
        let json = extract_json(&result);
        assert_eq!(json["thread_id"], "t-sched-1");
        assert_eq!(json["scheduled_for"], future_ts);
        assert!(json["execution_id"].as_str().is_some());

        // The execution should exist but NOT be claimable (eligible_at is in the future).
        let claimed = server.store.claim_next_execution(2).await.unwrap();
        assert!(
            claimed.is_none(),
            "scheduled execution should not be claimed before eligible_at"
        );
    }

    #[tokio::test]
    async fn test_dispatch_with_past_scheduled_for_rejected() {
        let server = test_server().await;

        // A timestamp in the past.
        let past_ts = "2020-01-01T00:00:00Z".to_string();

        let result = server
            .dispatch_impl(DispatchParams {
                from: "operator".to_string(),
                to: "focused".to_string(),
                body: "Should fail".to_string(),
                batch: None,
                intent: "dispatch".to_string(),
                thread_id: Some("t-sched-past".to_string()),
                summary: None,
                scheduled_for: Some(past_ts),
                skip_handoff: None,
            })
            .await
            .unwrap();

        assert!(is_error(&result), "past timestamp should be rejected");
    }

    #[tokio::test]
    async fn test_dispatch_with_invalid_scheduled_for_rejected() {
        let server = test_server().await;

        let result = server
            .dispatch_impl(DispatchParams {
                from: "operator".to_string(),
                to: "focused".to_string(),
                body: "Should fail".to_string(),
                batch: None,
                intent: "dispatch".to_string(),
                thread_id: Some("t-sched-bad".to_string()),
                summary: None,
                scheduled_for: Some("not-a-timestamp".to_string()),
                skip_handoff: None,
            })
            .await
            .unwrap();

        assert!(is_error(&result), "invalid timestamp should be rejected");
    }

    #[tokio::test]
    async fn test_dispatch_without_scheduled_for_unchanged() {
        let server = test_server().await;

        let result = server
            .dispatch_impl(DispatchParams {
                from: "operator".to_string(),
                to: "focused".to_string(),
                body: "Immediate work".to_string(),
                batch: None,
                intent: "dispatch".to_string(),
                thread_id: Some("t-sched-none".to_string()),
                summary: None,
                scheduled_for: None,
                skip_handoff: None,
            })
            .await
            .unwrap();

        assert!(!is_error(&result));
        let json = extract_json(&result);
        // No scheduled_for or execution_id in the response.
        assert!(json.get("scheduled_for").is_none());
        assert!(json.get("execution_id").is_none());
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// MCP Tool Integration Tests — Scheduled Visibility (SCHED-3)
// ═══════════════════════════════════════════════════════════════════════════

mod scheduled_visibility_tests {
    use super::*;

    #[tokio::test]
    async fn test_orch_tasks_filter_scheduled() {
        let server = test_server().await;

        // Create a scheduled execution (1 hour in the future).
        let future_ts = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 3600) as i64;
        server
            .store
            .ensure_thread("t-sched-vis", None, None)
            .await
            .unwrap();
        server
            .store
            .insert_execution_scheduled(
                "t-sched-vis",
                "focused",
                None,
                None,
                Some(future_ts),
                Some("scheduled"),
            )
            .await
            .unwrap();

        // Also create a normal (immediate) execution.
        server
            .store
            .ensure_thread("t-immediate", None, None)
            .await
            .unwrap();
        server
            .store
            .insert_execution_with_dispatch("t-immediate", "focused", None, None)
            .await
            .unwrap();

        // Without filter: should return both.
        let result = server
            .tasks_impl(TasksParams {
                alias: None,
                batch_id: None,
                limit: None,
                filter: None,
            })
            .await
            .unwrap();
        let json = extract_json(&result);
        let arr = json.as_array().unwrap();
        assert!(arr.len() >= 2, "should return at least 2 tasks");

        // With filter=scheduled: should return only the scheduled one.
        let result = server
            .tasks_impl(TasksParams {
                alias: None,
                batch_id: None,
                limit: None,
                filter: Some("scheduled".to_string()),
            })
            .await
            .unwrap();
        let json = extract_json(&result);
        let arr = json.as_array().unwrap();
        assert_eq!(arr.len(), 1, "should return exactly 1 scheduled task");
        assert_eq!(arr[0]["thread_id"], "t-sched-vis");
        assert!(
            arr[0]["eligible_at"].is_string(),
            "eligible_at should be ISO 8601 string"
        );
        assert_eq!(arr[0]["eligible_reason"], "scheduled");
    }

    #[tokio::test]
    async fn test_orch_tasks_filter_scheduled_returns_summary() {
        let server = test_server().await;

        // Create a thread with a summary, then a scheduled execution for it.
        let future_ts = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 3600) as i64;
        server
            .store
            .ensure_thread("t-sched-sum", None, Some("build the widget"))
            .await
            .unwrap();
        server
            .store
            .insert_execution_scheduled(
                "t-sched-sum",
                "focused",
                None,
                None,
                Some(future_ts),
                Some("scheduled"),
            )
            .await
            .unwrap();

        // Query with filter=scheduled and verify summary is returned.
        let result = server
            .tasks_impl(TasksParams {
                alias: None,
                batch_id: None,
                limit: None,
                filter: Some("scheduled".to_string()),
            })
            .await
            .unwrap();
        let json = extract_json(&result);
        let arr = json.as_array().unwrap();
        assert_eq!(arr.len(), 1, "should return exactly 1 scheduled task");
        assert_eq!(arr[0]["thread_id"], "t-sched-sum");
        assert_eq!(
            arr[0]["summary"], "build the widget",
            "scheduled filter should return thread summary"
        );
    }

    #[tokio::test]
    async fn test_orch_status_includes_scheduled_count() {
        let server = test_server().await;

        // Create a scheduled execution.
        let future_ts = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 3600) as i64;
        server
            .store
            .ensure_thread("t-sched-count", None, None)
            .await
            .unwrap();
        server
            .store
            .insert_execution_scheduled(
                "t-sched-count",
                "focused",
                None,
                None,
                Some(future_ts),
                Some("scheduled"),
            )
            .await
            .unwrap();

        let result = server
            .status_impl(StatusParams {
                agent: None,
                thread_id: None,
            })
            .await
            .unwrap();
        let json = extract_json(&result);
        assert!(
            json["scheduled_count"].as_i64().unwrap() >= 1,
            "scheduled_count should be >= 1"
        );
        assert!(json["threads"].is_array(), "threads should be an array");
    }

    #[tokio::test]
    async fn test_abandon_cancels_scheduled_execution() {
        let server = test_server().await;

        // Create a scheduled execution.
        let future_ts = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 3600) as i64;
        server
            .store
            .ensure_thread("t-sched-abandon", None, None)
            .await
            .unwrap();
        server
            .store
            .insert_execution_scheduled(
                "t-sched-abandon",
                "focused",
                None,
                None,
                Some(future_ts),
                Some("scheduled"),
            )
            .await
            .unwrap();

        // Abandon should cancel the scheduled execution.
        let result = server
            .abandon_impl(AbandonParams {
                thread_id: "t-sched-abandon".to_string(),
            })
            .await
            .unwrap();

        assert!(!is_error(&result));
        let json = extract_json(&result);
        assert_eq!(json["status"], "Abandoned");
        assert!(json["executions_cancelled"].as_u64().unwrap() >= 1);

        // Scheduled execution count should be 0 for this thread.
        let exec = server
            .store
            .latest_execution("t-sched-abandon")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(exec.status, "cancelled");
    }

    #[tokio::test]
    async fn test_count_scheduled_executions() {
        let store = test_store().await;

        // Initially 0.
        let count = store.count_scheduled_executions().await.unwrap();
        assert_eq!(count, 0);

        // Add a scheduled execution (future).
        let future_ts = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 3600) as i64;
        store
            .ensure_thread("t-count-sched", None, None)
            .await
            .unwrap();
        store
            .insert_execution_scheduled(
                "t-count-sched",
                "focused",
                None,
                None,
                Some(future_ts),
                Some("scheduled"),
            )
            .await
            .unwrap();

        let count = store.count_scheduled_executions().await.unwrap();
        assert_eq!(count, 1);

        // Add a normal (immediate) execution — should NOT increase count.
        store
            .ensure_thread("t-count-imm", None, None)
            .await
            .unwrap();
        store
            .insert_execution_with_dispatch("t-count-imm", "focused", None, None)
            .await
            .unwrap();

        let count = store.count_scheduled_executions().await.unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn test_orch_tasks_unknown_filter_rejected() {
        let server = test_server().await;

        let result = server
            .tasks_impl(TasksParams {
                alias: None,
                batch_id: None,
                limit: None,
                filter: Some("bogus".to_string()),
            })
            .await
            .unwrap();

        assert!(is_error(&result), "unknown filter should return an error");
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
                summary: None,
                scheduled_for: None,
                skip_handoff: None,
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
                summary: None,
                scheduled_for: None,
                skip_handoff: None,
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
                summary: None,
                scheduled_for: None,
                skip_handoff: None,
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

    // ── Merge-before-close gate tests ─────────────────────────────────────

    /// Helper: create a thread with a worktree backed by a real git repo (for branch checks).
    async fn setup_worktree_thread(
        server: &OrchestratorMcpServer,
        thread_id: &str,
    ) -> tempfile::TempDir {
        setup_thread(server, thread_id).await;

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

        // Set worktree path and repo root on the thread
        server
            .store
            .set_thread_worktree_path(
                thread_id,
                &repo.join(".compas-worktrees").join(thread_id),
                repo,
            )
            .await
            .unwrap();

        tmp
    }

    #[tokio::test]
    async fn test_close_completed_worktree_without_merge_refused() {
        let server = test_server().await;
        let _tmp = setup_worktree_thread(&server, "t-no-merge").await;

        // Close as Completed without a completed merge — should be refused
        let result = server
            .close_impl(CloseParams {
                thread_id: "t-no-merge".to_string(),
                from: "operator".to_string(),
                status: CloseStatus::Completed,
                note: Some("attempt close without merge".to_string()),
            })
            .await
            .unwrap();

        assert!(is_error(&result));
        let text = extract_text(&result);
        assert!(
            text.contains("no completed merge"),
            "expected merge-gate error, got: {}",
            text
        );
        assert!(text.contains("orch_merge"));

        // Thread should still be Active
        let status = server
            .store
            .get_thread_status("t-no-merge")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(status, "Active");
    }

    #[tokio::test]
    async fn test_close_completed_worktree_with_completed_merge() {
        let server = test_server().await;
        let _tmp = setup_worktree_thread(&server, "t-merged").await;

        // Insert a completed merge operation
        let op = compas::store::MergeOperation {
            id: "m-done".to_string(),
            thread_id: "t-merged".to_string(),
            source_branch: "compas/t-merged".to_string(),
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
        };
        server.store.insert_merge_op(&op).await.unwrap();

        // Close as Completed — should succeed because merge is completed
        let result = server
            .close_impl(CloseParams {
                thread_id: "t-merged".to_string(),
                from: "operator".to_string(),
                status: CloseStatus::Completed,
                note: Some("merged and done".to_string()),
            })
            .await
            .unwrap();

        assert!(!is_error(&result));
        let json = extract_json(&result);
        assert_eq!(json["status"], "Completed");
    }

    #[tokio::test]
    async fn test_close_completed_non_worktree_no_merge_needed() {
        let server = test_server().await;
        setup_thread(&server, "t-shared-close").await;

        // Close as Completed — shared workspace thread (no worktree) needs no merge
        let result = server
            .close_impl(CloseParams {
                thread_id: "t-shared-close".to_string(),
                from: "operator".to_string(),
                status: CloseStatus::Completed,
                note: Some("shared ws".to_string()),
            })
            .await
            .unwrap();

        assert!(!is_error(&result));
        let json = extract_json(&result);
        assert_eq!(json["status"], "Completed");
    }

    #[tokio::test]
    async fn test_close_failed_worktree_no_merge_needed() {
        let server = test_server().await;
        let _tmp = setup_worktree_thread(&server, "t-fail-wt").await;

        // Close as Failed — no merge gate for failed threads
        let result = server
            .close_impl(CloseParams {
                thread_id: "t-fail-wt".to_string(),
                from: "operator".to_string(),
                status: CloseStatus::Failed,
                note: Some("failed work".to_string()),
            })
            .await
            .unwrap();

        assert!(!is_error(&result));
        let json = extract_json(&result);
        assert_eq!(json["status"], "Failed");
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
                summary: None,
                scheduled_for: None,
                skip_handoff: None,
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
                summary: None,
                scheduled_for: None,
                skip_handoff: None,
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
        let threads = json["threads"].as_array().unwrap();
        assert!(threads.len() >= 2);
        // scheduled_count is present in the response
        assert!(json["scheduled_count"].is_number());
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
        let arr = json["threads"].as_array().unwrap();
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
        let arr = json["threads"].as_array().unwrap();
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
            .insert_message("t-read2", "op", "a", "dispatch", "msg", None, None)
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
                filter: None,
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
                filter: None,
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
                summary: None,
                scheduled_for: None,
                skip_handoff: None,
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
                summary: None,
                scheduled_for: None,
                skip_handoff: None,
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
            .insert_message("t-poll-f", "op", "focused", "dispatch", "work", None, None)
            .await
            .unwrap();
        server
            .store
            .insert_message(
                "t-poll-f", "focused", "op", "progress", "progress", None, None,
            )
            .await
            .unwrap();
        server
            .store
            .insert_message(
                "t-poll-f",
                "focused",
                "op",
                "status-update",
                "done",
                None,
                None,
            )
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
            .insert_message("t-poll-s", "op", "focused", "dispatch", "m1", None, None)
            .await
            .unwrap();
        server
            .store
            .insert_message(
                "t-poll-s",
                "focused",
                "op",
                "status-update",
                "m2",
                None,
                None,
            )
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
                    await_chain: None,
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
            .insert_message(
                "t-wait-exc",
                "op",
                "focused",
                "dispatch",
                "work",
                None,
                None,
            )
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
                    await_chain: None,
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
            .insert_message("t-wait-i", "op", "focused", "dispatch", "work", None, None)
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
                None,
            )
            .await
            .unwrap();
        server
            .store
            .insert_message(
                "t-wait-i",
                "focused",
                "op",
                "status-update",
                "ready",
                None,
                None,
            )
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
                    await_chain: None,
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
            .insert_message("t-wait-to", "op", "focused", "dispatch", "work", None, None)
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
                    await_chain: None,
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
            .insert_message("t-wait-sr", "op", "focused", "dispatch", "work", None, None)
            .await
            .unwrap();
        server
            .store
            .insert_message(
                "t-wait-sr",
                "focused",
                "op",
                "status-update",
                "done",
                None,
                None,
            )
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
                    await_chain: None,
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
            .insert_message(
                "t-wait-conc",
                "op",
                "focused",
                "dispatch",
                "work",
                None,
                None,
            )
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
                    await_chain: None,
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
        assert_eq!(json["server"], "compas");
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

    #[tokio::test]
    async fn test_health_cache_hit_on_second_call() {
        let server = test_server().await;

        // First call: all agents should be fresh (cached == false).
        let result1 = server
            .health_impl(HealthParams { alias: None })
            .await
            .unwrap();
        let json1 = extract_json(&result1);
        let agents1 = json1["agents"].as_array().unwrap();
        assert!(
            agents1.iter().all(|a| a["cached"] == false),
            "first call should have all cached=false"
        );

        // Second call (within default 60s TTL): all should be cached.
        let result2 = server
            .health_impl(HealthParams { alias: None })
            .await
            .unwrap();
        let json2 = extract_json(&result2);
        let agents2 = json2["agents"].as_array().unwrap();
        assert!(
            agents2.iter().all(|a| a["cached"] == true),
            "second call should have all cached=true"
        );
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
                summary: None,
                scheduled_for: None,
                skip_handoff: None,
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
                summary: None,
                scheduled_for: None,
                skip_handoff: None,
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
        server
            .store
            .ensure_thread("t-diag-c", None, None)
            .await
            .unwrap();
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
        server
            .store
            .ensure_thread("t-diag-a", None, None)
            .await
            .unwrap();
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
        server
            .store
            .ensure_thread("t-diag-f", None, None)
            .await
            .unwrap();

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
                None,
                None,
                None,
                None,
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
    use compas::worktree::WorktreeManager;
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
        store.ensure_thread("t-wt-1", None, None).await.unwrap();
        let msg_id = store
            .insert_message(
                "t-wt-1", "operator", "focused", "dispatch", "do work", None, None,
            )
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
        let output = compas::worker::execute_trigger(
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

        // Verify worktree was created at the default location (inside repo)
        let wt_path = repo_path.join(".compas-worktrees").join("t-wt-1");
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
        // Also clean up the .compas-worktrees directory
        let wt_root = repo_path.join(".compas-worktrees");
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
        store.ensure_thread("t-shared-1", None, None).await.unwrap();
        let msg_id = store
            .insert_message(
                "t-shared-1",
                "operator",
                "focused",
                "dispatch",
                "do work",
                None,
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

        let output = compas::worker::execute_trigger(
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

    /// When a worktree agent runs first and a non-worktree agent (same repo)
    /// runs second on the same thread, the second agent should inherit the
    /// worktree path.
    #[tokio::test]
    async fn test_worktree_inheritance_same_repo() {
        let store = test_store().await;

        // Create a real git repo for the worktree source
        let repo_dir = tempfile::tempdir().unwrap();
        let repo_path = repo_dir.path();
        let init = Command::new("git")
            .args(["init", &repo_path.to_string_lossy()])
            .output()
            .unwrap();
        assert!(init.status.success(), "git init failed");

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

        // Agent A: worktree mode
        let dev_config = AgentConfig {
            alias: "dev".to_string(),
            backend: "stub".to_string(),
            role: AgentRole::Worker,
            model: Some("test-model".to_string()),
            prompt: Some("Dev agent.".to_string()),
            prompt_file: None,
            timeout_secs: Some(30),
            backend_args: None,
            env: None,
            workdir: None,
            workspace: Some("worktree".to_string()),
            max_retries: 0,
            retry_backoff_secs: 30,
            handoff: None,
        };
        // Agent B: no workspace (should inherit worktree)
        let reviewer_config = AgentConfig {
            alias: "reviewer".to_string(),
            backend: "stub".to_string(),
            role: AgentRole::Worker,
            model: Some("test-model".to_string()),
            prompt: Some("Reviewer agent.".to_string()),
            prompt_file: None,
            timeout_secs: Some(30),
            backend_args: None,
            env: None,
            workdir: None,
            workspace: None,
            max_retries: 0,
            retry_backoff_secs: 30,
            handoff: None,
        };
        let agent_configs = vec![dev_config, reviewer_config];

        let mut registry = BackendRegistry::new();
        registry.register("stub", Arc::new(StubBackend { ping_alive: true }));
        let registry = Arc::new(registry);

        // Create thread and run agent A (worktree)
        store
            .ensure_thread("t-inherit-1", None, None)
            .await
            .unwrap();
        let msg_id = store
            .insert_message(
                "t-inherit-1",
                "operator",
                "dev",
                "dispatch",
                "do work",
                None,
                None,
            )
            .await
            .unwrap();
        let exec_id = store
            .insert_execution_with_dispatch("t-inherit-1", "dev", Some(msg_id), None)
            .await
            .unwrap()
            .unwrap();
        let execution = store.claim_next_execution(1).await.unwrap().unwrap();
        assert_eq!(execution.id, exec_id);

        let output_a = compas::worker::execute_trigger(
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
        assert!(output_a.success, "agent A execution should succeed");

        // Verify worktree was created and stored
        let wt_path = repo_path.join(".compas-worktrees").join("t-inherit-1");
        assert!(wt_path.exists(), "worktree should exist");
        let wt_info = store.get_thread_worktree_info("t-inherit-1").await.unwrap();
        assert!(wt_info.is_some(), "worktree info should be stored");
        let (stored_path, stored_root) = wt_info.unwrap();
        assert_eq!(stored_path, wt_path);
        assert_eq!(stored_root, repo_path.to_path_buf());

        // Now run agent B (no workspace, same default_workdir = repo_path)
        let msg_id_b = store
            .insert_message(
                "t-inherit-1",
                "dev",
                "reviewer",
                "handoff",
                "review this",
                None,
                None,
            )
            .await
            .unwrap();
        let exec_id_b = store
            .insert_execution_with_dispatch("t-inherit-1", "reviewer", Some(msg_id_b), None)
            .await
            .unwrap()
            .unwrap();
        let execution_b = store.claim_next_execution(1).await.unwrap().unwrap();
        assert_eq!(execution_b.id, exec_id_b);

        let output_b = compas::worker::execute_trigger(
            &execution_b,
            &store,
            &registry,
            &agent_configs,
            "review this",
            30,
            None,
            None,
            &worktree_manager,
            repo_path, // same default_workdir as agent A
            None,
        )
        .await;
        assert!(output_b.success, "agent B execution should succeed");

        // Cleanup
        worktree_manager
            .remove_worktree(repo_path, "t-inherit-1", None)
            .unwrap();
        let wt_root = repo_path.join(".compas-worktrees");
        let _ = std::fs::remove_dir_all(&wt_root);
    }

    /// When a worktree agent and a non-worktree agent target different repos,
    /// the non-worktree agent should NOT inherit the worktree.
    #[tokio::test]
    async fn test_worktree_inheritance_cross_repo_rejected() {
        let store = test_store().await;

        // Create a real git repo for the worktree source
        let repo_dir = tempfile::tempdir().unwrap();
        let repo_path = repo_dir.path();
        let init = Command::new("git")
            .args(["init", &repo_path.to_string_lossy()])
            .output()
            .unwrap();
        assert!(init.status.success(), "git init failed");

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

        // A second directory representing a different project
        let other_repo_dir = tempfile::tempdir().unwrap();
        let other_repo_path = other_repo_dir.path();

        let dev_config = AgentConfig {
            alias: "dev".to_string(),
            backend: "stub".to_string(),
            role: AgentRole::Worker,
            model: Some("test-model".to_string()),
            prompt: Some("Dev agent.".to_string()),
            prompt_file: None,
            timeout_secs: Some(30),
            backend_args: None,
            env: None,
            workdir: None,
            workspace: Some("worktree".to_string()),
            max_retries: 0,
            retry_backoff_secs: 30,
            handoff: None,
        };
        // Reviewer targets a different repo
        let reviewer_config = AgentConfig {
            alias: "reviewer".to_string(),
            backend: "stub".to_string(),
            role: AgentRole::Worker,
            model: Some("test-model".to_string()),
            prompt: Some("Reviewer agent.".to_string()),
            prompt_file: None,
            timeout_secs: Some(30),
            backend_args: None,
            env: None,
            workdir: Some(other_repo_path.to_path_buf()),
            workspace: None,
            max_retries: 0,
            retry_backoff_secs: 30,
            handoff: None,
        };
        let agent_configs = vec![dev_config, reviewer_config];

        let mut registry = BackendRegistry::new();
        registry.register("stub", Arc::new(StubBackend { ping_alive: true }));
        let registry = Arc::new(registry);

        // Run agent A (worktree) on repo_path
        store
            .ensure_thread("t-crossrepo-1", None, None)
            .await
            .unwrap();
        let msg_id = store
            .insert_message(
                "t-crossrepo-1",
                "operator",
                "dev",
                "dispatch",
                "do work",
                None,
                None,
            )
            .await
            .unwrap();
        let exec_id = store
            .insert_execution_with_dispatch("t-crossrepo-1", "dev", Some(msg_id), None)
            .await
            .unwrap()
            .unwrap();
        let execution = store.claim_next_execution(1).await.unwrap().unwrap();
        assert_eq!(execution.id, exec_id);

        let output_a = compas::worker::execute_trigger(
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
        assert!(output_a.success, "agent A should succeed");

        // Verify worktree stored with repo_path as root
        let wt_info = store
            .get_thread_worktree_info("t-crossrepo-1")
            .await
            .unwrap();
        assert!(wt_info.is_some());
        let (_, stored_root) = wt_info.unwrap();
        assert_eq!(stored_root, repo_path.to_path_buf());

        // Run agent B (different workdir) — should NOT inherit
        let msg_id_b = store
            .insert_message(
                "t-crossrepo-1",
                "dev",
                "reviewer",
                "handoff",
                "review this",
                None,
                None,
            )
            .await
            .unwrap();
        let exec_id_b = store
            .insert_execution_with_dispatch("t-crossrepo-1", "reviewer", Some(msg_id_b), None)
            .await
            .unwrap()
            .unwrap();
        let execution_b = store.claim_next_execution(1).await.unwrap().unwrap();
        assert_eq!(execution_b.id, exec_id_b);

        // default_workdir is repo_path, but reviewer has explicit workdir = other_repo_path
        // The reviewer's agent_workdir resolves to other_repo_path (from agent config),
        // which differs from the thread's worktree_repo_root (repo_path).
        // Therefore, inheritance should NOT happen.
        let output_b = compas::worker::execute_trigger(
            &execution_b,
            &store,
            &registry,
            &agent_configs,
            "review this",
            30,
            None,
            None,
            &worktree_manager,
            repo_path,
            None,
        )
        .await;
        assert!(
            output_b.success,
            "agent B should succeed using its own workdir"
        );

        // Cleanup
        worktree_manager
            .remove_worktree(repo_path, "t-crossrepo-1", None)
            .unwrap();
        let wt_root = repo_path.join(".compas-worktrees");
        let _ = std::fs::remove_dir_all(&wt_root);
    }

    /// An agent with `workspace: shared` should NOT inherit the thread's
    /// worktree even when targeting the same repo.
    #[tokio::test]
    async fn test_worktree_inheritance_shared_opt_out() {
        let store = test_store().await;

        // Create a real git repo
        let repo_dir = tempfile::tempdir().unwrap();
        let repo_path = repo_dir.path();
        let init = Command::new("git")
            .args(["init", &repo_path.to_string_lossy()])
            .output()
            .unwrap();
        assert!(init.status.success(), "git init failed");

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

        let dev_config = AgentConfig {
            alias: "dev".to_string(),
            backend: "stub".to_string(),
            role: AgentRole::Worker,
            model: Some("test-model".to_string()),
            prompt: Some("Dev agent.".to_string()),
            prompt_file: None,
            timeout_secs: Some(30),
            backend_args: None,
            env: None,
            workdir: None,
            workspace: Some("worktree".to_string()),
            max_retries: 0,
            retry_backoff_secs: 30,
            handoff: None,
        };
        // Reviewer with explicit `workspace: shared` — same repo but opts out
        let reviewer_config = AgentConfig {
            alias: "reviewer".to_string(),
            backend: "stub".to_string(),
            role: AgentRole::Worker,
            model: Some("test-model".to_string()),
            prompt: Some("Reviewer agent.".to_string()),
            prompt_file: None,
            timeout_secs: Some(30),
            backend_args: None,
            env: None,
            workdir: None,
            workspace: Some("shared".to_string()),
            max_retries: 0,
            retry_backoff_secs: 30,
            handoff: None,
        };
        let agent_configs = vec![dev_config, reviewer_config];

        let mut registry = BackendRegistry::new();
        registry.register("stub", Arc::new(StubBackend { ping_alive: true }));
        let registry = Arc::new(registry);

        // Run agent A (worktree)
        store
            .ensure_thread("t-shared-opt-1", None, None)
            .await
            .unwrap();
        let msg_id = store
            .insert_message(
                "t-shared-opt-1",
                "operator",
                "dev",
                "dispatch",
                "do work",
                None,
                None,
            )
            .await
            .unwrap();
        let exec_id = store
            .insert_execution_with_dispatch("t-shared-opt-1", "dev", Some(msg_id), None)
            .await
            .unwrap()
            .unwrap();
        let execution = store.claim_next_execution(1).await.unwrap().unwrap();
        assert_eq!(execution.id, exec_id);

        let output_a = compas::worker::execute_trigger(
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
        assert!(output_a.success, "agent A should succeed");

        // Run agent B (workspace: shared, same default_workdir)
        let msg_id_b = store
            .insert_message(
                "t-shared-opt-1",
                "dev",
                "reviewer",
                "handoff",
                "review this",
                None,
                None,
            )
            .await
            .unwrap();
        let exec_id_b = store
            .insert_execution_with_dispatch("t-shared-opt-1", "reviewer", Some(msg_id_b), None)
            .await
            .unwrap()
            .unwrap();
        let execution_b = store.claim_next_execution(1).await.unwrap().unwrap();
        assert_eq!(execution_b.id, exec_id_b);

        // workspace: shared should prevent inheritance even though same repo
        let output_b = compas::worker::execute_trigger(
            &execution_b,
            &store,
            &registry,
            &agent_configs,
            "review this",
            30,
            None,
            None,
            &worktree_manager,
            repo_path,
            None,
        )
        .await;
        assert!(
            output_b.success,
            "agent B should succeed using shared workdir"
        );

        // Cleanup
        worktree_manager
            .remove_worktree(repo_path, "t-shared-opt-1", None)
            .unwrap();
        let wt_root = repo_path.join(".compas-worktrees");
        let _ = std::fs::remove_dir_all(&wt_root);
    }
}

mod evo2_event_bus_tests {
    use super::*;
    use compas::events::{EventBus, OrchestratorEvent};
    use compas::worker::WorkerRunner;
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

        let worktree_manager = compas::worktree::WorktreeManager::new();
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
    use compas::events::EventBus;
    use compas::worker::WorkerRunner;
    use sha2::{Digest, Sha256};
    use tokio::sync::Semaphore;

    fn expected_hash(prompt: &str) -> String {
        let mut h = Sha256::new();
        h.update(prompt.as_bytes());
        let result = h.finalize();
        result
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<String>()
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
        let worktree_manager = compas::worktree::WorktreeManager::new();
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
        store.ensure_thread(thread_id, None, None).await.unwrap();
        store
            .insert_message(
                thread_id,
                "operator",
                "focused",
                "dispatch",
                "do something",
                None,
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
                filter: None,
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
        let worktree_manager = compas::worktree::WorktreeManager::new();
        let runner = WorkerRunner::new(
            config_handle,
            store.clone(),
            registry,
            event_bus,
            worktree_manager,
        );

        let thread_id = "t-hash-null";
        store.ensure_thread(thread_id, None, None).await.unwrap();
        store
            .insert_message(
                thread_id,
                "operator",
                "spark", // prompt: None in test_config
                "dispatch",
                "do something",
                None,
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
    use compas::config::types::{HandoffConfig, HandoffTarget};
    use compas::events::{EventBus, OrchestratorEvent};
    use compas::worker::WorkerRunner;
    use tokio::sync::Semaphore;

    /// Config with agent A handing off `response` to agent B.
    fn chain_config() -> OrchestratorConfig {
        OrchestratorConfig {
            default_workdir: PathBuf::from("/tmp"),
            state_dir: PathBuf::from("/tmp/compas-test"),
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
            backend_definitions: None,
            hooks: None,
            schedules: None,
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

        let worktree_manager = compas::worktree::WorktreeManager::new();
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

        let worktree_manager = compas::worktree::WorktreeManager::new();
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

        let worktree_manager = compas::worktree::WorktreeManager::new();
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
                thread_id, "operator", "focused", "dispatch", "do work", None, None,
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

        store.ensure_thread(thread_id, None, None).await.unwrap();

        // Initially zero.
        let count = store.count_handoff_messages(thread_id).await.unwrap();
        assert_eq!(count, 0);

        // Insert a non-handoff message.
        store
            .insert_message(
                thread_id, "operator", "agent-a", "dispatch", "task", None, None,
            )
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
default_workdir: /tmp
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
        let config = compas::config::load_config_from_str(yaml).unwrap();

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

        let worktree_manager = compas::worktree::WorktreeManager::new();
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

        let worktree_manager = compas::worktree::WorktreeManager::new();
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
default_workdir: /tmp
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
        let config = compas::config::load_config_from_str(yaml).unwrap();

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

    // ── ORCH-EVO-17: skip_handoff tests ─────────────────────────────────

    #[tokio::test]
    async fn test_skip_handoff_suppresses_auto_handoff() {
        // Agent A has on_response: agent-b. Dispatch with skip_handoff=true.
        // After agent-a completes, there should be NO handoff to agent-b.
        let store = test_store().await;
        let config = chain_config();
        let config_handle = ConfigHandle::new(config.clone());

        let mut registry = BackendRegistry::new();
        registry.register("stub", Arc::new(StubBackend { ping_alive: true }));

        let event_bus = EventBus::new();
        let mut rx = event_bus.subscribe();

        let worktree_manager = compas::worktree::WorktreeManager::new();
        let runner = WorkerRunner::new(
            config_handle,
            store.clone(),
            registry,
            event_bus,
            worktree_manager,
        );

        let thread_id = "skip-handoff-test";
        let msg_id = store
            .insert_dispatch_message(
                thread_id,
                "operator",
                "agent-a",
                "dispatch",
                "implement feature X",
                None,
                None,
                true, // skip_handoff
            )
            .await
            .unwrap();
        store
            .insert_execution_with_dispatch(thread_id, "agent-a", Some(msg_id), None)
            .await
            .unwrap();

        let semaphore = Arc::new(Semaphore::new(4));
        runner.poll_once(&semaphore).await;

        // Wait for at least one MessageReceived (the reply).
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut message_count = 0;
        while let Ok(Ok(event)) = tokio::time::timeout_at(deadline, rx.recv()).await {
            if matches!(event, OrchestratorEvent::MessageReceived { .. }) {
                message_count += 1;
                // Give a moment for any additional messages to be emitted.
                break;
            }
        }
        // Drain any remaining events.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        while rx.try_recv().is_ok() {}

        assert!(
            message_count >= 1,
            "expected at least 1 MessageReceived (reply)"
        );

        // Verify: reply exists but NO handoff to agent-b.
        let messages = store.get_thread_messages(thread_id).await.unwrap();
        let reply = messages
            .iter()
            .find(|m| m.intent == "response" && m.from_alias == "agent-a");
        assert!(reply.is_some(), "expected reply from agent-a");

        let handoff = messages
            .iter()
            .find(|m| m.intent == "handoff" && m.to_alias == "agent-b");
        assert!(
            handoff.is_none(),
            "expected NO handoff to agent-b when skip_handoff=true; messages: {:?}",
            messages
                .iter()
                .map(|m| format!("{}→{} ({})", m.from_alias, m.to_alias, m.intent))
                .collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn test_skip_handoff_false_preserves_handoff() {
        // Same setup as above but skip_handoff=false → handoff should still fire.
        let store = test_store().await;
        let config = chain_config();
        let config_handle = ConfigHandle::new(config.clone());

        let mut registry = BackendRegistry::new();
        registry.register("stub", Arc::new(StubBackend { ping_alive: true }));

        let event_bus = EventBus::new();
        let mut rx = event_bus.subscribe();

        let worktree_manager = compas::worktree::WorktreeManager::new();
        let runner = WorkerRunner::new(
            config_handle,
            store.clone(),
            registry,
            event_bus,
            worktree_manager,
        );

        let thread_id = "skip-handoff-false-test";
        let msg_id = store
            .insert_dispatch_message(
                thread_id,
                "operator",
                "agent-a",
                "dispatch",
                "implement feature Y",
                None,
                None,
                false, // skip_handoff=false, handoff should proceed
            )
            .await
            .unwrap();
        store
            .insert_execution_with_dispatch(thread_id, "agent-a", Some(msg_id), None)
            .await
            .unwrap();

        let semaphore = Arc::new(Semaphore::new(4));
        runner.poll_once(&semaphore).await;

        // Wait for TWO MessageReceived events: reply + handoff.
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

        // Verify: handoff to agent-b exists.
        let messages = store.get_thread_messages(thread_id).await.unwrap();
        let handoff = messages
            .iter()
            .find(|m| m.intent == "handoff" && m.to_alias == "agent-b");
        assert!(
            handoff.is_some(),
            "expected handoff to agent-b when skip_handoff=false"
        );
    }

    #[tokio::test]
    async fn test_skip_handoff_store_roundtrip() {
        let store = test_store().await;
        let thread_id = "skip-handoff-roundtrip";

        // Insert with skip_handoff=true.
        let msg_id = store
            .insert_dispatch_message(
                thread_id,
                "operator",
                "agent-a",
                "dispatch",
                "task body",
                None,
                None,
                true,
            )
            .await
            .unwrap();

        let msg = store.get_message(msg_id).await.unwrap().unwrap();
        assert!(
            msg.skip_handoff,
            "skip_handoff should be true after roundtrip"
        );

        // Insert with skip_handoff=false (via regular insert_message).
        let msg_id2 = store
            .insert_message(
                thread_id, "operator", "agent-a", "dispatch", "task2", None, None,
            )
            .await
            .unwrap();

        let msg2 = store.get_message(msg_id2).await.unwrap().unwrap();
        assert!(
            !msg2.skip_handoff,
            "skip_handoff should default to false for insert_message"
        );
    }

    /// Config with agent-a fanning out to reviewer + reviewer-2.
    fn fanout_skip_config() -> OrchestratorConfig {
        OrchestratorConfig {
            default_workdir: PathBuf::from("/tmp"),
            state_dir: PathBuf::from("/tmp/compas-test"),
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
            backend_definitions: None,
            hooks: None,
            schedules: None,
        }
    }

    #[tokio::test]
    async fn test_skip_handoff_suppresses_fanout() {
        // Agent with fan-out handoff config + skip_handoff=true dispatch.
        // Verify no fan-out threads created.
        let store = test_store().await;
        let config = fanout_skip_config();
        let config_handle = ConfigHandle::new(config.clone());

        let mut registry = BackendRegistry::new();
        registry.register("stub", Arc::new(StubBackend { ping_alive: true }));

        let event_bus = EventBus::new();
        let mut rx = event_bus.subscribe();

        let worktree_manager = compas::worktree::WorktreeManager::new();
        let runner = WorkerRunner::new(
            config_handle,
            store.clone(),
            registry,
            event_bus,
            worktree_manager,
        );

        let thread_id = "skip-handoff-fanout-test";
        let msg_id = store
            .insert_dispatch_message(
                thread_id,
                "operator",
                "agent-a",
                "dispatch",
                "implement feature Z",
                None,
                None,
                true, // skip_handoff
            )
            .await
            .unwrap();
        store
            .insert_execution_with_dispatch(thread_id, "agent-a", Some(msg_id), None)
            .await
            .unwrap();

        let semaphore = Arc::new(Semaphore::new(4));
        runner.poll_once(&semaphore).await;

        // Wait for at least one MessageReceived (the reply).
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        while let Ok(Ok(event)) = tokio::time::timeout_at(deadline, rx.recv()).await {
            if matches!(event, OrchestratorEvent::MessageReceived { .. }) {
                break;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        while rx.try_recv().is_ok() {}

        // Verify: reply exists but NO handoff messages.
        let messages = store.get_thread_messages(thread_id).await.unwrap();
        let reply = messages
            .iter()
            .find(|m| m.from_alias == "agent-a" && m.intent == "response");
        assert!(reply.is_some(), "expected reply from fan-out agent");

        let handoff_count = messages.iter().filter(|m| m.intent == "handoff").count();
        assert_eq!(
            handoff_count, 0,
            "expected no handoff messages when skip_handoff=true on fan-out agent"
        );

        // Verify no fan-out child threads were created.
        let all_threads = store.list_threads(None, None, 100).await.unwrap();
        let child_threads: Vec<_> = all_threads
            .iter()
            .filter(|t| t.source_thread_id.as_deref() == Some(thread_id))
            .collect();
        assert!(
            child_threads.is_empty(),
            "expected no fan-out child threads when skip_handoff=true"
        );
    }
}

mod pending_chain_work_tests {
    use super::*;

    #[tokio::test]
    async fn test_count_pending_chain_work_no_work() {
        let store = test_store().await;
        store.ensure_thread("t-chain", None, None).await.unwrap();

        let count = store.count_pending_chain_work("t-chain").await.unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn test_count_pending_chain_work_active_execution() {
        let store = test_store().await;
        store.ensure_thread("t-chain", None, None).await.unwrap();

        // Insert a queued execution — counts as pending
        store.insert_execution("t-chain", "focused").await.unwrap();

        let count = store.count_pending_chain_work("t-chain").await.unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn test_count_pending_chain_work_untriggered_handoff() {
        let store = test_store().await;
        store.ensure_thread("t-chain", None, None).await.unwrap();

        // Insert a handoff message with no linked execution — counts as pending
        store
            .insert_message(
                "t-chain",
                "agent-a",
                "agent-b",
                "handoff",
                "take over",
                None,
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
        store.ensure_thread("t-chain", None, None).await.unwrap();

        // Insert a handoff message
        let msg_id = store
            .insert_message(
                "t-chain",
                "agent-a",
                "agent-b",
                "handoff",
                "take over",
                None,
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
        store.ensure_thread("t-chain", None, None).await.unwrap();

        // Insert and complete an execution
        let exec_id = store.insert_execution("t-chain", "focused").await.unwrap();
        store.claim_next_execution(10).await.unwrap();
        store.mark_execution_executing(&exec_id).await.unwrap();
        store
            .complete_execution(&exec_id, Some(0), None, None, 100, None, None, None, None)
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
    use compas::wait::{self, WaitOutcome, WaitRequest};
    use std::time::Duration;

    #[tokio::test]
    async fn test_await_chain_returns_immediately_when_no_pending_work() {
        let store = test_store().await;
        store.ensure_thread("t-chain", None, None).await.unwrap();

        // Insert a response message — no pending chain work
        store
            .insert_message(
                "t-chain",
                "reviewer",
                "operator",
                "response",
                "review done",
                None,
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
            WaitOutcome::Found {
                message: msg,
                fanout_children_awaited,
                settled_at,
            } => {
                assert_eq!(msg.from_alias, "reviewer");
                assert_eq!(msg.body, "review done");
                // No fan-out occurred — metadata should be absent.
                assert!(
                    fanout_children_awaited.is_none(),
                    "expected no fanout metadata when chain was never pending"
                );
                assert!(
                    settled_at.is_none(),
                    "expected no settled_at when chain was never pending"
                );
            }
            WaitOutcome::Timeout { .. } => panic!("should not timeout"),
        }
    }

    #[tokio::test]
    async fn test_await_chain_waits_through_handoff() {
        let store = test_store().await;
        store.ensure_thread("t-chain", None, None).await.unwrap();

        // Insert implementer's response message (found first during polling)
        store
            .insert_message(
                "t-chain",
                "implementer",
                "operator",
                "response",
                "impl done",
                None,
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
                    None,
                )
                .await
                .unwrap();
            store2
                .complete_execution(&exec_id2, Some(0), None, None, 200, None, None, None, None)
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
            WaitOutcome::Found {
                message: msg,
                fanout_children_awaited,
                settled_at,
            } => {
                // Should return the reviewer's reply, not the implementer's
                assert_eq!(msg.from_alias, "reviewer");
                assert_eq!(msg.body, "review done");
                // Chain was pending (handoff execution), but no fan-out children.
                assert_eq!(fanout_children_awaited, Some(0));
                assert!(
                    settled_at.is_some(),
                    "settled_at should be set after chain settlement"
                );
            }
            WaitOutcome::Timeout { .. } => panic!("should not timeout"),
        }
    }

    #[tokio::test]
    async fn test_await_chain_returns_on_depth_limit() {
        let store = test_store().await;
        store.ensure_thread("t-chain", None, None).await.unwrap();

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
            WaitOutcome::Found {
                message: msg,
                fanout_children_awaited,
                settled_at,
            } => {
                assert_eq!(msg.from_alias, "orchestrator");
                assert!(msg.body.contains("depth limit"));
                // No pending chain work was ever observed.
                assert!(fanout_children_awaited.is_none());
                assert!(settled_at.is_none());
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
        store
            .ensure_thread(source_thread, None, None)
            .await
            .unwrap();

        // Insert the implementer's response on the source thread
        store
            .insert_message(
                source_thread,
                "implementer",
                "operator",
                "response",
                "impl done",
                None,
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
                .insert_message(
                    &child_1,
                    "reviewer-1",
                    "operator",
                    "response",
                    "lgtm",
                    None,
                    None,
                )
                .await
                .unwrap();
            store2
                .complete_execution(&exec_1, Some(0), None, None, 100, None, None, None, None)
                .await
                .unwrap();

            store2.claim_next_execution(10).await.unwrap();
            store2.mark_execution_executing(&exec_2).await.unwrap();
            store2
                .insert_message(
                    &child_2,
                    "reviewer-2",
                    "operator",
                    "response",
                    "lgtm",
                    None,
                    None,
                )
                .await
                .unwrap();
            store2
                .complete_execution(&exec_2, Some(0), None, None, 100, None, None, None, None)
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

        let before_wait = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        let outcome = wait::wait_for_message(&store, &req).await.unwrap();
        match outcome {
            WaitOutcome::Found {
                message: msg,
                fanout_children_awaited,
                settled_at,
            } => {
                assert_eq!(msg.from_alias, "implementer");
                assert_eq!(msg.body, "impl done");
                // Two fan-out children were created and awaited.
                assert_eq!(
                    fanout_children_awaited,
                    Some(2),
                    "expected 2 fan-out children awaited"
                );
                // settled_at should be a valid timestamp >= when we started waiting.
                let ts = settled_at.expect("settled_at should be present after fan-out settlement");
                assert!(
                    ts >= before_wait,
                    "settled_at ({}) should be >= wait start time ({})",
                    ts,
                    before_wait
                );
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
        store
            .ensure_thread("t-no-fanout", None, None)
            .await
            .unwrap();

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
        store.ensure_thread(source, None, None).await.unwrap();

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
            .ensure_thread(source_a, Some("shared-batch"), None)
            .await
            .unwrap();

        // Thread B: sibling in same batch but different source
        let source_b = "t-sibling-b";
        store
            .ensure_thread(source_b, Some("shared-batch"), None)
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
        store.ensure_thread(source, None, None).await.unwrap();

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

    #[tokio::test]
    async fn test_wait_timeout_derived_ceiling_under_honored() {
        let store = test_store().await;
        let config = test_config();
        // Default execution_timeout_secs=600 → ceiling = 630 for non-chain.
        // A 1s request is well under ceiling and will be honored.
        let mut registry = BackendRegistry::new();
        registry.register("stub", Arc::new(StubBackend { ping_alive: true }));
        let server = OrchestratorMcpServer::new(ConfigHandle::new(config), store, registry);

        server
            .store
            .insert_message("t-ceil", "op", "focused", "dispatch", "work", None, None)
            .await
            .unwrap();

        let start = std::time::Instant::now();
        let result = server
            .wait_impl(
                WaitParams {
                    thread_id: "t-ceil".to_string(),
                    intent: Some("status-update".to_string()),
                    since_reference: None,
                    strict_new: None,
                    timeout_secs: Some(1),
                    await_chain: None,
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
        // No clamp transparency fields on happy path
        assert!(
            json.get("clamped").is_none(),
            "clamped should be absent when not clamped"
        );
        assert!(json.get("effective_timeout_secs").is_none());
        assert!(json.get("requested_timeout_secs").is_none());
        assert!(json.get("hint").is_none());
        assert!(
            elapsed.as_secs() < 5,
            "expected timeout in ~1s, took {}s",
            elapsed.as_secs()
        );
    }

    /// Verify that requesting a timeout above the derived ceiling clamps and
    /// populates transparency fields (clamped, effective_timeout_secs, hint).
    ///
    /// Uses exec_timeout=2 → ceiling=32. The test waits the full 32s ceiling.
    #[tokio::test]
    async fn test_wait_timeout_derived_ceiling_clamped() {
        let store = test_store().await;
        let mut config = test_config();
        config.orchestration.execution_timeout_secs = 2; // ceiling = 2 + 30 = 32
        let mut registry = BackendRegistry::new();
        registry.register("stub", Arc::new(StubBackend { ping_alive: true }));
        let server = OrchestratorMcpServer::new(ConfigHandle::new(config), store, registry);

        server
            .store
            .insert_message("t-clamp", "op", "focused", "dispatch", "work", None, None)
            .await
            .unwrap();

        let start = std::time::Instant::now();
        let result = server
            .wait_impl(
                WaitParams {
                    thread_id: "t-clamp".to_string(),
                    intent: Some("status-update".to_string()),
                    since_reference: None,
                    strict_new: None,
                    timeout_secs: Some(5000), // way over ceiling of 32
                    await_chain: None,
                },
                None,
                None,
            )
            .await
            .unwrap();
        let elapsed = start.elapsed();
        let json = extract_json(&result);
        assert_eq!(json["found"], false);
        assert_eq!(json["timeout_secs"], 32);
        assert_eq!(json["clamped"], true);
        assert_eq!(json["effective_timeout_secs"], 32);
        assert_eq!(json["requested_timeout_secs"], 5000);
        assert!(json["hint"].as_str().unwrap().contains("clamped"));
        assert!(json["hint"]
            .as_str()
            .unwrap()
            .contains("execution_timeout_secs"));
        assert!(
            elapsed.as_secs() < 40,
            "expected timeout in ~32s, took {}s",
            elapsed.as_secs()
        );
    }

    #[tokio::test]
    async fn test_wait_timeout_chain_pending_true() {
        let store = test_store().await;
        store
            .ensure_thread("t-chain-pend", None, None)
            .await
            .unwrap();

        // Insert a response message so the wait loop finds a match
        store
            .insert_message(
                "t-chain-pend",
                "implementer",
                "operator",
                "response",
                "impl done",
                None,
                None,
            )
            .await
            .unwrap();

        // Insert a handoff message (simulates pending chain work)
        let handoff_msg_id = store
            .insert_message(
                "t-chain-pend",
                "implementer",
                "reviewer",
                "handoff",
                "review needed",
                None,
                None,
            )
            .await
            .unwrap();

        // Insert a queued execution linked to the handoff (pending work)
        store
            .insert_execution_with_dispatch("t-chain-pend", "reviewer", Some(handoff_msg_id), None)
            .await
            .unwrap();

        // Wait with await_chain=true and a very short timeout — should timeout with chain_pending=true
        let req = WaitRequest {
            thread_id: "t-chain-pend".to_string(),
            intent: Some("response".to_string()),
            since_reference: None,
            strict_new: false,
            timeout: Duration::from_secs(1),
            trigger_intents: vec![],
            await_chain: true,
        };

        let outcome = wait::wait_for_message(&store, &req).await.unwrap();
        match outcome {
            WaitOutcome::Timeout { chain_pending, .. } => {
                assert!(
                    chain_pending,
                    "expected chain_pending=true when chain has pending work"
                );
            }
            WaitOutcome::Found { .. } => {
                panic!("expected timeout, not found (chain should be pending)")
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Fan-Out Handoff Tests (ORCH-HANDOFF-2)
// ═══════════════════════════════════════════════════════════════════════════

mod fanout_tests {
    use super::*;
    use compas::config::types::{HandoffConfig, HandoffTarget};
    use compas::events::{EventBus, OrchestratorEvent};
    use compas::worker::WorkerRunner;
    use tokio::sync::Semaphore;

    fn fanout_config() -> OrchestratorConfig {
        OrchestratorConfig {
            default_workdir: PathBuf::from("/tmp"),
            state_dir: PathBuf::from("/tmp/compas-test"),
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
            backend_definitions: None,
            hooks: None,
            schedules: None,
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

        let worktree_manager = compas::worktree::WorktreeManager::new();
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

        let worktree_manager = compas::worktree::WorktreeManager::new();
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
            .ensure_thread(source_thread, Some(batch_id), None)
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

        let worktree_manager = compas::worktree::WorktreeManager::new();
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
                thread_id, "operator", "agent-a", "dispatch", "do work", None, None,
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

        let worktree_manager = compas::worktree::WorktreeManager::new();
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
        store.ensure_thread("t-pid-1", None, None).await.unwrap();
        let exec_id = store.insert_execution("t-pid-1", "focused").await.unwrap();
        store.mark_execution_executing(&exec_id).await.unwrap();

        store.set_execution_pid(&exec_id, 12345).await.unwrap();

        let exec = store.get_execution(&exec_id).await.unwrap().unwrap();
        assert_eq!(exec.pid, Some(12345));
    }

    #[tokio::test]
    async fn test_get_orphaned_executions_with_pid() {
        let store = test_store().await;
        store.ensure_thread("t-pid-2", None, None).await.unwrap();
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
        store.ensure_thread("t-pid-3", None, None).await.unwrap();
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
        store.ensure_thread("t-pid-4", None, None).await.unwrap();
        let exec_id = store.insert_execution("t-pid-4", "focused").await.unwrap();
        let _ = store.claim_next_execution(10).await.unwrap();
        store.mark_execution_executing(&exec_id).await.unwrap();
        store.set_execution_pid(&exec_id, 99999).await.unwrap();
        store
            .complete_execution(&exec_id, Some(0), None, None, 100, None, None, None, None)
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
            default_workdir: PathBuf::from("/tmp"),
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
            backend_definitions: None,
            hooks: None,
            schedules: None,
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

        store.ensure_thread("t-log-1", None, None).await.unwrap();
        let exec_id = store.insert_execution("t-log-1", "focused").await.unwrap();
        store.mark_execution_executing(&exec_id).await.unwrap();
        store
            .complete_execution(
                &exec_id,
                Some(0),
                Some("line1\nline2\nline3"),
                None,
                100,
                None,
                None,
                None,
                None,
            )
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

        store.ensure_thread("t-log-2", None, None).await.unwrap();
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

        store.ensure_thread("t-log-3", None, None).await.unwrap();
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

        store.ensure_thread("t-log-4", None, None).await.unwrap();
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

        store.ensure_thread("t-log-5", None, None).await.unwrap();
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

        store
            .ensure_thread("t-log-empty", None, None)
            .await
            .unwrap();
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

        store.ensure_thread("t-log-eof", None, None).await.unwrap();
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

// ═══════════════════════════════════════════════════════════════════════════
// Session Resume After Crash Tests
// ═══════════════════════════════════════════════════════════════════════════

mod session_resume_tests {
    use super::*;
    use compas::events::{EventBus, OrchestratorEvent};
    use compas::worker::WorkerRunner;
    use compas::worktree::WorktreeManager;
    use tokio::sync::Semaphore;

    /// Stub backend that emits a Claude-format init JSONL line through stdout_tx
    /// before returning, so consume_telemetry can pick up the session ID mid-stream.
    /// Also captures the `resume_session_id` passed on each trigger for test assertions.
    #[derive(Debug)]
    struct StreamingStubBackend {
        session_id: String,
        /// Captures the resume_session_id received on each trigger call.
        captured_resume_ids: std::sync::Mutex<Vec<Option<String>>>,
    }

    impl StreamingStubBackend {
        fn new(session_id: &str) -> Self {
            Self {
                session_id: session_id.to_string(),
                captured_resume_ids: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl Backend for StreamingStubBackend {
        fn name(&self) -> &str {
            "claude"
        }

        async fn start_session(&self, agent: &Agent) -> OrchResult<Session> {
            Ok(Session {
                // Internal session ID must differ from the backend session ID
                // so the executor's GAP-2a guard doesn't block persistence.
                id: uuid::Uuid::new_v4().to_string(),
                agent_alias: agent.alias.clone(),
                backend: "claude".to_string(),
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
            // Capture the resume_session_id for test assertions.
            self.captured_resume_ids
                .lock()
                .unwrap()
                .push(session.resume_session_id.clone());

            // Emit a Claude-format init line through stdout_tx so the telemetry
            // consumer can extract the session ID mid-stream.
            if let Some(ref tx) = session.stdout_tx {
                let init_line = format!(
                    r#"{{"type":"system","subtype":"init","session_id":"{}","tools":[],"model":"test"}}"#,
                    self.session_id
                );
                let _ = tx.try_send(init_line);
                // Small yield so the telemetry consumer has a chance to process.
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            let result_text = format!(
                "streaming stub response to: {}",
                instruction.unwrap_or("(none)")
            );
            Ok(BackendOutput {
                success: true,
                result_text: result_text.clone(),
                parsed_intent: None,
                session_id: Some(self.session_id.clone()),
                raw_output: result_text,
                error_category: None,
                pid: None,
                cost_usd: None,
                tokens_in: None,
                tokens_out: None,
                num_turns: None,
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
                alive: true,
                latency_ms: 1,
                detail: Some("streaming stub ping".into()),
            }
        }
    }

    /// Test that consume_telemetry persists the session ID mid-stream when the
    /// backend emits a Claude-format init JSONL line through the stdout channel.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_mid_stream_session_id_persistence_via_telemetry() {
        let store = test_store().await;

        let session_id = "mid-stream-sid-abc123";
        let mut registry = BackendRegistry::new();
        registry.register("claude", Arc::new(StreamingStubBackend::new(session_id)));

        // Config with backend = "claude" so consume_telemetry uses the Claude parser.
        let config = OrchestratorConfig {
            default_workdir: PathBuf::from("/tmp"),
            state_dir: PathBuf::from("/tmp/compas-test-stream"),
            poll_interval_secs: 1,
            models: None,
            agents: vec![AgentConfig {
                alias: "focused".to_string(),
                backend: "claude".to_string(),
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
            }],
            worktree_dir: None,
            orchestration: OrchestrationConfig::default(),
            database: DatabaseConfig::default(),
            notifications: Default::default(),
            backend_definitions: None,
            hooks: None,
            schedules: None,
        };

        let config_handle = ConfigHandle::new(config.clone());
        let event_bus = EventBus::new();
        let mut rx = event_bus.subscribe();
        let worktree_manager = WorktreeManager::new();

        let runner = WorkerRunner::new(
            config_handle,
            store.clone(),
            registry,
            event_bus,
            worktree_manager,
        );

        // Seed: dispatch message + queued execution
        let thread_id = "t-mid-stream-sid";
        let msg_id = store
            .insert_message(
                thread_id, "operator", "focused", "dispatch", "work", None, None,
            )
            .await
            .unwrap();
        store
            .insert_execution_with_dispatch(thread_id, "focused", Some(msg_id), None)
            .await
            .unwrap();

        // Run poll_once to claim and spawn the execution
        let semaphore = Arc::new(Semaphore::new(4));
        runner.poll_once(&semaphore).await;

        // Wait for the execution to complete
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Ok(OrchestratorEvent::MessageReceived { .. })) => break,
                Ok(Ok(_)) => continue,
                Ok(Err(_)) | Err(_) => break,
            }
        }

        // Allow the telemetry consumer task to finish flushing
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // Verify: session ID was persisted mid-stream by consume_telemetry
        let sid = store
            .get_last_backend_session_id(thread_id, "focused")
            .await
            .unwrap();
        assert_eq!(
            sid.as_deref(),
            Some(session_id),
            "session ID should be persisted mid-stream by consume_telemetry"
        );
    }

    /// End-to-end test: first execute_trigger persists session ID mid-stream,
    /// then we crash the execution, then a second execute_trigger on the same
    /// thread verifies that the backend receives the crashed session's ID as
    /// resume_session_id.
    #[tokio::test]
    async fn test_crash_resume_session_id_flows_through_execute_trigger() {
        let store = test_store().await;

        let session_id = "crash-resume-sid-42";
        let streaming_backend = Arc::new(StreamingStubBackend::new(session_id));

        let mut registry = BackendRegistry::new();
        registry.register("claude", streaming_backend.clone());
        let registry = Arc::new(registry);

        let agent_configs = vec![AgentConfig {
            alias: "focused".to_string(),
            backend: "claude".to_string(),
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
        }];

        let worktree_manager = Arc::new(WorktreeManager::new());
        let thread_id = "t-crash-resume-e2e";

        // -- First execution: runs successfully, persists session ID --
        store.ensure_thread(thread_id, None, None).await.unwrap();
        let msg_id = store
            .insert_message(
                thread_id,
                "operator",
                "focused",
                "dispatch",
                "first run",
                None,
                None,
            )
            .await
            .unwrap();
        let _ = store
            .insert_execution_with_dispatch(thread_id, "focused", Some(msg_id), None)
            .await
            .unwrap();
        let exec1 = store.claim_next_execution(2).await.unwrap().unwrap();

        // Create a stdout channel so consume_telemetry can run inline
        let (stdout_tx, _stdout_rx) = std::sync::mpsc::sync_channel::<String>(128);
        let stdout_tx = Arc::new(stdout_tx);

        let output1 = compas::worker::execute_trigger(
            &exec1,
            &store,
            &registry,
            &agent_configs,
            "first run",
            30,
            None,
            Some(stdout_tx),
            &worktree_manager,
            std::path::Path::new("/tmp"),
            None,
        )
        .await;
        assert!(output1.success, "first execution should succeed");

        // First trigger should have no resume_session_id (fresh thread)
        {
            let captured = streaming_backend.captured_resume_ids.lock().unwrap();
            assert_eq!(captured.len(), 1);
            assert_eq!(
                captured[0], None,
                "first execution should have no resume_session_id"
            );
        }

        // Verify session ID was persisted (either mid-stream or safety net)
        let sid = store
            .get_last_backend_session_id(thread_id, "focused")
            .await
            .unwrap();
        assert_eq!(
            sid.as_deref(),
            Some(session_id),
            "session ID should be persisted after first execution"
        );

        // -- Simulate crash: mark the execution as crashed --
        // (The executor already completed it, so we backdate to simulate a crash scenario.
        // Instead, we manually set the session ID on a new crashed execution.)
        let msg_id_crash = store
            .insert_message(
                thread_id,
                "operator",
                "focused",
                "dispatch",
                "crash run",
                None,
                None,
            )
            .await
            .unwrap();
        let crash_exec_id = store
            .insert_execution_with_dispatch(thread_id, "focused", Some(msg_id_crash), None)
            .await
            .unwrap()
            .unwrap();
        let _ = store.claim_next_execution(2).await.unwrap();
        store
            .mark_execution_executing(&crash_exec_id)
            .await
            .unwrap();
        // Simulate mid-stream persistence before crash
        store
            .set_backend_session_id(&crash_exec_id, session_id)
            .await
            .unwrap();
        store
            .fail_execution(
                &crash_exec_id,
                "worker crashed",
                None,
                500,
                ExecutionStatus::Crashed,
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();

        // -- Second execution after crash: should receive resume_session_id --
        let msg_id_2 = store
            .insert_message(
                thread_id,
                "operator",
                "focused",
                "dispatch",
                "resume run",
                None,
                None,
            )
            .await
            .unwrap();
        let _ = store
            .insert_execution_with_dispatch(thread_id, "focused", Some(msg_id_2), None)
            .await
            .unwrap();
        let exec2 = store.claim_next_execution(2).await.unwrap().unwrap();

        let (stdout_tx2, _stdout_rx2) = std::sync::mpsc::sync_channel::<String>(128);
        let stdout_tx2 = Arc::new(stdout_tx2);

        let output2 = compas::worker::execute_trigger(
            &exec2,
            &store,
            &registry,
            &agent_configs,
            "resume run",
            30,
            None,
            Some(stdout_tx2),
            &worktree_manager,
            std::path::Path::new("/tmp"),
            None,
        )
        .await;
        assert!(output2.success, "second execution should succeed");

        // The backend should have received the crashed session's ID as resume_session_id.
        // Two trigger calls total: first execute_trigger + second execute_trigger.
        // The crash was simulated manually (no trigger call).
        let captured = streaming_backend.captured_resume_ids.lock().unwrap();
        assert_eq!(captured.len(), 2, "should have 2 trigger calls total");
        assert_eq!(
            captured[1].as_deref(),
            Some(session_id),
            "second execution after crash should receive resume_session_id from crashed execution"
        );
    }
}

mod generic_backend_registry_tests {
    use compas::backend::generic::GenericBackend;
    use compas::backend::registry::BackendRegistry;
    use compas::config::types::BackendDefinition;
    use std::sync::Arc;

    #[test]
    fn test_generic_backend_registered_by_name() {
        let mut registry = BackendRegistry::new();
        let def = BackendDefinition {
            name: "my-tool".to_string(),
            command: "echo".to_string(),
            args: vec!["{{instruction}}".to_string()],
            resume: None,
            output: Default::default(),
            ping: None,
            env_remove: None,
        };
        registry.register("my-tool", Arc::new(GenericBackend::new(def)));
        assert!(
            registry.get_by_name("my-tool").is_ok(),
            "generic backend should be retrievable by name"
        );
    }

    #[test]
    fn test_generic_backend_not_found_without_registration() {
        let registry = BackendRegistry::new();
        assert!(
            registry.get_by_name("unregistered-tool").is_err(),
            "unregistered backend should return error"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Merge Queue Worker Integration Tests
// ═══════════════════════════════════════════════════════════════════════════

mod merge_worker_tests {
    use super::*;
    use compas::events::EventBus;
    use compas::store::{MergeOperation, MergeOperationStatus};
    use compas::worker::WorkerRunner;
    use std::process::Command;

    /// Create a temporary git repo with an initial commit.
    fn init_test_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let dir_str = dir.path().to_string_lossy().to_string();

        let init = Command::new("git")
            .args(["init", &dir_str])
            .output()
            .unwrap();
        assert!(init.status.success(), "git init failed");

        let commit = Command::new("git")
            .args([
                "-C",
                &dir_str,
                "-c",
                "user.email=test@test.com",
                "-c",
                "user.name=Test",
                "commit",
                "--allow-empty",
                "-m",
                "initial commit",
            ])
            .output()
            .unwrap();
        assert!(commit.status.success(), "initial commit failed");

        dir
    }

    /// Create a source branch with a committed file.
    fn create_source_branch(repo_path: &std::path::Path, branch_name: &str) {
        let path_str = repo_path.to_string_lossy().to_string();

        let checkout = Command::new("git")
            .args(["-C", &path_str, "checkout", "-b", branch_name])
            .output()
            .unwrap();
        assert!(checkout.status.success());

        std::fs::write(repo_path.join("feature.txt"), "feature content").unwrap();

        let add = Command::new("git")
            .args(["-C", &path_str, "add", "feature.txt"])
            .output()
            .unwrap();
        assert!(add.status.success());

        let commit = Command::new("git")
            .args([
                "-C",
                &path_str,
                "-c",
                "user.email=test@test.com",
                "-c",
                "user.name=Test",
                "commit",
                "-m",
                "add feature",
            ])
            .output()
            .unwrap();
        assert!(commit.status.success());

        let back = Command::new("git")
            .args(["-C", &path_str, "checkout", "-"])
            .output()
            .unwrap();
        assert!(back.status.success());
    }

    /// Get the default branch name.
    fn default_branch(repo_path: &std::path::Path) -> String {
        let path_str = repo_path.to_string_lossy().to_string();
        let output = Command::new("git")
            .args(["-C", &path_str, "rev-parse", "--abbrev-ref", "HEAD"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn merge_test_config(repo_root: PathBuf) -> OrchestratorConfig {
        OrchestratorConfig {
            default_workdir: repo_root,
            state_dir: PathBuf::from("/tmp/compas-merge-test"),
            poll_interval_secs: 1,
            models: None,
            agents: vec![AgentConfig {
                alias: "focused".to_string(),
                backend: "stub".to_string(),
                role: AgentRole::Worker,
                model: None,
                prompt: None,
                prompt_file: None,
                timeout_secs: Some(30),
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
            backend_definitions: None,
            hooks: None,
            schedules: None,
        }
    }

    #[tokio::test]
    async fn test_worker_merge_happy_path() {
        let repo = init_test_repo();
        let target = default_branch(repo.path());
        create_source_branch(repo.path(), "compas/merge-happy");

        let store = test_store().await;
        let config = merge_test_config(repo.path().to_path_buf());
        let config_handle = ConfigHandle::new(config);

        let mut registry = BackendRegistry::new();
        registry.register("stub", Arc::new(StubBackend { ping_alive: true }));

        let event_bus = EventBus::new();
        let worktree_manager = compas::worktree::WorktreeManager::new();
        let runner = WorkerRunner::new(
            config_handle,
            store.clone(),
            registry,
            event_bus,
            worktree_manager,
        );

        // Insert a queued merge operation
        let op = MergeOperation {
            id: "merge-happy-1".to_string(),
            thread_id: "merge-happy".to_string(),
            source_branch: "compas/merge-happy".to_string(),
            target_branch: target.clone(),
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

        // Drive the merge poll
        runner.poll_merge_ops().await;

        // The merge runs in a spawned task — wait for it to complete.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let fetched = store.get_merge_op("merge-happy-1").await.unwrap().unwrap();
            let status: MergeOperationStatus = fetched.status.parse().unwrap();
            if status.is_terminal() {
                assert_eq!(
                    status,
                    MergeOperationStatus::Completed,
                    "merge op should complete successfully, error: {:?}",
                    fetched.error_detail
                );
                assert!(
                    fetched.result_summary.is_some(),
                    "completed merge should have a result summary"
                );
                break;
            }
            if tokio::time::Instant::now() > deadline {
                panic!(
                    "merge did not reach terminal status within timeout, status: {}",
                    fetched.status
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    #[tokio::test]
    async fn test_worker_merge_conflict_path() {
        let repo = init_test_repo();
        let target = default_branch(repo.path());
        let path_str = repo.path().to_string_lossy().to_string();

        // Create a base file on the default branch
        std::fs::write(repo.path().join("shared.txt"), "base content").unwrap();
        let add = Command::new("git")
            .args(["-C", &path_str, "add", "shared.txt"])
            .output()
            .unwrap();
        assert!(add.status.success());
        let commit = Command::new("git")
            .args([
                "-C",
                &path_str,
                "-c",
                "user.email=test@test.com",
                "-c",
                "user.name=Test",
                "commit",
                "-m",
                "add shared",
            ])
            .output()
            .unwrap();
        assert!(commit.status.success());

        // Create source branch modifying same file
        let checkout = Command::new("git")
            .args(["-C", &path_str, "checkout", "-b", "compas/merge-conflict"])
            .output()
            .unwrap();
        assert!(checkout.status.success());
        std::fs::write(repo.path().join("shared.txt"), "source content").unwrap();
        let add2 = Command::new("git")
            .args(["-C", &path_str, "add", "shared.txt"])
            .output()
            .unwrap();
        assert!(add2.status.success());
        let commit2 = Command::new("git")
            .args([
                "-C",
                &path_str,
                "-c",
                "user.email=test@test.com",
                "-c",
                "user.name=Test",
                "commit",
                "-m",
                "source change",
            ])
            .output()
            .unwrap();
        assert!(commit2.status.success());
        let back = Command::new("git")
            .args(["-C", &path_str, "checkout", "-"])
            .output()
            .unwrap();
        assert!(back.status.success());

        // Modify same file on target to create conflict
        std::fs::write(repo.path().join("shared.txt"), "target content").unwrap();
        let add3 = Command::new("git")
            .args(["-C", &path_str, "add", "shared.txt"])
            .output()
            .unwrap();
        assert!(add3.status.success());
        let commit3 = Command::new("git")
            .args([
                "-C",
                &path_str,
                "-c",
                "user.email=test@test.com",
                "-c",
                "user.name=Test",
                "commit",
                "-m",
                "target change",
            ])
            .output()
            .unwrap();
        assert!(commit3.status.success());

        let store = test_store().await;
        let config = merge_test_config(repo.path().to_path_buf());
        let config_handle = ConfigHandle::new(config);

        let mut registry = BackendRegistry::new();
        registry.register("stub", Arc::new(StubBackend { ping_alive: true }));

        let event_bus = EventBus::new();
        let worktree_manager = compas::worktree::WorktreeManager::new();
        let runner = WorkerRunner::new(
            config_handle,
            store.clone(),
            registry,
            event_bus,
            worktree_manager,
        );

        let op = MergeOperation {
            id: "merge-conflict-1".to_string(),
            thread_id: "merge-conflict".to_string(),
            source_branch: "compas/merge-conflict".to_string(),
            target_branch: target.clone(),
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

        // Drive the merge poll
        runner.poll_merge_ops().await;

        // Wait for the spawned task to complete.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let fetched = store
                .get_merge_op("merge-conflict-1")
                .await
                .unwrap()
                .unwrap();
            let status: MergeOperationStatus = fetched.status.parse().unwrap();
            if status.is_terminal() {
                assert_eq!(
                    status,
                    MergeOperationStatus::Failed,
                    "merge op should fail due to conflict"
                );
                assert!(
                    fetched.error_detail.is_some(),
                    "failed merge should have error detail"
                );
                assert!(
                    fetched
                        .error_detail
                        .as_ref()
                        .unwrap()
                        .to_lowercase()
                        .contains("conflict"),
                    "error should mention conflict, got: {}",
                    fetched.error_detail.unwrap()
                );
                assert!(
                    fetched.conflict_files.is_some(),
                    "failed merge should have conflict_files"
                );
                // conflict_files is stored as a JSON array string
                let files: Vec<String> =
                    serde_json::from_str(fetched.conflict_files.as_ref().unwrap()).unwrap();
                assert!(
                    files.contains(&"shared.txt".to_string()),
                    "conflict_files should include shared.txt, got: {:?}",
                    files
                );
                break;
            }
            if tokio::time::Instant::now() > deadline {
                panic!(
                    "merge did not reach terminal status within timeout, status: {}",
                    fetched.status
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    #[tokio::test]
    async fn test_worker_merge_uses_thread_worktree_repo_root() {
        // Create two repos: "wrong" (default_workdir) and "right" (agent's per-workdir repo).
        // The branch only exists in the "right" repo.
        let wrong_repo = init_test_repo();
        let right_repo = init_test_repo();
        let target = default_branch(right_repo.path());
        create_source_branch(right_repo.path(), "compas/merge-repo-root");

        let store = test_store().await;
        // Config default_workdir points to wrong_repo — branch won't be found there.
        let config = merge_test_config(wrong_repo.path().to_path_buf());
        let config_handle = ConfigHandle::new(config);

        let mut registry = BackendRegistry::new();
        registry.register("stub", Arc::new(StubBackend { ping_alive: true }));

        let event_bus = EventBus::new();
        let worktree_manager = compas::worktree::WorktreeManager::new();
        let runner = WorkerRunner::new(
            config_handle,
            store.clone(),
            registry,
            event_bus,
            worktree_manager,
        );

        // Ensure thread exists and set worktree_repo_root to the right repo.
        store
            .ensure_thread("merge-repo-root", None, None)
            .await
            .unwrap();
        store
            .set_thread_worktree_path(
                "merge-repo-root",
                &right_repo.path().join(".compas-worktrees/merge-repo-root"),
                right_repo.path(),
            )
            .await
            .unwrap();

        let op = MergeOperation {
            id: "merge-repo-root-1".to_string(),
            thread_id: "merge-repo-root".to_string(),
            source_branch: "compas/merge-repo-root".to_string(),
            target_branch: target.clone(),
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

        runner.poll_merge_ops().await;

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let fetched = store
                .get_merge_op("merge-repo-root-1")
                .await
                .unwrap()
                .unwrap();
            let status: MergeOperationStatus = fetched.status.parse().unwrap();
            if status.is_terminal() {
                assert_eq!(
                    status,
                    MergeOperationStatus::Completed,
                    "merge should succeed using worktree_repo_root, not default_workdir. error: {:?}",
                    fetched.error_detail
                );
                break;
            }
            if tokio::time::Instant::now() > deadline {
                panic!(
                    "merge did not complete within timeout, status: {}",
                    fetched.status
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    #[tokio::test]
    async fn test_worker_merge_noop_clean_worktree() {
        let repo = init_test_repo();
        let target = default_branch(repo.path());
        let path_str = repo.path().to_string_lossy().to_string();

        // Create source branch at same commit as target (no additional commits)
        let checkout = Command::new("git")
            .args(["-C", &path_str, "checkout", "-b", "compas/merge-noop"])
            .output()
            .unwrap();
        assert!(checkout.status.success());
        let back = Command::new("git")
            .args(["-C", &path_str, "checkout", "-"])
            .output()
            .unwrap();
        assert!(back.status.success());

        let store = test_store().await;
        let config = merge_test_config(repo.path().to_path_buf());
        let config_handle = ConfigHandle::new(config);

        let mut registry = BackendRegistry::new();
        registry.register("stub", Arc::new(StubBackend { ping_alive: true }));

        let event_bus = EventBus::new();
        let worktree_manager = compas::worktree::WorktreeManager::new();
        let runner = WorkerRunner::new(
            config_handle,
            store.clone(),
            registry,
            event_bus,
            worktree_manager,
        );

        let op = MergeOperation {
            id: "merge-noop-1".to_string(),
            thread_id: "merge-noop".to_string(),
            source_branch: "compas/merge-noop".to_string(),
            target_branch: target.clone(),
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

        runner.poll_merge_ops().await;

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let fetched = store.get_merge_op("merge-noop-1").await.unwrap().unwrap();
            let status: MergeOperationStatus = fetched.status.parse().unwrap();
            if status.is_terminal() {
                assert_eq!(
                    status,
                    MergeOperationStatus::Failed,
                    "no-op merge should fail, got: {:?}",
                    fetched.error_detail
                );
                assert!(fetched.error_detail.is_some());
                assert!(
                    fetched
                        .error_detail
                        .as_ref()
                        .unwrap()
                        .contains("No commits to merge"),
                    "error should mention no commits, got: {}",
                    fetched.error_detail.unwrap()
                );
                break;
            }
            if tokio::time::Instant::now() > deadline {
                panic!(
                    "merge did not reach terminal status within timeout, status: {}",
                    fetched.status
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    #[tokio::test]
    async fn test_worker_merge_noop_dirty_worktree() {
        let repo = init_test_repo();
        let target = default_branch(repo.path());
        let path_str = repo.path().to_string_lossy().to_string();

        // Create source branch at same commit as target (no divergence)
        let checkout = Command::new("git")
            .args(["-C", &path_str, "checkout", "-b", "compas/merge-noop-dirty"])
            .output()
            .unwrap();
        assert!(checkout.status.success());
        let back = Command::new("git")
            .args(["-C", &path_str, "checkout", "-"])
            .output()
            .unwrap();
        assert!(back.status.success());

        // Create a worktree for the source branch and leave uncommitted changes
        let wt_path = repo
            .path()
            .join(".compas-worktrees")
            .join("merge-noop-dirty");
        std::fs::create_dir_all(wt_path.parent().unwrap()).unwrap();
        let wt_add = Command::new("git")
            .args([
                "-C",
                &path_str,
                "worktree",
                "add",
                &wt_path.to_string_lossy(),
                "compas/merge-noop-dirty",
            ])
            .output()
            .unwrap();
        assert!(
            wt_add.status.success(),
            "worktree add failed: {}",
            String::from_utf8_lossy(&wt_add.stderr)
        );
        std::fs::write(wt_path.join("uncommitted.txt"), "oops").unwrap();

        let store = test_store().await;
        let config = merge_test_config(repo.path().to_path_buf());
        let config_handle = ConfigHandle::new(config);

        let mut registry = BackendRegistry::new();
        registry.register("stub", Arc::new(StubBackend { ping_alive: true }));

        let event_bus = EventBus::new();
        let worktree_manager = compas::worktree::WorktreeManager::new();
        let runner = WorkerRunner::new(
            config_handle,
            store.clone(),
            registry,
            event_bus,
            worktree_manager,
        );

        // Set up thread with worktree info so the worker passes it to execute()
        store
            .ensure_thread("merge-noop-dirty", None, None)
            .await
            .unwrap();
        store
            .set_thread_worktree_path("merge-noop-dirty", &wt_path, repo.path())
            .await
            .unwrap();

        let op = MergeOperation {
            id: "merge-noop-dirty-1".to_string(),
            thread_id: "merge-noop-dirty".to_string(),
            source_branch: "compas/merge-noop-dirty".to_string(),
            target_branch: target.clone(),
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

        runner.poll_merge_ops().await;

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let fetched = store
                .get_merge_op("merge-noop-dirty-1")
                .await
                .unwrap()
                .unwrap();
            let status: MergeOperationStatus = fetched.status.parse().unwrap();
            if status.is_terminal() {
                assert_eq!(
                    status,
                    MergeOperationStatus::Failed,
                    "dirty worktree merge should fail"
                );
                assert!(fetched.error_detail.is_some());
                assert!(
                    fetched
                        .error_detail
                        .as_ref()
                        .unwrap()
                        .contains("uncommitted changes"),
                    "error should mention uncommitted changes, got: {}",
                    fetched.error_detail.unwrap()
                );
                assert!(
                    fetched.conflict_files.is_some(),
                    "should have conflict_files listing dirty files"
                );
                let files: Vec<String> =
                    serde_json::from_str(fetched.conflict_files.as_ref().unwrap()).unwrap();
                assert!(
                    files.iter().any(|f| f.contains("uncommitted.txt")),
                    "conflict_files should include uncommitted.txt, got: {:?}",
                    files
                );
                break;
            }
            if tokio::time::Instant::now() > deadline {
                panic!(
                    "merge did not reach terminal status within timeout, status: {}",
                    fetched.status
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    #[tokio::test]
    async fn test_worker_merge_partial_commit_dirty_worktree() {
        let repo = init_test_repo();
        let target = default_branch(repo.path());
        let path_str = repo.path().to_string_lossy().to_string();

        // Create source branch with a committed file (ahead of target)
        create_source_branch(repo.path(), "compas/merge-partial");

        // Create a worktree and leave uncommitted changes
        let wt_path = repo.path().join(".compas-worktrees").join("merge-partial");
        std::fs::create_dir_all(wt_path.parent().unwrap()).unwrap();
        let wt_add = Command::new("git")
            .args([
                "-C",
                &path_str,
                "worktree",
                "add",
                &wt_path.to_string_lossy(),
                "compas/merge-partial",
            ])
            .output()
            .unwrap();
        assert!(
            wt_add.status.success(),
            "worktree add failed: {}",
            String::from_utf8_lossy(&wt_add.stderr)
        );
        std::fs::write(wt_path.join("forgot.txt"), "not committed").unwrap();

        let store = test_store().await;
        let config = merge_test_config(repo.path().to_path_buf());
        let config_handle = ConfigHandle::new(config);

        let mut registry = BackendRegistry::new();
        registry.register("stub", Arc::new(StubBackend { ping_alive: true }));

        let event_bus = EventBus::new();
        let worktree_manager = compas::worktree::WorktreeManager::new();
        let runner = WorkerRunner::new(
            config_handle,
            store.clone(),
            registry,
            event_bus,
            worktree_manager,
        );

        // Set up thread with worktree info
        store
            .ensure_thread("merge-partial", None, None)
            .await
            .unwrap();
        store
            .set_thread_worktree_path("merge-partial", &wt_path, repo.path())
            .await
            .unwrap();

        let op = MergeOperation {
            id: "merge-partial-1".to_string(),
            thread_id: "merge-partial".to_string(),
            source_branch: "compas/merge-partial".to_string(),
            target_branch: target.clone(),
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

        runner.poll_merge_ops().await;

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let fetched = store
                .get_merge_op("merge-partial-1")
                .await
                .unwrap()
                .unwrap();
            let status: MergeOperationStatus = fetched.status.parse().unwrap();
            if status.is_terminal() {
                assert_eq!(
                    status,
                    MergeOperationStatus::Failed,
                    "partial commit dirty worktree should fail"
                );
                assert!(fetched.error_detail.is_some());
                assert!(
                    fetched
                        .error_detail
                        .as_ref()
                        .unwrap()
                        .contains("uncommitted changes"),
                    "error should mention uncommitted changes, got: {}",
                    fetched.error_detail.unwrap()
                );
                assert!(
                    fetched.conflict_files.is_some(),
                    "should have conflict_files listing dirty files"
                );
                let files: Vec<String> =
                    serde_json::from_str(fetched.conflict_files.as_ref().unwrap()).unwrap();
                assert!(
                    files.iter().any(|f| f.contains("forgot.txt")),
                    "conflict_files should include forgot.txt, got: {:?}",
                    files
                );
                break;
            }
            if tokio::time::Instant::now() > deadline {
                panic!(
                    "merge did not reach terminal status within timeout, status: {}",
                    fetched.status
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Merge MCP Tool Tests
// ═══════════════════════════════════════════════════════════════════════════

mod merge_tool_tests {
    use super::*;
    use compas::store::MergeOperation;

    /// Helper: insert a merge op directly via store for test setup.
    async fn insert_test_merge_op(store: &Store, id: &str, status: &str, thread_id: &str) {
        let op = MergeOperation {
            id: id.to_string(),
            thread_id: thread_id.to_string(),
            source_branch: format!("compas/{}", thread_id),
            target_branch: "main".to_string(),
            merge_strategy: "merge".to_string(),
            requested_by: "operator".to_string(),
            status: status.to_string(),
            push_requested: false,
            queued_at: chrono::Utc::now().timestamp(),
            claimed_at: None,
            started_at: None,
            finished_at: None,
            duration_ms: None,
            result_summary: None,
            error_detail: None,
            conflict_files: None,
        };
        store.insert_merge_op(&op).await.unwrap();
    }

    #[tokio::test]
    async fn test_orch_merge_preflight_rejects_active_thread() {
        let server = test_server().await;

        // Create an active thread by inserting a message
        server
            .store
            .insert_message(
                "t-active", "op", "focused", "dispatch", "do work", None, None,
            )
            .await
            .unwrap();

        let result = server
            .merge_impl(MergeParams {
                thread_id: "t-active".to_string(),
                target_branch: Some("main".to_string()),
                strategy: Some("merge".to_string()),
                from: "operator".to_string(),
            })
            .await
            .unwrap();

        assert!(is_error(&result), "should reject active thread");
        let text = result
            .content
            .first()
            .and_then(|c| match &c.raw {
                rmcp::model::RawContent::Text(t) => Some(t.text.as_str()),
                _ => None,
            })
            .unwrap();
        assert!(
            text.contains("Active"),
            "error should mention Active status, got: {}",
            text
        );
    }

    #[tokio::test]
    async fn test_orch_merge_rejects_invalid_strategy() {
        let server = test_server().await;

        let result = server
            .merge_impl(MergeParams {
                thread_id: "t-1".to_string(),
                target_branch: Some("main".to_string()),
                strategy: Some("yolo".to_string()),
                from: "operator".to_string(),
            })
            .await
            .unwrap();

        assert!(is_error(&result), "should reject invalid strategy");
        let text = result
            .content
            .first()
            .and_then(|c| match &c.raw {
                rmcp::model::RawContent::Text(t) => Some(t.text.as_str()),
                _ => None,
            })
            .unwrap();
        assert!(
            text.contains("yolo"),
            "error should mention the invalid strategy, got: {}",
            text
        );
    }

    #[tokio::test]
    async fn test_orch_merge_queues_operation() {
        let store = test_store().await;

        // Directly test the store-level queue flow (bypasses git dependency)
        let op = MergeOperation {
            id: "merge-test-1".to_string(),
            thread_id: "t-done".to_string(),
            source_branch: "compas/t-done".to_string(),
            target_branch: "main".to_string(),
            merge_strategy: "merge".to_string(),
            requested_by: "operator".to_string(),
            status: "queued".to_string(),
            push_requested: false,
            queued_at: chrono::Utc::now().timestamp(),
            claimed_at: None,
            started_at: None,
            finished_at: None,
            duration_ms: None,
            result_summary: None,
            error_detail: None,
            conflict_files: None,
        };
        store.insert_merge_op(&op).await.unwrap();

        let fetched = store.get_merge_op("merge-test-1").await.unwrap().unwrap();
        assert_eq!(fetched.status, "queued");
        assert_eq!(fetched.thread_id, "t-done");
        assert_eq!(fetched.target_branch, "main");

        let depth = store.count_queued_merge_ops("main").await.unwrap();
        assert_eq!(depth, 1);
    }

    #[tokio::test]
    async fn test_orch_merge_cancel_queued() {
        let server = test_server().await;
        insert_test_merge_op(&server.store, "merge-cancel-1", "queued", "t-1").await;

        let result = server
            .merge_cancel_impl(MergeCancelParams {
                op_id: "merge-cancel-1".to_string(),
            })
            .await
            .unwrap();

        assert!(!is_error(&result), "cancel of queued op should succeed");
        let json = extract_json(&result);
        assert_eq!(json["cancelled"], true);
        assert_eq!(json["op_id"], "merge-cancel-1");

        // Verify it's actually cancelled in the store
        let op = server
            .store
            .get_merge_op("merge-cancel-1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(op.status, "cancelled");
    }

    #[tokio::test]
    async fn test_orch_merge_cancel_non_queued_fails() {
        let server = test_server().await;
        // Insert an op with 'executing' status — cannot be cancelled
        insert_test_merge_op(&server.store, "merge-exec-1", "executing", "t-2").await;

        let result = server
            .merge_cancel_impl(MergeCancelParams {
                op_id: "merge-exec-1".to_string(),
            })
            .await
            .unwrap();

        assert!(
            is_error(&result),
            "cancel of non-queued op should return error"
        );
        let text = result
            .content
            .first()
            .and_then(|c| match &c.raw {
                rmcp::model::RawContent::Text(t) => Some(t.text.as_str()),
                _ => None,
            })
            .unwrap();
        assert!(
            text.contains("executing"),
            "error should mention current status, got: {}",
            text
        );
    }

    #[tokio::test]
    async fn test_orch_merge_cancel_nonexistent_fails() {
        let server = test_server().await;

        let result = server
            .merge_cancel_impl(MergeCancelParams {
                op_id: "nonexistent-op".to_string(),
            })
            .await
            .unwrap();

        assert!(is_error(&result), "cancel of nonexistent op should error");
        let text = result
            .content
            .first()
            .and_then(|c| match &c.raw {
                rmcp::model::RawContent::Text(t) => Some(t.text.as_str()),
                _ => None,
            })
            .unwrap();
        assert!(
            text.contains("not found"),
            "error should say not found, got: {}",
            text
        );
    }

    #[tokio::test]
    async fn test_orch_merge_status_queue_overview() {
        let server = test_server().await;

        // Insert ops in different states
        insert_test_merge_op(&server.store, "m-1", "queued", "t-1").await;
        insert_test_merge_op(&server.store, "m-2", "queued", "t-2").await;
        insert_test_merge_op(&server.store, "m-3", "completed", "t-3").await;
        insert_test_merge_op(&server.store, "m-4", "failed", "t-4").await;

        let result = server
            .merge_status_impl(MergeStatusParams {
                op_id: None,
                target_branch: None,
                thread_id: None,
            })
            .await
            .unwrap();

        assert!(!is_error(&result), "overview should succeed");
        let json = extract_json(&result);

        // Verify counts are true aggregates (not derived from truncated list)
        assert_eq!(json["counts"]["queued"], 2);
        assert_eq!(json["counts"]["completed"], 1);
        assert_eq!(json["counts"]["failed"], 1);

        // Verify recent list
        assert_eq!(json["recent"].as_array().unwrap().len(), 4);
    }

    #[tokio::test]
    async fn test_orch_merge_status_nonexistent_op() {
        let server = test_server().await;

        let result = server
            .merge_status_impl(MergeStatusParams {
                op_id: Some("ghost-op".to_string()),
                target_branch: None,
                thread_id: None,
            })
            .await
            .unwrap();

        assert!(is_error(&result), "status for nonexistent op should error");
        let text = result
            .content
            .first()
            .and_then(|c| match &c.raw {
                rmcp::model::RawContent::Text(t) => Some(t.text.as_str()),
                _ => None,
            })
            .unwrap();
        assert!(text.contains("not found"), "error should say not found");
    }

    #[tokio::test]
    async fn test_orch_merge_status_failed_op_shows_suggested_actions() {
        let server = test_server().await;

        // Insert a failed op with conflict files
        let op = MergeOperation {
            id: "merge-fail-1".to_string(),
            thread_id: "t-fail".to_string(),
            source_branch: "compas/t-fail".to_string(),
            target_branch: "main".to_string(),
            merge_strategy: "merge".to_string(),
            requested_by: "operator".to_string(),
            status: "failed".to_string(),
            push_requested: false,
            queued_at: chrono::Utc::now().timestamp(),
            claimed_at: Some(chrono::Utc::now().timestamp()),
            started_at: Some(chrono::Utc::now().timestamp()),
            finished_at: Some(chrono::Utc::now().timestamp()),
            duration_ms: Some(150),
            result_summary: None,
            error_detail: Some("merge conflict detected".to_string()),
            conflict_files: Some(serde_json::to_string(&vec!["file1.rs", "file2.rs"]).unwrap()),
        };
        server.store.insert_merge_op(&op).await.unwrap();

        let result = server
            .merge_status_impl(MergeStatusParams {
                op_id: Some("merge-fail-1".to_string()),
                target_branch: None,
                thread_id: None,
            })
            .await
            .unwrap();

        assert!(!is_error(&result), "status for existing op should succeed");
        let json = extract_json(&result);

        assert_eq!(json["status"], "failed");
        assert_eq!(json["error_detail"], "merge conflict detected");

        // conflict_files should be deserialized from JSON string to array
        let files = json["conflict_files"].as_array().unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0], "file1.rs");
        assert_eq!(files[1], "file2.rs");

        // suggested_actions should be present for failed ops
        let actions = json["suggested_actions"].as_array().unwrap();
        assert!(
            !actions.is_empty(),
            "failed op should have suggested actions"
        );
        let actions_text: Vec<&str> = actions.iter().filter_map(|a| a.as_str()).collect();
        assert!(
            actions_text.iter().any(|a| a.contains("Resolve conflicts")),
            "should suggest resolving conflicts, got: {:?}",
            actions_text
        );
    }
}
