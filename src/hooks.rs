//! Lifecycle hook execution engine.
//!
//! [`HookRunner`] spawns subprocess hooks at named execution lifecycle events.
//! Each hook receives event JSON on stdin, runs in a configurable working
//! directory, and is subject to a per-hook timeout enforced with
//! SIGTERM → grace period → SIGKILL.
//!
//! All failures are logged as [`tracing::warn`] and never propagate to callers
//! (fire-and-forget semantics). Hook failures never affect the execution path.

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
