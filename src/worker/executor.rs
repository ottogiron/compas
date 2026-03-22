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

/// Compare two paths for equivalence, resolving symlinks and trailing slashes.
fn paths_equivalent(a: &std::path::Path, b: &std::path::Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => a == b,
    }
}

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
            match store
                .fail_execution(
                    &exec_id,
                    &err,
                    None,
                    0,
                    ExecutionStatus::Failed,
                    None,
                    None,
                    None,
                    None,
                )
                .await
            {
                Ok(0) => {
                    tracing::warn!(exec_id = %exec_id, "fail_execution was a no-op — already terminal");
                }
                Err(e) => {
                    tracing::error!(exec_id = %exec_id, error = %e, "fail_execution failed");
                }
                _ => {}
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
            match store
                .fail_execution(
                    &exec_id,
                    &err,
                    None,
                    0,
                    ExecutionStatus::Failed,
                    None,
                    None,
                    None,
                    None,
                )
                .await
            {
                Ok(0) => {
                    tracing::warn!(exec_id = %exec_id, "fail_execution was a no-op — already terminal");
                }
                Err(e) => {
                    tracing::error!(exec_id = %exec_id, error = %e, "fail_execution failed");
                }
                _ => {}
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
                let mut stored = false;
                for attempt in 0..3u64 {
                    match store
                        .set_thread_worktree_path(&execution.thread_id, &path, &agent_workdir)
                        .await
                    {
                        Ok(()) => {
                            stored = true;
                            break;
                        }
                        Err(e) => {
                            tracing::warn!(
                                error = %e,
                                attempt = attempt,
                                thread_id = %execution.thread_id,
                                "failed to store worktree path, retrying"
                            );
                            tokio::time::sleep(std::time::Duration::from_millis(
                                100 * (attempt + 1),
                            ))
                            .await;
                        }
                    }
                }
                if !stored {
                    tracing::error!(
                        thread_id = %execution.thread_id,
                        "worktree path not persisted after 3 attempts — auto-merge will not trigger for this thread"
                    );
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
    } else if agent_config.workspace.as_deref() == Some("shared") {
        // Explicit opt-out: agent wants the main repo state, not a worktree.
        agent_config.workdir.clone()
    } else {
        // workspace is None — eligible for thread worktree inheritance.
        // Inherit if this thread has a worktree from a preceding agent AND
        // the worktree was created from the same repo this agent targets.
        match store.get_thread_worktree_info(&execution.thread_id).await {
            Ok(Some((wt_path, wt_repo_root)))
                if wt_path.exists() && paths_equivalent(&wt_repo_root, &agent_workdir) =>
            {
                tracing::info!(
                    thread_id = %execution.thread_id,
                    agent = %agent_alias,
                    inherited_path = %wt_path.display(),
                    "non-worktree agent inheriting thread worktree (same repo)"
                );
                Some(wt_path)
            }
            _ => agent_config.workdir.clone(),
        }
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

    // Returns (BackendOutput, internal_session_id). The internal UUID is
    // needed to guard against persisting it as a real backend session ID.
    let trigger_result: Result<(BackendOutput, String), String> =
        tokio::task::spawn_blocking(move || {
            // We need a runtime handle to call async methods from blocking context.
            // Use Handle::current() which was captured before spawn_blocking.
            let rt = tokio::runtime::Handle::current();
            rt.block_on(async {
                let mut session = backend
                    .start_session(&agent)
                    .await
                    .map_err(|e| e.to_string())?;
                let internal_session_id = session.id.clone();
                session.resume_session_id = resume_session_id;
                session.stdout_tx = stdout_tx;
                session.pid_tx = Some(pid_sender);
                let output = backend
                    .trigger(&agent, &session, Some(&instruction))
                    .await
                    .map_err(|e| e.to_string())?;
                Ok((output, internal_session_id))
            })
        })
        .await
        .unwrap_or_else(|e| Err(format!("spawn_blocking panicked: {}", e)));

    // Ensure PID persistence task completes (best-effort).
    let _ = pid_task.await;

    let duration_ms = start.elapsed().as_millis() as i64;

    match trigger_result {
        Ok((result, internal_session_id)) => {
            // Intent is already parsed by the backend — no executor-side parsing needed.
            let parsed_intent = result.parsed_intent.clone();
            let mut output_text = result.result_text.clone();
            let error_category = result.error_category.clone();
            let cost_usd = result.cost_usd;
            let tokens_in = result.tokens_in;
            let tokens_out = result.tokens_out;
            let num_turns = result.num_turns;

            // Persist the backend session ID (safety net).
            // The telemetry consumer may have already persisted it mid-stream,
            // but this ensures it's captured even if the consumer missed it.
            // The write is idempotent.
            //
            // Guard: only persist session IDs that came from actual backend
            // output, not the internal session UUID used as a fallback. If the
            // backend produced no real session ID (e.g., Claude exit-0 with
            // non-JSON output), `result.session_id` may fall back to the
            // internal UUID — persisting that would cause the next dispatch to
            // pass `-r <uuid>` which the backend rejects.
            if let Some(ref backend_session_id) = result.session_id {
                if !backend_session_id.is_empty() && *backend_session_id != internal_session_id {
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
                        .and_then(|r| r.ok())
                        .flatten()
                } else {
                    None
                };
                if let Some(s) = status_suffix {
                    output_text.push_str(&s);
                }
            }

            // Finalize the execution row in the DB. Retries up to 2 additional
            // times with backoff to survive transient SQLITE_BUSY errors that
            // occur when another connection holds the write lock (telemetry
            // flush, heartbeat, stale checker, MCP server).
            let output_preview = truncate(&output_text, 4096);
            if result.success {
                let finalized = finalize_with_retry(3, || {
                    store.complete_execution(
                        &exec_id,
                        Some(0),
                        Some(&output_preview),
                        parsed_intent.as_deref(),
                        duration_ms,
                        cost_usd,
                        tokens_in,
                        tokens_out,
                        num_turns,
                    )
                })
                .await;
                match finalized {
                    Ok(0) => {
                        tracing::warn!(
                            exec_id = %exec_id,
                            "complete_execution was a no-op — execution already in terminal state (stale check race)"
                        );
                    }
                    Err(e) => {
                        tracing::error!(
                            exec_id = %exec_id,
                            error = %e,
                            "complete_execution failed after retries — execution stuck in 'executing'"
                        );
                    }
                    _ => {}
                }
            } else {
                let finalized = finalize_with_retry(3, || {
                    store.fail_execution(
                        &exec_id,
                        &output_preview,
                        Some(1),
                        duration_ms,
                        ExecutionStatus::Failed,
                        cost_usd,
                        tokens_in,
                        tokens_out,
                        num_turns,
                    )
                })
                .await;
                match finalized {
                    Ok(0) => {
                        tracing::warn!(
                            exec_id = %exec_id,
                            "fail_execution was a no-op — execution already in terminal state (stale check race)"
                        );
                    }
                    Err(e) => {
                        tracing::error!(
                            exec_id = %exec_id,
                            error = %e,
                            "fail_execution failed after retries — execution stuck in 'executing'"
                        );
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
            let finalized = finalize_with_retry(3, || {
                store.fail_execution(
                    &exec_id,
                    &err,
                    None,
                    duration_ms,
                    status.clone(),
                    None,
                    None,
                    None,
                    None,
                )
            })
            .await;
            match finalized {
                Ok(0) => {
                    tracing::warn!(exec_id = %exec_id, "fail_execution was a no-op — already terminal");
                }
                Err(e) => {
                    tracing::error!(
                        exec_id = %exec_id,
                        error = %e,
                        "fail_execution failed after retries — execution stuck in 'executing'"
                    );
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

/// Retry a store finalization call up to `max_attempts` times with
/// exponential backoff (200ms, 400ms, 800ms, …). Returns the result of
/// the last attempt. Only retries on `Err`; any `Ok` is returned
/// immediately.
async fn finalize_with_retry<F, Fut>(max_attempts: u32, op: F) -> Result<u64, sqlx::Error>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<u64, sqlx::Error>>,
{
    let mut last_err = None;
    for attempt in 0..max_attempts {
        match op().await {
            Ok(rows) => return Ok(rows),
            Err(e) => {
                last_err = Some(e);
                if attempt + 1 < max_attempts {
                    tokio::time::sleep(std::time::Duration::from_millis(200 * (1u64 << attempt)))
                        .await;
                }
            }
        }
    }
    Err(last_err.expect("max_attempts must be >= 1"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("hello world", 5), "hello...(truncated)");
    }

    /// GAP-2a: The UUID guard prevents persisting the internal session UUID as a
    /// real backend session ID. This test validates the guard predicate directly.
    #[test]
    fn test_session_id_guard_blocks_internal_uuid() {
        let internal_session_id = "550e8400-e29b-41d4-a716-446655440000";
        // When backend output session_id == internal UUID → should NOT persist
        let backend_session_id = internal_session_id;
        let should_persist =
            !backend_session_id.is_empty() && backend_session_id != internal_session_id;
        assert!(!should_persist, "internal UUID must not be persisted");
    }

    #[test]
    fn test_session_id_guard_allows_real_session_id() {
        let internal_session_id = "550e8400-e29b-41d4-a716-446655440000";
        // When backend produces a real session ID → should persist
        let backend_session_id = "real-session-abc123";
        let should_persist =
            !backend_session_id.is_empty() && backend_session_id != internal_session_id;
        assert!(
            should_persist,
            "real backend session ID should be persisted"
        );
    }

    #[test]
    fn test_session_id_guard_blocks_empty() {
        let internal_session_id = "550e8400-e29b-41d4-a716-446655440000";
        let backend_session_id = "";
        let should_persist =
            !backend_session_id.is_empty() && backend_session_id != internal_session_id;
        assert!(!should_persist, "empty session ID must not be persisted");
    }

    #[tokio::test]
    async fn test_finalize_with_retry_succeeds_first_attempt() {
        let result = finalize_with_retry(3, || async { Ok(1u64) }).await;
        assert_eq!(result.unwrap(), 1);
    }

    #[tokio::test]
    async fn test_finalize_with_retry_succeeds_after_failures() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let attempts = AtomicU32::new(0);
        let result = finalize_with_retry(3, || {
            let n = attempts.fetch_add(1, Ordering::SeqCst);
            async move {
                if n < 2 {
                    Err(sqlx::Error::PoolTimedOut)
                } else {
                    Ok(1u64)
                }
            }
        })
        .await;
        assert_eq!(result.unwrap(), 1);
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn test_finalize_with_retry_exhausts_all_attempts() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let attempts = AtomicU32::new(0);
        let result = finalize_with_retry(3, || {
            attempts.fetch_add(1, Ordering::SeqCst);
            async { Err::<u64, _>(sqlx::Error::PoolTimedOut) }
        })
        .await;
        assert!(result.is_err());
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn test_finalize_with_retry_no_retry_on_success() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let attempts = AtomicU32::new(0);
        let result = finalize_with_retry(3, || {
            attempts.fetch_add(1, Ordering::SeqCst);
            async { Ok(0u64) }
        })
        .await;
        // Ok(0) should be returned immediately without retries.
        assert_eq!(result.unwrap(), 0);
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }
}
