use async_trait::async_trait;
use chrono::Utc;
use std::path::PathBuf;
use std::time::Duration;
use uuid::Uuid;

use super::process::{
    kill_process, parse_json_output, resolve_prompt, spawn_cli, wait_with_timeout, ProcessTracker,
};
use super::{Backend, PingResult};
use crate::error::Result;
use crate::model::agent::Agent;
use crate::model::session::{Session, SessionStatus, TriggerResult};

/// Claude Code CLI backend.
///
/// Uses `claude -p` for non-interactive sessions with JSON output.
/// Key flags: `-p`, `--dangerously-skip-permissions`, `--output-format json`,
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

        // JSON output
        args.push("--output-format".to_string());
        args.push("json".to_string());

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
        })
    }

    async fn trigger(
        &self,
        agent: &Agent,
        session: &Session,
        instruction: Option<&str>,
    ) -> Result<TriggerResult> {
        let instruction = instruction.unwrap_or("Check inbox and process pending tasks.");

        // Only resume if we have a real Claude session ID (from a previous trigger's output).
        // Our internally-generated UUIDs are not valid Claude session IDs.
        let real_sid = self.tracker.get_real_session_id(&session.id);
        let resume_id = real_sid.as_deref();

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

        let output = wait_with_timeout(child, Some(timeout), agent.log_path.as_deref());
        self.tracker.untrack(&session.id);

        match output {
            Ok(out) => {
                let json = parse_json_output(&out);
                let (output_text, real_session_id) = match &json {
                    Ok(val) => {
                        let text = val
                            .get("result")
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
                    Err(_) => (String::from_utf8_lossy(&out.stdout).to_string(), None),
                };

                // Store real session ID for future resume
                if let Some(ref real_sid) = real_session_id {
                    self.tracker.set_real_session_id(&session.id, real_sid);
                }

                // Consider the trigger successful if we got valid JSON output
                // with a result field, even if the exit code was non-zero.
                // Claude Code can exit non-zero while still producing valid output.
                let success =
                    out.status.success() || json.as_ref().is_ok_and(|v| v.get("result").is_some());

                Ok(TriggerResult {
                    session_id: real_session_id.unwrap_or_else(|| session.id.clone()),
                    success,
                    output: Some(output_text),
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
                match wait_with_timeout(child, Some(timeout), None) {
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

#[cfg(test)]
mod tests {
    use super::super::process::parse_json_output;
    use super::*;
    use std::process::{Command, Output};

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
        assert!(args.contains(&"json".to_string()));
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
    fn test_build_ping_args_includes_model() {
        let agent = test_agent();
        let args = ClaudeCodeBackend::build_ping_args(&agent);
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

    fn test_output(stdout: &str) -> Output {
        Output {
            status: Command::new("true").status().unwrap(),
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
        }
    }

    #[test]
    fn test_parse_claude_realistic_output() {
        // Real Claude CLI JSON output format
        let out = test_output(
            r#"{"type":"result","subtype":"success","cost_usd":0.003,"is_error":false,"duration_ms":5443,"duration_api_ms":3709,"num_turns":1,"result":"ok","session_id":"abc-123-def"}"#,
        );
        let val = parse_json_output(&out).unwrap();
        assert_eq!(val["result"], "ok");
        assert_eq!(val["session_id"], "abc-123-def");
        assert_eq!(val["type"], "result");
    }

    #[test]
    fn test_parse_claude_json_auto_reply() {
        // Agent embeds JSON auto-reply in the result field
        let out = test_output(
            r#"{"type":"result","subtype":"success","result":"{\"intent\":\"status-update\",\"to\":\"lead\",\"body\":\"Task done\"}","session_id":"s1"}"#,
        );
        let val = parse_json_output(&out).unwrap();
        let result_str = val["result"].as_str().unwrap();
        assert!(result_str.contains("status-update"));
        assert!(result_str.contains("Task done"));
    }

    #[test]
    fn test_parse_claude_non_json_fallback() {
        let out = test_output("plain text error from claude");
        assert!(parse_json_output(&out).is_err());
    }
}
