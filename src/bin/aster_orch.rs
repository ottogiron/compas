//! aster-orch binary — two-process orchestrator.
//!
//! Usage:
//!   aster_orch worker
//!   aster_orch mcp-server

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
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;
use tracing_subscriber::EnvFilter;

const DEFAULT_CONFIG_PATH: &str = ".aster-orch/config.yaml";

#[derive(Parser)]
#[command(name = "aster-orch", version, about = "Agent orchestrator")]
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
        config: Option<PathBuf>,
    },
    /// Run the MCP server only (stdio transport)
    McpServer {
        /// Path to config YAML
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Launch the TUI dashboard (reads SQLite directly, no MCP required)
    Dashboard {
        /// Path to config YAML
        #[arg(long)]
        config: Option<PathBuf>,
        /// How often (in seconds) to re-query SQLite for fresh metrics
        #[arg(long, default_value = "2")]
        poll_interval: u64,
        /// Run an embedded worker alongside the dashboard
        #[arg(long)]
        with_worker: bool,
    },
    /// Wait for a message on a thread (reads SQLite directly, no MCP required).
    ///
    /// Exits 0 when a matching message is found, 1 on timeout, 2 on error.
    /// Output is key=value lines on stdout for easy bash parsing.
    Wait {
        /// Path to config YAML
        #[arg(long)]
        config: Option<PathBuf>,
        /// Thread ID to wait on
        #[arg(long)]
        thread_id: String,
        /// Wait for a specific intent (e.g. "status-update", "completion")
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
            let config = effective_config_path(config);
            init_tracing();
            if let Err(e) = run_worker(config).await {
                eprintln!("error: {}", e);
                return ExitCode::from(2);
            }
        }
        Commands::McpServer { config } => {
            let config = effective_config_path(config);
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
            with_worker,
        } => {
            let config = effective_config_path(config);
            // TUI dashboard — no tracing to stdout (would corrupt the TUI)
            if let Err(e) = run_dashboard(config, poll_interval, with_worker).await {
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
            let config = effective_config_path(config);
            // Wait outputs key=value to stdout — no tracing there
            return run_wait(config, thread_id, intent, since, strict_new, timeout).await;
        }
    }

    ExitCode::SUCCESS
}

fn effective_config_path(config: Option<PathBuf>) -> PathBuf {
    config.unwrap_or_else(|| PathBuf::from(DEFAULT_CONFIG_PATH))
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
    db_path: &std::path::Path,
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

fn resolve_db_path(config: &aster_orch::config::types::OrchestratorConfig) -> PathBuf {
    config.db_path()
}

fn resolve_config_path(config_path: &PathBuf) -> PathBuf {
    std::fs::canonicalize(config_path).unwrap_or_else(|_| config_path.clone())
}

fn build_backend_registry(
    config: &aster_orch::config::types::OrchestratorConfig,
) -> BackendRegistry {
    let mut registry = BackendRegistry::new();

    let workdir = Some(config.target_repo_root.clone());

    // Register all known backends
    registry.register(
        "claude",
        Arc::new(ClaudeCodeBackend::with_workdir(workdir.clone())),
    );
    registry.register("codex", Arc::new(CodexBackend::new(workdir)));
    registry.register(
        "opencode",
        Arc::new(OpenCodeBackend::with_workdir(Some(
            config.target_repo_root.clone(),
        ))),
    );
    registry.register(
        "gemini",
        Arc::new(GeminiBackend::with_workdir(Some(
            config.target_repo_root.clone(),
        ))),
    );

    registry
}

// ---------------------------------------------------------------------------
// Worker mode
// ---------------------------------------------------------------------------

async fn run_worker(config_path: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let resolved_config_path = resolve_config_path(&config_path);
    let config = aster_orch::config::load_config(&config_path)?;
    let db_path = resolve_db_path(&config);
    tracing::info!(
        agents = config.agents.len(),
        config = %resolved_config_path.display(),
        db = %db_path.display(),
        "config loaded"
    );

    let backend_registry = build_backend_registry(&config);
    let pool = connect_db(&db_path, &config).await?;
    let store = aster_orch::store::Store::new(pool);

    // Start config file watcher and get a live-reloadable handle.
    let config_handle = aster_orch::config::watcher::start_watching(config_path.clone(), config)?;

    let runner = WorkerRunner::new(config_handle, store, backend_registry);

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
    let db_path = resolve_db_path(&config);
    tracing::info!(
        agents = config.agents.len(),
        config = %resolved_config_path.display(),
        db = %db_path.display(),
        "config loaded"
    );

    let backend_registry = build_backend_registry(&config);
    let pool = connect_db(&db_path, &config).await?;
    let store = aster_orch::store::Store::new(pool);

    // Start config file watcher and get a live-reloadable handle.
    let config_handle = aster_orch::config::watcher::start_watching(config_path.clone(), config)?;

    let server = OrchestratorMcpServer::new(config_handle, store, backend_registry);

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
    with_worker: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = aster_orch::config::load_config(&config_path)?;
    let db_path = resolve_db_path(&config);
    let pool = connect_db(&db_path, &config).await?;
    let store = aster_orch::store::Store::new(pool);

    // Start config file watcher and get a live-reloadable handle.
    // All consumers share the same handle and see updates atomically.
    let config_handle = aster_orch::config::watcher::start_watching(config_path.clone(), config)?;

    // If requested, spawn the worker as a separate OS process so its
    // lifecycle is independent of the dashboard. The dashboard can be
    // restarted or quit without interrupting running triggers.
    let worker_log_path = config_handle.load().state_dir.join("worker.log");
    let mut worker_pid: Option<u32> = None;
    if with_worker {
        let spawned = spawn_worker_process(&store, &config_handle, &config_path).await;
        match spawned {
            Ok(Some(pid)) => {
                worker_pid = Some(pid);
                eprintln!("spawned worker process (PID: {})", pid);
                eprintln!("worker log: {}", worker_log_path.display());
            }
            Ok(None) => {
                eprintln!("worker already running (recent heartbeat), skipping spawn");
            }
            Err(e) => {
                eprintln!("warning: failed to spawn worker process: {}", e);
                eprintln!("dashboard will start without an embedded worker");
            }
        }
    }

    // Capture the runtime handle before entering the blocking thread.
    // The TUI uses it to drive async store queries via Handle::block_on.
    let handle = tokio::runtime::Handle::current();

    // Resolve config path for display in the Settings tab.
    let resolved_config_path = resolve_config_path(&config_path);

    // The TUI event loop is synchronous (crossterm blocking I/O).
    // Run it in a dedicated blocking thread so the tokio runtime stays healthy.
    let tui_result = tokio::task::spawn_blocking(move || {
        aster_orch::dashboard::app::run_tui(
            store,
            config_handle,
            resolved_config_path,
            handle,
            poll_interval,
        )
    })
    .await;

    // The worker runs as its own process — no cleanup needed here.
    if let Some(pid) = worker_pid {
        eprintln!(
            "dashboard exited; worker process (PID: {}) continues running",
            pid
        );
        eprintln!("worker log: {}", worker_log_path.display());
        eprintln!("to stop it: kill {}", pid);
    } else if with_worker {
        // Pre-existing worker was detected at startup; remind user it's still running.
        eprintln!("dashboard exited; pre-existing worker is still running");
    }

    tui_result??;

    Ok(())
}

// ── Heartbeat guard (extracted for testability) ──────────────────────────

/// Check whether a worker is already running based on heartbeat recency.
///
/// A heartbeat is "recent" if it was written at most `max_age_secs` seconds
/// ago. Tolerates up to 5 s of forward clock skew.
fn is_worker_alive(
    heartbeat: &Option<(String, i64, i64, Option<String>)>,
    max_age_secs: i64,
) -> bool {
    match heartbeat {
        Some((_, last_beat_at, _, _)) => {
            let now_unix = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64;
            // Heartbeat is recent if it was written within [now - max_age, now + 5s].
            *last_beat_at >= now_unix.saturating_sub(max_age_secs) && *last_beat_at <= now_unix + 5
        }
        None => false,
    }
}

/// Heartbeat recency threshold for the duplicate-worker guard (seconds).
const WORKER_HEARTBEAT_MAX_AGE_SECS: i64 = 30;

// ── Worker process spawning ──────────────────────────────────────────────

/// Spawn the worker as a detached child process.
///
/// Returns `Ok(Some(pid))` if a new worker was spawned, `Ok(None)` if one is
/// already running (heartbeat younger than 30 s), or an error on spawn failure.
///
/// The child process runs `aster_orch worker --config <path>` with:
/// - stdin  → /dev/null
/// - stdout/stderr → `{state_dir}/worker.log` (append mode)
///
/// Safety mechanisms:
/// - **Exclusive lockfile** (`{state_dir}/worker.lock`) prevents TOCTOU races
///   when multiple dashboards start simultaneously.
/// - **Process group detach** (Unix): the child runs in its own process group
///   so Ctrl+C on the dashboard terminal does not propagate SIGINT.
/// - **Zombie prevention**: a background tokio task reaps the child process.
///
/// The child is fully independent: quitting the dashboard does NOT stop it.
async fn spawn_worker_process(
    store: &aster_orch::store::Store,
    config_handle: &aster_orch::config::ConfigHandle,
    config_path: &Path,
) -> Result<Option<u32>, Box<dyn std::error::Error>> {
    let state_dir = config_handle.load().state_dir.clone();
    tokio::fs::create_dir_all(&state_dir).await?;

    // Acquire an exclusive lock to prevent TOCTOU races when multiple
    // dashboards start simultaneously. The lock is held through heartbeat
    // check + spawn, then released.
    let lock_path = state_dir.join("worker.lock");
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)?;

    #[cfg(unix)]
    {
        use libc::{flock, LOCK_EX, LOCK_NB};
        use std::os::unix::io::AsRawFd;
        // Non-blocking exclusive lock. If another dashboard holds it, skip spawn.
        let ret = unsafe { flock(lock_file.as_raw_fd(), LOCK_EX | LOCK_NB) };
        if ret != 0 {
            // Another dashboard is currently spawning a worker — treat as "already running".
            return Ok(None);
        }
    }

    // Check if a worker is already running via heartbeat.
    let heartbeat = store.latest_heartbeat().await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "heartbeat query failed during worker spawn guard");
        None
    });

    if is_worker_alive(&heartbeat, WORKER_HEARTBEAT_MAX_AGE_SECS) {
        return Ok(None);
    }

    // Resolve paths for the child process.
    let exe = std::env::current_exe()?;
    let resolved_config = resolve_config_path(&config_path.to_path_buf());

    // Worker log lives in state_dir (not logs/) to avoid collision with
    // the per-execution log pruner in loop_runner.rs.
    let worker_log_path = state_dir.join("worker.log");
    let worker_log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&worker_log_path)?;
    let worker_log_err = worker_log.try_clone()?;

    // Write a separator so restarts are visible in the append log.
    use std::io::Write;
    let mut separator = worker_log.try_clone()?;
    let _ = writeln!(
        separator,
        "\n--- worker spawn at {} ---",
        chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ")
    );

    let mut cmd = tokio::process::Command::new(exe);
    cmd.arg("worker")
        .arg("--config")
        .arg(&resolved_config)
        .stdin(std::process::Stdio::null())
        .stdout(worker_log)
        .stderr(worker_log_err)
        .kill_on_drop(false);

    // Detach from the dashboard's process group so Ctrl+C does not
    // propagate SIGINT to the worker.
    #[cfg(unix)]
    cmd.process_group(0);

    let mut child = cmd.spawn()?;
    let pid = child.id();

    if pid.is_none() || pid == Some(0) {
        tracing::warn!("worker process exited before PID could be captured");
    }

    // Reap the child asynchronously to prevent zombie processes.
    tokio::spawn(async move {
        let _ = child.wait().await;
    });

    // Release the lockfile (drop closes the fd and releases flock).
    drop(lock_file);

    Ok(pid)
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
    let db_path = resolve_db_path(&config);
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
    use super::{
        effective_config_path, is_worker_alive, Cli, Commands, WORKER_HEARTBEAT_MAX_AGE_SECS,
    };
    use clap::Parser;
    use std::path::PathBuf;

    #[test]
    fn test_worker_parses_without_config_flag() {
        let parsed = Cli::try_parse_from(["aster-orch", "worker"]);
        assert!(parsed.is_ok());
        if let Ok(cli) = parsed {
            if let Commands::Worker { config } = cli.command {
                assert!(config.is_none());
            } else {
                panic!("expected Worker command");
            }
        }
    }

    #[test]
    fn test_mcp_server_parses_without_config_flag() {
        let parsed = Cli::try_parse_from(["aster-orch", "mcp-server"]);
        assert!(parsed.is_ok());
        if let Ok(cli) = parsed {
            if let Commands::McpServer { config } = cli.command {
                assert!(config.is_none());
            } else {
                panic!("expected McpServer command");
            }
        }
    }

    #[test]
    fn test_dashboard_parses_without_config_flag() {
        let parsed = Cli::try_parse_from(["aster-orch", "dashboard"]);
        assert!(parsed.is_ok());
        if let Ok(cli) = parsed {
            if let Commands::Dashboard { config, .. } = cli.command {
                assert!(config.is_none());
            } else {
                panic!("expected Dashboard command");
            }
        }
    }

    #[test]
    fn test_dashboard_parses_with_config_flag() {
        let parsed = Cli::try_parse_from(["aster-orch", "dashboard", "--config", "foo.yaml"]);
        assert!(parsed.is_ok());
        if let Ok(cli) = parsed {
            if let Commands::Dashboard { config, .. } = cli.command {
                assert_eq!(config, Some(PathBuf::from("foo.yaml")));
            } else {
                panic!("expected Dashboard command");
            }
        }
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
        let parsed = Cli::try_parse_from(["aster-orch", "dashboard"]).unwrap();
        if let Commands::Dashboard {
            poll_interval,
            config,
            ..
        } = parsed.command
        {
            assert_eq!(poll_interval, 2);
            assert!(config.is_none());
        } else {
            panic!("expected Dashboard command");
        }
    }

    #[test]
    fn test_dashboard_with_worker_default_false() {
        let parsed = Cli::try_parse_from(["aster-orch", "dashboard"]).unwrap();
        if let Commands::Dashboard {
            with_worker,
            config,
            ..
        } = parsed.command
        {
            assert!(!with_worker);
            assert!(config.is_none());
        } else {
            panic!("expected Dashboard command");
        }
    }

    #[test]
    fn test_dashboard_parses_with_with_worker_flag() {
        let parsed = Cli::try_parse_from([
            "aster-orch",
            "dashboard",
            "--config",
            "foo.yaml",
            "--poll-interval",
            "5",
            "--with-worker",
        ]);
        assert!(parsed.is_ok());
        if let Ok(cli) = parsed {
            if let Commands::Dashboard {
                with_worker,
                poll_interval,
                ..
            } = cli.command
            {
                assert!(with_worker);
                assert_eq!(poll_interval, 5);
            } else {
                panic!("expected Dashboard command");
            }
        }
    }

    #[test]
    fn test_wait_requires_thread_id() {
        let parsed = Cli::try_parse_from(["aster-orch", "wait"]);
        assert!(parsed.is_err());
        let parsed = Cli::try_parse_from(["aster-orch", "wait", "--timeout", "120"]);
        assert!(parsed.is_err());
    }

    #[test]
    fn test_wait_parses_minimal() {
        let parsed = Cli::try_parse_from(["aster-orch", "wait", "--thread-id", "t-123"]);
        assert!(parsed.is_ok());
        if let Ok(cli) = parsed {
            if let Commands::Wait {
                thread_id,
                timeout,
                config,
                ..
            } = cli.command
            {
                assert_eq!(thread_id, "t-123");
                assert_eq!(timeout, 120); // default
                assert!(config.is_none());
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
            "status-update",
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
                config,
                ..
            } = cli.command
            {
                assert_eq!(thread_id, "t-abc");
                assert_eq!(intent, Some("status-update".to_string()));
                assert_eq!(since, Some("db:42".to_string()));
                assert!(strict_new);
                assert_eq!(timeout, 300);
                assert_eq!(config, Some(PathBuf::from("foo.yaml")));
            } else {
                panic!("expected Wait command");
            }
        }
    }

    #[test]
    fn test_effective_config_path_defaults_to_standard_location() {
        assert_eq!(
            effective_config_path(None),
            PathBuf::from(".aster-orch/config.yaml")
        );
    }

    #[test]
    fn test_effective_config_path_honors_override() {
        assert_eq!(
            effective_config_path(Some(PathBuf::from("custom.yaml"))),
            PathBuf::from("custom.yaml")
        );
    }

    // ── Worker heartbeat guard tests ─────────────────────────────────────

    fn now_unix() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }

    fn heartbeat_at(ts: i64) -> Option<(String, i64, i64, Option<String>)> {
        Some((
            "worker-1".to_string(),
            ts,
            ts - 100,
            Some("0.2.0".to_string()),
        ))
    }

    #[test]
    fn test_is_worker_alive_recent_heartbeat_returns_true() {
        let hb = heartbeat_at(now_unix() - 5);
        assert!(is_worker_alive(&hb, WORKER_HEARTBEAT_MAX_AGE_SECS));
    }

    #[test]
    fn test_is_worker_alive_stale_heartbeat_returns_false() {
        let hb = heartbeat_at(now_unix() - 60);
        assert!(!is_worker_alive(&hb, WORKER_HEARTBEAT_MAX_AGE_SECS));
    }

    #[test]
    fn test_is_worker_alive_no_heartbeat_returns_false() {
        assert!(!is_worker_alive(&None, WORKER_HEARTBEAT_MAX_AGE_SECS));
    }

    #[test]
    fn test_is_worker_alive_at_exact_boundary_returns_true() {
        let hb = heartbeat_at(now_unix() - WORKER_HEARTBEAT_MAX_AGE_SECS);
        assert!(is_worker_alive(&hb, WORKER_HEARTBEAT_MAX_AGE_SECS));
    }

    #[test]
    fn test_is_worker_alive_just_past_boundary_returns_false() {
        let hb = heartbeat_at(now_unix() - WORKER_HEARTBEAT_MAX_AGE_SECS - 1);
        assert!(!is_worker_alive(&hb, WORKER_HEARTBEAT_MAX_AGE_SECS));
    }

    #[test]
    fn test_is_worker_alive_future_within_tolerance_returns_true() {
        // Small forward clock skew (3s) should be tolerated.
        let hb = heartbeat_at(now_unix() + 3);
        assert!(is_worker_alive(&hb, WORKER_HEARTBEAT_MAX_AGE_SECS));
    }

    #[test]
    fn test_is_worker_alive_future_beyond_tolerance_returns_false() {
        // Large forward clock skew (10s) should not pass the guard.
        let hb = heartbeat_at(now_unix() + 10);
        assert!(!is_worker_alive(&hb, WORKER_HEARTBEAT_MAX_AGE_SECS));
    }
}
