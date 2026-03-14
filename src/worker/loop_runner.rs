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
use crate::config::types::AgentRole;
use crate::config::ConfigHandle;
use crate::events::{EventBus, OrchestratorEvent};
use crate::store::Store;

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
}

impl WorkerRunner {
    pub fn new(
        config: ConfigHandle,
        store: Store,
        backend_registry: BackendRegistry,
        event_bus: EventBus,
    ) -> Self {
        let worker_id = format!("worker-{}", std::process::id());

        Self {
            config,
            store,
            backend_registry: Arc::new(backend_registry),
            worker_id,
            event_bus,
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

                    tokio::spawn(async move {
                        let _permit = permit; // held until task completes

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
                        )
                        .await;

                        // Post-execution: insert reply message and emit events.
                        handle_trigger_output(&store, &event_bus, &output).await;
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
            match self
                .store
                .insert_execution_with_dispatch(&thread_id, &agent_alias, Some(message_id))
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

/// Insert a reply message from the agent after trigger completion, and emit
/// events for `ExecutionCompleted`, `ThreadStatusChanged` (on failure), and
/// `MessageReceived`.
async fn handle_trigger_output(store: &Store, event_bus: &EventBus, output: &TriggerOutput) {
    let reply_intent = if output.success {
        output.parsed_intent.as_deref().unwrap_or("status-update")
    } else {
        "error"
    };

    let reply_body = output.output.as_deref().unwrap_or(if output.success {
        "(completed with no output)"
    } else {
        "(failed with no output)"
    });

    // Emit ExecutionCompleted before inserting the reply message.
    event_bus.emit(OrchestratorEvent::ExecutionCompleted {
        execution_id: output.execution_id.clone(),
        thread_id: output.thread_id.clone(),
        agent_alias: output.agent_alias.clone(),
        success: output.success,
        duration_ms: output.duration_ms,
    });

    // Emit ThreadStatusChanged for both success and failure paths so all
    // consumers get push notification regardless of outcome.
    let thread_status = if output.success {
        "Active" // thread stays Active after a successful execution; operator closes it
    } else {
        "Failed"
    };
    event_bus.emit(OrchestratorEvent::ThreadStatusChanged {
        thread_id: output.thread_id.clone(),
        new_status: thread_status.to_string(),
    });

    // Insert the reply message and emit MessageReceived on success.
    match store
        .insert_message(
            &output.thread_id,
            &output.agent_alias,
            "operator",
            reply_intent,
            reply_body,
            None,
        )
        .await
    {
        Ok(message_id) => {
            event_bus.emit(OrchestratorEvent::MessageReceived {
                thread_id: output.thread_id.clone(),
                message_id,
                from_alias: output.agent_alias.clone(),
                intent: reply_intent.to_string(),
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

    tracing::info!(
        exec_id = %output.execution_id,
        thread_id = %output.thread_id,
        agent = %output.agent_alias,
        success = output.success,
        duration_ms = output.duration_ms,
        intent = ?output.parsed_intent,
        "trigger completed"
    );
}
