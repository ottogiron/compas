use async_trait::async_trait;
use chrono::Utc;
use std::path::PathBuf;
use std::time::Duration;
use uuid::Uuid;

use super::process::{kill_process, resolve_prompt, spawn_cli, wait_with_timeout, ProcessTracker};
use super::{parse_intent_from_text, Backend, BackendOutput, PingResult};
use crate::error::Result;
use crate::model::agent::Agent;
use crate::model::session::{Session, SessionStatus};

/// Claude Code CLI backend.
///
/// Uses `claude -p` for non-interactive sessions with stream-json (JSONL) output.
/// Key flags: `-p`, `--dangerously-skip-permissions`, `--output-format stream-json`,
/// `--model`, `--system-prompt`/`--append-system-prompt`
#[derive(Debug)]
pub struct ClaudeCodeBackend {
    tracker: ProcessTracker,
    workdir: Option<PathBuf>,
}

impl ClaudeCodeBackend {
    pub fn new() -> Self {
        Self::with_workdir(None)
    }

    pub fn with_workdir(workdir: Option<PathBuf>) -> Self {
        Self {
            tracker: ProcessTracker::new(),
            workdir,
        }
    }

    fn build_args(
        agent: &Agent,
        instruction: &str,
        session_id: Option<&str>,
    ) -> Result<Vec<String>> {
        let mut args = Vec::new();

        // Resume or new session
        if let Some(sid) = session_id {
            args.push("-r".to_string());
            args.push(sid.to_string());
        }

        // Non-interactive print mode
        args.push("-p".to_string());

        // Skip permission prompts
        args.push("--dangerously-skip-permissions".to_string());

        // Stream-JSON output (JSONL events during execution, result line at end)
        args.push("--output-format".to_string());
        args.push("stream-json".to_string());

        // Model
        if let Some(ref model) = agent.model {
            args.push("--model".to_string());
            args.push(model.clone());
        }

        // System prompt
        let prompt = resolve_prompt(agent.prompt.as_deref(), agent.prompt_file.as_deref())?;
        if let Some(ref p) = prompt {
            if session_id.is_some() {
                args.push("--append-system-prompt".to_string());
            } else {
                args.push("--system-prompt".to_string());
            }
            args.push(p.clone());
        }

        // Extra backend args from config
        if let Some(extra) = &agent.backend_args {
            args.extend(extra.iter().cloned());
        }

        // Instruction (the prompt text)
        args.push(instruction.to_string());

        Ok(args)
    }

    fn build_ping_args(agent: &Agent) -> Vec<String> {
        let mut args = vec![
            "-p".to_string(),
            "--dangerously-skip-permissions".to_string(),
            "--output-format".to_string(),
            "json".to_string(),
            "--max-turns".to_string(),
            "1".to_string(),
        ];
        if let Some(ref model) = agent.model {
            args.push("--model".to_string());
            args.push(model.clone());
        }
        args.push("Reply with: ok".to_string());
        args
    }
}

impl Default for ClaudeCodeBackend {
    fn default() -> Self {
        Self::new()
    }
}

/// Extract result text and session_id from Claude stream-json (JSONL) output.
///
/// With `--output-format stream-json`, Claude Code emits JSONL events during
/// execution and a final result line:
/// ```jsonl
/// {"type":"system","subtype":"init","session_id":"abc-123",...}
/// {"type":"assistant","message":{"content":[...]}}
/// {"type":"result","subtype":"success","result":"Done.","session_id":"abc-123",...}
/// ```
///
/// Finds the last line with `"type":"result"` and extracts `result`, `session_id`,
/// and whether a result line was found at all.
/// Falls back to raw stdout if no result line is found.
fn extract_claude_stream_output(stdout: &[u8]) -> (String, Option<String>, bool) {
    let text = String::from_utf8_lossy(stdout);

    // Scan lines from the end — the result line is typically the last line.
    for line in text.lines().rev() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) {
            if val.get("type").and_then(|t| t.as_str()) == Some("result") {
                let result_text = val
                    .get("result")
                    .and_then(|r| r.as_str())
                    .unwrap_or("")
                    .to_string();
                let session_id = val
                    .get("session_id")
                    .and_then(|s| s.as_str())
                    .map(|s| s.to_string());
                return (result_text, session_id, true);
            }
        }
    }

    // Fallback: no result line found — return raw stdout.
    (text.to_string(), None, false)
}

#[async_trait]
impl Backend for ClaudeCodeBackend {
    fn name(&self) -> &str {
        "claude"
    }

    async fn start_session(&self, agent: &Agent) -> Result<Session> {
        let session_id = Uuid::new_v4().to_string();
        Ok(Session {
            id: session_id,
            agent_alias: agent.alias.clone(),
            backend: "claude".into(),
            started_at: Utc::now(),
            resume_session_id: None,
            stdout_tx: None,
        })
    }

    async fn trigger(
        &self,
        agent: &Agent,
        session: &Session,
        instruction: Option<&str>,
    ) -> Result<BackendOutput> {
        let instruction = instruction.unwrap_or("Check inbox and process pending tasks.");

        // Use the DB-persisted real Claude session ID when available so the
        // agent picks up its conversation history from the prior dispatch.
        let resume_id = session.resume_session_id.as_deref();

        let args = Self::build_args(agent, instruction, resume_id)?;
        let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();

        let timeout = agent
            .timeout_secs
            .map(Duration::from_secs)
            .unwrap_or(Duration::from_secs(300));

        let child = spawn_cli(
            "claude",
            &arg_refs,
            agent.env.as_ref(),
            self.workdir.as_deref(),
        )?;
        let pid = child.id();
        self.tracker.track(&session.id, pid);

        let output = wait_with_timeout(
            child,
            Some(timeout),
            agent.log_path.as_deref(),
            session.stdout_tx.clone(),
        );
        self.tracker.untrack(&session.id);

        match output {
            Ok(out) => {
                let raw_output = String::from_utf8_lossy(&out.stdout).to_string();
                let (result_text, real_session_id, found_result) =
                    extract_claude_stream_output(&out.stdout);

                // Consider the trigger successful if we got a valid result line
                // in the stream-json output, even if exit code was non-zero.
                // Claude Code can exit non-zero while still producing valid output.
                let success = out.status.success() || found_result;

                let parsed_intent = parse_intent_from_text(&result_text);

                Ok(BackendOutput {
                    success,
                    result_text,
                    parsed_intent,
                    session_id: real_session_id,
                    raw_output,
                })
            }
            Err(e) => Err(e),
        }
    }

    async fn ping(&self, agent: &Agent, timeout_secs: u64) -> PingResult {
        let start = std::time::Instant::now();
        let args = Self::build_ping_args(agent);
        let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        match spawn_cli(
            "claude",
            &arg_refs,
            agent.env.as_ref(),
            self.workdir.as_deref(),
        ) {
            Ok(child) => {
                let timeout = Duration::from_secs(timeout_secs);
                match wait_with_timeout(child, Some(timeout), None, None) {
                    Ok(out) => {
                        let latency_ms = start.elapsed().as_millis() as u64;
                        PingResult {
                            alive: out.status.success(),
                            latency_ms,
                            detail: if out.status.success() {
                                None
                            } else {
                                Some(format!("exit code {}", out.status.code().unwrap_or(-1)))
                            },
                        }
                    }
                    Err(e) => PingResult {
                        alive: false,
                        latency_ms: start.elapsed().as_millis() as u64,
                        detail: Some(e.to_string()),
                    },
                }
            }
            Err(e) => PingResult {
                alive: false,
                latency_ms: start.elapsed().as_millis() as u64,
                detail: Some(e.to_string()),
            },
        }
    }

    async fn session_status(&self, _agent: &Agent) -> Result<Option<SessionStatus>> {
        // No persistent session tracking for claude CLI — check last known PID
        Ok(None)
    }

    async fn kill_session(&self, _agent: &Agent, session: &Session, _reason: &str) -> Result<()> {
        if let Some(pid) = self.tracker.get_pid(&session.id) {
            kill_process(pid)?;
            self.tracker.untrack(&session.id);
        }
        Ok(())
    }
}

/// Maximum length for the `detail` field (raw JSON) stored per event.
const MAX_DETAIL_LEN: usize = 2048;

fn truncate_detail(s: &str) -> String {
    if s.len() <= MAX_DETAIL_LEN {
        s.to_string()
    } else {
        format!("{}…(truncated)", &s[..MAX_DETAIL_LEN])
    }
}

/// Parse a single Claude stream-json JSONL line into an `ExecutionEvent`.
///
/// Schemas observed from Claude Code CLI v1.0.x (`--output-format stream-json`).
/// Returns `None` for unrecognized or irrelevant event lines.
///
/// **Limitation**: When an `assistant` message contains multiple `tool_use`
/// blocks, only the *first* one is captured. Subsequent tool_use blocks in
/// the same message are dropped. This is acceptable because Claude Code
/// typically emits one tool_use per assistant message in stream-json mode.
pub fn parse_claude_stream_line(line: &str) -> Option<super::ExecutionEvent> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    let val: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    let event_type_str = val.get("type")?.as_str()?;

    let now_ms = chrono::Utc::now().timestamp_millis();

    match event_type_str {
        "assistant" => {
            // Look for tool_use blocks in message.content[].
            // Only the first tool_use is captured (see doc comment above).
            let content = val.pointer("/message/content")?;
            let items = content.as_array()?;
            for item in items {
                if item.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                    let name = item
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("unknown");
                    let summary = match name {
                        "Write" => {
                            let fp = item
                                .pointer("/input/file_path")
                                .and_then(|v| v.as_str())
                                .unwrap_or("?");
                            format!("Write to {}", fp)
                        }
                        "Bash" => {
                            let cmd = item
                                .pointer("/input/command")
                                .and_then(|v| v.as_str())
                                .unwrap_or("?");
                            let truncated: String = cmd.chars().take(60).collect();
                            format!("Bash: {}", truncated)
                        }
                        "Read" => {
                            let fp = item
                                .pointer("/input/file_path")
                                .and_then(|v| v.as_str())
                                .unwrap_or("?");
                            format!("Read {}", fp)
                        }
                        other => other.to_string(),
                    };
                    return Some(super::ExecutionEvent {
                        event_type: "tool_call".to_string(),
                        summary,
                        detail: Some(truncate_detail(trimmed)),
                        timestamp_ms: now_ms,
                        event_index: 0,
                    });
                }
            }
            None
        }
        "result" => Some(super::ExecutionEvent {
            event_type: "turn_complete".to_string(),
            summary: "completed".to_string(),
            detail: Some(truncate_detail(trimmed)),
            timestamp_ms: now_ms,
            event_index: 0,
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_agent() -> Agent {
        Agent {
            alias: "focused".into(),
            backend: "claude".into(),
            model: Some("sonnet".into()),
            prompt: Some("You are a test agent.".into()),
            prompt_file: None,
            timeout_secs: Some(60),
            backend_args: None,
            env: None,
            log_path: None,
        }
    }

    #[test]
    fn test_build_args_new_session() {
        let agent = test_agent();
        let args = ClaudeCodeBackend::build_args(&agent, "do something", None).unwrap();

        assert!(args.contains(&"-p".to_string()));
        assert!(args.contains(&"--dangerously-skip-permissions".to_string()));
        assert!(args.contains(&"--output-format".to_string()));
        assert!(args.contains(&"stream-json".to_string()));
        assert!(args.contains(&"--model".to_string()));
        assert!(args.contains(&"sonnet".to_string()));
        assert!(args.contains(&"--system-prompt".to_string()));
        assert!(args.contains(&"do something".to_string()));
        // No -r flag for new session
        assert!(!args.contains(&"-r".to_string()));
    }

    #[test]
    fn test_build_args_resume_session() {
        let agent = test_agent();
        let args =
            ClaudeCodeBackend::build_args(&agent, "continue work", Some("sess-123")).unwrap();

        assert!(args.contains(&"-r".to_string()));
        assert!(args.contains(&"sess-123".to_string()));
        // Resume uses --append-system-prompt
        assert!(args.contains(&"--append-system-prompt".to_string()));
        assert!(!args.contains(&"--system-prompt".to_string()));
    }

    #[test]
    fn test_build_args_output_format_is_stream_json() {
        let agent = test_agent();
        let args = ClaudeCodeBackend::build_args(&agent, "task", None).unwrap();
        // Find --output-format and verify next arg is stream-json
        let idx = args.iter().position(|a| a == "--output-format").unwrap();
        assert_eq!(args[idx + 1], "stream-json");
    }

    #[test]
    fn test_build_args_with_backend_args() {
        let mut agent = test_agent();
        agent.backend_args = Some(vec!["--verbose".into(), "--foo=bar".into()]);
        let args = ClaudeCodeBackend::build_args(&agent, "task", None).unwrap();
        assert!(args.contains(&"--verbose".to_string()));
        assert!(args.contains(&"--foo=bar".to_string()));
    }

    #[test]
    fn test_build_args_no_model_no_prompt() {
        let mut agent = test_agent();
        agent.model = None;
        agent.prompt = None;
        let args = ClaudeCodeBackend::build_args(&agent, "task", None).unwrap();

        assert!(!args.contains(&"--model".to_string()));
        assert!(!args.contains(&"--system-prompt".to_string()));
    }

    #[test]
    fn test_build_ping_args_uses_json_not_stream() {
        let agent = test_agent();
        let args = ClaudeCodeBackend::build_ping_args(&agent);
        let idx = args.iter().position(|a| a == "--output-format").unwrap();
        assert_eq!(
            args[idx + 1],
            "json",
            "ping should keep --output-format json"
        );
        assert!(args.contains(&"--model".to_string()));
        assert!(args.contains(&"sonnet".to_string()));
    }

    #[tokio::test]
    async fn test_start_session() {
        let backend = ClaudeCodeBackend::new();
        let agent = test_agent();
        let session = backend.start_session(&agent).await.unwrap();
        assert_eq!(session.agent_alias, "focused");
        assert_eq!(session.backend, "claude");
        assert!(!session.id.is_empty());
    }

    #[test]
    fn test_backend_name() {
        let backend = ClaudeCodeBackend::new();
        assert_eq!(backend.name(), "claude");
    }

    // -- extract_claude_stream_output tests --

    #[test]
    fn test_extract_stream_output_single_result_line() {
        let stdout = r#"{"type":"result","subtype":"success","cost_usd":0.003,"is_error":false,"duration_ms":5443,"duration_api_ms":3709,"num_turns":1,"result":"ok","session_id":"abc-123-def"}"#;
        let (text, sid, _found) = extract_claude_stream_output(stdout.as_bytes());
        assert_eq!(text, "ok");
        assert_eq!(sid, Some("abc-123-def".to_string()));
    }

    #[test]
    fn test_extract_stream_output_full_jsonl() {
        // Realistic stream-json output with multiple event lines
        let stdout = r#"{"type":"system","subtype":"init","session_id":"abc-123","tools":[],"model":"claude-sonnet-4-20250514"}
{"type":"assistant","message":{"id":"msg_01","type":"message","role":"assistant","content":[{"type":"text","text":"Working on it..."}]}}
{"type":"assistant","message":{"id":"msg_02","type":"message","role":"assistant","content":[{"type":"tool_use","id":"tu_01","name":"Write","input":{}}]}}
{"type":"result","subtype":"success","cost_usd":0.05,"is_error":false,"duration_ms":12000,"duration_api_ms":8000,"num_turns":3,"result":"Task completed successfully.","session_id":"abc-123"}"#;
        let (text, sid, _found) = extract_claude_stream_output(stdout.as_bytes());
        assert_eq!(text, "Task completed successfully.");
        assert_eq!(sid, Some("abc-123".to_string()));
    }

    #[test]
    fn test_extract_stream_output_no_result_line_fallback() {
        // No result line — falls back to raw stdout
        let stdout = "plain text error from claude\n";
        let (text, sid, _found) = extract_claude_stream_output(stdout.as_bytes());
        assert_eq!(text, "plain text error from claude\n");
        assert!(sid.is_none());
    }

    #[test]
    fn test_extract_stream_output_result_with_intent() {
        // Agent embeds JSON intent in the result field
        let stdout = r#"{"type":"system","subtype":"init","session_id":"s1"}
{"type":"result","subtype":"success","result":"{\"intent\":\"status-update\",\"to\":\"lead\",\"body\":\"Task done\"}","session_id":"s1"}"#;
        let (text, sid, _found) = extract_claude_stream_output(stdout.as_bytes());
        assert!(text.contains("status-update"));
        assert!(text.contains("Task done"));
        assert_eq!(sid, Some("s1".to_string()));
    }

    #[test]
    fn test_extract_stream_output_multiple_result_lines_uses_last() {
        // Edge case: multiple result lines — use the last one
        let stdout = r#"{"type":"result","subtype":"error","result":"first attempt failed","session_id":"s1"}
{"type":"result","subtype":"success","result":"final answer","session_id":"s2"}"#;
        let (text, sid, _found) = extract_claude_stream_output(stdout.as_bytes());
        assert_eq!(text, "final answer");
        assert_eq!(sid, Some("s2".to_string()));
    }

    #[test]
    fn test_extract_stream_output_result_without_session_id() {
        let stdout = r#"{"type":"result","subtype":"success","result":"done","cost_usd":0.01}"#;
        let (text, sid, _found) = extract_claude_stream_output(stdout.as_bytes());
        assert_eq!(text, "done");
        assert!(sid.is_none());
    }

    #[test]
    fn test_extract_stream_output_empty_result_field() {
        let stdout = r#"{"type":"result","subtype":"success","result":"","session_id":"s1"}"#;
        let (text, sid, _found) = extract_claude_stream_output(stdout.as_bytes());
        assert_eq!(text, "");
        assert_eq!(sid, Some("s1".to_string()));
    }

    #[test]
    fn test_extract_stream_output_empty_stdout() {
        let (text, sid, _found) = extract_claude_stream_output(b"");
        assert_eq!(text, "");
        assert!(sid.is_none());
    }

    // -- parse_claude_stream_line tests --
    // Schemas observed from Claude Code CLI v1.0.x (--output-format stream-json)

    #[test]
    fn test_parse_claude_stream_write_tool() {
        let line = r#"{"type":"assistant","message":{"id":"msg_01","type":"message","role":"assistant","content":[{"type":"tool_use","id":"tu_01","name":"Write","input":{"file_path":"src/events.rs","content":"// new file"}}]}}"#;
        let event = parse_claude_stream_line(line).expect("should parse Write tool_use");
        assert_eq!(event.event_type, "tool_call");
        assert_eq!(event.summary, "Write to src/events.rs");
        assert!(event.detail.is_some());
    }

    #[test]
    fn test_parse_claude_stream_bash_tool() {
        let line = r#"{"type":"assistant","message":{"id":"msg_02","type":"message","role":"assistant","content":[{"type":"tool_use","id":"tu_02","name":"Bash","input":{"command":"cargo test --lib"}}]}}"#;
        let event = parse_claude_stream_line(line).expect("should parse Bash tool_use");
        assert_eq!(event.event_type, "tool_call");
        assert_eq!(event.summary, "Bash: cargo test --lib");
    }

    #[test]
    fn test_parse_claude_stream_bash_tool_long_command() {
        let long_cmd = "a]".to_string() + &"x".repeat(100);
        let line = format!(
            r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","name":"Bash","input":{{"command":"{}"}}}}]}}}}"#,
            long_cmd
        );
        let event = parse_claude_stream_line(&line).expect("should parse");
        // Summary should be truncated to 60 chars of the command
        assert!(event.summary.starts_with("Bash: "));
        assert!(event.summary.len() <= "Bash: ".len() + 60);
    }

    #[test]
    fn test_parse_claude_stream_read_tool() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":"Cargo.toml"}}]}}"#;
        let event = parse_claude_stream_line(line).expect("should parse Read tool_use");
        assert_eq!(event.event_type, "tool_call");
        assert_eq!(event.summary, "Read Cargo.toml");
    }

    #[test]
    fn test_parse_claude_stream_other_tool() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Grep","input":{"pattern":"TODO"}}]}}"#;
        let event = parse_claude_stream_line(line).expect("should parse other tool_use");
        assert_eq!(event.event_type, "tool_call");
        assert_eq!(event.summary, "Grep");
    }

    #[test]
    fn test_parse_claude_stream_result() {
        let line = r#"{"type":"result","subtype":"success","cost_usd":0.003,"result":"Done.","session_id":"abc-123"}"#;
        let event = parse_claude_stream_line(line).expect("should parse result");
        assert_eq!(event.event_type, "turn_complete");
        assert_eq!(event.summary, "completed");
    }

    #[test]
    fn test_parse_claude_stream_system_init_returns_none() {
        let line = r#"{"type":"system","subtype":"init","session_id":"abc-123","tools":[],"model":"claude-sonnet-4-20250514"}"#;
        assert!(parse_claude_stream_line(line).is_none());
    }

    #[test]
    fn test_parse_claude_stream_text_content_returns_none() {
        // Assistant message with text content only (no tool_use) — not an actionable event
        let line = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Working on it..."}]}}"#;
        assert!(parse_claude_stream_line(line).is_none());
    }

    #[test]
    fn test_parse_claude_stream_empty_line() {
        assert!(parse_claude_stream_line("").is_none());
    }

    #[test]
    fn test_parse_claude_stream_garbage() {
        assert!(parse_claude_stream_line("not json at all").is_none());
    }

    #[test]
    fn test_parse_claude_stream_json_without_type() {
        assert!(parse_claude_stream_line(r#"{"foo":"bar"}"#).is_none());
    }
}
