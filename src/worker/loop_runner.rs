//! WorkerRunner — poll-loop based trigger worker.
//!
//! On startup:
//! 1. Kills orphaned backend processes (if PID recorded before crash)
//! 2. Marks orphaned executions (picked_up/executing) as crashed
//! 3. Writes initial heartbeat
//!
//! Main loop:
//! 1. Scans for untriggered messages and enqueues executions
//! 2. Polls `executions` table for queued work via `claim_next_execution`
//! 3. For each claimed execution, spawns a task to run the backend trigger
//! 4. Writes periodic heartbeats
//! 5. Inserts reply messages from completed triggers
//! 6. Emits `OrchestratorEvent`s on all state transitions

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::Semaphore;

use crate::backend::registry::BackendRegistry;
use crate::config::types::{AgentConfig, AgentRole, HandoffTarget};
use crate::config::ConfigHandle;
use crate::events::{EventBus, OrchestratorEvent};
use crate::merge::MergeExecutor;
use crate::store::{MergeOperationStatus, Store};
use crate::worker::circuit_breaker::{CircuitBreakerRegistry, CircuitState};
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
    circuit_breaker: Arc<Mutex<CircuitBreakerRegistry>>,
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
            circuit_breaker: Arc::new(Mutex::new(CircuitBreakerRegistry::new())),
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

        // Crash recovery: kill orphan backend processes, then mark executions crashed.
        // kill_process is blocking (SIGTERM → poll up to 5s → SIGKILL), so each
        // kill runs inside spawn_blocking to avoid starving the tokio runtime.
        if let Ok(orphans) = self.store.get_orphaned_executions_with_pid().await {
            for (exec_id, pid) in &orphans {
                #[cfg(unix)]
                {
                    let alive = unsafe { libc::kill(*pid as i32, 0) == 0 };
                    if alive {
                        tracing::warn!(
                            exec_id = %exec_id,
                            pid = pid,
                            "killing orphaned backend process"
                        );
                        let kill_pid = *pid;
                        let kill_exec_id = exec_id.clone();
                        let kill_result = tokio::task::spawn_blocking(move || {
                            crate::backend::process::kill_process(kill_pid)
                        })
                        .await;
                        match kill_result {
                            Ok(Err(e)) => {
                                tracing::warn!(
                                    exec_id = %kill_exec_id,
                                    pid = pid,
                                    error = %e,
                                    "failed to kill orphaned process"
                                );
                            }
                            Err(e) => {
                                tracing::warn!(
                                    exec_id = %kill_exec_id,
                                    pid = pid,
                                    error = %e,
                                    "spawn_blocking panicked during orphan kill"
                                );
                            }
                            Ok(Ok(())) => {}
                        }
                    }
                }
            }
        }
        let crashed = self.store.mark_orphaned_executions_crashed().await?;
        if crashed > 0 {
            tracing::warn!(count = crashed, "marked orphaned executions as crashed");
        }

        // Crash recovery: mark any claimed/executing merge operations as failed.
        match self.store.mark_stale_merge_ops_failed(0).await {
            Ok(count) if count > 0 => {
                tracing::warn!(count, "marked orphaned merge operations as failed");
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to mark orphaned merge ops");
            }
            _ => {}
        }

        // Crash recovery: clean up leftover merge worktrees from previous crash.
        {
            let repo_root = startup_config.default_workdir.clone();
            match tokio::task::spawn_blocking(move || {
                MergeExecutor::cleanup_orphaned_merge_worktrees(&repo_root, &[])
            })
            .await
            {
                Ok(Ok(count)) if count > 0 => {
                    tracing::info!(count, "cleaned up orphaned merge worktrees");
                }
                Ok(Err(e)) => {
                    tracing::warn!(error = %e, "failed to cleanup orphaned merge worktrees");
                }
                Err(e) => {
                    tracing::warn!(error = %e, "spawn_blocking panicked during merge worktree cleanup");
                }
                _ => {}
            }
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
        // Merge queue polling — same interval as the main execution poll.
        let mut merge_interval =
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

        // CRON-2: Schedule evaluation interval (every 60s).
        // On startup, load last-fire times from the durable schedule_runs
        // table to avoid double-firing after restart.
        let mut schedule_runs_cache: std::collections::HashMap<String, (i64, u64)> =
            match self.store.get_all_schedule_runs().await {
                Ok(runs) => {
                    if !runs.is_empty() {
                        tracing::info!(count = runs.len(), "loaded schedule run state from DB");
                    }
                    runs
                }
                Err(e) => {
                    tracing::warn!(error = %e, "failed to load schedule runs; starting fresh");
                    std::collections::HashMap::new()
                }
            };
        let mut schedule_interval = tokio::time::interval(Duration::from_secs(60));

        // Create the shutdown signal future once before the loop.
        let mut shutdown = std::pin::pin!(shutdown_signal());

        loop {
            tokio::select! {
                _ = poll_interval.tick() => {
                    self.poll_once(&semaphore).await;
                }
                _ = merge_interval.tick() => {
                    self.poll_merge_ops().await;
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

                    // Stale merge operation detection
                    let merge_timeout_secs = self.config.load().orchestration.merge_timeout_secs;
                    match self.store.mark_stale_merge_ops_failed(merge_timeout_secs).await {
                        Ok(count) if count > 0 => {
                            tracing::warn!(count, merge_timeout_secs, "marked stale merge operations as failed");
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "stale merge operation check failed");
                        }
                        _ => {}
                    }

                    // Worktree cleanup for terminal threads
                    match self.store.threads_with_stale_worktrees().await {
                        Ok(stale) => {
                            for (thread_id, worktree_path, repo_root) in &stale {
                                let repo_root = std::path::PathBuf::from(repo_root);
                                let wt_path = std::path::PathBuf::from(worktree_path);

                                // Safety check: refuse to delete worktrees with
                                // uncommitted changes or unverifiable status to prevent data loss.
                                match WorktreeManager::worktree_status(&wt_path) {
                                    Ok(None) => {
                                        // Clean — proceed with removal below.
                                    }
                                    Ok(Some(_)) => {
                                        tracing::warn!(
                                            thread_id = %thread_id,
                                            path = %wt_path.display(),
                                            branch = %format!("compas/{}", thread_id),
                                            "skipping worktree cleanup — uncommitted changes detected, merge or commit before closing"
                                        );
                                        // Do not clear worktree_path in DB — retry next cycle.
                                        continue;
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            thread_id = %thread_id,
                                            path = %wt_path.display(),
                                            branch = %format!("compas/{}", thread_id),
                                            error = %e,
                                            "skipping worktree cleanup — cannot verify worktree status"
                                        );
                                        // Do not clear worktree_path in DB — retry next cycle.
                                        continue;
                                    }
                                }

                                if let Err(e) = self.worktree_manager.remove_worktree_at_path(&repo_root, &wt_path, thread_id) {
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
                _ = schedule_interval.tick() => {
                    self.evaluate_schedules(&mut schedule_runs_cache).await;
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

                    // ── Circuit breaker check ──
                    // If the backend's circuit is Open, re-queue with a delay
                    // instead of burning tokens on a known-failing backend.
                    let cb_config = &trigger_config.orchestration.circuit_breaker;
                    if cb_config.enabled && !backend_name.is_empty() {
                        let cb_state = {
                            let mut cb = self
                                .circuit_breaker
                                .lock()
                                .unwrap_or_else(|e| e.into_inner());
                            cb.check(&backend_name, cb_config.cooldown_secs)
                        };
                        if cb_state == CircuitState::Open {
                            tracing::warn!(
                                exec_id = %execution.id,
                                backend = %backend_name,
                                agent = %execution.agent_alias,
                                "circuit breaker OPEN — re-queuing execution with delay"
                            );
                            // Re-queue: set retry_after to now + cooldown_secs
                            let retry_after =
                                chrono::Utc::now().timestamp() + cb_config.cooldown_secs as i64;
                            let _ = store
                                .set_execution_retry_after(&execution.id, retry_after)
                                .await;
                            // Release permit so other work can proceed
                            drop(permit);
                            continue;
                        }
                        // HalfOpen or Closed: proceed normally
                    }

                    let worktree_manager = self.worktree_manager.clone();
                    let default_workdir = trigger_config.default_workdir.clone();
                    let worktree_override_dir = trigger_config.worktree_dir.clone();

                    // Clone circuit breaker + config for post-execution recording.
                    let cb = self.circuit_breaker.clone();
                    let cb_enabled = trigger_config.orchestration.circuit_breaker.enabled;
                    let cb_threshold = trigger_config
                        .orchestration
                        .circuit_breaker
                        .failure_threshold;
                    let trigger_backend_name = backend_name.clone();

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
                        // linked to this execution. Retry executions use
                        // original_dispatch_message_id instead of dispatch_message_id
                        // (the latter is reserved for the UNIQUE index on first dispatch).
                        // Legacy/unlinked rows fall back to a placeholder instruction.
                        let effective_dispatch_id = execution
                            .dispatch_message_id
                            .or(execution.original_dispatch_message_id);
                        let instruction = if let Some(dispatch_id) = effective_dispatch_id {
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
                            &default_workdir,
                            worktree_override_dir,
                        )
                        .await;

                        // Wait for telemetry consumer to finish flushing.
                        // The stdout_tx is dropped when execute_trigger returns,
                        // which causes the consumer's recv to get Disconnected.
                        let _ = telemetry_handle.await;

                        // Post-execution: insert reply message, emit events, and
                        // potentially enqueue a retry for transient failures.
                        handle_trigger_output(
                            &store,
                            &event_bus,
                            &output,
                            &agent_configs,
                            &cb,
                            cb_enabled,
                            cb_threshold,
                            &trigger_backend_name,
                        )
                        .await;
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

    /// Poll for queued merge operations and execute them.
    ///
    /// Claims at most one merge op per poll cycle. The store's `claim_next_merge_op`
    /// enforces per-target-branch serialization (at most one active merge per target).
    /// Execution is spawned as a detached task to avoid blocking the select! loop.
    ///
    /// `pub` so integration tests in `tests/` can drive the worker directly without
    /// starting the full poll loop.
    pub async fn poll_merge_ops(&self) {
        let op = match self.store.claim_next_merge_op().await {
            Ok(Some(op)) => op,
            Ok(None) => return, // No work or all target branches busy
            Err(e) => {
                tracing::error!(error = %e, "failed to claim merge operation");
                return;
            }
        };

        tracing::info!(
            op_id = %op.id,
            source = %op.source_branch,
            target = %op.target_branch,
            strategy = %op.merge_strategy,
            "merge operation claimed"
        );

        // Transition to Executing
        if let Err(e) = self
            .store
            .update_merge_op_status(&op.id, MergeOperationStatus::Executing, None, None, None)
            .await
        {
            tracing::error!(op_id = %op.id, error = %e, "failed to set merge op to executing");
            return;
        }

        self.event_bus.emit(OrchestratorEvent::MergeStarted {
            op_id: op.id.clone(),
            thread_id: op.thread_id.clone(),
            source_branch: op.source_branch.clone(),
            target_branch: op.target_branch.clone(),
        });

        // Spawn a detached task so the merge does not block the select! loop.
        let store = self.store.clone();
        let event_bus = self.event_bus.clone();
        // Resolve repo_root from thread's worktree_repo_root (per-agent workdir),
        // falling back to config.default_workdir for shared-workspace or legacy threads.
        let repo_root = match self.store.get_thread_worktree_info(&op.thread_id).await {
            Ok(Some((_, root))) => root,
            Ok(None) => self.config.load().default_workdir.clone(),
            Err(e) => {
                tracing::warn!(op_id = %op.id, thread_id = %op.thread_id, error = %e,
                    "get_thread_worktree_info failed, falling back to default_workdir");
                self.config.load().default_workdir.clone()
            }
        };

        tokio::spawn(async move {
            // MergeExecutor::execute runs blocking git subprocesses — must use spawn_blocking.
            let op_clone = op.clone();
            let root = repo_root.clone();
            let result =
                tokio::task::spawn_blocking(move || MergeExecutor::execute(&op_clone, &root)).await;

            match result {
                Ok(Ok(merge_result)) if merge_result.success => {
                    tracing::info!(
                        op_id = %op.id,
                        source = %op.source_branch,
                        target = %op.target_branch,
                        "merge completed"
                    );
                    if let Err(e) = store
                        .update_merge_op_status(
                            &op.id,
                            MergeOperationStatus::Completed,
                            merge_result.summary.as_deref(),
                            None,
                            None,
                        )
                        .await
                    {
                        tracing::error!(op_id = %op.id, error = %e, "failed to update merge op to completed");
                    }
                    event_bus.emit(OrchestratorEvent::MergeCompleted {
                        op_id: op.id.clone(),
                        thread_id: op.thread_id.clone(),
                        success: true,
                    });
                }
                Ok(Ok(merge_result)) => {
                    // Merge executed but failed (e.g. conflict)
                    let conflict_json = merge_result
                        .conflict_files
                        .as_ref()
                        .and_then(|files| serde_json::to_string(files).ok());
                    let error_msg = merge_result
                        .error
                        .as_deref()
                        .unwrap_or("merge failed (unknown reason)");
                    tracing::info!(
                        op_id = %op.id,
                        error = %error_msg,
                        "merge failed"
                    );
                    if let Err(e) = store
                        .update_merge_op_status(
                            &op.id,
                            MergeOperationStatus::Failed,
                            None,
                            Some(error_msg),
                            conflict_json.as_deref(),
                        )
                        .await
                    {
                        tracing::error!(op_id = %op.id, error = %e, "failed to update merge op to failed");
                    }
                    event_bus.emit(OrchestratorEvent::MergeCompleted {
                        op_id: op.id.clone(),
                        thread_id: op.thread_id.clone(),
                        success: false,
                    });
                }
                Ok(Err(e)) => {
                    // MergeExecutor::execute returned Err (infrastructure failure)
                    tracing::error!(
                        op_id = %op.id,
                        error = %e,
                        "merge infrastructure failure"
                    );
                    if let Err(update_err) = store
                        .update_merge_op_status(
                            &op.id,
                            MergeOperationStatus::Failed,
                            None,
                            Some(&e),
                            None,
                        )
                        .await
                    {
                        tracing::error!(op_id = %op.id, error = %update_err, "failed to update merge op to failed");
                    }
                    event_bus.emit(OrchestratorEvent::MergeCompleted {
                        op_id: op.id.clone(),
                        thread_id: op.thread_id.clone(),
                        success: false,
                    });
                }
                Err(join_err) => {
                    // spawn_blocking panicked
                    let error_msg = format!("merge task panicked: {}", join_err);
                    tracing::error!(op_id = %op.id, error = %error_msg, "merge task panicked");
                    if let Err(e) = store
                        .update_merge_op_status(
                            &op.id,
                            MergeOperationStatus::Failed,
                            None,
                            Some(&error_msg),
                            None,
                        )
                        .await
                    {
                        tracing::error!(op_id = %op.id, error = %e, "failed to update merge op to failed after panic");
                    }
                    event_bus.emit(OrchestratorEvent::MergeCompleted {
                        op_id: op.id.clone(),
                        thread_id: op.thread_id.clone(),
                        success: false,
                    });
                }
            }
        });
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

    /// Evaluate configured cron schedules and dispatch messages for due schedules.
    ///
    /// Reads schedules from the live-reloaded config on each tick. For each
    /// enabled schedule that hasn't exceeded `max_runs`, check if the cron
    /// expression indicates a firing should have occurred since the last fire.
    /// When due, insert a dispatch message and record the fire in the durable
    /// `schedule_runs` table.
    async fn evaluate_schedules(&self, cache: &mut std::collections::HashMap<String, (i64, u64)>) {
        let config = self.config.load();
        let schedules = match config.schedules.as_ref() {
            Some(s) if !s.is_empty() => s,
            _ => return,
        };

        let now_utc = chrono::Utc::now();
        let now_ts = now_utc.timestamp();

        for sched in schedules {
            // Skip disabled schedules.
            if !sched.enabled {
                continue;
            }

            // Check max_runs cap.
            let (last_fired_at, run_count) = cache.get(&sched.name).copied().unwrap_or((0, 0));

            if run_count >= sched.max_runs {
                tracing::debug!(
                    schedule = %sched.name,
                    run_count,
                    max_runs = sched.max_runs,
                    "schedule hit max_runs cap, skipping"
                );
                continue;
            }

            // Parse cron expression.
            let cron = match sched.cron.parse::<croner::Cron>() {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(
                        schedule = %sched.name,
                        cron = %sched.cron,
                        error = %e,
                        "invalid cron expression, skipping"
                    );
                    continue;
                }
            };

            // Determine if the schedule is due.
            // Find the next occurrence after the last fire time. If that
            // occurrence is at or before now, the schedule is due.
            let reference_time = if last_fired_at > 0 {
                match chrono::DateTime::from_timestamp(last_fired_at, 0) {
                    Some(dt) => dt,
                    None => {
                        tracing::warn!(
                            schedule = %sched.name,
                            last_fired_at,
                            "invalid last_fired_at timestamp, treating as never fired"
                        );
                        chrono::DateTime::from_timestamp(0, 0).unwrap()
                    }
                }
            } else {
                // Never fired — use epoch so first due time triggers.
                chrono::DateTime::from_timestamp(0, 0).unwrap()
            };

            let next_occurrence = match cron.find_next_occurrence(&reference_time, false) {
                Ok(next) => next,
                Err(e) => {
                    tracing::warn!(
                        schedule = %sched.name,
                        error = %e,
                        "failed to compute next cron occurrence"
                    );
                    continue;
                }
            };

            if next_occurrence > now_utc {
                // Not due yet.
                continue;
            }

            // Schedule is due — create a dispatch message.
            let thread_id = ulid::Ulid::new().to_string();
            tracing::info!(
                schedule = %sched.name,
                agent = %sched.agent,
                thread_id = %thread_id,
                run_count = run_count + 1,
                max_runs = sched.max_runs,
                "cron schedule due, dispatching"
            );

            match self
                .store
                .insert_message(
                    &thread_id,
                    "scheduler",
                    &sched.agent,
                    "dispatch",
                    &sched.body,
                    sched.batch.as_deref(),
                    Some(&format!("[sched] {}", sched.name)),
                )
                .await
            {
                Ok(_msg_id) => {
                    // Record fire in the durable table, then update cache.
                    // Cache is always updated for in-process dedup, but the
                    // log severity differs: if the DB write fails, a restart
                    // will re-read stale DB state and double-fire.
                    match self.store.record_schedule_fire(&sched.name, now_ts).await {
                        Ok(()) => {
                            cache.insert(sched.name.clone(), (now_ts, run_count + 1));
                        }
                        Err(e) => {
                            tracing::error!(
                                schedule = %sched.name,
                                error = %e,
                                "failed to record schedule fire in DB; restart will cause double-fire"
                            );
                            // Still update cache to prevent in-process duplicate fires.
                            cache.insert(sched.name.clone(), (now_ts, run_count + 1));
                        }
                    }
                }
                Err(e) => {
                    tracing::error!(
                        schedule = %sched.name,
                        error = %e,
                        "failed to insert scheduled dispatch message"
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
    use crate::backend::gemini::parse_gemini_stream_line;
    use crate::backend::opencode::parse_opencode_stream_line;
    use crate::backend::ExecutionEvent;
    use std::time::{Duration, Instant};

    use crate::backend::claude::extract_session_id_from_line as claude_extract;
    use crate::backend::codex::extract_session_id_from_line as codex_extract;
    use crate::backend::opencode::extract_session_id_from_line as opencode_extract;

    let parser: fn(&str) -> Option<ExecutionEvent> = match backend_name {
        "claude" => parse_claude_stream_line,
        "codex" => parse_codex_stream_line,
        "gemini" => parse_gemini_stream_line,
        "opencode" => parse_opencode_stream_line,
        _ => return, // No parser for this backend
    };

    // Gemini doesn't emit a session ID in stream lines — use a no-op extractor.
    fn no_session_id(_line: &str) -> Option<String> {
        None
    }
    let session_id_extractor: fn(&str) -> Option<String> = match backend_name {
        "claude" => claude_extract,
        "codex" => codex_extract,
        "opencode" => opencode_extract,
        _ => no_session_id,
    };

    let mut buffer: Vec<ExecutionEvent> = Vec::new();
    let mut event_index: i32 = 0;
    let mut last_flush = Instant::now();
    let mut session_id_persisted = false;

    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(line) => {
                // Mid-stream session ID persistence: extract and persist the
                // backend session ID from the first matching stdout line. This
                // is a one-shot operation so the session ID survives crashes.
                if !session_id_persisted {
                    if let Some(sid) = session_id_extractor(&line) {
                        if let Err(e) = store.set_backend_session_id(execution_id, &sid).await {
                            tracing::warn!(
                                exec_id = %execution_id,
                                error = %e,
                                "failed to persist session ID mid-stream"
                            );
                        } else {
                            tracing::debug!(
                                exec_id = %execution_id,
                                session_id = %sid,
                                "persisted backend session ID mid-stream"
                            );
                            session_id_persisted = true;
                        }
                    }
                }

                if let Some(mut event) = parser(&line) {
                    event.event_index = event_index;
                    event_index += 1;

                    // Skip noisy event types from the live progress bus — they
                    // still get stored in the DB via the buffer flush below.
                    if event.event_type != "tool_result" && event.event_type != "turn_complete" {
                        event_bus.emit(OrchestratorEvent::ExecutionProgress {
                            execution_id: execution_id.to_string(),
                            thread_id: thread_id.to_string(),
                            agent_alias: agent_alias.to_string(),
                            summary: event.summary.clone(),
                        });
                    }

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
#[allow(clippy::too_many_arguments)]
async fn handle_trigger_output(
    store: &Store,
    event_bus: &EventBus,
    output: &TriggerOutput,
    agent_configs: &[AgentConfig],
    circuit_breaker: &Arc<Mutex<CircuitBreakerRegistry>>,
    cb_enabled: bool,
    cb_threshold: u32,
    backend_name: &str,
) {
    // Fetch thread summary for richer notifications/hooks.
    let thread_summary = store
        .get_thread(&output.thread_id)
        .await
        .ok()
        .flatten()
        .and_then(|t| t.summary);

    // Emit ExecutionCompleted before any retry/reply logic.
    event_bus.emit(OrchestratorEvent::ExecutionCompleted {
        execution_id: output.execution_id.clone(),
        thread_id: output.thread_id.clone(),
        agent_alias: output.agent_alias.clone(),
        success: output.success,
        duration_ms: output.duration_ms,
        thread_summary,
    });

    // ── Circuit breaker recording ──
    // Record success/failure for the backend's circuit breaker.
    // StaleSession errors that will be retried are NOT counted as failures
    // (the retry may succeed, proving the backend is healthy).
    //
    // The lock is scoped to avoid holding it across .await points.
    if cb_enabled && !backend_name.is_empty() {
        let will_retry = should_retry_execution(output, agent_configs);
        let is_stale_session_retry = !output.success
            && will_retry
            && output.error_category.as_ref() == Some(&crate::backend::ErrorCategory::StaleSession);

        if output.success {
            let state_changed = {
                let mut cb = circuit_breaker.lock().unwrap_or_else(|e| e.into_inner());
                let prev = cb.state_of(backend_name);
                cb.record_success(backend_name);
                prev != CircuitState::Closed
            };
            if state_changed {
                tracing::info!(
                    backend = %backend_name,
                    "circuit breaker reset to CLOSED after successful execution"
                );
                let _ = store
                    .set_circuit_breaker_state(backend_name, "closed", 0)
                    .await;
            }
        } else if !is_stale_session_retry {
            // Only record failure for terminal failures (not retryable stale sessions)
            if !will_retry {
                let (new_state, failures) = {
                    let mut cb = circuit_breaker.lock().unwrap_or_else(|e| e.into_inner());
                    let state = cb.record_failure(backend_name, cb_threshold);
                    let f = cb
                        .states()
                        .into_iter()
                        .find(|(n, _, _)| n == backend_name)
                        .map(|(_, _, f)| f)
                        .unwrap_or(0);
                    (state, f)
                };
                if new_state == CircuitState::Open {
                    tracing::warn!(
                        backend = %backend_name,
                        threshold = cb_threshold,
                        "circuit breaker OPENED — backend consistently failing"
                    );
                    let _ = store
                        .set_circuit_breaker_state(backend_name, "open", failures)
                        .await;
                }
            }
        }
    }

    if output.success {
        // ── Success path ──
        let reply_intent = output.parsed_intent.as_deref().unwrap_or("response");
        let reply_body = output
            .output
            .as_deref()
            .unwrap_or("(completed with no output)");

        event_bus.emit(OrchestratorEvent::ThreadStatusChanged {
            thread_id: output.thread_id.clone(),
            new_status: "Active".to_string(),
        });

        // Resolve the handoff type BEFORE inserting the reply so that
        // fan-out can use a single transaction (reply + fan-out threads).
        let handoff_resolution = resolve_handoff(store, output, agent_configs, reply_body).await;

        match handoff_resolution {
            HandoffResolution::FanOut {
                batch_id,
                targets,
                handoff_body,
            } => {
                // Atomic: reply + fan-out threads in one transaction.
                let params = crate::store::ReplyAndFanoutParams {
                    reply_thread_id: &output.thread_id,
                    reply_from: &output.agent_alias,
                    reply_to: "operator",
                    reply_intent,
                    reply_body,
                    source_thread_id: &output.thread_id,
                    batch_id: &batch_id,
                    targets: &targets,
                    handoff_from: &output.agent_alias,
                    handoff_body: &handoff_body,
                };
                match store.insert_reply_and_fanout(&params).await {
                    Ok((reply_msg_id, fanout_results)) => {
                        // Emit reply event
                        event_bus.emit(OrchestratorEvent::MessageReceived {
                            thread_id: output.thread_id.clone(),
                            message_id: reply_msg_id,
                            from_alias: output.agent_alias.clone(),
                            intent: reply_intent.to_string(),
                        });
                        // Emit fan-out events
                        for (idx, (thread_id, message_id)) in fanout_results.iter().enumerate() {
                            let target_alias = &targets[idx];
                            tracing::info!(
                                source_thread_id = %output.thread_id,
                                fanout_thread_id = %thread_id,
                                fanout_message_id = %message_id,
                                from = %output.agent_alias,
                                to = %target_alias,
                                batch_id = %batch_id,
                                "fan-out handoff dispatched (atomic)"
                            );
                            event_bus.emit(OrchestratorEvent::MessageReceived {
                                thread_id: thread_id.clone(),
                                message_id: *message_id,
                                from_alias: output.agent_alias.clone(),
                                intent: "handoff".to_string(),
                            });
                            event_bus.emit(OrchestratorEvent::ThreadStatusChanged {
                                thread_id: thread_id.clone(),
                                new_status: "Active".to_string(),
                            });
                        }
                    }
                    Err(e) => {
                        tracing::error!(
                            thread_id = %output.thread_id,
                            error = %e,
                            "atomic reply + fan-out failed; falling back to non-transactional reply"
                        );
                        // Fallback: insert reply separately so the agent's response
                        // is never silently lost.
                        insert_reply_message(store, event_bus, output, reply_intent, reply_body)
                            .await;
                        // Notify operator that fan-out failed so --await-chain
                        // doesn't silently return with 0 pending.
                        let fail_body = format!(
                            "Fan-out dispatch failed after agent '{}' completed. \
                             The agent's reply was saved but reviewer targets {:?} \
                             were not dispatched. Error: {}",
                            output.agent_alias, targets, e
                        );
                        if let Ok(msg_id) = store
                            .insert_message(
                                &output.thread_id,
                                &output.agent_alias,
                                "operator",
                                "review-request",
                                &fail_body,
                                None,
                                None,
                            )
                            .await
                        {
                            event_bus.emit(OrchestratorEvent::MessageReceived {
                                thread_id: output.thread_id.clone(),
                                message_id: msg_id,
                                from_alias: output.agent_alias.clone(),
                                intent: "review-request".to_string(),
                            });
                        }
                    }
                }
            }
            HandoffResolution::FanOutDepthExceeded { max_depth } => {
                // Fan-out depth limit reached — insert reply + review-request.
                insert_reply_message(store, event_bus, output, reply_intent, reply_body).await;

                let interrupt_body = format!(
                    "Fan-out auto-handoff chain interrupted at depth limit {}. \
                     Agent '{}' completed but fan-out targets were not dispatched. \
                     Please review and decide next step.",
                    max_depth, output.agent_alias
                );
                if let Ok(msg_id) = store
                    .insert_message(
                        &output.thread_id,
                        &output.agent_alias,
                        "operator",
                        "review-request",
                        &interrupt_body,
                        None,
                        None,
                    )
                    .await
                {
                    event_bus.emit(OrchestratorEvent::MessageReceived {
                        thread_id: output.thread_id.clone(),
                        message_id: msg_id,
                        from_alias: output.agent_alias.clone(),
                        intent: "review-request".to_string(),
                    });
                }
            }
            HandoffResolution::SingleOrNone => {
                // Non-fan-out: insert reply then maybe handoff (existing behavior).
                insert_reply_message(store, event_bus, output, reply_intent, reply_body).await;
                maybe_auto_handoff(
                    store,
                    event_bus,
                    output,
                    agent_configs,
                    reply_intent,
                    reply_body,
                )
                .await;
            }
        }
    } else {
        // ── Failure path — check for retry eligibility ──
        let should_retry = should_retry_execution(output, agent_configs);

        if should_retry {
            // If this is a stale session error, clear the backend session ID
            // so the retry starts a fresh session instead of re-using the stale one.
            if output.error_category.as_ref() == Some(&crate::backend::ErrorCategory::StaleSession)
            {
                tracing::warn!(
                    thread_id = %output.thread_id,
                    agent = %output.agent_alias,
                    "stale session detected — clearing backend_session_id for fresh retry"
                );
                let _ = store
                    .clear_backend_session_id(&output.thread_id, &output.agent_alias)
                    .await;
            }

            // Enqueue retry execution with exponential backoff.
            let agent_config = agent_configs.iter().find(|a| a.alias == output.agent_alias);
            let backoff_secs = agent_config.map(|a| a.retry_backoff_secs).unwrap_or(30);
            let next_attempt = output.attempt_number + 1;
            // Exponential backoff: base * 2^attempt (capped at 1 hour).
            // Cap exponent at 31 to prevent saturating_pow from overflowing u64.
            let exponent = (output.attempt_number as u32).min(31);
            let delay_secs = backoff_secs
                .saturating_mul(2u64.saturating_pow(exponent))
                .min(3600);
            let retry_after = chrono::Utc::now().timestamp() + delay_secs as i64;

            // Look up prompt_hash and resolve original dispatch message ID
            // from the failed execution for continuity.
            let (prompt_hash, orig_dispatch_id) =
                match store.get_execution(&output.execution_id).await {
                    Ok(Some(exec)) => {
                        // For first retry: use dispatch_message_id from the original execution.
                        // For subsequent retries: carry forward original_dispatch_message_id.
                        let orig_id = exec
                            .dispatch_message_id
                            .or(exec.original_dispatch_message_id);
                        (exec.prompt_hash, orig_id)
                    }
                    _ => (None, output.dispatch_message_id),
                };

            match store
                .insert_retry_execution(
                    &output.thread_id,
                    &output.agent_alias,
                    orig_dispatch_id,
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
/// - Error category is retryable (Transient or StaleSession)
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

/// Pre-resolved handoff type — determined BEFORE the reply is inserted so
/// that the fan-out case can use a single atomic transaction.
enum HandoffResolution {
    /// Fan-out to multiple targets: reply + fan-out must be transactional.
    FanOut {
        batch_id: String,
        targets: Vec<String>,
        handoff_body: String,
    },
    /// Fan-out depth limit reached — insert reply + review-request to operator.
    FanOutDepthExceeded { max_depth: i64 },
    /// Single-target handoff or no handoff — existing non-transactional path.
    SingleOrNone,
}

/// Determine the handoff type for a completing agent WITHOUT executing DB writes.
///
/// Returns `FanOut` when the agent has multi-target fan-out configured, so the
/// caller can use an atomic transaction. Returns `SingleOrNone` for everything
/// else (no handoff, single target, operator target, single-element fan-out).
async fn resolve_handoff(
    store: &Store,
    output: &TriggerOutput,
    agent_configs: &[AgentConfig],
    reply_body: &str,
) -> HandoffResolution {
    let agent_config = match agent_configs.iter().find(|a| a.alias == output.agent_alias) {
        Some(c) => c,
        None => return HandoffResolution::SingleOrNone,
    };

    let handoff = match agent_config.handoff.as_ref() {
        Some(h) => h,
        None => return HandoffResolution::SingleOrNone,
    };

    let target = match handoff.on_response.as_ref() {
        Some(t) => t,
        None => return HandoffResolution::SingleOrNone,
    };

    // Only multi-element FanOut gets the transactional path.
    let aliases = match target {
        HandoffTarget::FanOut(aliases) if aliases.len() > 1 => aliases,
        _ => return HandoffResolution::SingleOrNone,
    };

    // Check chain depth before building fan-out (same safety as single-target).
    let max_depth = handoff.max_chain_depth.unwrap_or(3) as i64;
    let current_depth = match store.count_handoff_messages(&output.thread_id).await {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(
                thread_id = %output.thread_id,
                error = %e,
                "failed to count handoff depth for fan-out; allowing fan-out"
            );
            0
        }
    };
    if current_depth >= max_depth {
        tracing::info!(
            thread_id = %output.thread_id,
            current_depth = current_depth,
            max_depth = max_depth,
            "fan-out chain depth exceeded"
        );
        return HandoffResolution::FanOutDepthExceeded { max_depth };
    }

    // Build the handoff body.
    let dispatch_context = match store.get_thread_messages(&output.thread_id).await {
        Ok(msgs) => msgs.first().map(|m| m.body.clone()).unwrap_or_default(),
        Err(_) => String::new(),
    };

    let mut handoff_body = String::new();
    if let Some(ref prompt) = handoff.handoff_prompt {
        handoff_body.push_str(prompt);
        handoff_body.push_str("\n\n");
    }
    handoff_body.push_str(&format!(
        "## Original dispatch\n{}\n\n## Reply from {}\n{}",
        dispatch_context, output.agent_alias, reply_body
    ));

    // Determine batch_id: inherit from originating thread or generate.
    let batch_id = match store.get_thread(&output.thread_id).await {
        Ok(Some(t)) => t
            .batch_id
            .unwrap_or_else(|| format!("fanout-{}", output.thread_id)),
        Ok(None) => format!("fanout-{}", output.thread_id),
        Err(e) => {
            tracing::warn!(
                thread_id = %output.thread_id,
                error = %e,
                "failed to fetch thread for batch_id; generating fallback"
            );
            format!("fanout-{}", output.thread_id)
        }
    };

    HandoffResolution::FanOut {
        batch_id,
        targets: aliases.clone(),
        handoff_body,
    }
}

/// Check handoff config for the completing agent and auto-dispatch to the
/// next agent if a route exists.
///
/// Handoff logic (ORCH-INTENT-2):
/// 1. Look up the agent's `HandoffConfig` from config.
/// 2. Check `on_response` — the single route, regardless of reply intent.
/// 3. If target is "operator" or `None` → do nothing (chain stops).
/// 4. If target is another agent: check chain depth vs max_chain_depth.
///    - Over limit → insert review-request to operator.
///    - Under limit → insert handoff message to target agent.
///
/// NOTE: Multi-element fan-out is handled atomically in `handle_trigger_output`
/// via `insert_reply_and_fanout`. This function only handles single-target
/// handoffs and single-element fan-out (which degrades to single).
async fn maybe_auto_handoff(
    store: &Store,
    event_bus: &EventBus,
    output: &TriggerOutput,
    agent_configs: &[AgentConfig],
    reply_intent: &str,
    reply_body: &str,
) {
    // Find the completing agent's config.
    let agent_config = match agent_configs.iter().find(|a| a.alias == output.agent_alias) {
        Some(c) => c,
        None => {
            tracing::warn!(
                agent_alias = %output.agent_alias,
                thread_id = %output.thread_id,
                "agent alias not found in config during handoff lookup"
            );
            return;
        }
    };

    // No handoff config → no auto-dispatch.
    let handoff = match agent_config.handoff.as_ref() {
        Some(h) => h,
        None => return,
    };

    let target = match handoff.on_response.as_ref() {
        Some(t) => t,
        None => return,
    };

    match target {
        HandoffTarget::Single(alias) if alias == "operator" => {}

        HandoffTarget::Single(alias) => {
            handle_single_handoff(
                store,
                event_bus,
                output,
                handoff,
                alias,
                reply_intent,
                reply_body,
            )
            .await;
        }
        HandoffTarget::FanOut(aliases) if aliases.len() == 1 => {
            let alias = &aliases[0];
            if alias == "operator" {
                return;
            }
            handle_single_handoff(
                store,
                event_bus,
                output,
                handoff,
                alias,
                reply_intent,
                reply_body,
            )
            .await;
        }
        HandoffTarget::FanOut(_aliases) => {
            // Multi-element fan-out is handled atomically in handle_trigger_output
            // via insert_reply_and_fanout. This branch is a no-op because the
            // caller already resolved the fan-out and used the transactional path.
        }
    }
}

/// Handle a single-target auto-handoff with chain depth checking.
async fn handle_single_handoff(
    store: &Store,
    event_bus: &EventBus,
    output: &TriggerOutput,
    handoff: &crate::config::types::HandoffConfig,
    target_alias: &str,
    reply_intent: &str,
    reply_body: &str,
) {
    let dispatch_context = match store.get_thread_messages(&output.thread_id).await {
        Ok(msgs) => msgs.first().map(|m| m.body.clone()).unwrap_or_default(),
        Err(_) => String::new(),
    };

    let mut handoff_body = String::new();
    if let Some(ref prompt) = handoff.handoff_prompt {
        handoff_body.push_str(prompt);
        handoff_body.push_str("\n\n");
    }
    handoff_body.push_str(&format!(
        "## Original dispatch\n{}\n\n## Reply from {}\n{}",
        dispatch_context, output.agent_alias, reply_body
    ));

    let max_depth = handoff.max_chain_depth.unwrap_or(3) as i64;
    match store
        .insert_handoff_if_under_depth(
            &output.thread_id,
            &output.agent_alias,
            target_alias,
            &handoff_body,
            max_depth,
        )
        .await
    {
        Ok(Some(message_id)) => {
            tracing::info!(
                thread_id = %output.thread_id,
                from = %output.agent_alias,
                to = %target_alias,
                max_depth = max_depth,
                "auto-handoff dispatched"
            );
            event_bus.emit(OrchestratorEvent::MessageReceived {
                thread_id: output.thread_id.clone(),
                message_id,
                from_alias: output.agent_alias.clone(),
                intent: "handoff".to_string(),
            });
        }
        Ok(None) => {
            // Chain depth exceeded — collect involved agents and notify operator.
            let agents_involved = match store.get_thread_messages(&output.thread_id).await {
                Ok(msgs) => {
                    let mut agents: Vec<String> = Vec::new();
                    for msg in &msgs {
                        if !agents.contains(&msg.from_alias) {
                            agents.push(msg.from_alias.clone());
                        }
                    }
                    agents.join(", ")
                }
                Err(_) => "(unknown)".to_string(),
            };

            let interrupt_body = format!(
                "Auto-handoff chain interrupted at depth limit {}. Agents involved: {}. \
                 Last agent ({}) replied with intent '{}'. \
                 Please review and decide next step.",
                max_depth, agents_involved, output.agent_alias, reply_intent
            );

            match store
                .insert_message(
                    &output.thread_id,
                    &output.agent_alias,
                    "operator",
                    "review-request",
                    &interrupt_body,
                    None,
                    None,
                )
                .await
            {
                Ok(message_id) => {
                    tracing::info!(
                        thread_id = %output.thread_id,
                        max_depth = max_depth,
                        "auto-handoff chain interrupted at depth limit"
                    );
                    event_bus.emit(OrchestratorEvent::MessageReceived {
                        thread_id: output.thread_id.clone(),
                        message_id,
                        from_alias: output.agent_alias.clone(),
                        intent: "review-request".to_string(),
                    });
                }
                Err(e) => {
                    tracing::error!(
                        thread_id = %output.thread_id,
                        error = %e,
                        "failed to insert chain-interrupt message"
                    );
                }
            }
        }
        Err(e) => {
            tracing::error!(
                thread_id = %output.thread_id,
                target = %target_alias,
                error = %e,
                "failed to insert handoff message"
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
        assert_eq!(events[0].tool_name.as_deref(), Some("Write"));
        assert_eq!(events[1].event_type, "tool_call");
        assert!(events[1].summary.starts_with("Bash:"));
        assert_eq!(events[1].event_index, 1);
        assert_eq!(events[1].tool_name.as_deref(), Some("Bash"));
        assert_eq!(events[2].event_type, "turn_complete");
        assert_eq!(events[2].event_index, 2);
        assert!(events[2].tool_name.is_none());

        // Verify EventBus received ExecutionProgress events.
        // Only tool_call events are emitted — tool_result and turn_complete are suppressed.
        let mut progress_count = 0;
        while let Ok(ev) = rx_events.try_recv() {
            if let OrchestratorEvent::ExecutionProgress { execution_id, .. } = ev {
                assert_eq!(execution_id, "exec-1");
                progress_count += 1;
            }
        }
        assert_eq!(
            progress_count, 2,
            "only tool_call events should be emitted to EventBus"
        );
    }

    #[tokio::test]
    async fn test_consume_telemetry_suppresses_noisy_events_from_bus() {
        let store = test_store().await;
        let event_bus = EventBus::new();
        let mut rx_events = event_bus.subscribe();

        let (tx, rx) = std::sync::mpsc::sync_channel::<String>(128);

        // tool_call → emitted to bus
        tx.send(r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":"src/lib.rs"}}]}}"#.to_string()).unwrap();
        // tool_result → stored in DB but NOT emitted to bus
        tx.send(r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"tu_99","content":"ok"}]}}"#.to_string()).unwrap();
        // turn_complete → stored in DB but NOT emitted to bus
        tx.send(
            r#"{"type":"result","subtype":"success","result":"Done.","session_id":"s2"}"#
                .to_string(),
        )
        .unwrap();

        drop(tx);

        consume_telemetry(
            rx,
            &store,
            &event_bus,
            "exec-sup",
            "thread-sup",
            "agent-sup",
            "claude",
        )
        .await;

        // All 3 events stored in DB.
        let events = store
            .get_execution_events("exec-sup", None, None, None)
            .await
            .unwrap();
        assert_eq!(events.len(), 3, "all events should be stored in DB");
        assert_eq!(events[0].event_type, "tool_call");
        assert_eq!(events[1].event_type, "tool_result");
        assert_eq!(events[2].event_type, "turn_complete");

        // Only tool_call emitted to EventBus — tool_result and turn_complete suppressed.
        let mut progress_count = 0;
        while let Ok(ev) = rx_events.try_recv() {
            if let OrchestratorEvent::ExecutionProgress { execution_id, .. } = ev {
                assert_eq!(execution_id, "exec-sup");
                progress_count += 1;
            }
        }
        assert_eq!(
            progress_count, 1,
            "only tool_call should be emitted; tool_result and turn_complete suppressed"
        );
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

        // "unknown-backend" has no parser — should return immediately without storing anything.
        consume_telemetry(
            rx,
            &store,
            &event_bus,
            "exec-2",
            "t-2",
            "agent-b",
            "unknown-backend",
        )
        .await;

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
            handoff: None,
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

    #[test]
    fn test_should_retry_stale_session_with_retries() {
        let configs = vec![test_agent_config("worker-a", 3)];
        let output = failed_trigger_output(
            "worker-a",
            Some(crate::backend::ErrorCategory::StaleSession),
            0,
            false,
        );
        assert!(should_retry_execution(&output, &configs));
    }

    #[test]
    fn test_should_not_retry_stale_session_no_retries() {
        let configs = vec![test_agent_config("worker-a", 0)];
        let output = failed_trigger_output(
            "worker-a",
            Some(crate::backend::ErrorCategory::StaleSession),
            0,
            false,
        );
        assert!(!should_retry_execution(&output, &configs));
    }

    #[tokio::test]
    async fn test_handle_trigger_output_retries_transient_error() {
        let store = test_store().await;
        let event_bus = EventBus::new();
        let mut rx = event_bus.subscribe();

        let configs = vec![test_agent_config("worker-a", 3)];

        // Setup: create thread, dispatch message, and failed execution
        store.ensure_thread("t-retry", None, None).await.unwrap();
        let msg_id = store
            .insert_message(
                "t-retry", "operator", "worker-a", "dispatch", "task", None, None,
            )
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

        handle_trigger_output(
            &store,
            &event_bus,
            &output,
            &configs,
            &Arc::new(Mutex::new(CircuitBreakerRegistry::new())),
            false,
            3,
            "claude",
        )
        .await;

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

        store.ensure_thread("t-term", None, None).await.unwrap();
        let msg_id = store
            .insert_message(
                "t-term", "operator", "worker-a", "dispatch", "task", None, None,
            )
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

        handle_trigger_output(
            &store,
            &event_bus,
            &output,
            &configs,
            &Arc::new(Mutex::new(CircuitBreakerRegistry::new())),
            false,
            3,
            "claude",
        )
        .await;

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

        store.ensure_thread("t-max", None, None).await.unwrap();
        let msg_id = store
            .insert_message(
                "t-max", "operator", "worker-a", "dispatch", "task", None, None,
            )
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

        handle_trigger_output(
            &store,
            &event_bus,
            &output,
            &configs,
            &Arc::new(Mutex::new(CircuitBreakerRegistry::new())),
            false,
            3,
            "claude",
        )
        .await;

        // Thread should be Failed — max retries exceeded
        let status = store.get_thread_status("t-max").await.unwrap();
        assert_eq!(status.as_deref(), Some("Failed"));
    }

    #[tokio::test]
    async fn test_handle_trigger_output_success_no_intent_defaults_to_response() {
        let store = test_store().await;
        let event_bus = EventBus::new();
        let configs = vec![test_agent_config("worker-a", 0)];

        store
            .ensure_thread("t-default-intent", None, None)
            .await
            .unwrap();
        let msg_id = store
            .insert_message(
                "t-default-intent",
                "operator",
                "worker-a",
                "dispatch",
                "task",
                None,
                None,
            )
            .await
            .unwrap();
        let exec_id = store
            .insert_execution_with_dispatch("t-default-intent", "worker-a", Some(msg_id), None)
            .await
            .unwrap()
            .unwrap();

        // parsed_intent is None — should fall back to "response"
        let output = TriggerOutput {
            execution_id: exec_id,
            thread_id: "t-default-intent".to_string(),
            agent_alias: "worker-a".to_string(),
            success: true,
            output: Some("all done".to_string()),
            exit_code: Some(0),
            duration_ms: 100,
            parsed_intent: None,
            error_category: None,
            attempt_number: 0,
            dispatch_message_id: Some(msg_id),
            timed_out: false,
        };

        handle_trigger_output(
            &store,
            &event_bus,
            &output,
            &configs,
            &Arc::new(Mutex::new(CircuitBreakerRegistry::new())),
            false,
            3,
            "claude",
        )
        .await;

        // The reply message should have intent "response", not "status-update"
        let messages = store.get_thread_messages("t-default-intent").await.unwrap();
        let reply = messages
            .iter()
            .find(|m| m.from_alias == "worker-a")
            .expect("should have a reply from the agent");
        assert_eq!(reply.intent, "response");
    }

    // ── Schedule evaluation tests (CRON-2) ──────────────────────────────

    use crate::config::types::ScheduleConfig;

    fn test_worker_runner(store: Store, schedules: Option<Vec<ScheduleConfig>>) -> WorkerRunner {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        let _keep = dir.keep();
        let config = crate::config::types::OrchestratorConfig {
            default_workdir: path,
            state_dir: std::path::PathBuf::from("/tmp/compas-test-sched"),
            poll_interval_secs: 5,
            models: None,
            agents: vec![crate::config::types::AgentConfig {
                alias: "coder".into(),
                backend: "stub".into(),
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
            orchestration: Default::default(),
            database: Default::default(),
            notifications: Default::default(),
            backend_definitions: None,
            hooks: None,
            schedules,
        };
        let config_handle = ConfigHandle::new(config);
        let backend_registry = BackendRegistry::new();
        let event_bus = EventBus::new();
        let worktree_manager = WorktreeManager::new();
        WorkerRunner::new(
            config_handle,
            store,
            backend_registry,
            event_bus,
            worktree_manager,
        )
    }

    #[tokio::test]
    async fn test_evaluate_schedules_fires_when_due() {
        let store = test_store().await;
        let runner = test_worker_runner(
            store.clone(),
            Some(vec![ScheduleConfig {
                name: "every-minute".into(),
                agent: "coder".into(),
                cron: "* * * * *".into(),
                body: "Run CI checks".into(),
                batch: Some("BATCH-1".into()),
                max_runs: 100,
                enabled: true,
            }]),
        );
        let mut cache = std::collections::HashMap::new();

        runner.evaluate_schedules(&mut cache).await;

        // Should have inserted a dispatch message.
        let count = store.message_count().await.unwrap();
        assert_eq!(count, 1, "expected 1 dispatch message");

        // Cache should be updated.
        assert!(cache.contains_key("every-minute"));
        let (_, run_count) = cache["every-minute"];
        assert_eq!(run_count, 1);

        // DB schedule_runs should be updated.
        let run = store
            .get_schedule_run("every-minute")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(run.1, 1); // run_count = 1
    }

    #[tokio::test]
    async fn test_evaluate_schedules_skips_disabled() {
        let store = test_store().await;
        let runner = test_worker_runner(
            store.clone(),
            Some(vec![ScheduleConfig {
                name: "disabled-sched".into(),
                agent: "coder".into(),
                cron: "* * * * *".into(),
                body: "Should not fire".into(),
                batch: None,
                max_runs: 100,
                enabled: false,
            }]),
        );
        let mut cache = std::collections::HashMap::new();

        runner.evaluate_schedules(&mut cache).await;

        let count = store.message_count().await.unwrap();
        assert_eq!(count, 0, "disabled schedule should not fire");
        assert!(cache.is_empty());
    }

    #[tokio::test]
    async fn test_evaluate_schedules_respects_max_runs() {
        let store = test_store().await;
        let runner = test_worker_runner(
            store.clone(),
            Some(vec![ScheduleConfig {
                name: "capped-sched".into(),
                agent: "coder".into(),
                cron: "* * * * *".into(),
                body: "Capped task".into(),
                batch: None,
                max_runs: 2,
                enabled: true,
            }]),
        );

        // Pre-populate cache as if 2 runs already happened (at cap).
        let mut cache = std::collections::HashMap::new();
        cache.insert("capped-sched".to_string(), (1700000000_i64, 2_u64));

        runner.evaluate_schedules(&mut cache).await;

        let count = store.message_count().await.unwrap();
        assert_eq!(count, 0, "schedule at max_runs should not fire");
        // Cache should remain unchanged.
        assert_eq!(cache["capped-sched"].1, 2);
    }

    // ── Circuit breaker integration tests (GAP-1) ────────────────────────

    #[tokio::test]
    async fn test_circuit_breaker_opens_after_threshold_failures() {
        let store = test_store().await;
        let event_bus = EventBus::new();
        let configs = vec![test_agent_config("worker-a", 0)]; // no retries
        let cb = Arc::new(Mutex::new(CircuitBreakerRegistry::new()));

        // Three consecutive non-retryable failures should open the circuit.
        for i in 0..3u32 {
            let thread_id = format!("t-cb-{}", i);
            store.ensure_thread(&thread_id, None, None).await.unwrap();
            let msg_id = store
                .insert_message(
                    &thread_id, "operator", "worker-a", "dispatch", "task", None, None,
                )
                .await
                .unwrap();
            let exec_id = store
                .insert_execution_with_dispatch(&thread_id, "worker-a", Some(msg_id), None)
                .await
                .unwrap()
                .unwrap();

            let output = TriggerOutput {
                execution_id: exec_id,
                thread_id: thread_id.clone(),
                agent_alias: "worker-a".to_string(),
                success: false,
                output: Some("backend error".to_string()),
                exit_code: Some(1),
                duration_ms: 100,
                parsed_intent: None,
                error_category: Some(crate::backend::ErrorCategory::AgentError),
                attempt_number: 0,
                dispatch_message_id: Some(msg_id),
                timed_out: false,
            };

            handle_trigger_output(
                &store, &event_bus, &output, &configs, &cb, true, // cb_enabled
                3, "claude",
            )
            .await;
        }

        // Circuit should now be Open.
        let state = cb.lock().unwrap().state_of("claude");
        assert_eq!(
            state,
            CircuitState::Open,
            "circuit should be open after 3 failures"
        );

        // Verify state was persisted to store.
        let stored = store.get_circuit_breaker_states().await.unwrap();
        let claude_entry = stored.iter().find(|(b, _, _)| b == "claude");
        assert!(
            claude_entry.is_some(),
            "circuit breaker state should be persisted"
        );
        assert_eq!(claude_entry.unwrap().1, "open");
    }

    #[tokio::test]
    async fn test_circuit_breaker_success_resets_to_closed() {
        let store = test_store().await;
        let event_bus = EventBus::new();
        let configs = vec![test_agent_config("worker-a", 0)];
        let cb = Arc::new(Mutex::new(CircuitBreakerRegistry::new()));

        // Open the circuit by recording failures directly.
        {
            let mut guard = cb.lock().unwrap();
            for _ in 0..3 {
                guard.record_failure("claude", 3);
            }
        }
        assert_eq!(cb.lock().unwrap().state_of("claude"), CircuitState::Open);

        // A successful execution should reset to Closed.
        store.ensure_thread("t-cb-reset", None, None).await.unwrap();
        let msg_id = store
            .insert_message(
                "t-cb-reset",
                "operator",
                "worker-a",
                "dispatch",
                "task",
                None,
                None,
            )
            .await
            .unwrap();
        let exec_id = store
            .insert_execution_with_dispatch("t-cb-reset", "worker-a", Some(msg_id), None)
            .await
            .unwrap()
            .unwrap();

        let output = TriggerOutput {
            execution_id: exec_id,
            thread_id: "t-cb-reset".to_string(),
            agent_alias: "worker-a".to_string(),
            success: true,
            output: Some("all good".to_string()),
            exit_code: Some(0),
            duration_ms: 100,
            parsed_intent: None,
            error_category: None,
            attempt_number: 0,
            dispatch_message_id: Some(msg_id),
            timed_out: false,
        };

        handle_trigger_output(
            &store, &event_bus, &output, &configs, &cb, true, 3, "claude",
        )
        .await;

        assert_eq!(
            cb.lock().unwrap().state_of("claude"),
            CircuitState::Closed,
            "circuit should reset to closed after success"
        );
    }

    #[tokio::test]
    async fn test_circuit_breaker_stale_session_not_counted_when_retrying() {
        let store = test_store().await;
        let event_bus = EventBus::new();
        let configs = vec![test_agent_config("worker-a", 3)]; // retries enabled
        let cb = Arc::new(Mutex::new(CircuitBreakerRegistry::new()));

        store.ensure_thread("t-cb-stale", None, None).await.unwrap();
        let msg_id = store
            .insert_message(
                "t-cb-stale",
                "operator",
                "worker-a",
                "dispatch",
                "task",
                None,
                None,
            )
            .await
            .unwrap();
        let exec_id = store
            .insert_execution_with_dispatch("t-cb-stale", "worker-a", Some(msg_id), None)
            .await
            .unwrap()
            .unwrap();

        // StaleSession error with retries available → should NOT count as CB failure.
        let output = TriggerOutput {
            execution_id: exec_id,
            thread_id: "t-cb-stale".to_string(),
            agent_alias: "worker-a".to_string(),
            success: false,
            output: Some("session not found".to_string()),
            exit_code: Some(1),
            duration_ms: 100,
            parsed_intent: None,
            error_category: Some(crate::backend::ErrorCategory::StaleSession),
            attempt_number: 0,
            dispatch_message_id: Some(msg_id),
            timed_out: false,
        };

        handle_trigger_output(
            &store, &event_bus, &output, &configs, &cb, true, 3, "claude",
        )
        .await;

        // Circuit should still be Closed — StaleSession retry is exempt.
        let states = cb.lock().unwrap().states();
        let claude = states.iter().find(|(n, _, _)| n == "claude");
        if let Some((_, state, failures)) = claude {
            assert_eq!(*state, CircuitState::Closed);
            assert_eq!(
                *failures, 0,
                "StaleSession retry should not increment failure count"
            );
        }
    }

    #[tokio::test]
    async fn test_circuit_breaker_stale_session_counted_when_no_retries() {
        let store = test_store().await;
        let event_bus = EventBus::new();
        let configs = vec![test_agent_config("worker-a", 0)]; // no retries
        let cb = Arc::new(Mutex::new(CircuitBreakerRegistry::new()));

        store
            .ensure_thread("t-cb-stale-nr", None, None)
            .await
            .unwrap();
        let msg_id = store
            .insert_message(
                "t-cb-stale-nr",
                "operator",
                "worker-a",
                "dispatch",
                "task",
                None,
                None,
            )
            .await
            .unwrap();
        let exec_id = store
            .insert_execution_with_dispatch("t-cb-stale-nr", "worker-a", Some(msg_id), None)
            .await
            .unwrap()
            .unwrap();

        // StaleSession error with max_retries=0 → SHOULD count as CB failure.
        let output = TriggerOutput {
            execution_id: exec_id,
            thread_id: "t-cb-stale-nr".to_string(),
            agent_alias: "worker-a".to_string(),
            success: false,
            output: Some("session not found".to_string()),
            exit_code: Some(1),
            duration_ms: 100,
            parsed_intent: None,
            error_category: Some(crate::backend::ErrorCategory::StaleSession),
            attempt_number: 0,
            dispatch_message_id: Some(msg_id),
            timed_out: false,
        };

        handle_trigger_output(
            &store, &event_bus, &output, &configs, &cb, true, 3, "claude",
        )
        .await;

        // Circuit should have 1 failure recorded (StaleSession not retried).
        let states = cb.lock().unwrap().states();
        let claude = states.iter().find(|(n, _, _)| n == "claude");
        assert!(claude.is_some(), "failure should be recorded");
        assert_eq!(claude.unwrap().2, 1, "should have 1 failure counted");
    }
}
