//! Executor — runs a backend trigger inside `spawn_blocking`.
//!
//! Wraps the blocking CLI subprocess execution so it doesn't starve the
//! tokio runtime. Captures output, exit code, and duration.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use crate::backend::registry::BackendRegistry;
use crate::backend::{BackendOutput, ErrorCategory};
use crate::config::types::AgentConfig;
use crate::model::agent::Agent;
use crate::store::{ExecutionRow, ExecutionStatus, Store};
use crate::worktree::WorktreeManager;

/// Result of running a trigger execution.
#[derive(Debug)]
pub struct TriggerOutput {
    pub execution_id: String,
    pub thread_id: String,
    pub agent_alias: String,
    pub success: bool,
    pub output: Option<String>,
    pub exit_code: Option<i32>,
    pub duration_ms: i64,
    pub parsed_intent: Option<String>,
    /// Classified error category for failure cases.
    pub error_category: Option<ErrorCategory>,
    /// Current attempt number (0-based).
    pub attempt_number: i32,
    /// The dispatch message ID that originated this execution chain.
    pub dispatch_message_id: Option<i64>,
    /// Whether the execution timed out.
    pub timed_out: bool,
}

/// Execute a trigger for a claimed execution row.
///
/// 1. Marks execution as `executing`
/// 2. Runs the backend trigger via `spawn_blocking`
/// 3. Updates execution with result (completed/failed)
/// 4. Returns the trigger output for downstream processing
///
/// `log_dir`: when `Some`, stdout/stderr are streamed to
/// `{log_dir}/{exec_id}.log` during execution.
///
/// `stdout_tx`: when `Some`, each stdout line is forwarded to a telemetry
/// consumer via the session. Wrapped in `Arc` so it can be shared with the
/// reader thread inside `wait_with_timeout`.
#[allow(clippy::too_many_arguments)]
pub async fn execute_trigger(
    execution: &ExecutionRow,
    store: &Store,
    backend_registry: &Arc<BackendRegistry>,
    agent_configs: &[AgentConfig],
    instruction: &str,
    execution_timeout_secs: u64,
    log_dir: Option<PathBuf>,
    stdout_tx: Option<std::sync::Arc<std::sync::mpsc::SyncSender<String>>>,
    worktree_manager: &Arc<WorktreeManager>,
    default_workdir: &std::path::Path,
    worktree_override_dir: Option<PathBuf>,
) -> TriggerOutput {
    let exec_id = execution.id.clone();
    let thread_id = execution.thread_id.clone();
    let agent_alias = execution.agent_alias.clone();

    let dispatch_message_id = execution.dispatch_message_id;
    let attempt_number = execution.attempt_number;

    // Mark as executing
    if let Err(e) = store.mark_execution_executing(&exec_id).await {
        tracing::error!(exec_id = %exec_id, error = %e, "failed to mark execution as executing");
        return TriggerOutput {
            execution_id: exec_id,
            thread_id,
            agent_alias,
            success: false,
            output: None,
            exit_code: None,
            duration_ms: 0,
            parsed_intent: None,
            error_category: None,
            attempt_number,
            dispatch_message_id,
            timed_out: false,
        };
    }

    // Find agent config
    let agent_config = match agent_configs.iter().find(|a| a.alias == agent_alias) {
        Some(c) => c,
        None => {
            let err = format!("no agent config for alias '{}'", agent_alias);
            tracing::error!(%err);
            if let Ok(0) = store
                .fail_execution(&exec_id, &err, None, 0, ExecutionStatus::Failed)
                .await
            {
                tracing::warn!(exec_id = %exec_id, "fail_execution was a no-op — already terminal");
            }
            return TriggerOutput {
                execution_id: exec_id,
                thread_id,
                agent_alias,
                success: false,
                output: Some(err),
                exit_code: None,
                duration_ms: 0,
                parsed_intent: None,
                error_category: Some(ErrorCategory::Unknown),
                attempt_number,
                dispatch_message_id,
                timed_out: false,
            };
        }
    };

    // Resolve backend
    let backend = match backend_registry.get(agent_config) {
        Ok(b) => b,
        Err(e) => {
            let err = format!("backend lookup failed: {}", e);
            tracing::error!(%err, agent = %agent_alias);
            if let Ok(0) = store
                .fail_execution(&exec_id, &err, None, 0, ExecutionStatus::Failed)
                .await
            {
                tracing::warn!(exec_id = %exec_id, "fail_execution was a no-op — already terminal");
            }
            return TriggerOutput {
                execution_id: exec_id,
                thread_id,
                agent_alias,
                success: false,
                output: Some(err),
                exit_code: None,
                duration_ms: 0,
                parsed_intent: None,
                error_category: Some(ErrorCategory::Unknown),
                attempt_number,
                dispatch_message_id,
                timed_out: false,
            };
        }
    };

    // Build Agent from AgentConfig.
    // Set log_path so backends can stream output to the per-execution log file.
    let log_path = log_dir
        .as_ref()
        .map(|dir| dir.join(format!("{}.log", exec_id)));
    // Resolve effective working directory for this execution.
    // Worktree creation runs blocking git subprocesses, so we use
    // spawn_blocking to avoid starving the tokio runtime.
    let agent_workdir = agent_config
        .workdir
        .clone()
        .unwrap_or_else(|| default_workdir.to_path_buf());
    // Track the actual worktree path (not fallback) for post-execution status check.
    let mut worktree_path_for_status: Option<PathBuf> = None;

    let execution_workdir = if agent_config.workspace.as_deref() == Some("worktree") {
        let wt_thread_id = execution.thread_id.clone();
        let wt_agent_workdir = agent_workdir.clone();
        let wt_override = worktree_override_dir.clone();
        let wt_result = {
            let wt_mgr = worktree_manager.clone();
            tokio::task::spawn_blocking(move || {
                wt_mgr.ensure_worktree(&wt_agent_workdir, &wt_thread_id, wt_override.as_deref())
            })
            .await
            .unwrap_or_else(|e| Err(format!("spawn_blocking panicked: {}", e)))
        };
        match wt_result {
            Ok(Some(path)) => {
                worktree_path_for_status = Some(path.clone());
                if let Err(e) = store
                    .set_thread_worktree_path(&execution.thread_id, &path, &agent_workdir)
                    .await
                {
                    tracing::warn!(error = %e, "failed to store worktree path");
                }
                Some(path)
            }
            Ok(None) => {
                tracing::warn!(
                    thread_id = %execution.thread_id,
                    "worktree mode requested but repo is not git — falling back to shared"
                );
                Some(agent_workdir)
            }
            Err(e) => {
                tracing::error!(error = %e, "failed to create worktree — falling back to shared");
                Some(agent_workdir)
            }
        }
    } else {
        agent_config.workdir.clone()
    };

    let agent = Agent {
        alias: agent_config.alias.clone(),
        backend: agent_config.backend.clone(),
        model: agent_config.model.clone(),
        prompt: agent_config.prompt.clone(),
        prompt_file: agent_config.prompt_file.clone(),
        timeout_secs: agent_config.timeout_secs.or(Some(execution_timeout_secs)),
        backend_args: agent_config.backend_args.clone(),
        env: agent_config.env.clone(),
        log_path,
        execution_workdir,
    };

    // Look up the last backend session ID for this thread+agent so the backend
    // can resume the prior CLI session and preserve conversational context.
    let resume_session_id = match store
        .get_last_backend_session_id(&thread_id, &agent_alias)
        .await
    {
        Ok(sid) => sid,
        Err(e) => {
            tracing::warn!(
                exec_id = %exec_id,
                error = %e,
                "failed to query last backend session ID — starting fresh session"
            );
            None
        }
    };

    // Start a session then trigger — all inside spawn_blocking.
    //
    // PID early-persistence: we create a sync_channel so the backend can report
    // the PID right after spawn_cli(). A separate tokio task receives it and
    // writes to the DB while the process is still running. This is critical for
    // orphan detection — if the worker crashes mid-execution, the PID is already
    // persisted and the next startup can kill the orphaned process.
    let instruction = instruction.to_string();
    let start = Instant::now();

    let (pid_sender, pid_receiver) = std::sync::mpsc::sync_channel::<u32>(1);

    // Spawn a task to persist PID as soon as the backend reports it.
    let pid_store = store.clone();
    let pid_exec_id = exec_id.clone();
    let pid_task = tokio::spawn(async move {
        // Block on the sync receiver in a spawn_blocking to avoid starving tokio.
        let pid = tokio::task::spawn_blocking(move || pid_receiver.recv().ok())
            .await
            .ok()
            .flatten();
        if let Some(pid) = pid {
            if let Err(e) = pid_store.set_execution_pid(&pid_exec_id, pid).await {
                tracing::warn!(
                    exec_id = %pid_exec_id,
                    pid = pid,
                    error = %e,
                    "failed to persist backend PID early"
                );
            }
        }
    });

    let trigger_result: Result<BackendOutput, String> = tokio::task::spawn_blocking(move || {
        // We need a runtime handle to call async methods from blocking context.
        // Use Handle::current() which was captured before spawn_blocking.
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async {
            let mut session = backend
                .start_session(&agent)
                .await
                .map_err(|e| e.to_string())?;
            session.resume_session_id = resume_session_id;
            session.stdout_tx = stdout_tx;
            session.pid_tx = Some(pid_sender);
            backend
                .trigger(&agent, &session, Some(&instruction))
                .await
                .map_err(|e| e.to_string())
        })
    })
    .await
    .unwrap_or_else(|e| Err(format!("spawn_blocking panicked: {}", e)));

    // Ensure PID persistence task completes (best-effort).
    let _ = pid_task.await;

    let duration_ms = start.elapsed().as_millis() as i64;

    match trigger_result {
        Ok(result) => {
            // Intent is already parsed by the backend — no executor-side parsing needed.
            let parsed_intent = result.parsed_intent.clone();
            let mut output_text = result.result_text.clone();
            let error_category = result.error_category.clone();

            // Persist the backend session ID unconditionally (safety net).
            // The telemetry consumer may have already persisted it mid-stream,
            // but this ensures it's captured even if the consumer missed it.
            // The write is idempotent.
            if let Some(ref backend_session_id) = result.session_id {
                if !backend_session_id.is_empty() {
                    if let Err(e) = store
                        .set_backend_session_id(&exec_id, backend_session_id)
                        .await
                    {
                        tracing::warn!(
                            exec_id = %exec_id,
                            error = %e,
                            "failed to persist backend session ID (safety net)"
                        );
                    }
                }
            }

            // Append worktree uncommitted change status when applicable.
            // Uses spawn_blocking because worktree_status runs git subprocesses.
            if result.success {
                let status_suffix = if let Some(wt_path) = worktree_path_for_status.clone() {
                    tokio::task::spawn_blocking(move || WorktreeManager::worktree_status(&wt_path))
                        .await
                        .ok()
                        .flatten()
                } else {
                    None
                };
                if let Some(s) = status_suffix {
                    output_text.push_str(&s);
                }
            }

            if result.success {
                match store
                    .complete_execution(
                        &exec_id,
                        Some(0),
                        Some(&truncate(&output_text, 4096)),
                        parsed_intent.as_deref(),
                        duration_ms,
                    )
                    .await
                {
                    Ok(0) => {
                        tracing::warn!(
                            exec_id = %exec_id,
                            "complete_execution was a no-op — execution already in terminal state (stale check race)"
                        );
                    }
                    Err(e) => {
                        tracing::warn!(exec_id = %exec_id, error = %e, "complete_execution failed");
                    }
                    _ => {}
                }
            } else {
                match store
                    .fail_execution(
                        &exec_id,
                        &truncate(&output_text, 4096),
                        Some(1),
                        duration_ms,
                        ExecutionStatus::Failed,
                    )
                    .await
                {
                    Ok(0) => {
                        tracing::warn!(
                            exec_id = %exec_id,
                            "fail_execution was a no-op — execution already in terminal state (stale check race)"
                        );
                    }
                    Err(e) => {
                        tracing::warn!(exec_id = %exec_id, error = %e, "fail_execution failed");
                    }
                    _ => {}
                }
                // Persist error category for diagnostics.
                if let Some(ref cat) = error_category {
                    if let Err(e) = store.set_error_category(&exec_id, cat.as_str()).await {
                        tracing::warn!(exec_id = %exec_id, error = %e, "failed to persist error_category");
                    }
                }
                // NOTE: mark_thread_failed_if_active is NOT called here —
                // it is handled by handle_trigger_output which decides whether
                // to retry or mark terminal.
            }

            TriggerOutput {
                execution_id: exec_id,
                thread_id,
                agent_alias,
                success: result.success,
                output: Some(output_text),
                exit_code: if result.success { Some(0) } else { Some(1) },
                duration_ms,
                parsed_intent,
                error_category,
                attempt_number,
                dispatch_message_id,
                timed_out: false,
            }
        }
        Err(err) => {
            let timed_out = err.contains("timed out");
            let status = if timed_out {
                ExecutionStatus::TimedOut
            } else {
                ExecutionStatus::Failed
            };
            // Classify the error from the raw error message.
            let error_category = if timed_out {
                // Timeouts are not retried per design — they indicate hung backends.
                Some(ErrorCategory::Unknown)
            } else {
                Some(crate::backend::classify_error(false, false, &err))
            };
            if let Ok(0) = store
                .fail_execution(&exec_id, &err, None, duration_ms, status)
                .await
            {
                tracing::warn!(exec_id = %exec_id, "fail_execution was a no-op — already terminal");
            }
            // Persist error category for diagnostics.
            if let Some(ref cat) = error_category {
                if let Err(e) = store.set_error_category(&exec_id, cat.as_str()).await {
                    tracing::warn!(exec_id = %exec_id, error = %e, "failed to persist error_category");
                }
            }
            // NOTE: mark_thread_failed_if_active is NOT called here —
            // it is handled by handle_trigger_output which decides whether
            // to retry or mark terminal.
            TriggerOutput {
                execution_id: exec_id,
                thread_id,
                agent_alias,
                success: false,
                output: Some(err),
                exit_code: None,
                duration_ms,
                parsed_intent: None,
                error_category,
                attempt_number,
                dispatch_message_id,
                timed_out,
            }
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...(truncated)", &s[..max])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("hello world", 5), "hello...(truncated)");
    }
}
