use crate::error::{OrchestratorError, Result};
use crate::redact::Redactor;
use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Tracked subprocess: maps session_id → child PID, plus real CLI session IDs.
#[derive(Debug, Default)]
pub struct ProcessTracker {
    pids: Mutex<HashMap<String, u32>>,
    /// Maps internal session UUID → real CLI session ID (returned by backend).
    real_session_ids: Mutex<HashMap<String, String>>,
}

impl ProcessTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn track(&self, session_id: &str, pid: u32) {
        self.pids
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session_id.to_string(), pid);
    }

    pub fn untrack(&self, session_id: &str) {
        self.pids
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(session_id);
    }

    pub fn get_pid(&self, session_id: &str) -> Option<u32> {
        self.pids
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(session_id)
            .copied()
    }

    pub fn is_running(&self, session_id: &str) -> bool {
        if let Some(pid) = self.get_pid(session_id) {
            // Check if process is still alive via kill(0)
            unsafe { libc::kill(pid as i32, 0) == 0 }
        } else {
            false
        }
    }

    /// Store the real CLI session ID returned by the backend for an internal session.
    pub fn set_real_session_id(&self, internal_id: &str, real_id: &str) {
        self.real_session_ids
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(internal_id.to_string(), real_id.to_string());
    }

    /// Get the real CLI session ID for resume, if one was stored.
    pub fn get_real_session_id(&self, internal_id: &str) -> Option<String> {
        self.real_session_ids
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(internal_id)
            .cloned()
    }
}

/// Check if a CLI command exists in PATH.
pub fn command_exists(name: &str) -> bool {
    Command::new("which")
        .arg(name)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Spawn a CLI subprocess with the given command, args, environment, and working directory.
/// Validates that the command exists in PATH before spawning.
pub fn spawn_cli(
    command: &str,
    args: &[&str],
    env: Option<&HashMap<String, String>>,
    workdir: Option<&Path>,
) -> Result<Child> {
    if !command_exists(command) {
        return Err(OrchestratorError::Backend(format!(
            "CLI '{}' not found in PATH — install it or check your environment",
            command
        )));
    }
    let mut cmd = Command::new(command);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // Clear env vars that prevent nested CLI sessions
    cmd.env_remove("CLAUDECODE");

    if let Some(dir) = workdir {
        cmd.current_dir(dir);
    }
    if let Some(env_vars) = env {
        for (k, v) in env_vars {
            cmd.env(k, v);
        }
    }

    cmd.spawn()
        .map_err(|e| OrchestratorError::Backend(format!("failed to spawn '{}': {}", command, e)))
}

/// Wait for a child process with an optional timeout, writing output lines incrementally
/// to `log_path` as they arrive (prevents pipe-buffer deadlock for long-running agents).
///
/// Stdout lines are written as-is; stderr lines are prefixed with `[stderr] `.
/// The returned `Output` contains the full collected stdout/stderr bytes, identical
/// in shape to what the previous blocking implementation returned.
///
/// `stdout_tx`: when `Some`, each stdout line is sent via `try_send()` for real-time
/// telemetry consumption. The send is best-effort and never blocks the agent process.
/// Wrapped in `Arc` so it can be cheaply cloned from `Session` into the reader thread.
pub fn wait_with_timeout(
    mut child: Child,
    timeout: Option<Duration>,
    log_path: Option<&Path>,
    stdout_tx: Option<std::sync::Arc<std::sync::mpsc::SyncSender<String>>>,
    redactor: Option<Arc<Redactor>>,
) -> Result<Output> {
    // Open (or create) the log file before spawning reader threads so that any
    // open error is surfaced early and doesn't swallow output silently.
    let log_file: Option<Arc<Mutex<std::fs::File>>> = if let Some(path) = log_path {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()
            .map(|f| Arc::new(Mutex::new(f)))
    } else {
        None
    };

    // Take stdout/stderr pipes before entering the wait loop so that we drain
    // them incrementally.  Without this, a long-running agent that produces
    // more output than the OS pipe buffer can hold will deadlock: the agent
    // blocks on write, the worker blocks on try_wait, neither makes progress.
    let child_stdout = child.stdout.take();
    let child_stderr = child.stderr.take();

    let stdout_bytes = Arc::new(Mutex::new(Vec::<u8>::new()));
    let stderr_bytes = Arc::new(Mutex::new(Vec::<u8>::new()));

    // Drain stdout in a background thread.
    let mut out_thread: Option<std::thread::JoinHandle<()>> = {
        let buf = Arc::clone(&stdout_bytes);
        let lf = log_file.clone();
        let rd = redactor.clone();
        child_stdout.map(|stream| {
            std::thread::spawn(move || {
                let reader = BufReader::new(stream);
                for l in reader.lines().map_while(|r| r.ok()) {
                    {
                        let mut b = buf.lock().unwrap_or_else(|e| e.into_inner());
                        b.extend_from_slice(l.as_bytes());
                        b.push(b'\n');
                    }
                    if let Some(ref f) = lf {
                        let redacted = match rd {
                            Some(ref r) => r.redact(&l),
                            None => l.clone(),
                        };
                        let mut guard = f.lock().unwrap_or_else(|e| e.into_inner());
                        let _ = writeln!(guard, "{}", redacted);
                    }
                    if let Some(ref tx) = stdout_tx {
                        if let Err(std::sync::mpsc::TrySendError::Full(_)) = tx.try_send(l.clone())
                        {
                            tracing::debug!("telemetry channel full, dropping stdout line");
                        }
                    }
                }
            })
        })
    };

    // Drain stderr in a background thread (lines prefixed with `[stderr] `).
    let mut err_thread: Option<std::thread::JoinHandle<()>> = {
        let buf = Arc::clone(&stderr_bytes);
        let lf = log_file.clone();
        child_stderr.map(|stream| {
            std::thread::spawn(move || {
                let reader = BufReader::new(stream);
                for l in reader.lines().map_while(|r| r.ok()) {
                    {
                        let mut b = buf.lock().unwrap_or_else(|e| e.into_inner());
                        b.extend_from_slice(l.as_bytes());
                        b.push(b'\n');
                    }
                    if let Some(ref f) = lf {
                        let redacted = match redactor {
                            Some(ref r) => r.redact(&l),
                            None => l.clone(),
                        };
                        let mut guard = f.lock().unwrap_or_else(|e| e.into_inner());
                        let _ = writeln!(guard, "[stderr] {}", redacted);
                    }
                }
            })
        })
    };

    match timeout {
        Some(dur) => {
            let start = std::time::Instant::now();
            loop {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        // Process exited — drain any remaining pipe data then return.
                        return Ok(collect_output(
                            status,
                            &mut out_thread,
                            &mut err_thread,
                            &stdout_bytes,
                            &stderr_bytes,
                        ));
                    }
                    Ok(None) => {
                        if start.elapsed() >= dur {
                            // Timeout — kill the process; drain remaining output so the
                            // log file is as complete as possible before returning error.
                            let _ = child.kill();
                            let _ = child.wait();
                            if let Some(t) = out_thread.take() {
                                let _ = t.join();
                            }
                            if let Some(t) = err_thread.take() {
                                let _ = t.join();
                            }
                            return Err(OrchestratorError::Timeout(format!(
                                "process timed out after {}s",
                                dur.as_secs()
                            )));
                        }
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    Err(e) => {
                        if let Some(t) = out_thread.take() {
                            let _ = t.join();
                        }
                        if let Some(t) = err_thread.take() {
                            let _ = t.join();
                        }
                        return Err(OrchestratorError::Backend(format!(
                            "error waiting for process: {}",
                            e
                        )));
                    }
                }
            }
        }
        None => {
            // No timeout: block until the process exits, then drain remaining pipe data.
            let status = child.wait().map_err(|e| {
                OrchestratorError::Backend(format!("failed to wait for process: {}", e))
            })?;
            Ok(collect_output(
                status,
                &mut out_thread,
                &mut err_thread,
                &stdout_bytes,
                &stderr_bytes,
            ))
        }
    }
}

/// Join reader threads and build an `Output` from the collected bytes.
///
/// Called after the child process has exited (or been killed) so the reader
/// threads will see EOF and terminate quickly.
fn collect_output(
    status: ExitStatus,
    out_thread: &mut Option<std::thread::JoinHandle<()>>,
    err_thread: &mut Option<std::thread::JoinHandle<()>>,
    stdout_bytes: &Arc<Mutex<Vec<u8>>>,
    stderr_bytes: &Arc<Mutex<Vec<u8>>>,
) -> Output {
    if let Some(t) = out_thread.take() {
        let _ = t.join();
    }
    if let Some(t) = err_thread.take() {
        let _ = t.join();
    }
    Output {
        status,
        stdout: stdout_bytes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone(),
        stderr: stderr_bytes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone(),
    }
}

/// Kill a process: SIGTERM first, then SIGKILL after 5 seconds if still alive.
pub fn kill_process(pid: u32) -> Result<()> {
    unsafe {
        // Send SIGTERM
        if libc::kill(pid as i32, libc::SIGTERM) != 0 {
            return Err(OrchestratorError::Backend(format!(
                "failed to send SIGTERM to pid {}",
                pid
            )));
        }
    }

    // Wait up to 5 seconds for graceful termination
    for _ in 0..50 {
        std::thread::sleep(Duration::from_millis(100));
        unsafe {
            if libc::kill(pid as i32, 0) != 0 {
                return Ok(()); // Process is gone
            }
        }
    }

    // Force kill
    unsafe {
        libc::kill(pid as i32, libc::SIGKILL);
    }
    Ok(())
}

/// Parse JSON output from a CLI subprocess.
pub fn parse_json_output(output: &Output) -> Result<serde_json::Value> {
    let stdout = String::from_utf8_lossy(&output.stdout);
    if stdout.trim().is_empty() {
        return Err(OrchestratorError::Backend(
            "empty output from subprocess".into(),
        ));
    }
    serde_json::from_str(stdout.trim()).map_err(|e| {
        let stderr = String::from_utf8_lossy(&output.stderr);
        OrchestratorError::Backend(format!(
            "failed to parse JSON output: {}; stdout: {}; stderr: {}",
            e,
            stdout.chars().take(500).collect::<String>(),
            stderr.chars().take(500).collect::<String>(),
        ))
    })
}

/// Resolve the effective system prompt for an agent.
/// If prompt_file is set and exists, its contents take precedence over inline prompt.
pub fn resolve_prompt(prompt: Option<&str>, prompt_file: Option<&Path>) -> Result<Option<String>> {
    if let Some(pf) = prompt_file {
        let content = std::fs::read_to_string(pf).map_err(|e| {
            OrchestratorError::Backend(format!(
                "failed to read prompt_file '{}': {}",
                pf.display(),
                e
            ))
        })?;
        return Ok(Some(content));
    }
    Ok(prompt.map(|s| s.to_string()))
}

/// Multi-stage text extraction from CLI output.
/// Tries: single JSON object → JSONL stream → raw stdout → raw stderr.
pub fn extract_output_text(output: &Output) -> String {
    // 1) Single JSON object with a known text payload field.
    if let Ok(val) = parse_json_output(output) {
        if let Some(text) = extract_text_from_value(&val) {
            return text;
        }
    }

    // 2) JSONL / event-stream: parse each line.
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    if let Some(text) = extract_text_from_jsonl(&stdout) {
        return text;
    }

    // 3) Raw stdout.
    if !stdout.trim().is_empty() {
        return stdout;
    }

    // 4) Raw stderr (last resort for diagnostics).
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if !stderr.trim().is_empty() {
        return stderr;
    }

    String::new()
}

/// Parse JSONL output, accumulating `content.delta` events or taking the last
/// `item.completed` text. Falls back to the last generic text match.
pub fn extract_text_from_jsonl(text: &str) -> Option<String> {
    let mut deltas = String::new();
    let mut last_item_text: Option<String> = None;
    let mut last_generic_text: Option<String> = None;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };

        // OpenCode streaming: accumulate content.delta part.text
        if value.get("type").and_then(|t| t.as_str()) == Some("content.delta") {
            if let Some(t) = value.pointer("/part/text").and_then(|v| v.as_str()) {
                deltas.push_str(t);
            }
            continue;
        }

        // Codex streaming: keep last item.completed item.text
        if value.get("type").and_then(|t| t.as_str()) == Some("item.completed") {
            if let Some(t) = value.pointer("/item/text").and_then(|v| v.as_str()) {
                last_item_text = Some(t.to_string());
            }
            continue;
        }

        // Generic: keep last match
        if let Some(found) = extract_text_from_value(&value) {
            last_generic_text = Some(found);
        }
    }

    if !deltas.is_empty() {
        return Some(deltas);
    }
    if let Some(text) = last_item_text {
        return Some(text);
    }
    last_generic_text
}

/// Recursively search a JSON value for text in common payload fields.
pub fn extract_text_from_value(value: &Value) -> Option<String> {
    match value {
        Value::Object(map) => {
            for key in ["result", "text", "content", "message", "response"] {
                if let Some(v) = map.get(key) {
                    match v {
                        Value::String(s) if !s.trim().is_empty() => {
                            return Some(s.to_string());
                        }
                        Value::Object(_) | Value::Array(_) => {
                            if let Some(found) = extract_text_from_value(v) {
                                return Some(found);
                            }
                        }
                        _ => {}
                    }
                }
            }
            for v in map.values() {
                if let Some(found) = extract_text_from_value(v) {
                    return Some(found);
                }
            }
            None
        }
        Value::Array(values) => {
            for v in values {
                if let Some(found) = extract_text_from_value(v) {
                    return Some(found);
                }
            }
            None
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn test_process_tracker_basic() {
        let tracker = ProcessTracker::new();
        tracker.track("sess-1", 99999);
        assert_eq!(tracker.get_pid("sess-1"), Some(99999));
        tracker.untrack("sess-1");
        assert_eq!(tracker.get_pid("sess-1"), None);
    }

    #[test]
    fn test_parse_json_output_valid() {
        let output = Output {
            status: Command::new("true").status().unwrap(),
            stdout: br#"{"result": "ok"}"#.to_vec(),
            stderr: Vec::new(),
        };
        let val = parse_json_output(&output).unwrap();
        assert_eq!(val["result"], "ok");
    }

    #[test]
    fn test_parse_json_output_empty() {
        let output = Output {
            status: Command::new("true").status().unwrap(),
            stdout: Vec::new(),
            stderr: Vec::new(),
        };
        assert!(parse_json_output(&output).is_err());
    }

    #[test]
    fn test_parse_json_output_invalid() {
        let output = Output {
            status: Command::new("true").status().unwrap(),
            stdout: b"not json".to_vec(),
            stderr: Vec::new(),
        };
        assert!(parse_json_output(&output).is_err());
    }

    #[test]
    fn test_process_tracker_real_session_id() {
        let tracker = ProcessTracker::new();
        assert!(tracker.get_real_session_id("sess-1").is_none());
        tracker.set_real_session_id("sess-1", "real-claude-sid-abc");
        assert_eq!(
            tracker.get_real_session_id("sess-1"),
            Some("real-claude-sid-abc".to_string())
        );
        // Overwrite
        tracker.set_real_session_id("sess-1", "real-claude-sid-xyz");
        assert_eq!(
            tracker.get_real_session_id("sess-1"),
            Some("real-claude-sid-xyz".to_string())
        );
    }

    #[test]
    fn test_resolve_prompt_inline() {
        let result = resolve_prompt(Some("hello"), None).unwrap();
        assert_eq!(result.as_deref(), Some("hello"));
    }

    #[test]
    fn test_resolve_prompt_none() {
        let result = resolve_prompt(None, None).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_resolve_prompt_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prompt.txt");
        std::fs::write(&path, "file prompt").unwrap();
        let result = resolve_prompt(Some("inline"), Some(&path)).unwrap();
        assert_eq!(result.as_deref(), Some("file prompt"));
    }

    #[test]
    fn test_command_exists_true() {
        assert!(command_exists("echo"));
    }

    #[test]
    fn test_command_exists_false() {
        assert!(!command_exists("nonexistent-cli-abc123"));
    }

    #[test]
    fn test_spawn_cli_rejects_missing_command() {
        let result = spawn_cli("nonexistent-cli-abc123", &["arg"], None, None);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("not found in PATH"), "error was: {}", err);
    }

    #[test]
    fn test_spawn_cli_echo() {
        let child = spawn_cli("echo", &["hello"], None, None).unwrap();
        let output =
            wait_with_timeout(child, Some(Duration::from_secs(5)), None, None, None).unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("hello"));
    }

    #[test]
    fn test_spawn_cli_respects_workdir() {
        let dir = tempfile::tempdir().unwrap();
        let child = spawn_cli("pwd", &[], None, Some(dir.path())).unwrap();
        let output =
            wait_with_timeout(child, Some(Duration::from_secs(5)), None, None, None).unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let cwd = std::fs::canonicalize(stdout.trim()).unwrap();
        let expected = std::fs::canonicalize(dir.path()).unwrap();
        assert_eq!(cwd, expected);
    }

    #[test]
    fn test_wait_with_timeout_expires() {
        let child = spawn_cli("sleep", &["60"], None, None).unwrap();
        let result = wait_with_timeout(child, Some(Duration::from_millis(200)), None, None, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("timed out"));
    }

    #[test]
    fn test_wait_with_timeout_logs_stdout_to_file() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("exec-test.log");
        let child = spawn_cli("echo", &["hello from stdout"], None, None).unwrap();
        let output = wait_with_timeout(
            child,
            Some(Duration::from_secs(5)),
            Some(&log_path),
            None,
            None,
        )
        .unwrap();
        // Output bytes are still collected correctly.
        assert!(String::from_utf8_lossy(&output.stdout).contains("hello from stdout"));
        // Log file contains the line.
        let log = std::fs::read_to_string(&log_path).unwrap();
        assert!(log.contains("hello from stdout"), "log was: {:?}", log);
    }

    #[test]
    fn test_wait_with_timeout_logs_stderr_with_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("exec-stderr.log");
        let child = spawn_cli("sh", &["-c", "echo 'error line' >&2"], None, None).unwrap();
        let _ = wait_with_timeout(
            child,
            Some(Duration::from_secs(5)),
            Some(&log_path),
            None,
            None,
        )
        .unwrap();
        let log = std::fs::read_to_string(&log_path).unwrap();
        assert!(log.contains("[stderr] error line"), "log was: {:?}", log);
    }

    #[test]
    fn test_wait_with_timeout_creates_log_dir() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("logs").join("nested").join("exec.log");
        let child = spawn_cli("echo", &["hi"], None, None).unwrap();
        let _ = wait_with_timeout(
            child,
            Some(Duration::from_secs(5)),
            Some(&log_path),
            None,
            None,
        )
        .unwrap();
        assert!(log_path.exists(), "log file should have been created");
    }

    fn test_output(stdout: &str, stderr: &str) -> Output {
        Output {
            status: Command::new("true").status().unwrap(),
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    // -- extract_text_from_value tests --

    #[test]
    fn test_extract_text_from_value_result_field() {
        let val: Value = serde_json::from_str(r#"{"result":"ok"}"#).unwrap();
        assert_eq!(extract_text_from_value(&val).unwrap(), "ok");
    }

    #[test]
    fn test_extract_text_from_value_response_field() {
        let val: Value = serde_json::from_str(r#"{"response":"ok"}"#).unwrap();
        assert_eq!(extract_text_from_value(&val).unwrap(), "ok");
    }

    #[test]
    fn test_extract_text_from_value_nested() {
        let val: Value = serde_json::from_str(r#"{"item":{"text":"nested value"}}"#).unwrap();
        assert_eq!(extract_text_from_value(&val).unwrap(), "nested value");
    }

    #[test]
    fn test_extract_text_from_value_empty_string_skipped() {
        let val: Value = serde_json::from_str(r#"{"result":"  ","text":"real"}"#).unwrap();
        assert_eq!(extract_text_from_value(&val).unwrap(), "real");
    }

    #[test]
    fn test_extract_text_from_value_no_match() {
        let val: Value = serde_json::from_str(r#"{"type":"event","id":42}"#).unwrap();
        assert!(extract_text_from_value(&val).is_none());
    }

    // -- extract_text_from_jsonl tests --

    #[test]
    fn test_jsonl_content_delta_accumulation() {
        let input = r#"{"type":"message.start","message":{"id":"m1","role":"assistant"}}
{"type":"content.start","index":0,"part":{"type":"text","text":""}}
{"type":"content.delta","index":0,"part":{"type":"text","text":"Hello "}}
{"type":"content.delta","index":0,"part":{"type":"text","text":"world."}}
{"type":"content.stop","index":0}
{"type":"message.stop","message":{"id":"m1"}}"#;
        assert_eq!(extract_text_from_jsonl(input).unwrap(), "Hello world.");
    }

    #[test]
    fn test_jsonl_item_completed_last_wins() {
        let input = r#"{"type":"item.completed","item":{"text":"first"}}
{"type":"item.completed","item":{"text":"last answer"}}"#;
        assert_eq!(extract_text_from_jsonl(input).unwrap(), "last answer");
    }

    #[test]
    fn test_jsonl_generic_last_wins() {
        let input = r#"{"type":"status","message":"running"}
{"result":"final result"}"#;
        assert_eq!(extract_text_from_jsonl(input).unwrap(), "final result");
    }

    #[test]
    fn test_jsonl_delta_takes_priority_over_item() {
        let input = r#"{"type":"content.delta","index":0,"part":{"type":"text","text":"from delta"}}
{"type":"item.completed","item":{"text":"from item"}}"#;
        assert_eq!(extract_text_from_jsonl(input).unwrap(), "from delta");
    }

    #[test]
    fn test_jsonl_no_text_returns_none() {
        let input = r#"{"type":"thread.started","thread_id":"abc"}
{"type":"turn.started"}"#;
        assert!(extract_text_from_jsonl(input).is_none());
    }

    // -- extract_output_text tests --

    #[test]
    fn test_extract_output_text_single_json() {
        let out = test_output(r#"{"result":"ok-from-json"}"#, "");
        assert_eq!(extract_output_text(&out), "ok-from-json");
    }

    #[test]
    fn test_extract_output_text_jsonl_stream() {
        let out = test_output(
            r#"{"type":"content.delta","index":0,"part":{"type":"text","text":"streamed"}}
{"type":"message.stop"}"#,
            "",
        );
        assert_eq!(extract_output_text(&out), "streamed");
    }

    #[test]
    fn test_extract_output_text_raw_stdout_fallback() {
        let out = test_output("plain text", "");
        assert_eq!(extract_output_text(&out), "plain text");
    }

    #[test]
    fn test_extract_output_text_stderr_fallback() {
        let out = test_output("  \n", "stderr diagnostic");
        assert_eq!(extract_output_text(&out), "stderr diagnostic");
    }

    #[test]
    fn test_extract_output_text_all_empty() {
        let out = test_output(" \n\t", " \n");
        assert!(extract_output_text(&out).is_empty());
    }
}
