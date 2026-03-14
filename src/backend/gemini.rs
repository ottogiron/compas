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
        })
    }

    async fn trigger(
        &self,
        agent: &Agent,
        session: &Session,
        instruction: Option<&str>,
    ) -> Result<TriggerResult> {
        let instruction = instruction.unwrap_or("Check inbox and process pending tasks.");

        let args = Self::build_args(agent, instruction)?;
        let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();

        let timeout = agent
            .timeout_secs
            .map(Duration::from_secs)
            .unwrap_or(Duration::from_secs(300));

        let child = spawn_cli(
            "gemini",
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
                    Err(_) => (String::from_utf8_lossy(&out.stdout).to_string(), None),
                };

                // Store real session ID for future resume (if we supported stateful resume)
                if let Some(ref real_sid) = real_session_id {
                    self.tracker.set_real_session_id(&session.id, real_sid);
                }

                Ok(TriggerResult {
                    session_id: real_session_id.unwrap_or_else(|| session.id.clone()),
                    success: out.status.success(),
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
            "gemini",
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
            alias: "spark".into(),
            backend: "gemini".into(),
            model: Some("gemini-1.5-pro".into()),
            prompt: Some("You are a test agent.".into()),
            prompt_file: None,
            timeout_secs: Some(60),
            backend_args: None,
            env: None,
            log_path: None,
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
}
