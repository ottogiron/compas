//! Executor — runs a backend trigger inside `spawn_blocking`.
//!
//! Wraps the blocking CLI subprocess execution so it doesn't starve the
//! tokio runtime. Captures output, exit code, and duration.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use crate::backend::registry::BackendRegistry;
use crate::config::types::AgentConfig;
use crate::model::agent::Agent;
use crate::model::session::TriggerResult;
use crate::store::{ExecutionRow, ExecutionStatus, Store};

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
pub async fn execute_trigger(
    execution: &ExecutionRow,
    store: &Store,
    backend_registry: &Arc<BackendRegistry>,
    agent_configs: &[AgentConfig],
    instruction: &str,
    execution_timeout_secs: u64,
    log_dir: Option<PathBuf>,
) -> TriggerOutput {
    let exec_id = execution.id.clone();
    let thread_id = execution.thread_id.clone();
    let agent_alias = execution.agent_alias.clone();

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
            let _ = store.mark_thread_failed_if_active(&thread_id).await;
            return TriggerOutput {
                execution_id: exec_id,
                thread_id,
                agent_alias,
                success: false,
                output: Some(err),
                exit_code: None,
                duration_ms: 0,
                parsed_intent: None,
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
            let _ = store.mark_thread_failed_if_active(&thread_id).await;
            return TriggerOutput {
                execution_id: exec_id,
                thread_id,
                agent_alias,
                success: false,
                output: Some(err),
                exit_code: None,
                duration_ms: 0,
                parsed_intent: None,
            };
        }
    };

    // Build Agent from AgentConfig.
    // Set log_path so backends can stream output to the per-execution log file.
    let log_path = log_dir
        .as_ref()
        .map(|dir| dir.join(format!("{}.log", exec_id)));
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

    // Start a session then trigger — all inside spawn_blocking
    let instruction = instruction.to_string();
    let start = Instant::now();

    let trigger_result: Result<TriggerResult, String> = tokio::task::spawn_blocking(move || {
        // We need a runtime handle to call async methods from blocking context.
        // Use Handle::current() which was captured before spawn_blocking.
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async {
            let mut session = backend
                .start_session(&agent)
                .await
                .map_err(|e| e.to_string())?;
            session.resume_session_id = resume_session_id;
            backend
                .trigger(&agent, &session, Some(&instruction))
                .await
                .map_err(|e| e.to_string())
        })
    })
    .await
    .unwrap_or_else(|e| Err(format!("spawn_blocking panicked: {}", e)));

    let duration_ms = start.elapsed().as_millis() as i64;

    match trigger_result {
        Ok(result) => {
            let output_text = result.output.clone().unwrap_or_default();
            let parsed_intent = parse_intent_from_output(&output_text);

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

                // Persist the backend session ID so the next dispatch to this
                // thread+agent can resume the same CLI session.
                if !result.session_id.is_empty() {
                    if let Err(e) = store
                        .set_backend_session_id(&exec_id, &result.session_id)
                        .await
                    {
                        tracing::warn!(
                            exec_id = %exec_id,
                            error = %e,
                            "failed to persist backend session ID"
                        );
                    }
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
                let _ = store.mark_thread_failed_if_active(&thread_id).await;
            }

            TriggerOutput {
                execution_id: exec_id,
                thread_id,
                agent_alias,
                success: result.success,
                output: result.output,
                exit_code: if result.success { Some(0) } else { Some(1) },
                duration_ms,
                parsed_intent,
            }
        }
        Err(err) => {
            let status = if err.contains("timed out") {
                ExecutionStatus::TimedOut
            } else {
                ExecutionStatus::Failed
            };
            if let Ok(0) = store
                .fail_execution(&exec_id, &err, None, duration_ms, status)
                .await
            {
                tracing::warn!(exec_id = %exec_id, "fail_execution was a no-op — already terminal");
            }
            let _ = store.mark_thread_failed_if_active(&thread_id).await;
            TriggerOutput {
                execution_id: exec_id,
                thread_id,
                agent_alias,
                success: false,
                output: Some(err),
                exit_code: None,
                duration_ms,
                parsed_intent: None,
            }
        }
    }
}

/// Try to parse structured intent from agent output.
///
/// Agents can embed JSON like `{"intent": "status-update", "to": "operator", "body": "..."}`
/// in their output text. We look for this pattern.
fn parse_intent_from_output(text: &str) -> Option<String> {
    // Try parsing the entire text as JSON
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(text) {
        if let Some(intent) = val.get("intent").and_then(|v| v.as_str()) {
            return Some(intent.to_string());
        }
    }
    // Try finding embedded JSON in the text (last {...} block)
    if let Some(start) = text.rfind('{') {
        if let Some(end) = text[start..].rfind('}') {
            let candidate = &text[start..=start + end];
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(candidate) {
                if let Some(intent) = val.get("intent").and_then(|v| v.as_str()) {
                    return Some(intent.to_string());
                }
            }
        }
    }
    None
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
    fn test_parse_intent_json() {
        let text = r#"{"intent": "status-update", "to": "operator", "body": "Done"}"#;
        assert_eq!(
            parse_intent_from_output(text),
            Some("status-update".to_string())
        );
    }

    #[test]
    fn test_parse_intent_embedded() {
        let text = r#"I finished the task. {"intent": "completion", "to": "lead"}"#;
        assert_eq!(
            parse_intent_from_output(text),
            Some("completion".to_string())
        );
    }

    #[test]
    fn test_parse_intent_none() {
        assert_eq!(parse_intent_from_output("just plain text"), None);
    }

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("hello world", 5), "hello...(truncated)");
    }
}
