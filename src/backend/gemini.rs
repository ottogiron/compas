use async_trait::async_trait;
use chrono::Utc;
use std::path::PathBuf;
use std::time::Duration;
use uuid::Uuid;

use super::process::{
    kill_process, parse_json_output, resolve_prompt, spawn_cli, wait_with_timeout, ProcessTracker,
};
use super::{classify_error, truncate_detail, Backend, BackendOutput, PingResult};
use crate::error::Result;
use crate::model::agent::Agent;
use crate::model::session::{Session, SessionStatus};

/// Gemini CLI backend.
///
/// Uses `gemini` for non-interactive sessions.
/// Key flags: `--model`, `--output-format json`, `-p <prompt>`, `--yolo`
#[derive(Debug)]
pub struct GeminiBackend {
    tracker: ProcessTracker,
    workdir: Option<PathBuf>,
}

impl GeminiBackend {
    pub fn new() -> Self {
        Self::with_workdir(None)
    }

    pub fn with_workdir(workdir: Option<PathBuf>) -> Self {
        Self {
            tracker: ProcessTracker::new(),
            workdir,
        }
    }

    fn build_args(agent: &Agent, instruction: &str) -> Result<Vec<String>> {
        let mut args = Vec::new();

        // Model
        if let Some(ref model) = agent.model {
            args.push("--model".to_string());
            args.push(model.clone());
        }

        // Format
        args.push("--output-format".to_string());
        args.push("json".to_string());

        // YOLO mode (automatic tool flow)
        args.push("--yolo".to_string());

        // Disable MCP servers — prevents the CLI from hanging in non-interactive mode
        // when MCP connections (e.g. context7) keep the process alive after completion.
        args.push("--allowed-mcp-server-names".to_string());
        args.push("".to_string());

        // Construct full prompt (System + User)
        let prompt_content = resolve_prompt(agent.prompt.as_deref(), agent.prompt_file.as_deref())?;
        let mut final_instruction = instruction.to_string();
        if let Some(ref p) = prompt_content {
            // CLI doesn't support --system, so we prepend it.
            final_instruction = format!("System: {}\n\nUser: {}", p, instruction);
        }

        // Extra backend args from config (e.g. --sandbox)
        if let Some(extra) = &agent.backend_args {
            args.extend(extra.iter().cloned());
        }

        // Instruction via -p
        args.push("--prompt".to_string());
        args.push(final_instruction);

        Ok(args)
    }

    fn build_ping_args(agent: &Agent) -> Vec<String> {
        let mut args = Vec::new();
        if let Some(ref model) = agent.model {
            args.push("--model".to_string());
            args.push(model.clone());
        }
        args.push("--output-format".to_string());
        args.push("json".to_string());
        args.push("--prompt".to_string());
        args.push("Reply with: ok".to_string());
        args
    }
}

impl Default for GeminiBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Backend for GeminiBackend {
    fn name(&self) -> &str {
        "gemini"
    }

    async fn start_session(&self, agent: &Agent) -> Result<Session> {
        let session_id = Uuid::new_v4().to_string();
        Ok(Session {
            id: session_id,
            agent_alias: agent.alias.clone(),
            backend: "gemini".into(),
            started_at: Utc::now(),
            resume_session_id: None,
            stdout_tx: None,
            pid_tx: None,
        })
    }

    async fn trigger(
        &self,
        agent: &Agent,
        session: &Session,
        instruction: Option<&str>,
    ) -> Result<BackendOutput> {
        let instruction = instruction.unwrap_or("Check inbox and process pending tasks.");

        let args = Self::build_args(agent, instruction)?;
        let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();

        let timeout = agent
            .timeout_secs
            .map(Duration::from_secs)
            .unwrap_or(Duration::from_secs(300));

        let workdir = agent
            .execution_workdir
            .as_deref()
            .or(self.workdir.as_deref());
        let child = spawn_cli("gemini", &arg_refs, agent.env.as_ref(), workdir)?;
        let pid = child.id();
        if let Some(ref tx) = session.pid_tx {
            let _ = tx.send(pid);
        }
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
                let json = parse_json_output(&out);
                let (result_text, real_session_id) = match &json {
                    Ok(val) => {
                        // Check "result" first, then "response"
                        let text = val
                            .get("result")
                            .or_else(|| val.get("response"))
                            .and_then(|r| r.as_str())
                            .unwrap_or_else(|| {
                                std::str::from_utf8(&out.stdout).unwrap_or("(binary output)")
                            })
                            .to_string();
                        let sid = val
                            .get("session_id")
                            .and_then(|s| s.as_str())
                            .map(|s| s.to_string());
                        (text, sid)
                    }
                    Err(_) => (raw_output.clone(), None),
                };

                // Store real session ID for future resume (if we supported stateful resume)
                if let Some(ref real_sid) = real_session_id {
                    self.tracker.set_real_session_id(&session.id, real_sid);
                }

                let parsed_intent = None;
                let success = out.status.success();

                let error_category = if !success {
                    let has_result_output = json.is_ok();
                    Some(classify_error(false, has_result_output, &result_text))
                } else {
                    None
                };

                // Extract token counts from Gemini JSON output.
                let (gemini_tokens_in, gemini_tokens_out) = match &json {
                    Ok(val) => extract_gemini_token_counts(val),
                    Err(_) => (None, None),
                };

                Ok(BackendOutput {
                    success,
                    result_text,
                    parsed_intent,
                    session_id: real_session_id,
                    raw_output,
                    error_category,
                    pid: Some(pid),
                    cost_usd: None, // Gemini does not report cost
                    tokens_in: gemini_tokens_in,
                    tokens_out: gemini_tokens_out,
                    num_turns: None,
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
            "gemini",
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

/// Extract token counts from Gemini JSON output.
///
/// Gemini CLI can emit stats in a `result.stats` block (stream format) or as
/// top-level `input_tokens`/`output_tokens` (simple JSON format). We check both.
fn extract_gemini_token_counts(val: &serde_json::Value) -> (Option<i64>, Option<i64>) {
    // Stream format: {"type":"result","stats":{"input_tokens":N,"output_tokens":N,...}}
    if let Some(stats) = val.get("stats") {
        let inp = stats.get("input_tokens").and_then(|v| v.as_i64());
        let out = stats.get("output_tokens").and_then(|v| v.as_i64());
        if inp.is_some() || out.is_some() {
            return (inp, out);
        }
    }
    // Simple format: top-level fields
    let inp = val.get("input_tokens").and_then(|v| v.as_i64());
    let out = val.get("output_tokens").and_then(|v| v.as_i64());
    if inp.is_some() || out.is_some() {
        return (inp, out);
    }
    (None, None)
}

/// Parse a single Gemini CLI JSONL line into an `ExecutionEvent`.
///
/// Schemas observed from Gemini CLI (--output-format json output).
/// Returns `None` for unrecognized or irrelevant event lines.
pub fn parse_gemini_stream_line(line: &str) -> Option<super::ExecutionEvent> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    let val: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    let event_type_str = val.get("type")?.as_str()?;

    let now_ms = chrono::Utc::now().timestamp_millis();

    match event_type_str {
        "message" => {
            let role = val.get("role").and_then(|r| r.as_str()).unwrap_or("");
            if role == "assistant" {
                let content = val.get("content").and_then(|c| c.as_str()).unwrap_or("");
                let truncated: String = content.chars().take(60).collect();
                if truncated.is_empty() {
                    return None;
                }
                Some(super::ExecutionEvent {
                    event_type: "message".to_string(),
                    summary: truncated,
                    detail: Some(truncate_detail(trimmed)),
                    timestamp_ms: now_ms,
                    event_index: 0,
                    tool_name: None,
                })
            } else {
                None
            }
        }
        "result" => Some(super::ExecutionEvent {
            event_type: "turn_complete".to_string(),
            summary: "completed".to_string(),
            detail: Some(truncate_detail(trimmed)),
            timestamp_ms: now_ms,
            event_index: 0,
            tool_name: None,
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::super::process::parse_json_output;
    use super::*;
    use std::process::{Command, Output};

    fn test_agent() -> Agent {
        Agent {
            alias: "spark".into(),
            backend: "gemini".into(),
            model: Some("gemini-1.5-pro".into()),
            prompt: Some("You are a test agent.".into()),
            prompt_file: None,
            timeout_secs: Some(60),
            backend_args: None,
            env: None,
            log_path: None,
            execution_workdir: None,
        }
    }

    #[test]
    fn test_build_args() {
        let agent = test_agent();
        let args = GeminiBackend::build_args(&agent, "do something").unwrap();

        assert!(!args.contains(&"run".to_string()));
        assert!(args.contains(&"--model".to_string()));
        assert!(args.contains(&"gemini-1.5-pro".to_string()));
        assert!(args.contains(&"--output-format".to_string()));
        assert!(args.contains(&"json".to_string()));
        assert!(args.contains(&"--yolo".to_string()));

        let prompt_arg = args.last().unwrap();
        assert!(prompt_arg.contains("System: You are a test agent."));
        assert!(prompt_arg.contains("User: do something"));
        assert!(args[args.len() - 2] == "--prompt");
    }

    #[test]
    fn test_build_args_with_backend_args() {
        let mut agent = test_agent();
        agent.backend_args = Some(vec!["--verbose".into()]);
        let args = GeminiBackend::build_args(&agent, "task").unwrap();
        assert!(args.contains(&"--verbose".to_string()));
    }

    #[test]
    fn test_build_ping_args_includes_model() {
        let agent = test_agent();
        let args = GeminiBackend::build_ping_args(&agent);
        assert!(args.contains(&"--model".to_string()));
        assert!(args.contains(&"gemini-1.5-pro".to_string()));
    }

    #[tokio::test]
    async fn test_start_session() {
        let backend = GeminiBackend::new();
        let agent = test_agent();
        let session = backend.start_session(&agent).await.unwrap();
        assert_eq!(session.agent_alias, "spark");
        assert_eq!(session.backend, "gemini");
        assert!(!session.id.is_empty());
    }

    #[test]
    fn test_backend_name() {
        let backend = GeminiBackend::new();
        assert_eq!(backend.name(), "gemini");
    }

    fn test_output(stdout: &str) -> Output {
        Output {
            status: Command::new("true").status().unwrap(),
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
        }
    }

    #[test]
    fn test_parse_gemini_response_field() {
        // Real Gemini CLI uses "response" not "result"
        let out = test_output(r#"{"response":"ok"}"#);
        let val = parse_json_output(&out).unwrap();
        assert!(val.get("result").is_none());
        assert_eq!(val["response"], "ok");
    }

    #[test]
    fn test_parse_gemini_json_auto_reply() {
        // Agent embeds JSON auto-reply in the response field
        let out = test_output(
            r#"{"response":"{\"intent\":\"completion\",\"to\":\"operator\",\"body\":\"Done\"}"}"#,
        );
        let val = parse_json_output(&out).unwrap();
        let response_str = val["response"].as_str().unwrap();
        assert!(response_str.contains("completion"));
        assert!(response_str.contains("Done"));
    }

    #[test]
    fn test_gemini_trigger_extracts_response_field() {
        // Verify that the trigger extraction logic picks up .response
        let out = test_output(r#"{"response":"hello from gemini"}"#);
        let val = parse_json_output(&out).unwrap();
        let text = val
            .get("result")
            .or_else(|| val.get("response"))
            .and_then(|r| r.as_str())
            .unwrap();
        assert_eq!(text, "hello from gemini");
    }

    // -- extract_gemini_token_counts tests --

    #[test]
    fn test_extract_gemini_token_counts_stats_block() {
        let val: serde_json::Value = serde_json::from_str(
            r#"{"type":"result","status":"success","stats":{"total_tokens":7066,"input_tokens":6989,"output_tokens":9,"cached":0}}"#,
        ).unwrap();
        let (inp, out) = extract_gemini_token_counts(&val);
        assert_eq!(inp, Some(6989));
        assert_eq!(out, Some(9));
    }

    #[test]
    fn test_extract_gemini_token_counts_no_stats() {
        let val: serde_json::Value = serde_json::from_str(r#"{"response":"ok"}"#).unwrap();
        let (inp, out) = extract_gemini_token_counts(&val);
        assert!(inp.is_none());
        assert!(out.is_none());
    }

    // -- parse_gemini_stream_line tests --

    #[test]
    fn test_parse_gemini_stream_assistant_message() {
        let line =
            r#"{"type":"message","role":"assistant","content":"Hello from Gemini","delta":true}"#;
        let event = parse_gemini_stream_line(line).expect("should parse assistant message");
        assert_eq!(event.event_type, "message");
        assert!(event.summary.contains("Hello from Gemini"));
        assert!(event.tool_name.is_none());
    }

    #[test]
    fn test_parse_gemini_stream_result() {
        let line = r#"{"type":"result","status":"success","stats":{"total_tokens":100,"input_tokens":80,"output_tokens":20}}"#;
        let event = parse_gemini_stream_line(line).expect("should parse result");
        assert_eq!(event.event_type, "turn_complete");
        assert_eq!(event.summary, "completed");
    }

    #[test]
    fn test_parse_gemini_stream_user_message_returns_none() {
        let line = r#"{"type":"message","role":"user","content":"some prompt"}"#;
        assert!(parse_gemini_stream_line(line).is_none());
    }

    #[test]
    fn test_parse_gemini_stream_empty_content_returns_none() {
        let line = r#"{"type":"message","role":"assistant","content":""}"#;
        assert!(parse_gemini_stream_line(line).is_none());
    }

    #[test]
    fn test_parse_gemini_stream_init_returns_none() {
        let line = r#"{"type":"init","session_id":"abc","model":"gemini-3-pro"}"#;
        assert!(parse_gemini_stream_line(line).is_none());
    }

    #[test]
    fn test_parse_gemini_stream_empty_and_garbage() {
        assert!(parse_gemini_stream_line("").is_none());
        assert!(parse_gemini_stream_line("not json").is_none());
        assert!(parse_gemini_stream_line(r#"{"no_type":true}"#).is_none());
    }
}
