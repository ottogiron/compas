//! WorkerRunner — poll-loop based trigger worker.
//!
//! On startup:
//! 1. Marks orphaned executions (picked_up/executing) as crashed
//! 2. Writes initial heartbeat
//!
//! Main loop:
//! 1. Polls `executions` table for queued work via `claim_next_execution`
//! 2. For each claimed execution, spawns a task to run the backend trigger
//! 3. Writes periodic heartbeats
//! 4. Inserts reply messages from completed triggers

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Semaphore;

use crate::backend::registry::BackendRegistry;
use crate::config::types::OrchestratorConfig;
use crate::store::Store;

use super::executor::{execute_trigger, TriggerOutput};

/// Worker runner configuration.
pub struct WorkerRunner {
    config: Arc<OrchestratorConfig>,
    store: Store,
    backend_registry: Arc<BackendRegistry>,
    worker_id: String,
    poll_interval: Duration,
    max_per_agent: usize,
}

impl WorkerRunner {
    pub fn new(
        config: OrchestratorConfig,
        store: Store,
        backend_registry: BackendRegistry,
    ) -> Self {
        let poll_interval = Duration::from_secs(config.poll_interval_secs.max(1));
        let max_per_agent = config.orchestration.max_triggers_per_agent;
        let worker_id = format!("worker-{}", std::process::id());

        Self {
            config: Arc::new(config),
            store,
            backend_registry: Arc::new(backend_registry),
            worker_id,
            poll_interval,
            max_per_agent,
        }
    }

    /// Run the worker loop. This never returns unless cancelled.
    pub async fn run(&self) -> Result<(), Box<dyn std::error::Error>> {
        tracing::info!(
            worker_id = %self.worker_id,
            poll_interval_ms = self.poll_interval.as_millis() as u64,
            max_per_agent = self.max_per_agent,
            "worker starting"
        );

        // Crash recovery: mark orphaned executions
        let crashed = self.store.mark_orphaned_executions_crashed().await?;
        if crashed > 0 {
            tracing::warn!(count = crashed, "marked orphaned executions as crashed");
        }

        // Create log directory and prune old log files on startup.
        let log_dir = self.config.log_dir();
        if let Err(e) = std::fs::create_dir_all(&log_dir) {
            tracing::warn!(path = %log_dir.display(), error = %e, "failed to create log dir");
        }
        prune_log_files(&log_dir, self.config.orchestration.log_retention_count);

        // Initial heartbeat
        self.store
            .write_heartbeat(&self.worker_id, env!("CARGO_PKG_VERSION"))
            .await?;

        // Concurrency semaphore (global limit)
        let max_concurrent = self.config.effective_max_concurrent_triggers();
        let semaphore = Arc::new(Semaphore::new(max_concurrent));

        let mut heartbeat_interval = tokio::time::interval(Duration::from_secs(10));
        let mut poll_interval = tokio::time::interval(self.poll_interval);

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
            }
        }
    }

    async fn poll_once(&self, semaphore: &Arc<Semaphore>) {
        // Try to claim work. We loop to drain all available queued items.
        loop {
            // Check if we have global capacity
            if semaphore.available_permits() == 0 {
                return;
            }

            match self.store.claim_next_execution(self.max_per_agent).await {
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
                    let agent_configs = self.config.agents.clone();
                    let execution_timeout_secs = self.config.orchestration.execution_timeout_secs;
                    let log_dir = Some(self.config.log_dir());
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
        .filter(|e| e.path().extension().map_or(false, |ext| ext == "log"))
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
