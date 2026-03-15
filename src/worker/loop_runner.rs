//! WorkerRunner — poll-loop based trigger worker.
//!
//! On startup:
//! 1. Marks orphaned executions (picked_up/executing) as crashed
//! 2. Writes initial heartbeat
//!
//! Main loop:
//! 1. Scans for untriggered messages and enqueues executions
//! 2. Polls `executions` table for queued work via `claim_next_execution`
//! 3. For each claimed execution, spawns a task to run the backend trigger
//! 4. Writes periodic heartbeats
//! 5. Inserts reply messages from completed triggers
//! 6. Emits `OrchestratorEvent`s on all state transitions

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Semaphore;

use crate::backend::registry::BackendRegistry;
use crate::config::types::{AgentConfig, AgentRole};
use crate::config::ConfigHandle;
use crate::events::{EventBus, OrchestratorEvent};
use crate::store::Store;
use crate::worktree::WorktreeManager;

use super::executor::{execute_trigger, TriggerOutput};

/// Wait for a shutdown signal (SIGTERM or Ctrl+C).
async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm =
            signal(SignalKind::terminate()).expect("failed to register SIGTERM handler");
        tokio::select! {
            _ = ctrl_c => {},
            _ = sigterm.recv() => {},
        }
    }
    #[cfg(not(unix))]
    ctrl_c.await.ok();
}

/// Worker runner configuration.
pub struct WorkerRunner {
    config: ConfigHandle,
    store: Store,
    backend_registry: Arc<BackendRegistry>,
    worker_id: String,
    event_bus: EventBus,
    worktree_manager: Arc<WorktreeManager>,
}

impl WorkerRunner {
    pub fn new(
        config: ConfigHandle,
        store: Store,
        backend_registry: BackendRegistry,
        event_bus: EventBus,
        worktree_manager: WorktreeManager,
    ) -> Self {
        let worker_id = format!("worker-{}", std::process::id());

        Self {
            config,
            store,
            backend_registry: Arc::new(backend_registry),
            worker_id,
            event_bus,
            worktree_manager: Arc::new(worktree_manager),
        }
    }

    /// Run the worker loop. Returns when a shutdown signal is received or cancelled.
    pub async fn run(&self) -> Result<(), Box<dyn std::error::Error>> {
        // Read startup-only config values once.
        let startup_config = self.config.load();
        let poll_interval_secs = startup_config.poll_interval_secs;
        let max_concurrent = startup_config.effective_max_concurrent_triggers();

        tracing::info!(
            worker_id = %self.worker_id,
            poll_interval_secs,
            "worker starting"
        );

        // Crash recovery: mark orphaned executions
        let crashed = self.store.mark_orphaned_executions_crashed().await?;
        if crashed > 0 {
            tracing::warn!(count = crashed, "marked orphaned executions as crashed");
        }

        // Create log directory and prune old log files on startup.
        let log_dir = startup_config.log_dir();
        if let Err(e) = std::fs::create_dir_all(&log_dir) {
            tracing::warn!(path = %log_dir.display(), error = %e, "failed to create log dir");
        }
        prune_log_files(&log_dir, startup_config.orchestration.log_retention_count);

        // Prune old execution telemetry events (same retention threshold as logs).
        match self
            .store
            .prune_execution_events(startup_config.orchestration.log_retention_count as i64)
            .await
        {
            Ok(count) if count > 0 => {
                tracing::info!(removed = count, "pruned old execution_events rows");
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to prune execution_events");
            }
            _ => {}
        }

        // Initial heartbeat
        self.store
            .write_heartbeat(&self.worker_id, env!("CARGO_PKG_VERSION"))
            .await?;

        // Concurrency semaphore (global limit — startup-only, restart to change).
        let semaphore = Arc::new(Semaphore::new(max_concurrent));

        let mut heartbeat_interval = tokio::time::interval(Duration::from_secs(10));
        let mut poll_interval =
            tokio::time::interval(Duration::from_secs(poll_interval_secs.max(1)));
        // Periodic stale execution check: detect executions stuck in
        // picked_up/executing beyond the trigger timeout. These are
        // likely from a panicked spawn_blocking task or a hung backend.
        // First tick deferred by 60s to avoid duplicating the startup
        // orphan check that already ran above.
        let mut stale_exec_interval = tokio::time::interval_at(
            tokio::time::Instant::now() + Duration::from_secs(60),
            Duration::from_secs(60),
        );

        // Create the shutdown signal future once before the loop.
        let mut shutdown = std::pin::pin!(shutdown_signal());

        loop {
            tokio::select! {
                _ = poll_interval.tick() => {
                    self.poll_once(&semaphore).await;
                }
                _ = heartbeat_interval.tick() => {
                    if let Err(e) = self.store
                        .write_heartbeat(&self.worker_id, env!("CARGO_PKG_VERSION"))
                        .await
                    {
                        tracing::warn!(error = %e, "heartbeat write failed");
                    }
                }
                _ = stale_exec_interval.tick() => {
                    let timeout_secs = self.config.load().orchestration.execution_timeout_secs;
                    match self.store.mark_stale_executions_crashed(timeout_secs).await {
                        Ok(count) if count > 0 => {
                            tracing::warn!(count, timeout_secs, "marked stale executions as crashed");
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "stale execution check failed");
                        }
                        _ => {}
                    }

                    // Worktree cleanup for terminal threads
                    match self.store.threads_with_stale_worktrees().await {
                        Ok(stale) => {
                            for (thread_id, _worktree_path, repo_root) in &stale {
                                let repo_root = std::path::PathBuf::from(repo_root);
                                if let Err(e) = self.worktree_manager.remove_worktree(&repo_root, thread_id) {
                                    tracing::warn!(thread_id = %thread_id, error = %e, "worktree cleanup failed");
                                }
                                // If clear_thread_worktree_path fails after a successful
                                // remove_worktree, the next cleanup cycle safely retries:
                                // remove_worktree returns Ok(()) when the directory is
                                // already gone.
                                if let Err(e) = self.store.clear_thread_worktree_path(thread_id).await {
                                    tracing::warn!(thread_id = %thread_id, error = %e, "failed to clear worktree path");
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "failed to query stale worktrees");
                        }
                    }
                }
                _ = &mut shutdown => {
                    tracing::info!("received shutdown signal, draining in-flight executions...");
                    break;
                }
            }
        }

        // Drain in-flight executions by waiting for all semaphore permits to
        // become available (meaning no tasks are running).
        //
        // `max_permits` must use the startup value (matches semaphore capacity,
        // which is fixed at startup). `drain_timeout` reads live config — safe
        // because it's independent of semaphore capacity.
        let max_permits =
            u32::try_from(max_concurrent).expect("max_concurrent_triggers exceeds u32::MAX");
        let config = self.config.load();
        let drain_timeout = Duration::from_secs(config.orchestration.execution_timeout_secs);

        tracing::info!(
            drain_timeout_secs = drain_timeout.as_secs(),
            "waiting for in-flight executions to complete..."
        );

        match tokio::time::timeout(drain_timeout, semaphore.acquire_many(max_permits)).await {
            Ok(Ok(_)) => tracing::info!("all executions drained, shutting down cleanly"),
            Ok(Err(_)) => tracing::warn!("semaphore closed during drain"),
            Err(_) => tracing::warn!(
                "drain timeout after {}s, some executions may still be running",
                drain_timeout.as_secs()
            ),
        }

        Ok(())
    }

    /// Poll for queued executions and spawn trigger tasks for any that are claimed.
    ///
    /// `pub` so integration tests in `tests/` can drive the worker directly without
    /// starting the full poll loop.
    pub async fn poll_once(&self, semaphore: &Arc<Semaphore>) {
        // Read live-reloadable config values each poll cycle.
        let config = self.config.load();
        let max_per_agent = config.orchestration.max_triggers_per_agent;

        // Scan for untriggered messages and enqueue executions before claiming.
        self.scan_and_enqueue_triggers(&config).await;

        // Try to claim work. We loop to drain all available queued items.
        loop {
            // Check if we have global capacity
            if semaphore.available_permits() == 0 {
                return;
            }

            match self.store.claim_next_execution(max_per_agent).await {
                Ok(Some(execution)) => {
                    let permit = match semaphore.clone().try_acquire_owned() {
                        Ok(p) => p,
                        Err(_) => return, // at capacity
                    };

                    tracing::info!(
                        exec_id = %execution.id,
                        thread_id = %execution.thread_id,
                        agent = %execution.agent_alias,
                        "claimed execution"
                    );

                    // Emit ExecutionStarted before spawning.
                    self.event_bus.emit(OrchestratorEvent::ExecutionStarted {
                        execution_id: execution.id.clone(),
                        thread_id: execution.thread_id.clone(),
                        agent_alias: execution.agent_alias.clone(),
                    });

                    // Get the instruction from the latest dispatch message on this thread
                    let store = self.store.clone();
                    let backend_registry = self.backend_registry.clone();
                    // Snapshot config for this trigger (live-reloadable).
                    let trigger_config = self.config.load().clone();
                    let agent_configs = trigger_config.agents.clone();
                    let execution_timeout_secs =
                        trigger_config.orchestration.execution_timeout_secs;
                    let log_dir = Some(trigger_config.log_dir());
                    let thread_id = execution.thread_id.clone();
                    let event_bus = self.event_bus.clone();

                    // Determine backend name for telemetry parser selection.
                    let backend_name = agent_configs
                        .iter()
                        .find(|a| a.alias == execution.agent_alias)
                        .map(|a| a.backend.clone())
                        .unwrap_or_default();

                    let worktree_manager = self.worktree_manager.clone();
                    let target_repo_root = trigger_config.target_repo_root.clone();

                    // Create telemetry channel for real-time stdout line forwarding.
                    let (stdout_tx, stdout_rx) = std::sync::mpsc::sync_channel::<String>(128);
                    let stdout_tx = std::sync::Arc::new(stdout_tx);

                    // Clone values for the telemetry consumer task.
                    let telem_store = store.clone();
                    let telem_event_bus = event_bus.clone();
                    let telem_exec_id = execution.id.clone();
                    let telem_thread_id = execution.thread_id.clone();
                    let telem_agent_alias = execution.agent_alias.clone();

                    tokio::spawn(async move {
                        let _permit = permit; // held until task completes

                        // Spawn telemetry consumer before the trigger so it's
                        // ready to receive lines as soon as stdout starts flowing.
                        let telemetry_handle = tokio::spawn({
                            let store = telem_store;
                            let event_bus = telem_event_bus;
                            let exec_id = telem_exec_id;
                            let thread_id = telem_thread_id;
                            let agent_alias = telem_agent_alias;
                            let backend = backend_name;
                            async move {
                                consume_telemetry(
                                    stdout_rx,
                                    &store,
                                    &event_bus,
                                    &exec_id,
                                    &thread_id,
                                    &agent_alias,
                                    &backend,
                                )
                                .await;
                            }
                        });

                        // Strict provenance: execute only from the dispatch message
                        // linked to this execution. Legacy/unlinked rows fall back to
                        // an explicit placeholder instruction.
                        let instruction = if let Some(dispatch_id) = execution.dispatch_message_id {
                            match store.get_message(dispatch_id).await {
                                Ok(Some(msg)) => msg.body,
                                Ok(None) => {
                                    tracing::warn!(
                                        exec_id = %execution.id,
                                        thread_id = %thread_id,
                                        dispatch_message_id = dispatch_id,
                                        "execution dispatch message not found; using placeholder instruction"
                                    );
                                    "Dispatch message unavailable for this execution.".to_string()
                                }
                                Err(e) => {
                                    tracing::error!(
                                        exec_id = %execution.id,
                                        thread_id = %thread_id,
                                        dispatch_message_id = dispatch_id,
                                        error = %e,
                                        "failed to fetch linked dispatch message; using placeholder instruction"
                                    );
                                    "Dispatch message unavailable for this execution.".to_string()
                                }
                            }
                        } else {
                            tracing::warn!(
                                exec_id = %execution.id,
                                thread_id = %thread_id,
                                "execution has no dispatch linkage; using placeholder instruction"
                            );
                            "Dispatch message unavailable for this execution.".to_string()
                        };

                        let output = execute_trigger(
                            &execution,
                            &store,
                            &backend_registry,
                            &agent_configs,
                            &instruction,
                            execution_timeout_secs,
                            log_dir,
                            Some(stdout_tx),
                            &worktree_manager,
                            &target_repo_root,
                        )
                        .await;

                        // Wait for telemetry consumer to finish flushing.
                        // The stdout_tx is dropped when execute_trigger returns,
                        // which causes the consumer's recv to get Disconnected.
                        let _ = telemetry_handle.await;

                        // Post-execution: insert reply message, emit events, and
                        // potentially enqueue a retry for transient failures.
                        handle_trigger_output(&store, &event_bus, &output, &agent_configs).await;
                    });
                }
                Ok(None) => {
                    // No queued work
                    return;
                }
                Err(e) => {
                    tracing::error!(error = %e, "failed to claim execution");
                    return;
                }
            }
        }
    }

    /// Scan for messages that should trigger an execution but haven't yet.
    ///
    /// For each untriggered message (matching `trigger_intents` and addressed to
    /// a worker alias), enqueue a new execution linked to that message. The
    /// partial UNIQUE index on `dispatch_message_id` prevents double-enqueue
    /// across concurrent workers.
    async fn scan_and_enqueue_triggers(&self, config: &crate::config::types::OrchestratorConfig) {
        let trigger_intents = &config.orchestration.trigger_intents;
        let worker_aliases: Vec<String> = config
            .agents
            .iter()
            .filter(|a| a.role == AgentRole::Worker)
            .map(|a| a.alias.clone())
            .collect();

        let untriggered = match self
            .store
            .find_untriggered_messages(trigger_intents, &worker_aliases)
            .await
        {
            Ok(msgs) => msgs,
            Err(e) => {
                tracing::error!(error = %e, "failed to scan untriggered messages");
                return;
            }
        };

        for (message_id, thread_id, agent_alias) in untriggered {
            // Compute a SHA-256 hash of the agent's prompt at enqueue time so
            // executions can be correlated to the prompt version that produced them.
            // Agents without a prompt field get None — storing a hash of the empty
            // string would be misleading since it implies a known prompt existed.
            let prompt_hash = config
                .agents
                .iter()
                .find(|a| a.alias == agent_alias)
                .and_then(|a| a.prompt.as_deref())
                .map(sha256_hex);
            match self
                .store
                .insert_execution_with_dispatch(
                    &thread_id,
                    &agent_alias,
                    Some(message_id),
                    prompt_hash.as_deref(),
                )
                .await
            {
                Ok(Some(exec_id)) => {
                    tracing::info!(
                        exec_id = %exec_id,
                        thread_id = %thread_id,
                        agent = %agent_alias,
                        dispatch_message_id = message_id,
                        "enqueued trigger from untriggered message"
                    );
                }
                Ok(None) => {
                    // Duplicate — already enqueued by another worker, skip.
                }
                Err(e) => {
                    tracing::error!(
                        thread_id = %thread_id,
                        agent = %agent_alias,
                        dispatch_message_id = message_id,
                        error = %e,
                        "failed to enqueue trigger"
                    );
                }
            }
        }
    }
}

/// Compute the SHA-256 hex digest of a string.
///
/// Used to fingerprint agent prompts at execution creation time so that
/// executions can be correlated to the prompt version that produced them.
fn sha256_hex(text: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Prune old execution log files, keeping only the `retention_count` most recent.
///
/// Files are sorted by name (ULID exec IDs sort chronologically), so the oldest
/// files appear first and are removed first.
fn prune_log_files(log_dir: &Path, retention_count: usize) {
    let Ok(entries) = std::fs::read_dir(log_dir) else {
        return;
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "log"))
        .map(|e| e.path())
        .collect();

    files.sort();

    if files.len() > retention_count {
        let to_remove = files.len() - retention_count;
        for path in files.iter().take(to_remove) {
            if let Err(e) = std::fs::remove_file(path) {
                tracing::warn!(path = %path.display(), error = %e, "failed to prune log file");
            } else {
                tracing::debug!(path = %path.display(), "pruned old log file");
            }
        }
        tracing::info!(
            removed = to_remove,
            retained = retention_count,
            "pruned execution logs"
        );
    }
}

/// Consume stdout lines from a running backend process, parse them into
/// structured execution events, and flush to the store in batches.
///
/// Runs as a separate tokio task alongside the trigger execution. The channel
/// disconnects when the trigger finishes, which terminates this consumer.
async fn consume_telemetry(
    rx: std::sync::mpsc::Receiver<String>,
    store: &Store,
    event_bus: &EventBus,
    execution_id: &str,
    thread_id: &str,
    agent_alias: &str,
    backend_name: &str,
) {
    use crate::backend::claude::parse_claude_stream_line;
    use crate::backend::codex::parse_codex_stream_line;
    use crate::backend::opencode::parse_opencode_stream_line;
    use crate::backend::ExecutionEvent;
    use std::time::{Duration, Instant};

    let parser: fn(&str) -> Option<ExecutionEvent> = match backend_name {
        "claude" => parse_claude_stream_line,
        "codex" => parse_codex_stream_line,
        "opencode" => parse_opencode_stream_line,
        _ => return, // No parser for this backend (e.g., gemini)
    };

    let mut buffer: Vec<ExecutionEvent> = Vec::new();
    let mut event_index: i32 = 0;
    let mut last_flush = Instant::now();

    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(line) => {
                if let Some(mut event) = parser(&line) {
                    event.event_index = event_index;
                    event_index += 1;

                    event_bus.emit(OrchestratorEvent::ExecutionProgress {
                        execution_id: execution_id.to_string(),
                        thread_id: thread_id.to_string(),
                        agent_alias: agent_alias.to_string(),
                        summary: event.summary.clone(),
                    });

                    buffer.push(event);
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }

        if buffer.len() >= 20
            || (last_flush.elapsed() > Duration::from_millis(500) && !buffer.is_empty())
        {
            if let Err(e) = store.insert_execution_events(execution_id, &buffer).await {
                tracing::warn!(error = %e, "failed to flush telemetry events");
            }
            buffer.clear();
            last_flush = Instant::now();
        }
    }

    // Final flush
    if !buffer.is_empty() {
        if let Err(e) = store.insert_execution_events(execution_id, &buffer).await {
            tracing::warn!(error = %e, "failed to flush final telemetry events");
        }
    }
}

/// Insert a reply message from the agent after trigger completion, and emit
/// events for `ExecutionCompleted`, `ThreadStatusChanged` (on failure), and
/// `MessageReceived`.
///
/// For failed executions with transient errors: if the agent config allows
/// retries and the attempt limit hasn't been reached, enqueue a retry
/// execution with exponential backoff instead of marking the thread Failed.
async fn handle_trigger_output(
    store: &Store,
    event_bus: &EventBus,
    output: &TriggerOutput,
    agent_configs: &[AgentConfig],
) {
    // Emit ExecutionCompleted before any retry/reply logic.
    event_bus.emit(OrchestratorEvent::ExecutionCompleted {
        execution_id: output.execution_id.clone(),
        thread_id: output.thread_id.clone(),
        agent_alias: output.agent_alias.clone(),
        success: output.success,
        duration_ms: output.duration_ms,
    });

    if output.success {
        // ── Success path ──
        let reply_intent = output.parsed_intent.as_deref().unwrap_or("status-update");
        let reply_body = output
            .output
            .as_deref()
            .unwrap_or("(completed with no output)");

        event_bus.emit(OrchestratorEvent::ThreadStatusChanged {
            thread_id: output.thread_id.clone(),
            new_status: "Active".to_string(),
        });

        insert_reply_message(store, event_bus, output, reply_intent, reply_body).await;
    } else {
        // ── Failure path — check for retry eligibility ──
        let should_retry = should_retry_execution(output, agent_configs);

        if should_retry {
            // Enqueue retry execution with exponential backoff.
            let agent_config = agent_configs.iter().find(|a| a.alias == output.agent_alias);
            let backoff_secs = agent_config.map(|a| a.retry_backoff_secs).unwrap_or(30);
            let next_attempt = output.attempt_number + 1;
            // Exponential backoff: base * 2^attempt (capped at 1 hour)
            let delay_secs =
                (backoff_secs * 2u64.saturating_pow(output.attempt_number as u32)).min(3600);
            let retry_after = chrono::Utc::now().timestamp() + delay_secs as i64;

            // Look up prompt_hash from the failed execution for continuity.
            let prompt_hash = match store.get_execution(&output.execution_id).await {
                Ok(Some(exec)) => exec.prompt_hash,
                _ => None,
            };

            match store
                .insert_retry_execution(
                    &output.thread_id,
                    &output.agent_alias,
                    output.dispatch_message_id,
                    prompt_hash.as_deref(),
                    next_attempt,
                    retry_after,
                )
                .await
            {
                Ok(retry_exec_id) => {
                    tracing::info!(
                        exec_id = %output.execution_id,
                        retry_exec_id = %retry_exec_id,
                        thread_id = %output.thread_id,
                        agent = %output.agent_alias,
                        attempt = next_attempt,
                        retry_after = retry_after,
                        delay_secs = delay_secs,
                        error_category = ?output.error_category,
                        "enqueued retry execution"
                    );

                    // Emit retry event — thread stays Active
                    event_bus.emit(OrchestratorEvent::ExecutionRetrying {
                        execution_id: output.execution_id.clone(),
                        retry_execution_id: retry_exec_id,
                        thread_id: output.thread_id.clone(),
                        agent_alias: output.agent_alias.clone(),
                        attempt: next_attempt,
                        retry_after,
                    });

                    // Do NOT insert error reply or mark thread failed — retry is pending
                }
                Err(e) => {
                    tracing::error!(
                        exec_id = %output.execution_id,
                        thread_id = %output.thread_id,
                        error = %e,
                        "failed to enqueue retry — falling through to terminal failure"
                    );
                    // Fall through to terminal failure
                    mark_terminal_failure(store, event_bus, output).await;
                }
            }
        } else {
            // Terminal failure — no retry
            mark_terminal_failure(store, event_bus, output).await;
        }
    }

    tracing::info!(
        exec_id = %output.execution_id,
        thread_id = %output.thread_id,
        agent = %output.agent_alias,
        success = output.success,
        duration_ms = output.duration_ms,
        intent = ?output.parsed_intent,
        error_category = ?output.error_category,
        attempt = output.attempt_number,
        "trigger completed"
    );
}

/// Determine if a failed execution should be retried.
///
/// Criteria:
/// - Error category is retryable (Transient only)
/// - Execution did not time out (timed out = hung backend, not retried)
/// - Agent config allows retries (max_retries > 0)
/// - Current attempt is under the limit
fn should_retry_execution(output: &TriggerOutput, agent_configs: &[AgentConfig]) -> bool {
    // Never retry successful executions
    if output.success {
        return false;
    }

    // Never retry timeouts (hung backends)
    if output.timed_out {
        return false;
    }

    // Must have a retryable error category
    let is_retryable = output
        .error_category
        .as_ref()
        .is_some_and(|cat| cat.is_retryable());
    if !is_retryable {
        return false;
    }

    // Check agent config for retry limit
    let agent_config = agent_configs.iter().find(|a| a.alias == output.agent_alias);
    let max_retries = agent_config.map(|a| a.max_retries).unwrap_or(0);

    if max_retries == 0 {
        return false;
    }

    // attempt_number is 0-based; after attempt 0, we've done 1 try.
    // max_retries is the number of retry attempts allowed.
    // So we retry if (attempt_number + 1) <= max_retries,
    // i.e., attempt_number < max_retries.
    (output.attempt_number as u32) < max_retries
}

/// Mark a failed execution as terminal: set thread Failed, insert error reply.
async fn mark_terminal_failure(store: &Store, event_bus: &EventBus, output: &TriggerOutput) {
    let _ = store.mark_thread_failed_if_active(&output.thread_id).await;

    event_bus.emit(OrchestratorEvent::ThreadStatusChanged {
        thread_id: output.thread_id.clone(),
        new_status: "Failed".to_string(),
    });

    let reply_body = output
        .output
        .as_deref()
        .unwrap_or("(failed with no output)");
    insert_reply_message(store, event_bus, output, "error", reply_body).await;
}

/// Insert a reply message and emit MessageReceived event.
async fn insert_reply_message(
    store: &Store,
    event_bus: &EventBus,
    output: &TriggerOutput,
    intent: &str,
    body: &str,
) {
    match store
        .insert_message(
            &output.thread_id,
            &output.agent_alias,
            "operator",
            intent,
            body,
            None,
        )
        .await
    {
        Ok(message_id) => {
            event_bus.emit(OrchestratorEvent::MessageReceived {
                thread_id: output.thread_id.clone(),
                message_id,
                from_alias: output.agent_alias.clone(),
                intent: intent.to_string(),
            });
        }
        Err(e) => {
            tracing::error!(
                thread_id = %output.thread_id,
                error = %e,
                "failed to insert reply message"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_store() -> Store {
        let pool = sqlx::sqlite::SqlitePool::connect("sqlite::memory:")
            .await
            .unwrap();
        let store = Store::new(pool);
        store.setup().await.unwrap();
        store
    }

    #[tokio::test]
    async fn test_consume_telemetry_stores_and_flushes() {
        let store = test_store().await;
        let event_bus = EventBus::new();
        let mut rx_events = event_bus.subscribe();

        let (tx, rx) = std::sync::mpsc::sync_channel::<String>(128);

        // Feed Claude-style JSONL lines through the channel.
        tx.send(r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Write","input":{"file_path":"src/main.rs"}}]}}"#.to_string()).unwrap();
        tx.send(r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"cargo test"}}]}}"#.to_string()).unwrap();
        tx.send(
            r#"{"type":"result","subtype":"success","result":"Done.","session_id":"s1"}"#
                .to_string(),
        )
        .unwrap();
        // Also send an unrecognized line — should be silently ignored.
        tx.send(r#"{"type":"system","subtype":"init"}"#.to_string())
            .unwrap();

        // Drop sender to signal disconnect.
        drop(tx);

        consume_telemetry(
            rx, &store, &event_bus, "exec-1", "thread-1", "agent-a", "claude",
        )
        .await;

        // Verify events were stored in SQLite.
        let events = store
            .get_execution_events("exec-1", None, None, None)
            .await
            .unwrap();
        assert_eq!(
            events.len(),
            3,
            "expected 3 parsed events (2 tool_call + 1 turn_complete)"
        );
        assert_eq!(events[0].event_type, "tool_call");
        assert_eq!(events[0].summary, "Write to src/main.rs");
        assert_eq!(events[0].event_index, 0);
        assert_eq!(events[1].event_type, "tool_call");
        assert!(events[1].summary.starts_with("Bash:"));
        assert_eq!(events[1].event_index, 1);
        assert_eq!(events[2].event_type, "turn_complete");
        assert_eq!(events[2].event_index, 2);

        // Verify EventBus received ExecutionProgress events.
        let mut progress_count = 0;
        while let Ok(ev) = rx_events.try_recv() {
            if let OrchestratorEvent::ExecutionProgress { execution_id, .. } = ev {
                assert_eq!(execution_id, "exec-1");
                progress_count += 1;
            }
        }
        assert_eq!(progress_count, 3);
    }

    #[test]
    fn test_prompt_hash_same_prompt_same_hash() {
        let h1 = sha256_hex("You are a focused agent.");
        let h2 = sha256_hex("You are a focused agent.");
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_prompt_hash_different_prompts_different_hashes() {
        let h1 = sha256_hex("You are agent A.");
        let h2 = sha256_hex("You are agent B.");
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_prompt_hash_empty_prompt_is_valid() {
        let h = sha256_hex("");
        // SHA-256 of empty string is well-known
        assert_eq!(
            h,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[tokio::test]
    async fn test_consume_telemetry_unknown_backend_returns_immediately() {
        let store = test_store().await;
        let event_bus = EventBus::new();
        let (tx, rx) = std::sync::mpsc::sync_channel::<String>(128);
        tx.send(r#"{"type":"result"}"#.to_string()).unwrap();
        drop(tx);

        // "gemini" has no parser — should return immediately without storing anything.
        consume_telemetry(rx, &store, &event_bus, "exec-2", "t-2", "agent-b", "gemini").await;

        let events = store
            .get_execution_events("exec-2", None, None, None)
            .await
            .unwrap();
        assert!(events.is_empty());
    }

    // ── Retry logic tests ──

    fn test_agent_config(alias: &str, max_retries: u32) -> AgentConfig {
        AgentConfig {
            alias: alias.to_string(),
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
            max_retries,
            retry_backoff_secs: 10,
        }
    }

    fn failed_trigger_output(
        agent_alias: &str,
        error_category: Option<crate::backend::ErrorCategory>,
        attempt_number: i32,
        timed_out: bool,
    ) -> TriggerOutput {
        TriggerOutput {
            execution_id: "exec-test".to_string(),
            thread_id: "t-test".to_string(),
            agent_alias: agent_alias.to_string(),
            success: false,
            output: Some("error text".to_string()),
            exit_code: Some(1),
            duration_ms: 1000,
            parsed_intent: None,
            error_category,
            attempt_number,
            dispatch_message_id: Some(1),
            timed_out,
        }
    }

    #[test]
    fn test_should_retry_transient_error_with_retries_configured() {
        let configs = vec![test_agent_config("worker-a", 3)];
        let output = failed_trigger_output(
            "worker-a",
            Some(crate::backend::ErrorCategory::Transient),
            0,
            false,
        );
        assert!(should_retry_execution(&output, &configs));
    }

    #[test]
    fn test_should_not_retry_quota_error() {
        let configs = vec![test_agent_config("worker-a", 3)];
        let output = failed_trigger_output(
            "worker-a",
            Some(crate::backend::ErrorCategory::QuotaExhausted),
            0,
            false,
        );
        assert!(!should_retry_execution(&output, &configs));
    }

    #[test]
    fn test_should_not_retry_auth_error() {
        let configs = vec![test_agent_config("worker-a", 3)];
        let output = failed_trigger_output(
            "worker-a",
            Some(crate::backend::ErrorCategory::AuthFailure),
            0,
            false,
        );
        assert!(!should_retry_execution(&output, &configs));
    }

    #[test]
    fn test_should_not_retry_agent_error() {
        let configs = vec![test_agent_config("worker-a", 3)];
        let output = failed_trigger_output(
            "worker-a",
            Some(crate::backend::ErrorCategory::AgentError),
            0,
            false,
        );
        assert!(!should_retry_execution(&output, &configs));
    }

    #[test]
    fn test_should_not_retry_unknown_error() {
        let configs = vec![test_agent_config("worker-a", 3)];
        let output = failed_trigger_output(
            "worker-a",
            Some(crate::backend::ErrorCategory::Unknown),
            0,
            false,
        );
        assert!(!should_retry_execution(&output, &configs));
    }

    #[test]
    fn test_should_not_retry_when_max_retries_zero() {
        let configs = vec![test_agent_config("worker-a", 0)];
        let output = failed_trigger_output(
            "worker-a",
            Some(crate::backend::ErrorCategory::Transient),
            0,
            false,
        );
        assert!(!should_retry_execution(&output, &configs));
    }

    #[test]
    fn test_should_not_retry_at_max_attempts() {
        let configs = vec![test_agent_config("worker-a", 2)];
        // attempt_number = 2, max_retries = 2 → already used both retries
        let output = failed_trigger_output(
            "worker-a",
            Some(crate::backend::ErrorCategory::Transient),
            2,
            false,
        );
        assert!(!should_retry_execution(&output, &configs));
    }

    #[test]
    fn test_should_retry_under_max_attempts() {
        let configs = vec![test_agent_config("worker-a", 3)];
        // attempt_number = 1, max_retries = 3 → 1 < 3, should retry
        let output = failed_trigger_output(
            "worker-a",
            Some(crate::backend::ErrorCategory::Transient),
            1,
            false,
        );
        assert!(should_retry_execution(&output, &configs));
    }

    #[test]
    fn test_should_not_retry_on_timeout() {
        let configs = vec![test_agent_config("worker-a", 3)];
        let output = failed_trigger_output(
            "worker-a",
            Some(crate::backend::ErrorCategory::Transient),
            0,
            true, // timed_out
        );
        assert!(!should_retry_execution(&output, &configs));
    }

    #[test]
    fn test_should_not_retry_success() {
        let configs = vec![test_agent_config("worker-a", 3)];
        let mut output = failed_trigger_output(
            "worker-a",
            Some(crate::backend::ErrorCategory::Transient),
            0,
            false,
        );
        output.success = true;
        assert!(!should_retry_execution(&output, &configs));
    }

    #[test]
    fn test_should_not_retry_no_error_category() {
        let configs = vec![test_agent_config("worker-a", 3)];
        let output = failed_trigger_output("worker-a", None, 0, false);
        assert!(!should_retry_execution(&output, &configs));
    }

    #[tokio::test]
    async fn test_handle_trigger_output_retries_transient_error() {
        let store = test_store().await;
        let event_bus = EventBus::new();
        let mut rx = event_bus.subscribe();

        let configs = vec![test_agent_config("worker-a", 3)];

        // Setup: create thread, dispatch message, and failed execution
        store.ensure_thread("t-retry", None).await.unwrap();
        let msg_id = store
            .insert_message("t-retry", "operator", "worker-a", "dispatch", "task", None)
            .await
            .unwrap();
        let exec_id = store
            .insert_execution_with_dispatch("t-retry", "worker-a", Some(msg_id), None)
            .await
            .unwrap()
            .unwrap();

        // Simulate failed execution
        let output = TriggerOutput {
            execution_id: exec_id.clone(),
            thread_id: "t-retry".to_string(),
            agent_alias: "worker-a".to_string(),
            success: false,
            output: Some("connection refused".to_string()),
            exit_code: Some(1),
            duration_ms: 500,
            parsed_intent: None,
            error_category: Some(crate::backend::ErrorCategory::Transient),
            attempt_number: 0,
            dispatch_message_id: Some(msg_id),
            timed_out: false,
        };

        handle_trigger_output(&store, &event_bus, &output, &configs).await;

        // Thread should still be Active (not Failed)
        let status = store.get_thread_status("t-retry").await.unwrap();
        assert_eq!(status.as_deref(), Some("Active"));

        // Should have enqueued a retry execution
        let execs = store.get_thread_executions("t-retry").await.unwrap();
        assert_eq!(execs.len(), 2, "should have original + retry execution");
        // Fetch full execution to get retry-specific fields
        let retry_exec = store.get_execution(&execs[1].id).await.unwrap().unwrap();
        assert_eq!(retry_exec.status, "queued");
        assert_eq!(retry_exec.attempt_number, 1);
        assert!(retry_exec.retry_after.is_some());

        // Check events
        let mut found_retrying = false;
        while let Ok(ev) = rx.try_recv() {
            if let OrchestratorEvent::ExecutionRetrying { attempt, .. } = ev {
                assert_eq!(attempt, 1);
                found_retrying = true;
            }
        }
        assert!(
            found_retrying,
            "should have emitted ExecutionRetrying event"
        );
    }

    #[tokio::test]
    async fn test_handle_trigger_output_terminal_on_non_retryable() {
        let store = test_store().await;
        let event_bus = EventBus::new();

        let configs = vec![test_agent_config("worker-a", 3)];

        store.ensure_thread("t-term", None).await.unwrap();
        let msg_id = store
            .insert_message("t-term", "operator", "worker-a", "dispatch", "task", None)
            .await
            .unwrap();
        let exec_id = store
            .insert_execution_with_dispatch("t-term", "worker-a", Some(msg_id), None)
            .await
            .unwrap()
            .unwrap();

        let output = TriggerOutput {
            execution_id: exec_id,
            thread_id: "t-term".to_string(),
            agent_alias: "worker-a".to_string(),
            success: false,
            output: Some("quota exceeded".to_string()),
            exit_code: Some(1),
            duration_ms: 500,
            parsed_intent: None,
            error_category: Some(crate::backend::ErrorCategory::QuotaExhausted),
            attempt_number: 0,
            dispatch_message_id: Some(msg_id),
            timed_out: false,
        };

        handle_trigger_output(&store, &event_bus, &output, &configs).await;

        // Thread should be Failed
        let status = store.get_thread_status("t-term").await.unwrap();
        assert_eq!(status.as_deref(), Some("Failed"));

        // Should NOT have enqueued a retry
        let execs = store.get_thread_executions("t-term").await.unwrap();
        assert_eq!(execs.len(), 1, "should only have original execution");
    }

    #[tokio::test]
    async fn test_handle_trigger_output_terminal_at_max_retries() {
        let store = test_store().await;
        let event_bus = EventBus::new();

        let configs = vec![test_agent_config("worker-a", 1)]; // max_retries = 1

        store.ensure_thread("t-max", None).await.unwrap();
        let msg_id = store
            .insert_message("t-max", "operator", "worker-a", "dispatch", "task", None)
            .await
            .unwrap();
        let exec_id = store
            .insert_execution_with_dispatch("t-max", "worker-a", Some(msg_id), None)
            .await
            .unwrap()
            .unwrap();

        // attempt_number = 1 means we've already done the first retry
        // max_retries = 1 → no more retries
        let output = TriggerOutput {
            execution_id: exec_id,
            thread_id: "t-max".to_string(),
            agent_alias: "worker-a".to_string(),
            success: false,
            output: Some("connection refused again".to_string()),
            exit_code: Some(1),
            duration_ms: 500,
            parsed_intent: None,
            error_category: Some(crate::backend::ErrorCategory::Transient),
            attempt_number: 1, // already at the limit
            dispatch_message_id: Some(msg_id),
            timed_out: false,
        };

        handle_trigger_output(&store, &event_bus, &output, &configs).await;

        // Thread should be Failed — max retries exceeded
        let status = store.get_thread_status("t-max").await.unwrap();
        assert_eq!(status.as_deref(), Some("Failed"));
    }
}
