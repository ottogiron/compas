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

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Semaphore;

use crate::backend::registry::BackendRegistry;
use crate::config::types::AgentRole;
use crate::config::ConfigHandle;
use crate::store::Store;

use super::executor::{execute_trigger, TriggerOutput};

/// Worker runner configuration.
pub struct WorkerRunner {
    config: ConfigHandle,
    store: Store,
    backend_registry: Arc<BackendRegistry>,
    worker_id: String,
}

impl WorkerRunner {
    pub fn new(config: ConfigHandle, store: Store, backend_registry: BackendRegistry) -> Self {
        let worker_id = format!("worker-{}", std::process::id());

        Self {
            config,
            store,
            backend_registry: Arc::new(backend_registry),
            worker_id,
        }
    }

    /// Run the worker loop. This never returns unless cancelled.
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
            }
        }
    }

    async fn poll_once(&self, semaphore: &Arc<Semaphore>) {
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

                        // Post-execution: insert reply message if we got output
                        handle_trigger_output(&store, &output).await;
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

/// Insert a reply message from the agent after trigger completion.
async fn handle_trigger_output(store: &Store, output: &TriggerOutput) {
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

    if let Err(e) = store
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
        tracing::error!(
            thread_id = %output.thread_id,
            error = %e,
            "failed to insert reply message"
        );
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
