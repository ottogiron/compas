//! aster-orch binary — two-process orchestrator.
//!
//! Usage:
//!   aster_orch worker --config .aster-orch/config.yaml
//!   aster_orch mcp-server --config .aster-orch/config.yaml

use aster_orch::backend::claude::ClaudeCodeBackend;
use aster_orch::backend::codex::CodexBackend;
use aster_orch::backend::gemini::GeminiBackend;
use aster_orch::backend::opencode::OpenCodeBackend;
use aster_orch::backend::registry::BackendRegistry;
use aster_orch::mcp::server::OrchestratorMcpServer;
use aster_orch::worker::WorkerRunner;
use clap::{Parser, Subcommand};
use rmcp::ServiceExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
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
        #[arg(long)]
        config: PathBuf,
    },
    /// Run the MCP server only (stdio transport)
    McpServer {
        /// Path to config YAML
        #[arg(long)]
        config: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Worker { config } => {
            init_tracing();
            run_worker(config).await?;
        }
        Commands::McpServer { config } => {
            // MCP server uses stdio — don't pollute stdout with tracing
            init_tracing_stderr();
            run_mcp_server(config).await?;
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

async fn connect_db(
    db_path: &PathBuf,
    config: &aster_orch::config::types::OrchestratorConfig,
) -> Result<sqlx::SqlitePool, Box<dyn std::error::Error>> {
    let db_url = format!("sqlite:{}?mode=rwc", db_path.display());
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(config.apalis.db_max_connections)
        .min_connections(config.apalis.db_min_connections)
        .acquire_timeout(Duration::from_millis(config.apalis.db_acquire_timeout_ms))
        .connect(&db_url)
        .await?;
    let store = aster_orch::store::Store::new(pool.clone());
    store.setup().await?;
    Ok(pool)
}

fn resolve_db_path(
    config_path: &PathBuf,
    config: &aster_orch::config::types::OrchestratorConfig,
) -> PathBuf {
    let raw = &config.db_path;
    if raw.is_absolute() {
        return raw.clone();
    }
    if let Some(as_str) = raw.to_str() {
        if as_str == "~" {
            if let Ok(home) = std::env::var("HOME") {
                return PathBuf::from(home);
            }
        }
        if let Some(rest) = as_str.strip_prefix("~/") {
            if let Ok(home) = std::env::var("HOME") {
                return PathBuf::from(home).join(rest);
            }
        }
    }
    let base = config_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    base.join(raw)
}

fn resolve_config_path(config_path: &PathBuf) -> PathBuf {
    std::fs::canonicalize(config_path).unwrap_or_else(|_| config_path.clone())
}

fn build_backend_registry(
    config: &aster_orch::config::types::OrchestratorConfig,
) -> BackendRegistry {
    let mut registry = BackendRegistry::new();

    // Determine workdir from config state_dir
    let workdir = Some(config.state_dir.clone());

    // Register all known backends
    registry.register("claude", Arc::new(ClaudeCodeBackend::new()));
    registry.register("codex", Arc::new(CodexBackend::new(workdir)));
    registry.register("opencode", Arc::new(OpenCodeBackend::new()));
    registry.register("gemini", Arc::new(GeminiBackend::new()));

    registry
}

// ---------------------------------------------------------------------------
// Worker mode
// ---------------------------------------------------------------------------

async fn run_worker(config_path: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let resolved_config_path = resolve_config_path(&config_path);
    let config = aster_orch::config::load_config(&config_path)?;
    let db_path = resolve_db_path(&config_path, &config);
    tracing::info!(
        agents = config.agents.len(),
        config = %resolved_config_path.display(),
        db = %db_path.display(),
        "config loaded"
    );

    let backend_registry = build_backend_registry(&config);
    let pool = connect_db(&db_path, &config).await?;
    let store = aster_orch::store::Store::new(pool);

    let runner = WorkerRunner::new(config, store, backend_registry);

    tracing::info!(
        db = %db_path.display(),
        "starting trigger worker"
    );

    runner.run().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// MCP server mode
// ---------------------------------------------------------------------------

async fn run_mcp_server(config_path: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let resolved_config_path = resolve_config_path(&config_path);
    let config = aster_orch::config::load_config(&config_path)?;
    let db_path = resolve_db_path(&config_path, &config);
    tracing::info!(
        agents = config.agents.len(),
        config = %resolved_config_path.display(),
        db = %db_path.display(),
        "config loaded"
    );

    let backend_registry = build_backend_registry(&config);
    let pool = connect_db(&db_path, &config).await?;
    let store = aster_orch::store::Store::new(pool);

    let server = OrchestratorMcpServer::new(config, store, backend_registry);

    tracing::info!("starting MCP server on stdio");
    let transport = rmcp::transport::io::stdio();
    let running = server.serve(transport).await?;
    running.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::Cli;
    use clap::Parser;

    #[test]
    fn test_worker_requires_config_flag() {
        let parsed = Cli::try_parse_from(["aster-orch", "worker"]);
        assert!(parsed.is_err());
    }

    #[test]
    fn test_mcp_server_requires_config_flag() {
        let parsed = Cli::try_parse_from(["aster-orch", "mcp-server"]);
        assert!(parsed.is_err());
    }
}
