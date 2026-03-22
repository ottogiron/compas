//! compas binary — two-process orchestrator.
//!
//! Usage:
//!   compas worker
//!   compas mcp-server

use clap::{Parser, Subcommand};
use compas::backend::claude::ClaudeCodeBackend;
use compas::backend::codex::CodexBackend;
use compas::backend::gemini::GeminiBackend;
use compas::backend::generic::GenericBackend;
use compas::backend::opencode::OpenCodeBackend;
use compas::backend::registry::BackendRegistry;
use compas::mcp::server::OrchestratorMcpServer;
use compas::wait::{self, WaitOutcome, WaitRequest};
use compas::wait_merge::{self, WaitMergeOutcome, WaitMergeRequest};
use compas::worker::{self, WorkerRunner};
use rmcp::ServiceExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;
use tracing_subscriber::EnvFilter;

const DEFAULT_CONFIG_PATH: &str = "~/.compas/config.yaml";

#[derive(Parser)]
#[command(name = "compas", version, about = "Agent orchestrator")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run the trigger worker only
    Worker {
        /// Path to config YAML (default: ~/.compas/config.yaml)
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Run the MCP server only (stdio transport)
    McpServer {
        /// Path to config YAML (default: ~/.compas/config.yaml)
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Launch the TUI dashboard (reads SQLite directly, no MCP required)
    Dashboard {
        /// Path to config YAML (default: ~/.compas/config.yaml)
        #[arg(long)]
        config: Option<PathBuf>,
        /// How often (in seconds) to re-query SQLite for fresh metrics
        #[arg(long, default_value = "2")]
        poll_interval: u64,
        /// Run without an embedded worker (dashboard-only, no execution)
        #[arg(long)]
        standalone: bool,
        /// [deprecated: worker is now embedded by default] No-op, kept for backward compat.
        #[arg(long, hide = true, conflicts_with = "standalone")]
        with_worker: bool,
    },
    /// Register compas as an MCP server in coding tools
    #[command(name = "setup-mcp")]
    SetupMcp {
        /// Target specific tool (claude, codex, opencode, gemini). Default: auto-detect all.
        #[arg(long)]
        tool: Option<String>,
        /// Unregister compas from tools
        #[arg(long)]
        remove: bool,
        /// Show what would be done without executing
        #[arg(long)]
        dry_run: bool,
    },
    /// Validate setup and check readiness
    Doctor {
        /// Config file path (default: ~/.compas/config.yaml)
        #[arg(long)]
        config: Option<PathBuf>,
        /// Auto-fix issues where possible (e.g., register MCP servers)
        #[arg(long)]
        fix: bool,
    },
    /// Create a new compas configuration file
    Init {
        /// Overwrite existing config file
        #[arg(long)]
        force: bool,
        /// Skip interactive prompts, use defaults or explicit flags
        #[arg(long)]
        non_interactive: bool,
        /// Target repository root (default: current directory)
        #[arg(long)]
        repo: Option<PathBuf>,
        /// Backend to use (claude, codex, gemini, opencode)
        #[arg(long)]
        backend: Option<String>,
        /// Agent alias (default: dev)
        #[arg(long, alias = "alias")]
        agent_alias: Option<String>,
        /// Model identifier (optional)
        #[arg(long)]
        model: Option<String>,
        /// Generate minimal config without comments
        #[arg(long)]
        minimal: bool,
        /// Config file path (default: ~/.compas/config.yaml)
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Wait for a message on a thread (reads SQLite directly, no MCP required).
    ///
    /// Exits 0 when a matching message is found, 1 on timeout, 2 on error.
    /// Output is key=value lines on stdout for easy bash parsing.
    Wait {
        /// Path to config YAML (default: ~/.compas/config.yaml)
        #[arg(long)]
        config: Option<PathBuf>,
        /// Thread ID to wait on
        #[arg(long)]
        thread_id: String,
        /// Wait for a specific intent (e.g. "response", "review-request")
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
        /// Keep waiting until the entire handoff chain settles (no active executions
        /// and no untriggered handoff messages on the thread).
        #[arg(long, default_value_t = false)]
        await_chain: bool,
    },
    /// Wait for a merge operation to reach terminal status (reads SQLite directly, no MCP required).
    ///
    /// Exits 0 on completed, 1 on failure/cancelled/timeout, 2 on error.
    /// Output is key=value lines on stdout for easy bash parsing.
    #[command(name = "wait-merge")]
    WaitMerge {
        /// Path to config YAML (default: ~/.compas/config.yaml)
        #[arg(long)]
        config: Option<PathBuf>,
        /// Merge operation ID (ULID) returned by orch_merge
        #[arg(long)]
        op_id: String,
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
        Commands::Doctor { config, fix } => {
            let config = effective_config_path(config);
            let report = compas::cli::doctor::run(config, fix).await;
            print!("{}", compas::cli::doctor::format_report(&report));
            if report.has_failures() {
                return ExitCode::from(1);
            }
        }
        Commands::SetupMcp {
            tool,
            remove,
            dry_run,
        } => {
            if let Err(e) = compas::cli::setup_mcp::run(tool.as_deref(), remove, dry_run) {
                eprintln!("error: {}", e);
                return ExitCode::from(1);
            }
        }
        Commands::Init {
            force,
            non_interactive,
            repo,
            backend,
            agent_alias,
            model,
            minimal,
            config,
        } => {
            let config_path = config
                .map(|p| compas::config::expand_tilde(&p))
                .unwrap_or_else(|| effective_config_path(None));
            if let Err(e) = compas::cli::init::run(
                config_path,
                force,
                non_interactive,
                repo,
                backend,
                agent_alias,
                model,
                minimal,
            ) {
                eprintln!("error: {}", e);
                return ExitCode::from(1);
            }
        }
        Commands::Dashboard {
            config,
            poll_interval,
            standalone,
            with_worker,
        } => {
            let config = effective_config_path(config);
            if with_worker {
                eprintln!("note: --with-worker is now the default and can be omitted");
            }
            // Worker is embedded by default; --standalone opts out.
            let spawn_worker = !standalone;
            // TUI dashboard — no tracing to stdout (would corrupt the TUI)
            if let Err(e) = run_dashboard(config, poll_interval, spawn_worker).await {
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
            await_chain,
        } => {
            let config = effective_config_path(config);
            // Wait outputs key=value to stdout — no tracing there
            return run_wait(
                config,
                thread_id,
                intent,
                since,
                strict_new,
                timeout,
                await_chain,
            )
            .await;
        }
        Commands::WaitMerge {
            config,
            op_id,
            timeout,
        } => {
            let config = effective_config_path(config);
            // wait-merge outputs key=value to stdout — no tracing there
            return run_wait_merge(config, op_id, timeout).await;
        }
    }

    ExitCode::SUCCESS
}

fn effective_config_path(config: Option<PathBuf>) -> PathBuf {
    let raw = config.unwrap_or_else(|| PathBuf::from(DEFAULT_CONFIG_PATH));
    compas::config::expand_tilde(&raw)
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
    config: &compas::config::types::OrchestratorConfig,
) -> Result<sqlx::SqlitePool, Box<dyn std::error::Error>> {
    let db_url = format!("sqlite:{}?mode=rwc", db_path.display());
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(config.database.max_connections)
        .min_connections(config.database.min_connections)
        .acquire_timeout(Duration::from_millis(config.database.acquire_timeout_ms))
        .connect(&db_url)
        .await?;
    let store = compas::store::Store::new(pool.clone());
    store.setup().await?;
    Ok(pool)
}

fn resolve_db_path(config: &compas::config::types::OrchestratorConfig) -> PathBuf {
    config.db_path()
}

fn resolve_config_path(config_path: &PathBuf) -> PathBuf {
    std::fs::canonicalize(config_path).unwrap_or_else(|_| config_path.clone())
}

fn build_backend_registry(config: &compas::config::types::OrchestratorConfig) -> BackendRegistry {
    let mut registry = BackendRegistry::new();

    let workdir = Some(config.default_workdir.clone());

    // Register all known backends
    registry.register(
        "claude",
        Arc::new(ClaudeCodeBackend::with_workdir(workdir.clone())),
    );
    registry.register("codex", Arc::new(CodexBackend::new(workdir)));
    registry.register(
        "opencode",
        Arc::new(OpenCodeBackend::with_workdir(Some(
            config.default_workdir.clone(),
        ))),
    );
    registry.register(
        "gemini",
        Arc::new(GeminiBackend::with_workdir(Some(
            config.default_workdir.clone(),
        ))),
    );

    // Register config-driven generic backends.
    if let Some(ref definitions) = config.backend_definitions {
        for def in definitions {
            registry.register(
                &def.name,
                Arc::new(GenericBackend::with_workdir(
                    def.clone(),
                    Some(config.default_workdir.clone()),
                )),
            );
        }
    }

    registry
}

// ---------------------------------------------------------------------------
// Worker mode
// ---------------------------------------------------------------------------

async fn run_worker(config_path: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let resolved_config_path = resolve_config_path(&config_path);
    let config = compas::config::load_config(&config_path)?;
    let db_path = resolve_db_path(&config);
    tracing::info!(
        agents = config.agents.len(),
        config = %resolved_config_path.display(),
        db = %db_path.display(),
        "config loaded"
    );

    let backend_registry = build_backend_registry(&config);
    let pool = connect_db(&db_path, &config).await?;
    let store = compas::store::Store::new(pool);

    // Acquire singleton guard — fails fast if another worker is alive.
    let _worker_lock = worker::guard::acquire_worker_lock(&config.state_dir, &store).await?;

    let worktree_manager = compas::worktree::WorktreeManager::new();

    // Log legacy worktree directory if it exists.
    let legacy_worktree_dir = config.state_dir.join("worktrees");
    if legacy_worktree_dir.exists() {
        tracing::warn!(
            "legacy worktree directory found at {}; worktrees now live at {{repo_root}}/../.compas-worktrees/. Remove manually: rm -rf {}",
            legacy_worktree_dir.display(),
            legacy_worktree_dir.display()
        );
    }

    // Start config file watcher and get a live-reloadable handle.
    let config_handle = compas::config::watcher::start_watching(config_path.clone(), config)?;
    let event_bus = compas::events::EventBus::new();

    // Note: notification toggle is evaluated once at startup.
    // Changing notifications.desktop in config requires worker restart.
    if config_handle.load().notifications.desktop {
        compas::notifications::spawn_notification_consumer(&event_bus);
        tracing::info!("desktop notifications enabled");
    }

    // Lifecycle hooks — always active; no-op when `hooks:` is absent from config.
    // Config is re-read per-event so hooks can be added/removed without a restart.
    {
        let default_workdir = config_handle.load().default_workdir.clone();
        compas::hooks::spawn_hook_consumer(&event_bus, config_handle.clone(), default_workdir);
    }

    let runner = WorkerRunner::new(
        config_handle,
        store,
        backend_registry,
        event_bus,
        worktree_manager,
    );

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
    let config = compas::config::load_config(&config_path)?;
    let db_path = resolve_db_path(&config);
    tracing::info!(
        agents = config.agents.len(),
        config = %resolved_config_path.display(),
        db = %db_path.display(),
        "config loaded"
    );

    let backend_registry = build_backend_registry(&config);
    let pool = connect_db(&db_path, &config).await?;
    let store = compas::store::Store::new(pool);

    // Start config file watcher and get a live-reloadable handle.
    let config_handle = compas::config::watcher::start_watching(config_path.clone(), config)?;

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
    spawn_worker: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = compas::config::load_config(&config_path)?;
    let db_path = resolve_db_path(&config);
    let pool = connect_db(&db_path, &config).await?;
    let store = compas::store::Store::new(pool);

    // Start config file watcher and get a live-reloadable handle.
    // All consumers share the same handle and see updates atomically.
    let config_handle = compas::config::watcher::start_watching(config_path.clone(), config)?;

    // If requested, spawn the worker as a separate OS process so its
    // lifecycle is independent of the dashboard. The dashboard can be
    // restarted or quit without interrupting running triggers.
    let worker_log_path = config_handle.load().state_dir.join("worker.log");
    let mut worker_pid: Option<u32> = None;
    if spawn_worker {
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
        compas::dashboard::app::run_tui(
            store,
            config_handle,
            resolved_config_path,
            handle,
            poll_interval,
            None, // event_bus: worker runs as separate process
        )
    })
    .await;

    // Send SIGTERM to the worker for graceful shutdown.
    if let Some(pid) = worker_pid {
        #[cfg(unix)]
        {
            let pid_i32 = pid as i32;
            unsafe {
                libc::kill(pid_i32, libc::SIGTERM);
            }
            eprintln!(
                "sent SIGTERM to worker (PID: {}), waiting for graceful shutdown...",
                pid
            );

            // Wait up to 10s for the worker to exit.
            let mut exited = false;
            for _ in 0..20 {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                if unsafe { libc::kill(pid_i32, 0) } != 0 {
                    exited = true;
                    break;
                }
            }

            if exited {
                eprintln!("worker shut down cleanly");
            } else {
                eprintln!("worker still running after 10s — it may be draining executions");
                eprintln!("worker log: {}", worker_log_path.display());
                eprintln!("to force kill: kill -9 {}", pid);
            }
        }
        #[cfg(not(unix))]
        {
            eprintln!(
                "dashboard exited; worker (PID: {}) may still be running",
                pid
            );
            eprintln!("to stop it: taskkill /PID {}", pid);
        }
    } else if spawn_worker {
        // Pre-existing worker was detected at startup; remind user it's still running.
        eprintln!("dashboard exited; pre-existing worker is still running");
    }

    tui_result??;

    Ok(())
}

// ── Heartbeat guard (delegated to worker::guard) ─────────────────────────

/// Delegate to `worker::guard::is_worker_alive` — kept here so
/// `spawn_worker_process` and existing tests continue to compile.
fn is_worker_alive(
    heartbeat: &Option<(String, i64, i64, Option<String>)>,
    max_age_secs: i64,
) -> bool {
    worker::is_worker_alive(heartbeat, max_age_secs)
}

/// Re-export from guard module — single source of truth.
const WORKER_HEARTBEAT_MAX_AGE_SECS: i64 = worker::WORKER_HEARTBEAT_MAX_AGE_SECS;

// ── Worker process spawning ──────────────────────────────────────────────

/// Spawn the worker as a detached child process.
///
/// Returns `Ok(Some(pid))` if a new worker was spawned, `Ok(None)` if one is
/// already running (heartbeat younger than 30 s), or an error on spawn failure.
///
/// The child process runs `compas worker --config <path>` with:
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
    store: &compas::store::Store,
    config_handle: &compas::config::ConfigHandle,
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
    await_chain: bool,
) -> ExitCode {
    let config = match compas::config::load_config(&config_path) {
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
    let store = compas::store::Store::new(pool);

    let req = WaitRequest {
        thread_id,
        intent,
        since_reference: since,
        strict_new,
        timeout: Duration::from_secs(timeout),
        trigger_intents: config.orchestration.trigger_intents.clone(),
        await_chain,
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
            chain_pending,
        }) => {
            println!("found=false");
            println!("thread_id={}", thread_id);
            println!("timeout_secs={}", timeout_secs);
            if let Some(intent) = intent_filter {
                println!("intent_filter={}", intent);
            }
            if chain_pending {
                println!("chain_pending=true");
            }
            ExitCode::from(1)
        }
        Err(e) => {
            eprintln!("error: {}", e);
            ExitCode::from(2)
        }
    }
}

// ---------------------------------------------------------------------------
// Wait-merge mode
// ---------------------------------------------------------------------------

async fn run_wait_merge(config_path: PathBuf, op_id: String, timeout: u64) -> ExitCode {
    let config = match compas::config::load_config(&config_path) {
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
    let store = compas::store::Store::new(pool);

    let req = WaitMergeRequest {
        op_id,
        timeout: Duration::from_secs(timeout),
    };

    match wait_merge::wait_for_merge_op(&store, &req).await {
        Ok(WaitMergeOutcome::Found(op)) => {
            println!("found=true");
            println!("op_id={}", op.id);
            println!("thread_id={}", op.thread_id);
            println!("status={}", op.status);
            println!("source_branch={}", op.source_branch);
            println!("target_branch={}", op.target_branch);
            if let Some(ms) = op.duration_ms {
                println!("duration_ms={}", ms);
            }
            if let Some(ref files) = op.conflict_files {
                println!("conflict_files={}", files);
            }
            // Body last — may be multiline. Delimited for easy parsing.
            println!("---BODY---");
            if op.status == "completed" {
                if let Some(ref summary) = op.result_summary {
                    println!("{}", summary);
                }
            } else if let Some(ref detail) = op.error_detail {
                println!("{}", detail);
            }
            if op.status == "completed" {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            }
        }
        Ok(WaitMergeOutcome::Timeout {
            op_id,
            timeout_secs,
            last_status,
        }) => {
            println!("found=false");
            println!("op_id={}", op_id);
            println!("timeout_secs={}", timeout_secs);
            if let Some(status) = last_status {
                println!("last_status={}", status);
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
        let parsed = Cli::try_parse_from(["compas", "worker"]);
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
        let parsed = Cli::try_parse_from(["compas", "mcp-server"]);
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
        let parsed = Cli::try_parse_from(["compas", "dashboard"]);
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
        let parsed = Cli::try_parse_from(["compas", "dashboard", "--config", "foo.yaml"]);
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
            "compas",
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
        let parsed = Cli::try_parse_from(["compas", "dashboard"]).unwrap();
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
    fn test_dashboard_spawns_worker_by_default() {
        let parsed = Cli::try_parse_from(["compas", "dashboard"]).unwrap();
        if let Commands::Dashboard {
            standalone, config, ..
        } = parsed.command
        {
            // Default: worker is embedded (standalone is false).
            assert!(!standalone);
            assert!(config.is_none());
        } else {
            panic!("expected Dashboard command");
        }
    }

    #[test]
    fn test_dashboard_standalone_disables_worker() {
        let parsed = Cli::try_parse_from(["compas", "dashboard", "--standalone"]).unwrap();
        if let Commands::Dashboard { standalone, .. } = parsed.command {
            assert!(standalone);
        } else {
            panic!("expected Dashboard command");
        }
    }

    #[test]
    fn test_dashboard_with_worker_flag_still_accepted() {
        // --with-worker is a hidden no-op for backward compat.
        let parsed = Cli::try_parse_from([
            "compas",
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
                standalone,
                ..
            } = cli.command
            {
                assert!(with_worker); // flag was passed
                assert!(!standalone); // standalone not set
                assert_eq!(poll_interval, 5);
            } else {
                panic!("expected Dashboard command");
            }
        }
    }

    #[test]
    fn test_dashboard_standalone_with_worker_conflict() {
        let parsed = Cli::try_parse_from(["compas", "dashboard", "--standalone", "--with-worker"]);
        assert!(
            parsed.is_err(),
            "--standalone and --with-worker should conflict"
        );
    }

    #[test]
    fn test_wait_requires_thread_id() {
        let parsed = Cli::try_parse_from(["compas", "wait"]);
        assert!(parsed.is_err());
        let parsed = Cli::try_parse_from(["compas", "wait", "--timeout", "120"]);
        assert!(parsed.is_err());
    }

    #[test]
    fn test_wait_parses_minimal() {
        let parsed = Cli::try_parse_from(["compas", "wait", "--thread-id", "t-123"]);
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
            "compas",
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
    fn test_wait_merge_parses_with_op_id() {
        let parsed = Cli::try_parse_from([
            "compas",
            "wait-merge",
            "--op-id",
            "01ABC000000000000000000000",
        ]);
        assert!(parsed.is_ok());
        if let Ok(cli) = parsed {
            if let Commands::WaitMerge {
                op_id,
                timeout,
                config,
            } = cli.command
            {
                assert_eq!(op_id, "01ABC000000000000000000000");
                assert_eq!(timeout, 120); // default
                assert!(config.is_none());
            } else {
                panic!("expected WaitMerge command");
            }
        }
    }

    #[test]
    fn test_effective_config_path_defaults_to_standard_location() {
        let home = std::env::var("HOME").expect("HOME must be set");
        let expected = PathBuf::from(format!("{}/.compas/config.yaml", home));
        assert_eq!(effective_config_path(None), expected);
    }

    #[test]
    fn test_effective_config_path_honors_override() {
        // A plain relative path with no tilde passes through unchanged.
        assert_eq!(
            effective_config_path(Some(PathBuf::from("custom.yaml"))),
            PathBuf::from("custom.yaml")
        );
    }

    #[test]
    fn test_effective_config_path_expands_tilde_in_override() {
        let home = std::env::var("HOME").expect("HOME must be set");
        let expected = PathBuf::from(format!("{}/.compas/config.yaml", home));
        assert_eq!(
            effective_config_path(Some(PathBuf::from("~/.compas/config.yaml"))),
            expected
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
        // PID 1 (init/launchd) is always alive on Unix
        heartbeat_at_pid(ts, 1)
    }

    fn heartbeat_at_pid(ts: i64, pid: u32) -> Option<(String, i64, i64, Option<String>)> {
        Some((
            format!("worker-{}", pid),
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

    #[test]
    fn test_is_worker_alive_fresh_heartbeat_but_dead_pid() {
        // Heartbeat is recent but the process doesn't exist → stale
        // Use PID 99999999 which almost certainly doesn't exist
        let hb = heartbeat_at_pid(now_unix() - 5, 99_999_999);
        assert!(!is_worker_alive(&hb, WORKER_HEARTBEAT_MAX_AGE_SECS));
    }

    #[test]
    fn test_is_worker_alive_unparseable_worker_id() {
        // worker_id doesn't have the expected format → treat as dead
        let hb = Some((
            "bad-format".to_string(),
            now_unix() - 5,
            now_unix() - 105,
            Some("0.2.0".to_string()),
        ));
        assert!(!is_worker_alive(&hb, WORKER_HEARTBEAT_MAX_AGE_SECS));
    }

    #[test]
    fn test_wait_parses_await_chain_flag() {
        let parsed =
            Cli::try_parse_from(["compas", "wait", "--thread-id", "t-xyz", "--await-chain"]);
        assert!(parsed.is_ok());
        if let Ok(cli) = parsed {
            if let Commands::Wait { await_chain, .. } = cli.command {
                assert!(await_chain);
            } else {
                panic!("expected Wait command");
            }
        }
    }

    #[test]
    fn test_wait_await_chain_defaults_false() {
        let parsed = Cli::try_parse_from(["compas", "wait", "--thread-id", "t-abc"]).unwrap();
        if let Commands::Wait { await_chain, .. } = parsed.command {
            assert!(!await_chain);
        } else {
            panic!("expected Wait command");
        }
    }

    // ── SetupMcp subcommand tests ──────────────────────────────────────────

    #[test]
    fn test_setup_mcp_parses_minimal() {
        let parsed = Cli::try_parse_from(["compas", "setup-mcp"]);
        assert!(parsed.is_ok());
        if let Ok(cli) = parsed {
            if let Commands::SetupMcp {
                tool,
                remove,
                dry_run,
            } = cli.command
            {
                assert!(tool.is_none());
                assert!(!remove);
                assert!(!dry_run);
            } else {
                panic!("expected SetupMcp command");
            }
        }
    }

    #[test]
    fn test_setup_mcp_parses_tool_flag() {
        let parsed = Cli::try_parse_from(["compas", "setup-mcp", "--tool", "claude"]);
        assert!(parsed.is_ok());
        if let Ok(cli) = parsed {
            if let Commands::SetupMcp { tool, .. } = cli.command {
                assert_eq!(tool, Some("claude".to_string()));
            } else {
                panic!("expected SetupMcp command");
            }
        }
    }

    #[test]
    fn test_setup_mcp_parses_remove_flag() {
        let parsed = Cli::try_parse_from(["compas", "setup-mcp", "--remove"]);
        assert!(parsed.is_ok());
        if let Ok(cli) = parsed {
            if let Commands::SetupMcp { remove, .. } = cli.command {
                assert!(remove);
            } else {
                panic!("expected SetupMcp command");
            }
        }
    }

    #[test]
    fn test_setup_mcp_parses_dry_run_flag() {
        let parsed = Cli::try_parse_from(["compas", "setup-mcp", "--dry-run"]);
        assert!(parsed.is_ok());
        if let Ok(cli) = parsed {
            if let Commands::SetupMcp { dry_run, .. } = cli.command {
                assert!(dry_run);
            } else {
                panic!("expected SetupMcp command");
            }
        }
    }

    #[test]
    fn test_setup_mcp_parses_all_flags() {
        let parsed = Cli::try_parse_from([
            "compas",
            "setup-mcp",
            "--tool",
            "opencode",
            "--remove",
            "--dry-run",
        ]);
        assert!(parsed.is_ok());
        if let Ok(cli) = parsed {
            if let Commands::SetupMcp {
                tool,
                remove,
                dry_run,
            } = cli.command
            {
                assert_eq!(tool, Some("opencode".to_string()));
                assert!(remove);
                assert!(dry_run);
            } else {
                panic!("expected SetupMcp command");
            }
        }
    }

    // ── Init subcommand tests ─────────────────────────────────────────────

    #[test]
    fn test_init_parses_minimal() {
        let parsed = Cli::try_parse_from(["compas", "init"]);
        assert!(parsed.is_ok());
        if let Ok(cli) = parsed {
            if let Commands::Init {
                force,
                non_interactive,
                repo,
                backend,
                agent_alias,
                model,
                minimal,
                config,
            } = cli.command
            {
                assert!(!force);
                assert!(!non_interactive);
                assert!(repo.is_none());
                assert!(backend.is_none());
                assert!(agent_alias.is_none());
                assert!(model.is_none());
                assert!(!minimal);
                assert!(config.is_none());
            } else {
                panic!("expected Init command");
            }
        }
    }

    #[test]
    fn test_init_parses_all_flags() {
        let parsed = Cli::try_parse_from([
            "compas",
            "init",
            "--force",
            "--non-interactive",
            "--repo",
            "/tmp/repo",
            "--backend",
            "claude",
            "--agent-alias",
            "coder",
            "--model",
            "opus",
            "--minimal",
            "--config",
            "custom.yaml",
        ]);
        assert!(parsed.is_ok());
        if let Ok(cli) = parsed {
            if let Commands::Init {
                force,
                non_interactive,
                repo,
                backend,
                agent_alias,
                model,
                minimal,
                config,
            } = cli.command
            {
                assert!(force);
                assert!(non_interactive);
                assert_eq!(repo, Some(PathBuf::from("/tmp/repo")));
                assert_eq!(backend, Some("claude".to_string()));
                assert_eq!(agent_alias, Some("coder".to_string()));
                assert_eq!(model, Some("opus".to_string()));
                assert!(minimal);
                assert_eq!(config, Some(PathBuf::from("custom.yaml")));
            } else {
                panic!("expected Init command");
            }
        }
    }

    #[test]
    fn test_init_alias_flag_works() {
        let parsed = Cli::try_parse_from(["compas", "init", "--alias", "my-agent"]);
        assert!(parsed.is_ok());
        if let Ok(cli) = parsed {
            if let Commands::Init { agent_alias, .. } = cli.command {
                assert_eq!(agent_alias, Some("my-agent".to_string()));
            } else {
                panic!("expected Init command");
            }
        }
    }
}
