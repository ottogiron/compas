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
use aster_orch::wait::{self, WaitOutcome, WaitRequest};
use aster_orch::worker::WorkerRunner;
use clap::{Parser, Subcommand};
use rmcp::ServiceExt;
use std::path::PathBuf;
use std::process::ExitCode;
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
    /// Launch the TUI dashboard (reads SQLite directly, no MCP required)
    Dashboard {
        /// Path to config YAML
        #[arg(long)]
        config: PathBuf,
        /// How often (in seconds) to re-query SQLite for fresh metrics
        #[arg(long, default_value = "2")]
        poll_interval: u64,
    },
    /// Wait for a message on a thread (reads SQLite directly, no MCP required).
    ///
    /// Exits 0 when a matching message is found, 1 on timeout, 2 on error.
    /// Output is key=value lines on stdout for easy bash parsing.
    Wait {
        /// Path to config YAML
        #[arg(long)]
        config: PathBuf,
        /// Thread ID to wait on
        #[arg(long)]
        thread_id: String,
        /// Wait for a specific intent (e.g. "review-request", "approved")
        #[arg(long)]
        intent: Option<String>,
        /// Message cursor — only consider messages newer than this (db:<id> or numeric)
        #[arg(long)]
        since: Option<String>,
        /// Only consider messages created after this command starts
        #[arg(long)]
        strict_new: bool,
        /// Timeout in seconds (default 120)
        #[arg(long, default_value = "120")]
        timeout: u64,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    match cli.command {
        Commands::Worker { config } => {
            init_tracing();
            if let Err(e) = run_worker(config).await {
                eprintln!("error: {}", e);
                return ExitCode::from(2);
            }
        }
        Commands::McpServer { config } => {
            // MCP server uses stdio — don't pollute stdout with tracing
            init_tracing_stderr();
            if let Err(e) = run_mcp_server(config).await {
                eprintln!("error: {}", e);
                return ExitCode::from(2);
            }
        }
        Commands::Dashboard {
            config,
            poll_interval,
        } => {
            // TUI dashboard — no tracing to stdout (would corrupt the TUI)
            if let Err(e) = run_dashboard(config, poll_interval).await {
                eprintln!("error: {}", e);
                return ExitCode::from(2);
            }
        }
        Commands::Wait {
            config,
            thread_id,
            intent,
            since,
            strict_new,
            timeout,
        } => {
            // Wait outputs key=value to stdout — no tracing there
            return run_wait(config, thread_id, intent, since, strict_new, timeout).await;
        }
    }

    ExitCode::SUCCESS
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
        .max_connections(config.database.max_connections)
        .min_connections(config.database.min_connections)
        .acquire_timeout(Duration::from_millis(config.database.acquire_timeout_ms))
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

// ---------------------------------------------------------------------------
// Dashboard mode
// ---------------------------------------------------------------------------

async fn run_dashboard(
    config_path: PathBuf,
    poll_interval: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = aster_orch::config::load_config(&config_path)?;
    let db_path = resolve_db_path(&config_path, &config);
    let pool = connect_db(&db_path, &config).await?;
    let store = aster_orch::store::Store::new(pool);

    // Capture the runtime handle before entering the blocking thread.
    // The TUI uses it to drive async store queries via Handle::block_on.
    let handle = tokio::runtime::Handle::current();

    // Resolve config path for display in the Settings tab.
    let resolved_config_path = resolve_config_path(&config_path);

    // The TUI event loop is synchronous (crossterm blocking I/O).
    // Run it in a dedicated blocking thread so the tokio runtime stays healthy.
    tokio::task::spawn_blocking(move || {
        aster_orch::dashboard::app::run_tui(
            store,
            config,
            resolved_config_path,
            handle,
            poll_interval,
        )
    })
    .await??;

    Ok(())
}

// ---------------------------------------------------------------------------
// Wait mode
// ---------------------------------------------------------------------------

async fn run_wait(
    config_path: PathBuf,
    thread_id: String,
    intent: Option<String>,
    since: Option<String>,
    strict_new: bool,
    timeout: u64,
) -> ExitCode {
    let config = match aster_orch::config::load_config(&config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: failed to load config: {}", e);
            return ExitCode::from(2);
        }
    };
    let db_path = resolve_db_path(&config_path, &config);
    let pool = match connect_db(&db_path, &config).await {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: failed to connect to database: {}", e);
            return ExitCode::from(2);
        }
    };
    let store = aster_orch::store::Store::new(pool);

    let req = WaitRequest {
        thread_id,
        intent,
        since_reference: since,
        strict_new,
        timeout: Duration::from_secs(timeout),
        trigger_intents: config.orchestration.trigger_intents.clone(),
    };

    match wait::wait_for_message(&store, &req).await {
        Ok(WaitOutcome::Found(msg)) => {
            println!("found=true");
            println!("thread_id={}", msg.thread_id);
            println!("message_id={}", msg.id);
            println!("ref=db:{}", msg.id);
            println!("from={}", msg.from_alias);
            println!("to={}", msg.to_alias);
            println!("intent={}", msg.intent);
            if let Some(batch) = &msg.batch_id {
                println!("batch={}", batch);
            }
            println!("created_at={}", msg.created_at);
            // Body last — may be multiline. Delimited for easy parsing.
            println!("---BODY---");
            println!("{}", msg.body);
            ExitCode::SUCCESS
        }
        Ok(WaitOutcome::Timeout {
            thread_id,
            timeout_secs,
            intent_filter,
        }) => {
            println!("found=false");
            println!("thread_id={}", thread_id);
            println!("timeout_secs={}", timeout_secs);
            if let Some(intent) = intent_filter {
                println!("intent_filter={}", intent);
            }
            ExitCode::from(1)
        }
        Err(e) => {
            eprintln!("error: {}", e);
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Cli, Commands};
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

    #[test]
    fn test_dashboard_requires_config_flag() {
        let parsed = Cli::try_parse_from(["aster-orch", "dashboard"]);
        assert!(parsed.is_err());
    }

    #[test]
    fn test_dashboard_parses_with_config_flag() {
        let parsed = Cli::try_parse_from(["aster-orch", "dashboard", "--config", "foo.yaml"]);
        assert!(parsed.is_ok());
    }

    #[test]
    fn test_dashboard_parses_with_poll_interval() {
        let parsed = Cli::try_parse_from([
            "aster-orch",
            "dashboard",
            "--config",
            "foo.yaml",
            "--poll-interval",
            "5",
        ]);
        assert!(parsed.is_ok());
        if let Ok(cli) = parsed {
            if let Commands::Dashboard { poll_interval, .. } = cli.command {
                assert_eq!(poll_interval, 5);
            } else {
                panic!("expected Dashboard command");
            }
        }
    }

    #[test]
    fn test_dashboard_poll_interval_default() {
        let parsed =
            Cli::try_parse_from(["aster-orch", "dashboard", "--config", "foo.yaml"]).unwrap();
        if let Commands::Dashboard { poll_interval, .. } = parsed.command {
            assert_eq!(poll_interval, 2);
        } else {
            panic!("expected Dashboard command");
        }
    }

    #[test]
    fn test_wait_requires_config_and_thread_id() {
        let parsed = Cli::try_parse_from(["aster-orch", "wait"]);
        assert!(parsed.is_err());
        let parsed = Cli::try_parse_from(["aster-orch", "wait", "--config", "foo.yaml"]);
        assert!(parsed.is_err());
    }

    #[test]
    fn test_wait_parses_minimal() {
        let parsed = Cli::try_parse_from([
            "aster-orch",
            "wait",
            "--config",
            "foo.yaml",
            "--thread-id",
            "t-123",
        ]);
        assert!(parsed.is_ok());
        if let Ok(cli) = parsed {
            if let Commands::Wait {
                thread_id, timeout, ..
            } = cli.command
            {
                assert_eq!(thread_id, "t-123");
                assert_eq!(timeout, 120); // default
            } else {
                panic!("expected Wait command");
            }
        }
    }

    #[test]
    fn test_wait_parses_all_flags() {
        let parsed = Cli::try_parse_from([
            "aster-orch",
            "wait",
            "--config",
            "foo.yaml",
            "--thread-id",
            "t-abc",
            "--intent",
            "review-request",
            "--since",
            "db:42",
            "--strict-new",
            "--timeout",
            "300",
        ]);
        assert!(parsed.is_ok());
        if let Ok(cli) = parsed {
            if let Commands::Wait {
                thread_id,
                intent,
                since,
                strict_new,
                timeout,
                ..
            } = cli.command
            {
                assert_eq!(thread_id, "t-abc");
                assert_eq!(intent, Some("review-request".to_string()));
                assert_eq!(since, Some("db:42".to_string()));
                assert!(strict_new);
                assert_eq!(timeout, 300);
            } else {
                panic!("expected Wait command");
            }
        }
    }
}
