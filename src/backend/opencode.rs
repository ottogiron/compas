use async_trait::async_trait;
use chrono::Utc;
use std::path::PathBuf;
use std::time::Duration;
use uuid::Uuid;

use super::process::{
    extract_output_text, kill_process, resolve_prompt, spawn_cli, wait_with_timeout, ProcessTracker,
};
use super::{Backend, PingResult};
use crate::error::Result;
use crate::model::agent::Agent;
use crate::model::session::{Session, SessionStatus, TriggerResult};

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

    fn build_args(agent: &Agent, instruction: &str) -> Result<Vec<String>> {
        let mut args = vec!["run".to_string()];

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
            "opencode",
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
                let output_text = extract_output_text(&out);

                Ok(TriggerResult {
                    session_id: session.id.clone(),
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
            "opencode",
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
        let args = OpenCodeBackend::build_args(&agent, "do work").unwrap();

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
        let args = OpenCodeBackend::build_args(&agent, "continue").unwrap();

        assert!(!args.contains(&"-s".to_string()));
        assert!(args.contains(&"continue".to_string()));
    }

    #[test]
    fn test_build_args_with_full_prompt_inlines_guidance() {
        let mut agent = test_agent();
        agent.prompt = Some("You are GLM5.\nFollow AGENTS.md.".into());
        let args = OpenCodeBackend::build_args(&agent, "do work").unwrap();

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
        let args = OpenCodeBackend::build_args(&agent, "task").unwrap();
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
}
