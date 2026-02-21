use async_trait::async_trait;
use chrono::Utc;
use std::path::PathBuf;
use std::time::Duration;
use uuid::Uuid;

use super::process::{
    extract_output_text, kill_process, spawn_cli, wait_with_timeout, ProcessTracker,
};
use super::{Backend, PingResult};
use crate::error::Result;
use crate::model::agent::Agent;
use crate::model::session::{Session, SessionStatus, TriggerResult};

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
        resume: bool,
        workdir: Option<&PathBuf>,
    ) -> Vec<String> {
        let mut args = vec!["exec".to_string()];

        if resume {
            args.push("resume".to_string());
            args.push("--last".to_string());
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

        // Full auto mode
        args.push("--full-auto".to_string());

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
        })
    }

    async fn trigger(
        &self,
        agent: &Agent,
        session: &Session,
        instruction: Option<&str>,
    ) -> Result<TriggerResult> {
        let instruction = instruction.unwrap_or("Check inbox and process pending tasks.");

        // Use resume for existing sessions (not the first trigger)
        let is_resume = false; // First trigger is always a new exec
        let args = Self::build_args(agent, instruction, is_resume, self.workdir.as_ref());
        let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();

        let timeout = agent
            .timeout_secs
            .map(Duration::from_secs)
            .unwrap_or(Duration::from_secs(300));

        let child = spawn_cli(
            "codex",
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
        let args = vec![
            "exec".to_string(),
            "--full-auto".to_string(),
            "--json".to_string(),
            "Reply with: ok".to_string(),
        ];
        let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        match spawn_cli(
            "codex",
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
            alias: "spark".into(),
            identity: "Codex".into(),
            backend: "codex".into(),
            model: Some("gpt-5.3-codex".into()),
            prompt: None,
            prompt_file: None,
            timeout_secs: Some(180),
            backend_args: None,
            env: None,
            log_path: None,
        }
    }

    #[test]
    fn test_build_args_new_session() {
        let agent = test_agent();
        let workdir = PathBuf::from("/home/user/project");
        let args = CodexBackend::build_args(&agent, "implement X", false, Some(&workdir));

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
        let args = CodexBackend::build_args(&agent, "continue", true, None);

        assert_eq!(args[0], "exec");
        assert_eq!(args[1], "resume");
        assert_eq!(args[2], "--last");
    }

    #[test]
    fn test_build_args_no_workdir() {
        let agent = test_agent();
        let args = CodexBackend::build_args(&agent, "task", false, None);

        assert!(!args.contains(&"-C".to_string()));
    }

    #[test]
    fn test_build_args_with_backend_args() {
        let mut agent = test_agent();
        agent.backend_args = Some(vec!["--sandbox".into(), "workspace-write".into()]);
        let args = CodexBackend::build_args(&agent, "task", false, None);
        assert!(args.contains(&"--sandbox".to_string()));
        assert!(args.contains(&"workspace-write".to_string()));
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
    fn test_extract_codex_jsonl_with_code_braces() {
        // Codex JSONL with aggregated_output containing code with braces
        let out = test_output(
            r#"{"type":"thread.started","thread_id":"019c5d27"}
{"type":"item.completed","item":{"id":"item_1","type":"command_execution","aggregated_output":"function() {\n  return { x: 1 };\n}\n","exit_code":0}}
{"type":"item.completed","item":{"id":"item_2","type":"agent_message","text":"{\"intent\":\"review-request\",\"to\":\"lead\",\"body\":\"Done\"}"}}"#,
        );
        let text = extract_output_text(&out);
        // Should get the last item.completed text (the agent message), not the code
        assert_eq!(
            text,
            r#"{"intent":"review-request","to":"lead","body":"Done"}"#
        );
    }
}
