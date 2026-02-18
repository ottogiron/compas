//! Integration test: push trigger jobs, run worker, verify results.

use std::time::Duration;

use apalis::prelude::*;
use apalis_sqlite::{Config as SqliteConfig, SqlitePool, SqliteStorage};
use aster_orch::backend::registry::BackendRegistry;
use aster_orch::config::types::{AgentConfig, AgentRole, OrchestratorConfig, OrchestrationConfig};
use aster_orch::testing::StubBackend;
use aster_orch::worker::context::TriggerContext;
use aster_orch::worker::pipeline;
use aster_orch::worker::TriggerJob;
use futures::stream;
use std::sync::Arc;

fn test_config() -> OrchestratorConfig {
    OrchestratorConfig {
        state_dir: "/tmp/test-orch".into(),
        poll_interval_secs: 1,
        models: None,
        agents: vec![
            AgentConfig {
                alias: "focused".into(),
                identity: "Test Agent".into(),
                backend: "stub".into(),
                role: AgentRole::Worker,
                model: None,
                models: None,
                preferred_models: None,
                prompt: None,
                prompt_file: None,
                timeout_secs: Some(30),
                backend_args: None,
                env: None,
            },
            AgentConfig {
                alias: "chill".into(),
                identity: "Test Agent 2".into(),
                backend: "stub".into(),
                role: AgentRole::Worker,
                model: None,
                models: None,
                preferred_models: None,
                prompt: None,
                prompt_file: None,
                timeout_secs: Some(30),
                backend_args: None,
                env: None,
            },
            AgentConfig {
                alias: "spark".into(),
                identity: "Test Agent 3".into(),
                backend: "stub".into(),
                role: AgentRole::Worker,
                model: None,
                models: None,
                preferred_models: None,
                prompt: None,
                prompt_file: None,
                timeout_secs: Some(30),
                backend_args: None,
                env: None,
            },
        ],
        orchestration: OrchestrationConfig::default(),
        telegram: None,
        audit_log_path: None,
    }
}

#[tokio::test]
async fn test_worker_processes_trigger_jobs() {
    // Setup in-memory SQLite
    let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
    SqliteStorage::setup(&pool).await.unwrap();

    let store = aster_orch::store::Store::new(pool.clone());
    store.setup().await.unwrap();

    let apalis_config = SqliteConfig::new("trigger-queue").set_buffer_size(10);
    let mut storage: SqliteStorage<TriggerJob, _, _> =
        SqliteStorage::new_with_config(&pool, &apalis_config);

    // Build test context with stub backend that returns parseable JSON reply
    let config = test_config();
    let mut registry = BackendRegistry::new();
    let stub = StubBackend {
        trigger_success: true,
        trigger_output: Some(
            r#"{"intent":"review-request","to":"operator","body":"Done with the task"}"#.into(),
        ),
    };
    registry.register("stub", Arc::new(stub));
    let ctx = TriggerContext::new(config, registry, store.clone());

    // Ensure threads exist
    for tid in ["t-001", "t-002", "t-003"] {
        store.ensure_thread(tid, None).await.unwrap();
    }

    // Push 3 test jobs
    let jobs = vec![
        TriggerJob {
            thread_id: "t-001".into(),
            agent_alias: "focused".into(),
            message_body: "Implement feature X".into(),
            from_alias: "operator".into(),
            intent: "dispatch".into(),
            batch_id: Some("TICKET-1".into()),
        },
        TriggerJob {
            thread_id: "t-002".into(),
            agent_alias: "chill".into(),
            message_body: "Update docs".into(),
            from_alias: "operator".into(),
            intent: "dispatch".into(),
            batch_id: Some("TICKET-1".into()),
        },
        TriggerJob {
            thread_id: "t-003".into(),
            agent_alias: "spark".into(),
            message_body: "Fix CI".into(),
            from_alias: "operator".into(),
            intent: "dispatch".into(),
            batch_id: None,
        },
    ];
    let mut job_stream = stream::iter(jobs);
    storage.push_stream(&mut job_stream).await.unwrap();

    // Auto-stop when all jobs complete
    let stop_pool = pool.clone();
    let stop_queue = apalis_config.queue().to_string();
    let stop_store = store.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let count: (i64,) = sqlx::query_as(
                "SELECT COUNT(*) FROM Jobs WHERE status IN ('Pending', 'Running') AND job_type = ?",
            )
            .bind(&stop_queue)
            .fetch_one(&stop_pool)
            .await
            .unwrap();
            if count.0 == 0 {
                tokio::time::sleep(Duration::from_millis(300)).await;
                let recheck: (i64,) = sqlx::query_as(
                    "SELECT COUNT(*) FROM Jobs WHERE status IN ('Pending', 'Running') AND job_type = ?",
                )
                .bind(&stop_queue)
                .fetch_one(&stop_pool)
                .await
                .unwrap();
                if recheck.0 == 0 {
                    // Verify thread statuses were updated to ReviewPending
                    // (StubBackend returns a review-request JSON reply)
                    for tid in ["t-001", "t-002", "t-003"] {
                        let status = stop_store.get_thread_status(tid).await.unwrap();
                        assert_eq!(
                            status.as_deref(),
                            Some("ReviewPending"),
                            "thread {} should be ReviewPending",
                            tid
                        );
                    }

                    std::process::exit(0);
                }
            }
        }
    });

    let worker = WorkerBuilder::new("test-worker")
        .backend(storage)
        .data(ctx)
        .concurrency(2)
        .build(|job: TriggerJob, ctx: Data<TriggerContext>| async move {
            let output = pipeline::execute_trigger(job, ctx.clone()).await?;
            let reply = pipeline::parse_reply(output).await?;
            let _reply = pipeline::dispatch_result(reply, ctx).await?;
            Ok::<(), BoxDynError>(())
        });

    worker.run().await.unwrap();
}
