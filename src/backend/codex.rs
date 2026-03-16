use async_trait::async_trait;
use chrono::Utc;
use std::path::PathBuf;
use std::time::Duration;
use uuid::Uuid;

use super::process::{
    extract_output_text, kill_process, spawn_cli, wait_with_timeout, ProcessTracker,
};
use super::{classify_error, parse_intent_from_text, Backend, BackendOutput, PingResult};
use crate::error::Result;
use crate::model::agent::Agent;
use crate::model::session::{Session, SessionStatus};
use serde_json::Value;

/// Codex CLI backend.
///
/// Uses `codex exec` for non-interactive sessions.
/// Key flags: `exec`, `-m model`, `--full-auto`, `--json`, `-C dir`
#[derive(Debug)]
pub struct CodexBackend {
    tracker: ProcessTracker,
    workdir: Option<PathBuf>,
}

impl CodexBackend {
    pub fn new(workdir: Option<PathBuf>) -> Self {
        Self {
            tracker: ProcessTracker::new(),
            workdir,
        }
    }

    fn build_args(
        agent: &Agent,
        instruction: &str,
        resume_session_id: Option<&str>,
        workdir: Option<&PathBuf>,
    ) -> Vec<String> {
        let mut args = vec!["exec".to_string()];

        if let Some(thread_id) = resume_session_id {
            args.push("resume".to_string());
            args.push(thread_id.to_string());
        }

        // Model
        if let Some(ref model) = agent.model {
            args.push("-m".to_string());
            args.push(model.clone());
        }

        // Working directory
        if let Some(dir) = workdir {
            args.push("-C".to_string());
            args.push(dir.to_string_lossy().to_string());
        }

        // Full auto mode.
        // `--full-auto` is mutually exclusive with `--dangerously-bypass-approvals-and-sandbox`:
        // when the bypass flag is present (agents that need unrestricted filesystem/network
        // access, e.g. to run `./gradlew check`), skip `--full-auto` so the Codex CLI does
        // not error out on conflicting flags.
        let has_sandbox_bypass = agent
            .backend_args
            .as_ref()
            .map(|args| {
                args.iter()
                    .any(|a| a == "--dangerously-bypass-approvals-and-sandbox")
            })
            .unwrap_or(false);

        if !has_sandbox_bypass {
            args.push("--full-auto".to_string());
        }

        // JSON output
        args.push("--json".to_string());

        // Extra backend args from config
        if let Some(extra) = &agent.backend_args {
            args.extend(extra.iter().cloned());
        }

        // Instruction
        args.push(instruction.to_string());

        args
    }

    /// Extract the Codex thread ID from JSONL output.
    ///
    /// Codex emits a `{"type":"thread.started","thread_id":"..."}` event on
    /// the first line of every session. We use this as the backend session ID
    /// so future dispatches can resume via `codex exec resume <thread_id>`.
    fn extract_thread_id_from_output(stdout: &[u8]) -> Option<String> {
        let text = String::from_utf8_lossy(stdout);
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Ok(val) = serde_json::from_str::<Value>(line) {
                if val.get("type").and_then(|t| t.as_str()) == Some("thread.started") {
                    if let Some(tid) = val.get("thread_id").and_then(|v| v.as_str()) {
                        if !tid.is_empty() {
                            return Some(tid.to_string());
                        }
                    }
                }
            }
        }
        None
    }

    fn build_ping_args(agent: &Agent) -> Vec<String> {
        let mut args = vec!["exec".to_string()];
        if let Some(ref model) = agent.model {
            args.push("-m".to_string());
            args.push(model.clone());
        }
        // Ping always uses `--full-auto`; it is a lightweight health-check that does
        // not need filesystem/network bypass, so no conflict can arise here.
        args.push("--full-auto".to_string());
        args.push("--json".to_string());
        // Forward `--skip-git-repo-check` when the agent has it in `backend_args`.
        // Without this, the ping fails with exit code 1 when `target_repo_root` is
        // not a git repository, causing all such agents to appear unhealthy.
        let has_skip_git = agent
            .backend_args
            .as_ref()
            .map(|a| a.iter().any(|s| s == "--skip-git-repo-check"))
            .unwrap_or(false);
        if has_skip_git {
            args.push("--skip-git-repo-check".to_string());
        }
        args.push("Reply with: ok".to_string());
        args
    }

    fn effective_workdir<'a>(&'a self, agent: &'a Agent) -> Option<&'a std::path::Path> {
        agent
            .execution_workdir
            .as_deref()
            .or(self.workdir.as_deref())
    }
}

impl Default for CodexBackend {
    fn default() -> Self {
        Self::new(None)
    }
}

#[async_trait]
impl Backend for CodexBackend {
    fn name(&self) -> &str {
        "codex"
    }

    async fn start_session(&self, agent: &Agent) -> Result<Session> {
        let session_id = Uuid::new_v4().to_string();
        Ok(Session {
            id: session_id,
            agent_alias: agent.alias.clone(),
            backend: "codex".into(),
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

        // Resume the prior Codex session when the DB provided a thread_id from
        // a previous completed execution for this thread+agent.
        let effective_workdir = self.effective_workdir(agent).map(|p| p.to_path_buf());
        let args = Self::build_args(
            agent,
            instruction,
            session.resume_session_id.as_deref(),
            effective_workdir.as_ref(),
        );
        let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();

        let timeout = agent
            .timeout_secs
            .map(Duration::from_secs)
            .unwrap_or(Duration::from_secs(300));

        let child = spawn_cli(
            "codex",
            &arg_refs,
            agent.env.as_ref(),
            effective_workdir.as_deref(),
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
                // Use the Codex thread_id as the session ID so the next
                // dispatch for this thread+agent can resume via
                // `codex exec resume <thread_id>`.
                let session_id = Self::extract_thread_id_from_output(&out.stdout);
                let parsed_intent = parse_intent_from_text(&result_text);
                let success = out.status.success();

                let error_category = if !success {
                    Some(classify_error(false, !result_text.is_empty(), &result_text))
                } else {
                    None
                };

                Ok(BackendOutput {
                    success,
                    result_text,
                    parsed_intent,
                    session_id,
                    raw_output,
                    error_category,
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
            "codex",
            &arg_refs,
            agent.env.as_ref(),
            self.effective_workdir(agent),
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

/// Maximum length for the `detail` field (raw JSON) stored per event.
const MAX_DETAIL_LEN: usize = 2048;

fn truncate_detail(s: &str) -> String {
    if s.len() <= MAX_DETAIL_LEN {
        s.to_string()
    } else {
        format!("{}…(truncated)", &s[..MAX_DETAIL_LEN])
    }
}

/// Parse a single Codex CLI JSONL line into an `ExecutionEvent`.
///
/// Schemas observed from Codex CLI v0.1.x (`--json` output).
/// Returns `None` for unrecognized or irrelevant event lines.
pub fn parse_codex_stream_line(line: &str) -> Option<super::ExecutionEvent> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    let val: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    let event_type_str = val.get("type")?.as_str()?;

    let now_ms = chrono::Utc::now().timestamp_millis();

    match event_type_str {
        "item.completed" => {
            let item = val.get("item")?;
            let item_type = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
            match item_type {
                "function_call" => {
                    let name = item
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("function_call");
                    Some(super::ExecutionEvent {
                        event_type: "tool_call".to_string(),
                        summary: name.to_string(),
                        detail: Some(truncate_detail(trimmed)),
                        timestamp_ms: now_ms,
                        event_index: 0,
                    })
                }
                "agent_message" => {
                    let text = item.get("text").and_then(|t| t.as_str()).unwrap_or("");
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
        "turn.completed" => Some(super::ExecutionEvent {
            event_type: "turn_complete".to_string(),
            summary: "turn completed".to_string(),
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
    use std::process::{Command, Output};

    fn test_agent() -> Agent {
        Agent {
            alias: "spark".into(),
            backend: "codex".into(),
            model: Some("gpt-5.3-codex".into()),
            prompt: None,
            prompt_file: None,
            timeout_secs: Some(180),
            backend_args: None,
            env: None,
            log_path: None,
            execution_workdir: None,
        }
    }

    #[test]
    fn test_build_args_new_session() {
        let agent = test_agent();
        let workdir = PathBuf::from("/home/user/project");
        let args = CodexBackend::build_args(&agent, "implement X", None, Some(&workdir));

        assert_eq!(args[0], "exec");
        assert!(args.contains(&"-m".to_string()));
        assert!(args.contains(&"gpt-5.3-codex".to_string()));
        assert!(args.contains(&"-C".to_string()));
        assert!(args.contains(&"/home/user/project".to_string()));
        assert!(args.contains(&"--full-auto".to_string()));
        assert!(args.contains(&"--json".to_string()));
        assert!(args.contains(&"implement X".to_string()));
    }

    #[test]
    fn test_build_args_resume() {
        let agent = test_agent();
        let args = CodexBackend::build_args(&agent, "continue", Some("thread-abc-123"), None);

        assert_eq!(args[0], "exec");
        assert_eq!(args[1], "resume");
        assert_eq!(args[2], "thread-abc-123");
    }

    #[test]
    fn test_build_args_no_workdir() {
        let agent = test_agent();
        let args = CodexBackend::build_args(&agent, "task", None, None);

        assert!(!args.contains(&"-C".to_string()));
    }

    #[test]
    fn test_build_args_with_backend_args() {
        let mut agent = test_agent();
        agent.backend_args = Some(vec!["--sandbox".into(), "workspace-write".into()]);
        let args = CodexBackend::build_args(&agent, "task", None, None);
        assert!(args.contains(&"--sandbox".to_string()));
        assert!(args.contains(&"workspace-write".to_string()));
    }

    /// When `--dangerously-bypass-approvals-and-sandbox` is in `backend_args`,
    /// `--full-auto` must NOT be emitted (the two flags are mutually exclusive in
    /// the Codex CLI).
    #[test]
    fn test_build_args_with_bypass_skips_full_auto() {
        let mut agent = test_agent();
        agent.backend_args = Some(vec!["--dangerously-bypass-approvals-and-sandbox".into()]);
        let args = CodexBackend::build_args(&agent, "task", None, None);
        assert!(
            !args.contains(&"--full-auto".to_string()),
            "--full-auto should be absent when bypass flag is set; got: {:?}",
            args
        );
        assert!(
            args.contains(&"--dangerously-bypass-approvals-and-sandbox".to_string()),
            "bypass flag should be present; got: {:?}",
            args
        );
    }

    /// Without the bypass flag, `--full-auto` must still be emitted (existing
    /// behaviour preserved).
    #[test]
    fn test_build_args_without_bypass_includes_full_auto() {
        let agent = test_agent();
        let args = CodexBackend::build_args(&agent, "task", None, None);
        assert!(
            args.contains(&"--full-auto".to_string()),
            "--full-auto should be present when bypass flag is absent; got: {:?}",
            args
        );
    }

    #[test]
    fn test_build_ping_args_includes_model() {
        let agent = test_agent();
        let args = CodexBackend::build_ping_args(&agent);
        assert!(args.contains(&"-m".to_string()));
        assert!(args.contains(&"gpt-5.3-codex".to_string()));
    }

    #[test]
    fn test_effective_workdir_prefers_execution_workdir() {
        let mut agent = test_agent();
        agent.execution_workdir = Some(PathBuf::from("/tmp/agent-workdir"));
        let backend = CodexBackend::new(Some(PathBuf::from("/tmp/backend-workdir")));

        assert_eq!(
            backend.effective_workdir(&agent),
            Some(std::path::Path::new("/tmp/agent-workdir"))
        );
    }

    #[test]
    fn test_effective_workdir_falls_back_to_backend_workdir() {
        let agent = test_agent();
        let backend = CodexBackend::new(Some(PathBuf::from("/tmp/backend-workdir")));

        assert_eq!(
            backend.effective_workdir(&agent),
            Some(std::path::Path::new("/tmp/backend-workdir"))
        );
    }

    /// `--skip-git-repo-check` must be forwarded to the ping command when the
    /// agent declares it in `backend_args`, so that health checks succeed in
    /// repos where the working directory is not a git repository.
    #[test]
    fn test_build_ping_args_includes_skip_git_repo_check() {
        let mut agent = test_agent();
        agent.backend_args = Some(vec!["--skip-git-repo-check".into()]);
        let args = CodexBackend::build_ping_args(&agent);
        assert!(
            args.contains(&"--skip-git-repo-check".to_string()),
            "--skip-git-repo-check should be forwarded to ping args; got: {:?}",
            args
        );
    }

    /// When `--skip-git-repo-check` is absent from `backend_args`, it must not
    /// appear in the ping args (no accidental injection).
    #[test]
    fn test_build_ping_args_excludes_skip_git_repo_check_when_absent() {
        let agent = test_agent(); // backend_args: None
        let args = CodexBackend::build_ping_args(&agent);
        assert!(
            !args.contains(&"--skip-git-repo-check".to_string()),
            "--skip-git-repo-check should be absent when not in backend_args; got: {:?}",
            args
        );
    }

    #[tokio::test]
    async fn test_start_session() {
        let backend = CodexBackend::new(None);
        let agent = test_agent();
        let session = backend.start_session(&agent).await.unwrap();
        assert_eq!(session.agent_alias, "spark");
        assert_eq!(session.backend, "codex");
    }

    #[test]
    fn test_backend_name() {
        let backend = CodexBackend::new(None);
        assert_eq!(backend.name(), "codex");
    }

    fn test_output(stdout: &str) -> Output {
        Output {
            status: Command::new("true").status().unwrap(),
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
        }
    }

    #[test]
    fn test_extract_codex_single_json() {
        let out = test_output(r#"{"result":"ok"}"#);
        assert_eq!(extract_output_text(&out), "ok");
    }

    #[test]
    fn test_extract_codex_item_completed_jsonl() {
        let out = test_output(
            r#"{"type":"thread.started","thread_id":"abc"}
{"type":"turn.started"}
{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"task complete"}}
{"type":"turn.completed"}"#,
        );
        assert_eq!(extract_output_text(&out), "task complete");
    }

    #[test]
    fn test_extract_thread_id_from_output_found() {
        let stdout =
            b"{\"type\":\"thread.started\",\"thread_id\":\"019c5d27-abc\"}\n{\"type\":\"turn.started\"}";
        assert_eq!(
            CodexBackend::extract_thread_id_from_output(stdout),
            Some("019c5d27-abc".to_string())
        );
    }

    #[test]
    fn test_extract_thread_id_from_output_not_found() {
        let stdout = b"{\"type\":\"turn.started\"}\n{\"type\":\"turn.completed\"}";
        assert_eq!(CodexBackend::extract_thread_id_from_output(stdout), None);
    }

    #[test]
    fn test_extract_thread_id_from_output_empty() {
        assert_eq!(CodexBackend::extract_thread_id_from_output(b""), None);
    }

    #[test]
    fn test_extract_codex_jsonl_with_code_braces() {
        // Codex JSONL with aggregated_output containing code with braces
        let out = test_output(
            r#"{"type":"thread.started","thread_id":"019c5d27"}
{"type":"item.completed","item":{"id":"item_1","type":"command_execution","aggregated_output":"function() {\n  return { x: 1 };\n}\n","exit_code":0}}
{"type":"item.completed","item":{"id":"item_2","type":"agent_message","text":"{\"intent\":\"status-update\",\"to\":\"lead\",\"body\":\"Done\"}"}}"#,
        );
        let text = extract_output_text(&out);
        // Should get the last item.completed text (the agent message), not the code
        assert_eq!(
            text,
            r#"{"intent":"status-update","to":"lead","body":"Done"}"#
        );
    }

    // -- parse_codex_stream_line tests --
    // Schemas observed from Codex CLI v0.1.x (--json output)

    #[test]
    fn test_parse_codex_function_call() {
        let line = r#"{"type":"item.completed","item":{"id":"item_1","type":"function_call","name":"shell","arguments":"{\"command\":\"cargo test\"}"}}"#;
        let event = parse_codex_stream_line(line).expect("should parse function_call");
        assert_eq!(event.event_type, "tool_call");
        assert_eq!(event.summary, "shell");
    }

    #[test]
    fn test_parse_codex_agent_message() {
        let line = r#"{"type":"item.completed","item":{"id":"item_2","type":"agent_message","text":"I have completed the implementation of the feature."}}"#;
        let event = parse_codex_stream_line(line).expect("should parse agent_message");
        assert_eq!(event.event_type, "message");
        assert_eq!(
            event.summary,
            "I have completed the implementation of the feature."
        );
    }

    #[test]
    fn test_parse_codex_agent_message_truncated() {
        let long_text = "A".repeat(200);
        let line = format!(
            r#"{{"type":"item.completed","item":{{"type":"agent_message","text":"{}"}}}}"#,
            long_text
        );
        let event = parse_codex_stream_line(&line).expect("should parse");
        assert_eq!(event.summary.len(), 60);
    }

    #[test]
    fn test_parse_codex_turn_completed() {
        let line = r#"{"type":"turn.completed"}"#;
        let event = parse_codex_stream_line(line).expect("should parse turn.completed");
        assert_eq!(event.event_type, "turn_complete");
        assert_eq!(event.summary, "turn completed");
    }

    #[test]
    fn test_parse_codex_thread_started_returns_none() {
        let line = r#"{"type":"thread.started","thread_id":"019c5d27-abc"}"#;
        assert!(parse_codex_stream_line(line).is_none());
    }

    #[test]
    fn test_parse_codex_command_execution_returns_none() {
        // command_execution item type is not function_call or agent_message
        let line = r#"{"type":"item.completed","item":{"type":"command_execution","aggregated_output":"ok","exit_code":0}}"#;
        assert!(parse_codex_stream_line(line).is_none());
    }

    #[test]
    fn test_parse_codex_empty_and_garbage() {
        assert!(parse_codex_stream_line("").is_none());
        assert!(parse_codex_stream_line("not json").is_none());
        assert!(parse_codex_stream_line(r#"{"no_type":true}"#).is_none());
    }
}
