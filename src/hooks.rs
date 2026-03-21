//! Lifecycle hook execution engine.
//!
//! [`HookRunner`] spawns subprocess hooks at named execution lifecycle events.
//! Each hook receives event JSON on stdin, runs in a configurable working
//! directory, and is subject to a per-hook timeout enforced with
//! SIGTERM → grace period → SIGKILL.
//!
//! All failures are logged as [`tracing::warn`] and never propagate to callers
//! (fire-and-forget semantics). Hook failures never affect the execution path.
//!
//! [`spawn_hook_consumer`] subscribes to the [`crate::events::EventBus`] and
//! fires the appropriate hook group for each matching event. Hook config is
//! re-read on every event to support hot-reload without a worker restart.

use crate::config::types::HookEntry;
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

/// Executes lifecycle hooks as subprocess commands.
pub struct HookRunner;

impl HookRunner {
    /// Run a single hook: spawn subprocess, pass `event_json` on stdin, enforce timeout.
    ///
    /// Fire-and-forget: failures are logged as `tracing::warn` and never returned
    /// to the caller. A failing hook never affects the execution path.
    pub fn run_hook(hook_entry: &HookEntry, event_json: &str, workdir: &Path) {
        if let Err(e) = Self::run_hook_inner(hook_entry, event_json, workdir) {
            tracing::warn!(
                command = %hook_entry.command,
                error = %e,
                "hook execution failed"
            );
        }
    }

    /// Run multiple hooks sequentially in declaration order.
    ///
    /// A failure in one hook is logged but does not prevent subsequent hooks
    /// from running.
    pub fn run_hooks(hooks: &[HookEntry], event_json: &str, workdir: &Path) {
        for hook in hooks {
            Self::run_hook(hook, event_json, workdir);
        }
    }

    fn run_hook_inner(
        hook_entry: &HookEntry,
        event_json: &str,
        workdir: &Path,
    ) -> Result<(), String> {
        use std::process::{Command, Stdio};

        let mut cmd = Command::new(&hook_entry.command);

        if let Some(ref args) = hook_entry.args {
            cmd.args(args);
        }

        cmd.stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .current_dir(workdir);

        if let Some(ref env) = hook_entry.env {
            for (k, v) in env {
                cmd.env(k, v);
            }
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("failed to spawn hook '{}': {}", hook_entry.command, e))?;

        // Write event JSON to stdin then close the pipe (signals EOF to the subprocess).
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(event_json.as_bytes());
            // Drop closes the pipe.
        }

        let timeout = Duration::from_secs(hook_entry.timeout_secs);
        let start = Instant::now();

        loop {
            match child.try_wait() {
                Ok(Some(_status)) => return Ok(()),
                Ok(None) => {
                    if start.elapsed() >= timeout {
                        // Graceful shutdown: SIGTERM → 5 s grace → SIGKILL
                        let _ = crate::backend::process::kill_process(child.id());
                        // Reap the process regardless of kill outcome.
                        let _ = child.wait();
                        return Err(format!(
                            "hook '{}' timed out after {}s",
                            hook_entry.command, hook_entry.timeout_secs
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => {
                    return Err(format!(
                        "error waiting for hook '{}': {}",
                        hook_entry.command, e
                    ));
                }
            }
        }
    }
}

/// Spawn a long-lived task that subscribes to the [`crate::events::EventBus`] and fires
/// lifecycle hooks on matching events.
///
/// Hook commands are re-read from [`crate::config::watcher::ConfigHandle`] on every event
/// to support hot-reload — operators can add or remove hooks without restarting the worker.
///
/// Each hook group runs inside a separate [`tokio::task::spawn_blocking`] call so a slow
/// or hung hook subprocess cannot stall the subscriber loop.
///
/// [`tokio::sync::broadcast::error::RecvError::Lagged`] is handled gracefully: a warning
/// is logged and the loop continues.
pub fn spawn_hook_consumer(
    event_bus: &crate::events::EventBus,
    config: crate::config::watcher::ConfigHandle,
    default_workdir: std::path::PathBuf,
) -> tokio::task::JoinHandle<()> {
    use crate::events::OrchestratorEvent;
    use tokio::sync::broadcast;

    let mut rx = event_bus.subscribe();
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    // Re-read hooks config on every event for hot-reload support.
                    let maybe_hooks = {
                        let cfg = config.load();
                        cfg.hooks.clone()
                    };
                    let workdir = default_workdir.clone();
                    let timestamp = chrono::Utc::now().to_rfc3339();

                    match event {
                        OrchestratorEvent::ExecutionStarted {
                            execution_id,
                            thread_id,
                            agent_alias,
                        } => {
                            let hooks = maybe_hooks
                                .map(|h| h.on_execution_started)
                                .unwrap_or_default();
                            if !hooks.is_empty() {
                                let payload = serde_json::json!({
                                    "event": "execution_started",
                                    "thread_id": thread_id,
                                    "execution_id": execution_id,
                                    "agent_alias": agent_alias,
                                    "timestamp": timestamp,
                                });
                                let event_json = serde_json::to_string(&payload)
                                    .unwrap_or_else(|_| "{}".to_string());
                                tokio::task::spawn_blocking(move || {
                                    HookRunner::run_hooks(&hooks, &event_json, &workdir);
                                });
                            }
                        }
                        OrchestratorEvent::ExecutionCompleted {
                            execution_id,
                            thread_id,
                            agent_alias,
                            success,
                            duration_ms,
                            thread_summary,
                        } => {
                            let hooks = maybe_hooks
                                .map(|h| h.on_execution_completed)
                                .unwrap_or_default();
                            if !hooks.is_empty() {
                                let payload = serde_json::json!({
                                    "event": "execution_completed",
                                    "thread_id": thread_id,
                                    "execution_id": execution_id,
                                    "agent_alias": agent_alias,
                                    "success": success,
                                    "duration_ms": duration_ms,
                                    "thread_summary": thread_summary,
                                    "timestamp": timestamp,
                                });
                                let event_json = serde_json::to_string(&payload)
                                    .unwrap_or_else(|_| "{}".to_string());
                                tokio::task::spawn_blocking(move || {
                                    HookRunner::run_hooks(&hooks, &event_json, &workdir);
                                });
                            }
                        }
                        OrchestratorEvent::ThreadStatusChanged {
                            thread_id,
                            new_status,
                        } if new_status == "Completed" => {
                            let hooks = maybe_hooks.map(|h| h.on_thread_closed).unwrap_or_default();
                            if !hooks.is_empty() {
                                let payload = serde_json::json!({
                                    "event": "thread_closed",
                                    "thread_id": thread_id,
                                    "new_status": new_status,
                                    "timestamp": timestamp,
                                });
                                let event_json = serde_json::to_string(&payload)
                                    .unwrap_or_else(|_| "{}".to_string());
                                tokio::task::spawn_blocking(move || {
                                    HookRunner::run_hooks(&hooks, &event_json, &workdir);
                                });
                            }
                        }
                        OrchestratorEvent::ThreadStatusChanged {
                            thread_id,
                            new_status,
                        } if new_status == "Failed" => {
                            let hooks = maybe_hooks.map(|h| h.on_thread_failed).unwrap_or_default();
                            if !hooks.is_empty() {
                                let payload = serde_json::json!({
                                    "event": "thread_failed",
                                    "thread_id": thread_id,
                                    "new_status": new_status,
                                    "timestamp": timestamp,
                                });
                                let event_json = serde_json::to_string(&payload)
                                    .unwrap_or_else(|_| "{}".to_string());
                                tokio::task::spawn_blocking(move || {
                                    HookRunner::run_hooks(&hooks, &event_json, &workdir);
                                });
                            }
                        }
                        _ => {}
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(skipped = n, "hook consumer lagged");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::types::HookEntry;
    use std::collections::HashMap;

    fn simple_hook(command: &str, args: Option<Vec<String>>, timeout_secs: u64) -> HookEntry {
        HookEntry {
            command: command.to_string(),
            args,
            timeout_secs,
            env: None,
        }
    }

    #[test]
    fn test_hook_runner_runs_successfully() {
        let dir = tempfile::tempdir().unwrap();
        let hook = simple_hook("true", None, 10);
        // Must not panic and must not log an error.
        HookRunner::run_hook(&hook, r#"{"event":"test"}"#, dir.path());
    }

    #[test]
    fn test_hook_runner_receives_stdin_json() {
        let dir = tempfile::tempdir().unwrap();
        let out_file = dir.path().join("event.json");
        let hook = HookEntry {
            command: "sh".to_string(),
            args: Some(vec![
                "-c".to_string(),
                format!("cat > {}", out_file.display()),
            ]),
            timeout_secs: 10,
            env: None,
        };
        HookRunner::run_hook(&hook, r#"{"event":"started"}"#, dir.path());
        let content = std::fs::read_to_string(&out_file).unwrap();
        assert_eq!(content, r#"{"event":"started"}"#);
    }

    #[test]
    fn test_hook_runner_missing_command_logs_warn_no_panic() {
        let dir = tempfile::tempdir().unwrap();
        let hook = simple_hook("__compas_nonexistent_cmd_abc123__", None, 10);
        // Fire-and-forget: must not panic.
        HookRunner::run_hook(&hook, "{}", dir.path());
    }

    #[test]
    fn test_hook_runner_timeout_kills_process() {
        let dir = tempfile::tempdir().unwrap();
        let hook = HookEntry {
            command: "sleep".to_string(),
            args: Some(vec!["60".to_string()]),
            timeout_secs: 1,
            env: None,
        };
        // Should complete without panic after the timeout kills the subprocess.
        HookRunner::run_hook(&hook, "{}", dir.path());
    }

    #[test]
    fn test_run_hooks_runs_all_sequentially() {
        let dir = tempfile::tempdir().unwrap();
        let out_file = dir.path().join("out.txt");
        let hooks = vec![
            HookEntry {
                command: "sh".to_string(),
                args: Some(vec![
                    "-c".to_string(),
                    format!("echo first >> {}", out_file.display()),
                ]),
                timeout_secs: 10,
                env: None,
            },
            HookEntry {
                command: "sh".to_string(),
                args: Some(vec![
                    "-c".to_string(),
                    format!("echo second >> {}", out_file.display()),
                ]),
                timeout_secs: 10,
                env: None,
            },
        ];
        HookRunner::run_hooks(&hooks, "{}", dir.path());
        let content = std::fs::read_to_string(&out_file).unwrap();
        let first_pos = content.find("first").expect("first line missing");
        let second_pos = content.find("second").expect("second line missing");
        assert!(
            first_pos < second_pos,
            "hooks ran out of order: content={:?}",
            content
        );
    }

    #[test]
    fn test_run_hooks_failure_does_not_stop_subsequent_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let sentinel = dir.path().join("ran");
        let hooks = vec![
            simple_hook("__compas_nonexistent_cmd_abc123__", None, 10), // will fail
            HookEntry {
                command: "sh".to_string(),
                args: Some(vec![
                    "-c".to_string(),
                    format!("touch {}", sentinel.display()),
                ]),
                timeout_secs: 10,
                env: None,
            },
        ];
        HookRunner::run_hooks(&hooks, "{}", dir.path());
        assert!(
            sentinel.exists(),
            "second hook should have run after first failed"
        );
    }

    #[test]
    fn test_hook_env_vars_are_passed_to_subprocess() {
        let dir = tempfile::tempdir().unwrap();
        let out_file = dir.path().join("env_out.txt");
        let mut env = HashMap::new();
        env.insert(
            "COMPAS_HOOK_TEST_VAR".to_string(),
            "hello-hooks".to_string(),
        );
        let hook = HookEntry {
            command: "sh".to_string(),
            args: Some(vec![
                "-c".to_string(),
                format!("echo $COMPAS_HOOK_TEST_VAR > {}", out_file.display()),
            ]),
            timeout_secs: 10,
            env: Some(env),
        };
        HookRunner::run_hook(&hook, "{}", dir.path());
        let content = std::fs::read_to_string(&out_file).unwrap();
        assert!(
            content.contains("hello-hooks"),
            "env var not passed; content={:?}",
            content
        );
    }

    #[tokio::test]
    async fn test_spawn_hook_consumer_fires_on_events() {
        use crate::config::types::{HooksConfig, OrchestratorConfig};
        use crate::config::watcher::ConfigHandle;
        use crate::events::{EventBus, OrchestratorEvent};
        use std::path::PathBuf;

        let dir = tempfile::tempdir().unwrap();
        let workdir = dir.path().to_path_buf();

        // Create a stub script that writes stdin JSON to the file path given as $1
        let script_path = dir.path().join("write-event.sh");
        std::fs::write(&script_path, "#!/bin/sh\ncat > \"$1\"\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        // Sentinel files for each hook point
        let started_file = dir.path().join("started.json");
        let completed_file = dir.path().join("completed.json");
        let closed_file = dir.path().join("closed.json");
        let failed_file = dir.path().join("failed.json");
        let progress_file = dir.path().join("progress.json");

        let script = script_path.to_string_lossy().to_string();
        let hooks = HooksConfig {
            on_execution_started: vec![HookEntry {
                command: script.clone(),
                args: Some(vec![started_file.to_string_lossy().to_string()]),
                timeout_secs: 10,
                env: None,
            }],
            on_execution_completed: vec![HookEntry {
                command: script.clone(),
                args: Some(vec![completed_file.to_string_lossy().to_string()]),
                timeout_secs: 10,
                env: None,
            }],
            on_thread_closed: vec![HookEntry {
                command: script.clone(),
                args: Some(vec![closed_file.to_string_lossy().to_string()]),
                timeout_secs: 10,
                env: None,
            }],
            on_thread_failed: vec![HookEntry {
                command: script.clone(),
                args: Some(vec![failed_file.to_string_lossy().to_string()]),
                timeout_secs: 10,
                env: None,
            }],
        };

        let mut config = OrchestratorConfig {
            default_workdir: PathBuf::from("/tmp"),
            state_dir: PathBuf::from("/tmp/test"),
            poll_interval_secs: 1,
            models: None,
            agents: vec![],
            worktree_dir: None,
            orchestration: Default::default(),
            database: Default::default(),
            notifications: Default::default(),
            backend_definitions: None,
            hooks: Some(hooks),
            schedules: None,
        };
        // Suppress validation error for empty agents
        config.agents.push(crate::config::types::AgentConfig {
            alias: "stub".to_string(),
            backend: "stub".to_string(),
            role: Default::default(),
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
        });

        let config_handle = ConfigHandle::new(config);
        let event_bus = EventBus::new();

        let _handle = spawn_hook_consumer(&event_bus, config_handle, workdir);

        // Emit all four hooked events
        event_bus.emit(OrchestratorEvent::ExecutionStarted {
            execution_id: "e-1".to_string(),
            thread_id: "t-1".to_string(),
            agent_alias: "worker-a".to_string(),
        });
        event_bus.emit(OrchestratorEvent::ExecutionCompleted {
            execution_id: "e-2".to_string(),
            thread_id: "t-2".to_string(),
            agent_alias: "worker-b".to_string(),
            success: true,
            duration_ms: 5000,
            thread_summary: Some("Implement caching layer".to_string()),
        });
        event_bus.emit(OrchestratorEvent::ThreadStatusChanged {
            thread_id: "t-3".to_string(),
            new_status: "Completed".to_string(),
        });
        event_bus.emit(OrchestratorEvent::ThreadStatusChanged {
            thread_id: "t-4".to_string(),
            new_status: "Failed".to_string(),
        });

        // Emit a non-hooked event
        event_bus.emit(OrchestratorEvent::ExecutionProgress {
            execution_id: "e-99".to_string(),
            thread_id: "t-99".to_string(),
            agent_alias: "worker-x".to_string(),
            summary: "doing stuff".to_string(),
        });

        // Wait for hooks to fire (spawn_blocking + subprocess)
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;

        // Verify each sentinel file exists and has correct JSON
        let started: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&started_file).expect("started.json missing"),
        )
        .unwrap();
        assert_eq!(started["event"], "execution_started");
        assert_eq!(started["thread_id"], "t-1");
        assert_eq!(started["execution_id"], "e-1");

        let completed: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&completed_file).expect("completed.json missing"),
        )
        .unwrap();
        assert_eq!(completed["event"], "execution_completed");
        assert_eq!(completed["success"], true);
        assert_eq!(completed["duration_ms"], 5000);
        assert_eq!(completed["thread_summary"], "Implement caching layer");

        let closed: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&closed_file).expect("closed.json missing"),
        )
        .unwrap();
        assert_eq!(closed["event"], "thread_closed");
        assert_eq!(closed["new_status"], "Completed");

        let failed: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&failed_file).expect("failed.json missing"),
        )
        .unwrap();
        assert_eq!(failed["event"], "thread_failed");
        assert_eq!(failed["new_status"], "Failed");

        // Non-hooked event should NOT produce a sentinel file
        assert!(
            !progress_file.exists(),
            "ExecutionProgress should not trigger any hook"
        );
    }

    #[test]
    fn test_hook_uses_provided_workdir() {
        let dir = tempfile::tempdir().unwrap();
        let out_file = dir.path().join("cwd.txt");
        let hook = HookEntry {
            command: "sh".to_string(),
            args: Some(vec![
                "-c".to_string(),
                format!("pwd > {}", out_file.display()),
            ]),
            timeout_secs: 10,
            env: None,
        };
        HookRunner::run_hook(&hook, "{}", dir.path());
        let content = std::fs::read_to_string(&out_file).unwrap();
        let reported = std::path::PathBuf::from(content.trim());
        let canonical_reported = std::fs::canonicalize(&reported).unwrap_or(reported);
        let canonical_expected = std::fs::canonicalize(dir.path()).unwrap();
        assert_eq!(
            canonical_reported, canonical_expected,
            "hook ran in wrong workdir"
        );
    }
}
