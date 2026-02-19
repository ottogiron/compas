//! aster-orch binary — worker-based orchestrator.
//!
//! Usage:
//!   aster_orch worker --config .aster-orch/config.yaml
//!   aster_orch mcp-server --config .aster-orch/config.yaml
//!   aster_orch run --config .aster-orch/config.yaml   # unified: worker + MCP

use apalis::prelude::*;
use apalis_core::backend::poll_strategy::{BackoffConfig, IntervalStrategy, StrategyBuilder};
use apalis_sqlite::{Config as SqliteConfig, SqlitePool, SqliteStorage};
use aster_orch::mcp::server::OrchestratorMcpServer;
use aster_orch::worker::context::{build_backend_registry, TriggerContext};
use aster_orch::worker::pipeline;
use aster_orch::worker::trigger::TRIGGER_QUEUE;
use aster_orch::worker::TriggerJob;
use clap::{Parser, Subcommand};
use rmcp::ServiceExt;
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
    /// Run the trigger worker only
    Worker {
        /// Path to config YAML
        #[arg(long, default_value = ".aster-orch/config.yaml")]
        config: PathBuf,
        /// Max concurrent triggers (overrides config; default: worker agent count)
        #[arg(long)]
        concurrency: Option<usize>,
        /// SQLite database path
        #[arg(long, default_value = ".aster-orch/jobs.sqlite")]
        db: PathBuf,
    },
    /// Run the MCP server only (stdio transport)
    McpServer {
        /// Path to config YAML
        #[arg(long, default_value = ".aster-orch/config.yaml")]
        config: PathBuf,
        /// SQLite database path
        #[arg(long, default_value = ".aster-orch/jobs.sqlite")]
        db: PathBuf,
    },
    /// Run worker + MCP server in the same process
    Run {
        /// Path to config YAML
        #[arg(long, default_value = ".aster-orch/config.yaml")]
        config: PathBuf,
        /// Max concurrent triggers (overrides config; default: worker agent count)
        #[arg(long)]
        concurrency: Option<usize>,
        /// SQLite database path
        #[arg(long, default_value = ".aster-orch/jobs.sqlite")]
        db: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Worker {
            config, concurrency, db,
        } => {
            init_tracing();
            run_worker(config, db, concurrency).await?;
        }
        Commands::McpServer { config, db } => {
            // MCP server uses stdio — don't pollute stdout with tracing
            init_tracing_stderr();
            run_mcp_server(config, db).await?;
        }
        Commands::Run {
            config, concurrency, db,
        } => {
            // Unified mode: tracing to stderr since MCP uses stdio
            init_tracing_stderr();
            run_unified(config, db, concurrency).await?;
        }
    }

    Ok(())
}

fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
}

fn init_tracing_stderr() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
}

async fn connect_db(db_path: &PathBuf) -> Result<SqlitePool, Box<dyn std::error::Error>> {
    let db_url = format!("sqlite:{}?mode=rwc", db_path.display());
    let pool = SqlitePool::connect(&db_url).await?;
    SqliteStorage::setup(&pool).await?;
    let store = aster_orch::store::Store::new(pool.clone());
    store.setup().await?;
    Ok(pool)
}

fn build_apalis_config(config: &aster_orch::config::types::OrchestratorConfig) -> SqliteConfig {
    let backoff = BackoffConfig::new(std::time::Duration::from_millis(
        config.apalis.poll_max_backoff_ms,
    ))
    .with_jitter((config.apalis.poll_jitter_pct as f64) / 100.0);
    let strategy = StrategyBuilder::new()
        .apply(
            IntervalStrategy::new(std::time::Duration::from_millis(
                config.apalis.poll_interval_ms,
            ))
            .with_backoff(backoff),
        )
        .build();

    SqliteConfig::new(TRIGGER_QUEUE)
        .set_buffer_size(config.apalis.buffer_size)
        .with_poll_interval(strategy)
}

fn worker_instance_name() -> String {
    format!("trigger-worker-{}", std::process::id())
}

// ---------------------------------------------------------------------------
// Worker mode
// ---------------------------------------------------------------------------

async fn run_worker(
    config_path: PathBuf,
    db_path: PathBuf,
    concurrency_override: Option<usize>,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = aster_orch::config::load_config(&config_path)?;
    tracing::info!(agents = config.agents.len(), "config loaded");

    let concurrency = concurrency_override.unwrap_or_else(|| config.effective_max_concurrent_triggers());

    let backend_registry = build_backend_registry(&config);
    let pool = connect_db(&db_path).await?;
    let store = aster_orch::store::Store::new(pool.clone());
    let db_url = format!("sqlite:{}?mode=rwc", db_path.display());

    let ctx = TriggerContext::new(config, backend_registry, store);
    let worker_name = worker_instance_name();

    tracing::info!(
        concurrency,
        worker_id = %worker_name,
        db = %db_path.display(),
        "starting trigger worker"
    );

    let apalis_config = build_apalis_config(&ctx.config);
    if ctx.config.apalis.listener_enabled {
        let storage: SqliteStorage<TriggerJob, _, _> =
            SqliteStorage::new_with_callback(&db_url, &apalis_config);
        let worker = WorkerBuilder::new(worker_name.clone())
            .backend(storage)
            .data(ctx)
            .concurrency(concurrency)
            .build(|job: TriggerJob, ctx: Data<TriggerContext>| async move {
                let output = pipeline::execute_trigger(job, ctx.clone()).await?;
                let reply = pipeline::parse_reply(output).await?;
                pipeline::dispatch_result(reply, ctx).await?;
                Ok::<(), BoxDynError>(())
            });
        worker.run().await?;
    } else {
        let storage: SqliteStorage<TriggerJob, _, _> =
            SqliteStorage::new_with_config(&pool, &apalis_config);
        let worker = WorkerBuilder::new(worker_name)
            .backend(storage)
            .data(ctx)
            .concurrency(concurrency)
            .build(|job: TriggerJob, ctx: Data<TriggerContext>| async move {
                let output = pipeline::execute_trigger(job, ctx.clone()).await?;
                let reply = pipeline::parse_reply(output).await?;
                pipeline::dispatch_result(reply, ctx).await?;
                Ok::<(), BoxDynError>(())
            });
        worker.run().await?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// MCP server mode
// ---------------------------------------------------------------------------

async fn run_mcp_server(
    config_path: PathBuf,
    db_path: PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = aster_orch::config::load_config(&config_path)?;
    tracing::info!(agents = config.agents.len(), "config loaded");

    let backend_registry = build_backend_registry(&config);
    let pool = connect_db(&db_path).await?;
    let store = aster_orch::store::Store::new(pool.clone());

    let server = OrchestratorMcpServer::new(config, store, backend_registry);

    tracing::info!("starting MCP server on stdio");
    let transport = rmcp::transport::io::stdio();
    let running = server.serve(transport).await?;
    running.waiting().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Unified mode (worker + MCP)
// ---------------------------------------------------------------------------

async fn run_unified(
    config_path: PathBuf,
    db_path: PathBuf,
    concurrency_override: Option<usize>,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = aster_orch::config::load_config(&config_path)?;
    tracing::info!(agents = config.agents.len(), "config loaded");

    let concurrency = concurrency_override.unwrap_or_else(|| config.effective_max_concurrent_triggers());

    let backend_registry = build_backend_registry(&config);
    let pool = connect_db(&db_path).await?;
    let store = aster_orch::store::Store::new(pool.clone());
    let db_url = format!("sqlite:{}?mode=rwc", db_path.display());

    // Worker setup
    let apalis_config = build_apalis_config(&config);
    let listener_enabled = config.apalis.listener_enabled;
    let worker_ctx = TriggerContext::new(
        config.clone(),
        build_backend_registry(&config),
        store.clone(),
    );
    let worker_name = worker_instance_name();

    // MCP server setup
    let server = OrchestratorMcpServer::new(config, store, backend_registry);

    tracing::info!(
        concurrency,
        worker_id = %worker_name,
        db = %db_path.display(),
        "starting unified mode (worker + MCP)"
    );

    // Run worker in background
    let worker_handle = if listener_enabled {
        let storage: SqliteStorage<TriggerJob, _, _> =
            SqliteStorage::new_with_callback(&db_url, &apalis_config);
        let worker = WorkerBuilder::new(worker_name.clone())
            .backend(storage)
            .data(worker_ctx)
            .concurrency(concurrency)
            .build(|job: TriggerJob, ctx: Data<TriggerContext>| async move {
                let output = pipeline::execute_trigger(job, ctx.clone()).await?;
                let reply = pipeline::parse_reply(output).await?;
                pipeline::dispatch_result(reply, ctx).await?;
                Ok::<(), BoxDynError>(())
            });
        tokio::spawn(async move {
            if let Err(e) = worker.run().await {
                tracing::error!(error = %e, "worker exited with error");
            }
        })
    } else {
        let storage: SqliteStorage<TriggerJob, _, _> =
            SqliteStorage::new_with_config(&pool, &apalis_config);
        let worker = WorkerBuilder::new(worker_name)
            .backend(storage)
            .data(worker_ctx)
            .concurrency(concurrency)
            .build(|job: TriggerJob, ctx: Data<TriggerContext>| async move {
                let output = pipeline::execute_trigger(job, ctx.clone()).await?;
                let reply = pipeline::parse_reply(output).await?;
                pipeline::dispatch_result(reply, ctx).await?;
                Ok::<(), BoxDynError>(())
            });
        tokio::spawn(async move {
            if let Err(e) = worker.run().await {
                tracing::error!(error = %e, "worker exited with error");
            }
        })
    };

    // MCP server on stdio in foreground
    let transport = rmcp::transport::io::stdio();
    let running = server.serve(transport).await?;
    running.waiting().await?;

    // MCP disconnected — shut down worker
    worker_handle.abort();
    Ok(())
}
