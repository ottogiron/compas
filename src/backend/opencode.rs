use async_trait::async_trait;
use chrono::Utc;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;
use uuid::Uuid;

use super::process::{
    extract_output_text, kill_process, resolve_prompt, spawn_cli, wait_with_timeout, ProcessTracker,
};
use super::{parse_intent_from_text, Backend, BackendOutput, PingResult};
use crate::error::Result;
use crate::model::agent::Agent;
use crate::model::session::{Session, SessionStatus};

/// OpenCode CLI backend.
///
/// Uses `opencode run` for non-interactive sessions.
/// Key flags: `run`, `-m provider/model`, `--format json`, `--agent`
#[derive(Debug)]
pub struct OpenCodeBackend {
    tracker: ProcessTracker,
    workdir: Option<PathBuf>,
}

impl OpenCodeBackend {
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
        resume_session_id: Option<&str>,
    ) -> Result<Vec<String>> {
        let mut args = vec!["run".to_string()];

        // Resume an existing session when the DB provides one.
        if let Some(sid) = resume_session_id {
            args.push("-s".to_string());
            args.push(sid.to_string());
        }

        // Model
        if let Some(ref model) = agent.model {
            args.push("-m".to_string());
            args.push(model.clone());
        }

        // The --agent flag expects a named profile.
        // OpenCode CLI does not support --prompt; inline full prompts into instruction text.
        let prompt = resolve_prompt(agent.prompt.as_deref(), agent.prompt_file.as_deref())?;
        let mut instruction_text = instruction.to_string();
        if let Some(ref p) = prompt {
            if Self::looks_like_agent_name(p) {
                args.push("--agent".to_string());
                args.push(p.clone());
            } else {
                instruction_text = format!("System guidance:\n{}\n\nTask:\n{}", p, instruction);
            }
        }

        // JSON output
        args.push("--format".to_string());
        args.push("json".to_string());

        // Extra backend args from config
        if let Some(extra) = &agent.backend_args {
            args.extend(extra.iter().cloned());
        }

        // Instruction
        args.push(instruction_text);

        Ok(args)
    }

    fn looks_like_agent_name(value: &str) -> bool {
        !value.trim().is_empty() && !value.chars().any(char::is_whitespace)
    }

    fn build_ping_args(agent: &Agent) -> Vec<String> {
        let mut args = vec!["run".to_string()];
        if let Some(ref model) = agent.model {
            args.push("-m".to_string());
            args.push(model.clone());
        }
        args.push("--format".to_string());
        args.push("json".to_string());
        args.push("Reply with: ok".to_string());
        args
    }

    /// Extract the OpenCode session ID from JSONL output.
    ///
    /// Every event line from `opencode run --format json` contains a top-level
    /// `"sessionID"` field. We scan for the first occurrence.
    fn extract_session_id_from_output(stdout: &[u8]) -> Option<String> {
        let text = String::from_utf8_lossy(stdout);
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Ok(val) = serde_json::from_str::<Value>(line) {
                if let Some(sid) = val.get("sessionID").and_then(|v| v.as_str()) {
                    if !sid.is_empty() {
                        return Some(sid.to_string());
                    }
                }
            }
        }
        None
    }

    /// Best-effort cleanup: delete the OpenCode session created by a trigger/ping.
    ///
    /// Runs `opencode session delete <session_id>` as a fire-and-forget subprocess.
    /// Failures are logged but never propagated — session cleanup is non-critical.
    fn cleanup_session(session_id: &str, workdir: Option<&Path>) {
        let mut cmd = Command::new("opencode");
        cmd.args(["session", "delete", session_id])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        if let Some(dir) = workdir {
            cmd.current_dir(dir);
        }
        match cmd.output() {
            Ok(out) if out.status.success() => {
                tracing::debug!(
                    opencode_session = %session_id,
                    "cleaned up opencode session"
                );
            }
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                tracing::warn!(
                    opencode_session = %session_id,
                    exit_code = out.status.code().unwrap_or(-1),
                    stderr = %stderr.trim(),
                    "opencode session cleanup returned non-zero"
                );
            }
            Err(e) => {
                tracing::warn!(
                    opencode_session = %session_id,
                    error = %e,
                    "failed to spawn opencode session cleanup"
                );
            }
        }
    }
}

impl Default for OpenCodeBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Backend for OpenCodeBackend {
    fn name(&self) -> &str {
        "opencode"
    }

    async fn start_session(&self, agent: &Agent) -> Result<Session> {
        let session_id = Uuid::new_v4().to_string();
        Ok(Session {
            id: session_id,
            agent_alias: agent.alias.clone(),
            backend: "opencode".into(),
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

        let args = Self::build_args(agent, instruction, session.resume_session_id.as_deref())?;
        let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();

        let timeout = agent
            .timeout_secs
            .map(Duration::from_secs)
            .unwrap_or(Duration::from_secs(300));

        let child = spawn_cli(
            "opencode",
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
                let result_text = extract_output_text(&out);
                // Extract the OpenCode session ID to persist in the DB so future
                // dispatches to this thread+agent resume the same session.
                // We do NOT clean up dispatch sessions — they need to persist
                // for resumption. Only ping sessions (below) are cleaned up.
                let session_id = Self::extract_session_id_from_output(&out.stdout);
                let parsed_intent = parse_intent_from_text(&result_text);

                Ok(BackendOutput {
                    success: out.status.success(),
                    result_text,
                    parsed_intent,
                    session_id,
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
            "opencode",
            &arg_refs,
            agent.env.as_ref(),
            self.workdir.as_deref(),
        ) {
            Ok(child) => {
                let timeout = Duration::from_secs(timeout_secs);
                match wait_with_timeout(child, Some(timeout), None, None) {
                    Ok(out) => {
                        // Clean up the throwaway ping session.
                        if let Some(oc_sid) = Self::extract_session_id_from_output(&out.stdout) {
                            Self::cleanup_session(&oc_sid, self.workdir.as_deref());
                        }

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

/// Maximum length for the `detail` field (raw JSON) stored per event.
const MAX_DETAIL_LEN: usize = 2048;

fn truncate_detail(s: &str) -> String {
    if s.len() <= MAX_DETAIL_LEN {
        s.to_string()
    } else {
        format!("{}…(truncated)", &s[..MAX_DETAIL_LEN])
    }
}

/// Parse a single OpenCode CLI JSONL line into an `ExecutionEvent`.
///
/// Schemas observed from OpenCode CLI v0.2.x (`--format json` output).
/// Returns `None` for unrecognized or irrelevant event lines.
pub fn parse_opencode_stream_line(line: &str) -> Option<super::ExecutionEvent> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    let val: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    let event_type_str = val.get("type")?.as_str()?;

    let now_ms = chrono::Utc::now().timestamp_millis();

    match event_type_str {
        "tool_start" => {
            let name = val.get("name").and_then(|n| n.as_str()).unwrap_or("tool");
            Some(super::ExecutionEvent {
                event_type: "tool_call".to_string(),
                summary: name.to_string(),
                detail: Some(truncate_detail(trimmed)),
                timestamp_ms: now_ms,
                event_index: 0,
            })
        }
        "step_finish" => Some(super::ExecutionEvent {
            event_type: "turn_complete".to_string(),
            summary: "step finished".to_string(),
            detail: Some(truncate_detail(trimmed)),
            timestamp_ms: now_ms,
            event_index: 0,
        }),
        "text" => {
            let text = val.pointer("/part/text").and_then(|v| v.as_str())?;
            if text.len() <= 10 {
                return None; // skip tiny deltas
            }
            let truncated: String = text.chars().take(60).collect();
            Some(super::ExecutionEvent {
                event_type: "message".to_string(),
                summary: truncated,
                detail: Some(truncate_detail(trimmed)),
                timestamp_ms: now_ms,
                event_index: 0,
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Output};

    fn test_agent() -> Agent {
        Agent {
            alias: "chill".into(),
            backend: "opencode".into(),
            model: Some("zai-coding-plan/glm-5".into()),
            prompt: Some("chill-agent".into()),
            prompt_file: None,
            timeout_secs: Some(120),
            backend_args: None,
            env: None,
            log_path: None,
        }
    }

    #[test]
    fn test_build_args_new_session() {
        let agent = test_agent();
        let args = OpenCodeBackend::build_args(&agent, "do work", None).unwrap();

        assert_eq!(args[0], "run");
        assert!(args.contains(&"-m".to_string()));
        assert!(args.contains(&"zai-coding-plan/glm-5".to_string()));
        assert!(args.contains(&"--agent".to_string()));
        assert!(args.contains(&"--format".to_string()));
        assert!(args.contains(&"json".to_string()));
        assert!(args.contains(&"do work".to_string()));
        assert!(!args.contains(&"-s".to_string()));
    }

    #[test]
    fn test_build_args_resume_session() {
        let agent = test_agent();
        let args = OpenCodeBackend::build_args(&agent, "continue", Some("ses_abc123")).unwrap();

        assert!(args.contains(&"-s".to_string()));
        assert!(args.contains(&"ses_abc123".to_string()));
        assert!(args.contains(&"continue".to_string()));
    }

    #[test]
    fn test_build_args_no_resume_session() {
        let agent = test_agent();
        let args = OpenCodeBackend::build_args(&agent, "continue", None).unwrap();

        assert!(!args.contains(&"-s".to_string()));
        assert!(args.contains(&"continue".to_string()));
    }

    #[test]
    fn test_build_args_with_full_prompt_inlines_guidance() {
        let mut agent = test_agent();
        agent.prompt = Some("You are GLM5.\nFollow AGENTS.md.".into());
        let args = OpenCodeBackend::build_args(&agent, "do work", None).unwrap();

        assert!(!args.contains(&"--agent".to_string()));
        let last = args.last().expect("instruction should exist");
        assert!(last.contains("System guidance:"));
        assert!(last.contains("Follow AGENTS.md."));
        assert!(last.contains("Task:\ndo work"));
    }

    #[tokio::test]
    async fn test_start_session() {
        let backend = OpenCodeBackend::new();
        let agent = test_agent();
        let session = backend.start_session(&agent).await.unwrap();
        assert_eq!(session.agent_alias, "chill");
        assert_eq!(session.backend, "opencode");
    }

    #[test]
    fn test_backend_name() {
        let backend = OpenCodeBackend::new();
        assert_eq!(backend.name(), "opencode");
    }

    #[test]
    fn test_build_args_with_backend_args() {
        let mut agent = test_agent();
        agent.backend_args = Some(vec!["--sandbox".into(), "workspace-write".into()]);
        let args = OpenCodeBackend::build_args(&agent, "task", None).unwrap();
        assert!(args.contains(&"--sandbox".to_string()));
        assert!(args.contains(&"workspace-write".to_string()));
    }

    #[test]
    fn test_build_ping_args_includes_model() {
        let agent = test_agent();
        let args = OpenCodeBackend::build_ping_args(&agent);
        assert!(args.contains(&"-m".to_string()));
        assert!(args.contains(&"zai-coding-plan/glm-5".to_string()));
    }

    fn test_output(stdout: &str, stderr: &str) -> Output {
        Output {
            status: Command::new("true").status().unwrap(),
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    #[test]
    fn test_extract_output_text_json_result() {
        let out = test_output(r#"{"result":"ok-from-json"}"#, "");
        let text = extract_output_text(&out);
        assert_eq!(text, "ok-from-json");
    }

    #[test]
    fn test_extract_output_text_jsonl_events() {
        let out = test_output(
            r#"{"type":"thread.started"}
{"type":"item.completed","item":{"text":"ok-from-jsonl"}}"#,
            "",
        );
        let text = extract_output_text(&out);
        assert_eq!(text, "ok-from-jsonl");
    }

    #[test]
    fn test_extract_output_text_content_delta_stream() {
        // Real OpenCode JSONL format with content.delta events
        let out = test_output(
            r#"{"type":"message.start","message":{"id":"msg_1","model":"claude","role":"assistant"}}
{"type":"content.start","index":0,"part":{"type":"text","text":""}}
{"type":"content.delta","index":0,"part":{"type":"text","text":"Here is "}}
{"type":"content.delta","index":0,"part":{"type":"text","text":"the response."}}
{"type":"content.stop","index":0}
{"type":"message.stop","message":{"id":"msg_1","usage":{"input_tokens":10,"output_tokens":5}}}"#,
            "",
        );
        let text = extract_output_text(&out);
        assert_eq!(text, "Here is the response.");
    }

    #[test]
    fn test_extract_output_text_delta_with_json_reply() {
        // Agent outputs JSON auto-reply via content.delta events
        let out = test_output(
            r#"{"type":"message.start","message":{"id":"msg_1","role":"assistant"}}
{"type":"content.delta","index":0,"part":{"type":"text","text":"{\"intent\":\"completion\",\"to\":\"operator\",\"body\":\"Done\"}"}}"#,
            "",
        );
        let text = extract_output_text(&out);
        assert_eq!(
            text,
            r#"{"intent":"completion","to":"operator","body":"Done"}"#
        );
    }

    #[test]
    fn test_extract_output_text_stdout_raw_fallback() {
        let out = test_output("plain stdout text", "");
        let text = extract_output_text(&out);
        assert_eq!(text, "plain stdout text");
    }

    #[test]
    fn test_extract_output_text_stderr_fallback_when_stdout_empty() {
        let out = test_output("   \n", "stderr diagnostic");
        let text = extract_output_text(&out);
        assert_eq!(text, "stderr diagnostic");
    }

    #[test]
    fn test_extract_output_text_all_empty() {
        let out = test_output(" \n\t", " \n");
        let text = extract_output_text(&out);
        assert!(text.is_empty());
    }

    // -- extract_session_id_from_output tests --

    #[test]
    fn test_extract_session_id_from_real_jsonl() {
        let stdout = br#"{"type":"step_start","timestamp":1771873095732,"sessionID":"ses_374224a2cffeGrPrAe417w2LKA","part":{"id":"prt_1"}}
{"type":"text","timestamp":1771873095788,"sessionID":"ses_374224a2cffeGrPrAe417w2LKA","part":{"id":"prt_2","text":"pong"}}
{"type":"step_finish","timestamp":1771873095880,"sessionID":"ses_374224a2cffeGrPrAe417w2LKA","part":{"id":"prt_3"}}"#;
        assert_eq!(
            OpenCodeBackend::extract_session_id_from_output(stdout),
            Some("ses_374224a2cffeGrPrAe417w2LKA".to_string())
        );
    }

    #[test]
    fn test_extract_session_id_single_event() {
        let stdout = br#"{"type":"step_start","sessionID":"ses_abc123"}"#;
        assert_eq!(
            OpenCodeBackend::extract_session_id_from_output(stdout),
            Some("ses_abc123".to_string())
        );
    }

    #[test]
    fn test_extract_session_id_no_session_field() {
        let stdout = br#"{"type":"step_start","timestamp":123}
{"type":"text","part":{"text":"hello"}}"#;
        assert_eq!(
            OpenCodeBackend::extract_session_id_from_output(stdout),
            None
        );
    }

    #[test]
    fn test_extract_session_id_empty_output() {
        assert_eq!(OpenCodeBackend::extract_session_id_from_output(b""), None);
    }

    #[test]
    fn test_extract_session_id_non_json_output() {
        assert_eq!(
            OpenCodeBackend::extract_session_id_from_output(b"plain text output"),
            None
        );
    }

    #[test]
    fn test_extract_session_id_empty_string_skipped() {
        let stdout = br#"{"type":"event","sessionID":""}"#;
        assert_eq!(
            OpenCodeBackend::extract_session_id_from_output(stdout),
            None
        );
    }

    #[test]
    fn test_extract_session_id_mixed_lines() {
        // Non-JSON lines interspersed (e.g. from stderr bleeding into stdout)
        let stdout =
            b"some garbage\n{\"type\":\"start\",\"sessionID\":\"ses_found\"}\nmore garbage";
        assert_eq!(
            OpenCodeBackend::extract_session_id_from_output(stdout),
            Some("ses_found".to_string())
        );
    }

    // -- parse_opencode_stream_line tests --
    // Schemas observed from OpenCode CLI v0.2.x (--format json output)

    #[test]
    fn test_parse_opencode_tool_start() {
        let line = r#"{"type":"tool_start","timestamp":1771873095732,"sessionID":"ses_abc","name":"bash"}"#;
        let event = parse_opencode_stream_line(line).expect("should parse tool_start");
        assert_eq!(event.event_type, "tool_call");
        assert_eq!(event.summary, "bash");
    }

    #[test]
    fn test_parse_opencode_tool_start_no_name() {
        let line = r#"{"type":"tool_start","timestamp":123}"#;
        let event = parse_opencode_stream_line(line).expect("should parse");
        assert_eq!(event.event_type, "tool_call");
        assert_eq!(event.summary, "tool");
    }

    #[test]
    fn test_parse_opencode_step_finish() {
        let line = r#"{"type":"step_finish","timestamp":1771873095880,"sessionID":"ses_abc","part":{"id":"prt_3"}}"#;
        let event = parse_opencode_stream_line(line).expect("should parse step_finish");
        assert_eq!(event.event_type, "turn_complete");
        assert_eq!(event.summary, "step finished");
    }

    #[test]
    fn test_parse_opencode_text_long() {
        let line = r#"{"type":"text","timestamp":123,"sessionID":"ses_abc","part":{"id":"prt_2","text":"Here is a longer response from the agent that exceeds ten characters."}}"#;
        let event = parse_opencode_stream_line(line).expect("should parse text");
        assert_eq!(event.event_type, "message");
        assert_eq!(event.summary.len(), 60); // truncated
    }

    #[test]
    fn test_parse_opencode_text_short_skipped() {
        // Text with 10 or fewer chars should be skipped (tiny delta)
        let line = r#"{"type":"text","timestamp":123,"part":{"text":"ok"}}"#;
        assert!(parse_opencode_stream_line(line).is_none());
    }

    #[test]
    fn test_parse_opencode_text_no_part() {
        let line = r#"{"type":"text","timestamp":123}"#;
        assert!(parse_opencode_stream_line(line).is_none());
    }

    #[test]
    fn test_parse_opencode_step_start_returns_none() {
        // step_start is not an event type we recognize — only tool_start
        let line = r#"{"type":"step_start","timestamp":123,"sessionID":"ses_abc"}"#;
        assert!(parse_opencode_stream_line(line).is_none());
    }

    #[test]
    fn test_parse_opencode_empty_and_garbage() {
        assert!(parse_opencode_stream_line("").is_none());
        assert!(parse_opencode_stream_line("not json").is_none());
        assert!(parse_opencode_stream_line(r#"{"no_type":true}"#).is_none());
    }
}
