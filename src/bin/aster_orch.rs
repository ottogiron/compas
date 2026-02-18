//! aster-orch binary — worker-based orchestrator.
//!
//! Usage:
//!   aster_orch worker --config .aster-orch/config.yaml
//!   aster_orch worker --config .aster-orch/config.yaml --concurrency 3

use apalis::prelude::*;
use apalis_sqlite::{Config as SqliteConfig, SqlitePool, SqliteStorage};
use aster_orch::worker::pipeline;
use aster_orch::worker::TriggerJob;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "aster-orch", about = "Agent orchestrator")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run the trigger worker
    Worker {
        /// Path to config YAML
        #[arg(long, default_value = ".aster-orch/config.yaml")]
        config: PathBuf,
        /// Max concurrent triggers
        #[arg(long, default_value = "2")]
        concurrency: usize,
        /// SQLite database path
        #[arg(long, default_value = ".aster-orch/jobs.sqlite")]
        db: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Worker {
            config: _config_path,
            concurrency,
            db,
        } => {
            run_worker(db, concurrency).await?;
        }
    }

    Ok(())
}

async fn run_worker(
    db_path: PathBuf,
    concurrency: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    // Connect to SQLite
    let db_url = format!("sqlite:{}?mode=rwc", db_path.display());
    let pool = SqlitePool::connect(&db_url).await?;
    SqliteStorage::setup(&pool).await?;

    // Set up our companion threads table
    let store = aster_orch::store::Store::new(pool.clone());
    store.setup().await?;

    // Configure apalis storage
    let config = SqliteConfig::new("trigger-queue").set_buffer_size(10);
    let storage: SqliteStorage<TriggerJob, _, _> = SqliteStorage::new_with_config(&pool, &config);

    tracing::info!(concurrency, db = %db_path.display(), "starting trigger worker");

    // Flat handler that runs the full pipeline inline.
    // Each job: execute → parse → dispatch, all in one handler call.
    // apalis manages concurrency, claiming, heartbeat, and orphan recovery.
    async fn handle_trigger(job: TriggerJob) -> Result<(), BoxDynError> {
        // Step 1: Execute trigger
        let output = pipeline::execute_trigger(job).await?;
        // Step 2: Parse reply
        let reply = pipeline::parse_reply(output).await?;
        // Step 3: Dispatch result
        pipeline::dispatch_result(reply).await?;
        Ok(())
    }

    let worker = WorkerBuilder::new("aster-trigger-worker")
        .backend(storage)
        .concurrency(concurrency)
        .on_event(|_ctx, ev| match ev {
            Event::Error(err) => tracing::error!(?err, "worker error"),
            Event::Start => tracing::info!("worker started"),
            Event::Stop => tracing::info!("worker stopped"),
            _ => {}
        })
        .build(handle_trigger);

    worker.run().await?;

    Ok(())
}
