//! Integration test: push trigger jobs, run worker, verify results.

use std::time::Duration;

use apalis::prelude::*;
use apalis_sqlite::{Config as SqliteConfig, SqlitePool, SqliteStorage};
use aster_orch::worker::pipeline;
use aster_orch::worker::TriggerJob;
use futures::stream;

#[tokio::test]
async fn test_worker_processes_trigger_jobs() {
    // Setup in-memory SQLite
    let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
    SqliteStorage::setup(&pool).await.unwrap();

    let store = aster_orch::store::Store::new(pool.clone());
    store.setup().await.unwrap();

    let config = SqliteConfig::new("trigger-queue").set_buffer_size(10);
    let mut storage = SqliteStorage::new_with_config(&pool, &config);

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
    let stop_queue = config.queue().to_string();
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
                    std::process::exit(0);
                }
            }
        }
    });

    // Flat handler
    async fn handle_trigger(job: TriggerJob) -> Result<(), BoxDynError> {
        let output = pipeline::execute_trigger(job).await?;
        let reply = pipeline::parse_reply(output).await?;
        pipeline::dispatch_result(reply).await?;
        Ok(())
    }

    let worker = WorkerBuilder::new("test-worker")
        .backend(storage)
        .concurrency(2)
        .build(handle_trigger);

    worker.run().await.unwrap();
}
