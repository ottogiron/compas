use async_trait::async_trait;
use chrono::Utc;
use std::path::PathBuf;
use std::time::Duration;
use uuid::Uuid;

use super::process::{kill_process, spawn_cli, wait_with_timeout, ProcessTracker};
use super::{classify_error, Backend, BackendOutput, PingResult};
use crate::config::types::{BackendDefinition, OutputFormat};
use crate::error::Result;
use crate::model::agent::Agent;
use crate::model::session::{Session, SessionStatus};

/// Config-driven generic backend.
///
/// Implements `Backend` for any CLI tool defined in `backend_definitions`.
/// Template variables `{{instruction}}`, `{{model}}`, `{{session_id}}` in args
/// are substituted at trigger time.
#[derive(Debug)]
pub struct GenericBackend {
    definition: BackendDefinition,
    tracker: ProcessTracker,
    workdir: Option<PathBuf>,
}

impl GenericBackend {
    pub fn new(definition: BackendDefinition) -> Self {
        Self::with_workdir(definition, None)
    }

    pub fn with_workdir(definition: BackendDefinition, workdir: Option<PathBuf>) -> Self {
        Self {
            definition,
            tracker: ProcessTracker::new(),
            workdir,
        }
    }

    /// Perform template substitution on the configured args.
    ///
    /// Replaces `{{instruction}}`, `{{model}}`, `{{session_id}}` with actual values.
    /// Args that resolve to an empty string after substitution of a template
    /// variable are omitted from the final list (handles missing optional vars).
    fn substitute_args(
        &self,
        instruction: &str,
        model: Option<&str>,
        session_id: Option<&str>,
    ) -> Vec<String> {
        let mut result = Vec::new();
        for arg in &self.definition.args {
            let mut value = arg.clone();
            value = value.replace("{{instruction}}", instruction);
            value = value.replace("{{model}}", model.unwrap_or(""));
            value = value.replace("{{session_id}}", session_id.unwrap_or(""));

            // If the arg was purely a template variable for an absent optional
            // value, it will now be empty — skip it to avoid passing blank args.
            if arg.contains("{{model}}") && model.is_none() && value.trim().is_empty() {
                continue;
            }
            if arg.contains("{{session_id}}") && session_id.is_none() && value.trim().is_empty() {
                continue;
            }

            result.push(value);
        }
        result
    }

    /// Parse the subprocess output according to the configured output format.
    ///
    /// Returns `(result_text, session_id, has_result_output)`.
    fn parse_output(&self, stdout: &str) -> (String, Option<String>, bool) {
        match self.definition.output.format {
            OutputFormat::Plaintext => {
                // Raw stdout is the result text; no structured session ID extraction.
                let text = stdout.to_string();
                let has_output = !text.trim().is_empty();
                (text, None, has_output)
            }
            OutputFormat::Json => self.parse_json_result(stdout),
            OutputFormat::Jsonl => self.parse_jsonl_result(stdout),
        }
    }

    /// Parse a single JSON object from stdout.
    fn parse_json_result(&self, stdout: &str) -> (String, Option<String>, bool) {
        let trimmed = stdout.trim();
        if trimmed.is_empty() {
            return (String::new(), None, false);
        }

        let val: serde_json::Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => {
                // Not valid JSON — fall back to raw text.
                return (stdout.to_string(), None, false);
            }
        };

        let result_text = self.extract_field(&val, self.definition.output.result_field.as_deref());
        let session_id =
            self.extract_field(&val, self.definition.output.session_id_field.as_deref());
        let has_output = result_text.is_some();

        (result_text.unwrap_or_default(), session_id, has_output)
    }

    /// Parse JSONL output: extract from the last JSON line.
    fn parse_jsonl_result(&self, stdout: &str) -> (String, Option<String>, bool) {
        // Find the last non-empty line that is valid JSON.
        let last_json = stdout
            .lines()
            .rev()
            .filter(|l| !l.trim().is_empty())
            .find_map(|l| serde_json::from_str::<serde_json::Value>(l.trim()).ok());

        match last_json {
            Some(val) => {
                let result_text =
                    self.extract_field(&val, self.definition.output.result_field.as_deref());
                let session_id =
                    self.extract_field(&val, self.definition.output.session_id_field.as_deref());
                let has_output = result_text.is_some();
                (result_text.unwrap_or_default(), session_id, has_output)
            }
            None => {
                // No valid JSON lines — fall back to raw text.
                (stdout.to_string(), None, false)
            }
        }
    }

    /// Extract a string value from a JSON object by field name.
    ///
    /// Supports dot-separated paths (e.g., `"data.result"`) for nested extraction,
    /// and simple top-level field names.
    fn extract_field(&self, val: &serde_json::Value, field: Option<&str>) -> Option<String> {
        let field = field?;

        // Support dot-separated paths for nested fields.
        let pointer = format!("/{}", field.replace('.', "/"));
        if let Some(v) = val.pointer(&pointer) {
            return v.as_str().map(|s| s.to_string());
        }

        // Direct field lookup as fallback (handles single-level field names).
        val.get(field)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    }
}

#[async_trait]
impl Backend for GenericBackend {
    fn name(&self) -> &str {
        &self.definition.name
    }

    async fn start_session(&self, agent: &Agent) -> Result<Session> {
        let session_id = Uuid::new_v4().to_string();

        // If resume config is present, look up the previous real session ID
        // from the tracker so the next trigger can pass it.
        let resume_session_id = if self.definition.resume.is_some() {
            self.tracker.get_real_session_id(&agent.alias)
        } else {
            None
        };

        Ok(Session {
            id: session_id,
            agent_alias: agent.alias.clone(),
            backend: self.definition.name.clone(),
            started_at: Utc::now(),
            resume_session_id,
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
        let instruction = instruction.unwrap_or("Process pending tasks.");

        // Build the resume session ID from the session's resume field.
        let resume_sid = session.resume_session_id.as_deref();

        // Substitute template variables in configured args.
        let mut args = self.substitute_args(instruction, agent.model.as_deref(), resume_sid);

        // If resume config is present and we have a previous session ID,
        // prepend the resume flag and session ID arg.
        if let Some(ref resume_cfg) = self.definition.resume {
            if let Some(ref sid) = session.resume_session_id {
                let resume_arg = resume_cfg.session_id_arg.replace("{{session_id}}", sid);
                args.insert(0, resume_arg);
                args.insert(0, resume_cfg.flag.clone());
            }
        }

        // Append extra backend args from agent config.
        if let Some(ref extra) = agent.backend_args {
            args.extend(extra.iter().cloned());
        }

        let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();

        let timeout = agent
            .timeout_secs
            .map(Duration::from_secs)
            .unwrap_or(Duration::from_secs(300));

        let workdir = agent
            .execution_workdir
            .as_deref()
            .or(self.workdir.as_deref());

        // Build env: start with agent env, then apply env_remove from backend definition.
        let mut env_map = agent.env.clone().unwrap_or_default();
        if let Some(ref removals) = self.definition.env_remove {
            for key in removals {
                env_map.remove(key);
            }
        }
        let env = if env_map.is_empty() {
            None
        } else {
            Some(&env_map)
        };

        let child = spawn_cli(&self.definition.command, &arg_refs, env, workdir)?;
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
                let (result_text, real_session_id, has_result_output) =
                    self.parse_output(&raw_output);

                let success = out.status.success();

                // Store the real session ID for future resume if extracted.
                // Keyed by agent alias so start_session() can look it up.
                if let Some(ref sid) = real_session_id {
                    self.tracker.set_real_session_id(&agent.alias, sid);
                }

                let stderr_text = String::from_utf8_lossy(&out.stderr);
                let error_text = format!("{}\n{}", raw_output, stderr_text);
                let error_category = if !success {
                    Some(classify_error(false, has_result_output, &error_text))
                } else {
                    None
                };

                Ok(BackendOutput {
                    success,
                    result_text,
                    parsed_intent: None,
                    session_id: real_session_id,
                    raw_output,
                    error_category,
                    pid: Some(pid),
                    cost_usd: None,
                    tokens_in: None,
                    tokens_out: None,
                    num_turns: None,
                })
            }
            Err(e) => Err(e),
        }
    }

    async fn session_status(&self, _agent: &Agent) -> Result<Option<SessionStatus>> {
        // GenericBackend doesn't maintain persistent session state.
        // If the tracker has a PID for any active session, check liveness.
        // Since session_status is called without a specific session, return None.
        Ok(None)
    }

    async fn kill_session(&self, _agent: &Agent, session: &Session, _reason: &str) -> Result<()> {
        if let Some(pid) = self.tracker.get_pid(&session.id) {
            kill_process(pid)?;
            self.tracker.untrack(&session.id);
        }
        Ok(())
    }

    async fn ping(&self, agent: &Agent, timeout_secs: u64) -> PingResult {
        let start = std::time::Instant::now();

        let (command, args) = if let Some(ref ping_cfg) = self.definition.ping {
            let arg_refs: Vec<String> = ping_cfg.args.clone();
            (ping_cfg.command.clone(), arg_refs)
        } else {
            // Default: command --version
            (
                self.definition.command.clone(),
                vec!["--version".to_string()],
            )
        };

        let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let workdir = agent
            .execution_workdir
            .as_deref()
            .or(self.workdir.as_deref());

        match spawn_cli(&command, &arg_refs, agent.env.as_ref(), workdir) {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::types::{OutputConfig, OutputFormat, PingConfig, ResumeConfig};

    fn test_definition() -> BackendDefinition {
        BackendDefinition {
            name: "test-tool".to_string(),
            command: "echo".to_string(),
            args: vec!["{{instruction}}".to_string()],
            resume: None,
            output: OutputConfig::default(),
            ping: None,
            env_remove: None,
        }
    }

    fn test_agent() -> Agent {
        Agent {
            alias: "worker-1".into(),
            backend: "test-tool".into(),
            model: Some("gpt-4".into()),
            prompt: None,
            prompt_file: None,
            timeout_secs: Some(30),
            backend_args: None,
            env: None,
            log_path: None,
            execution_workdir: None,
        }
    }

    // ── name() ──

    #[test]
    fn test_generic_backend_name() {
        let backend = GenericBackend::new(test_definition());
        assert_eq!(backend.name(), "test-tool");
    }

    // ── start_session() ──

    #[tokio::test]
    async fn test_generic_start_session() {
        let backend = GenericBackend::new(test_definition());
        let agent = test_agent();
        let session = backend.start_session(&agent).await.unwrap();
        assert_eq!(session.agent_alias, "worker-1");
        assert_eq!(session.backend, "test-tool");
        assert!(!session.id.is_empty());
        assert!(session.resume_session_id.is_none());
    }

    #[tokio::test]
    async fn test_generic_start_session_with_resume_no_prior() {
        let mut def = test_definition();
        def.resume = Some(ResumeConfig {
            flag: "--resume".into(),
            session_id_arg: "{{session_id}}".into(),
        });
        let backend = GenericBackend::new(def);
        let agent = test_agent();
        let session = backend.start_session(&agent).await.unwrap();
        // No prior session stored, so resume_session_id is None.
        assert!(session.resume_session_id.is_none());
    }

    #[tokio::test]
    async fn test_generic_resume_round_trip() {
        // Backend emits JSON with a session ID → trigger stores it →
        // next start_session picks it up as resume_session_id.
        let def = BackendDefinition {
            name: "resumable".into(),
            command: "echo".into(),
            args: vec![r#"{"result":"ok","sid":"real-sess-42"}"#.into()],
            resume: Some(ResumeConfig {
                flag: "--resume".into(),
                session_id_arg: "{{session_id}}".into(),
            }),
            output: OutputConfig {
                format: OutputFormat::Json,
                result_field: Some("result".into()),
                session_id_field: Some("sid".into()),
            },
            ping: None,
            env_remove: None,
        };
        let backend = GenericBackend::new(def);
        let agent = test_agent();

        // First trigger: no resume yet.
        let session1 = backend.start_session(&agent).await.unwrap();
        assert!(session1.resume_session_id.is_none());
        let output = backend
            .trigger(&agent, &session1, Some("first"))
            .await
            .unwrap();
        assert!(output.success);
        assert_eq!(output.session_id, Some("real-sess-42".to_string()));

        // Second start_session: should pick up the stored session ID.
        let session2 = backend.start_session(&agent).await.unwrap();
        assert_eq!(session2.resume_session_id, Some("real-sess-42".to_string()));
    }

    // ── substitute_args() ──

    #[test]
    fn test_generic_substitute_args_all_present() {
        let backend = GenericBackend::new(BackendDefinition {
            name: "tool".into(),
            command: "tool".into(),
            args: vec![
                "--prompt".into(),
                "{{instruction}}".into(),
                "--model".into(),
                "{{model}}".into(),
            ],
            resume: None,
            output: OutputConfig::default(),
            ping: None,
            env_remove: None,
        });
        let args = backend.substitute_args("do stuff", Some("gpt-4"), None);
        assert_eq!(args, vec!["--prompt", "do stuff", "--model", "gpt-4"]);
    }

    #[test]
    fn test_generic_substitute_args_missing_model_omitted() {
        let backend = GenericBackend::new(BackendDefinition {
            name: "tool".into(),
            command: "tool".into(),
            args: vec![
                "--prompt".into(),
                "{{instruction}}".into(),
                "{{model}}".into(),
            ],
            resume: None,
            output: OutputConfig::default(),
            ping: None,
            env_remove: None,
        });
        let args = backend.substitute_args("do stuff", None, None);
        // The "{{model}}" arg resolves to "" and is omitted since model is None.
        assert_eq!(args, vec!["--prompt", "do stuff"]);
    }

    #[test]
    fn test_generic_substitute_args_model_in_compound_arg() {
        let backend = GenericBackend::new(BackendDefinition {
            name: "tool".into(),
            command: "tool".into(),
            args: vec!["--model={{model}}".into(), "{{instruction}}".into()],
            resume: None,
            output: OutputConfig::default(),
            ping: None,
            env_remove: None,
        });
        // When model is None, "--model={{model}}" becomes "--model=" which is not
        // blank, so it stays (the flag prefix is still present).
        let args = backend.substitute_args("do stuff", None, None);
        assert_eq!(args, vec!["--model=", "do stuff"]);
    }

    #[test]
    fn test_generic_substitute_args_missing_session_id_omitted() {
        let backend = GenericBackend::new(BackendDefinition {
            name: "tool".into(),
            command: "tool".into(),
            args: vec!["{{instruction}}".into(), "{{session_id}}".into()],
            resume: None,
            output: OutputConfig::default(),
            ping: None,
            env_remove: None,
        });
        let args = backend.substitute_args("do stuff", None, None);
        assert_eq!(args, vec!["do stuff"]);
    }

    // ── parse_output() ──

    #[test]
    fn test_generic_parse_output_plaintext() {
        let backend = GenericBackend::new(test_definition());
        let (text, sid, has) = backend.parse_output("hello world\n");
        assert_eq!(text, "hello world\n");
        assert!(sid.is_none());
        assert!(has);
    }

    #[test]
    fn test_generic_parse_output_plaintext_empty() {
        let backend = GenericBackend::new(test_definition());
        let (text, sid, has) = backend.parse_output("  \n");
        assert_eq!(text, "  \n");
        assert!(sid.is_none());
        assert!(!has);
    }

    #[test]
    fn test_generic_parse_output_json() {
        let mut def = test_definition();
        def.output = OutputConfig {
            format: OutputFormat::Json,
            result_field: Some("result".into()),
            session_id_field: Some("sid".into()),
        };
        let backend = GenericBackend::new(def);
        let (text, sid, has) = backend.parse_output(r#"{"result":"done","sid":"s123"}"#);
        assert_eq!(text, "done");
        assert_eq!(sid, Some("s123".to_string()));
        assert!(has);
    }

    #[test]
    fn test_generic_parse_output_json_missing_field() {
        let mut def = test_definition();
        def.output = OutputConfig {
            format: OutputFormat::Json,
            result_field: Some("result".into()),
            session_id_field: None,
        };
        let backend = GenericBackend::new(def);
        let (text, sid, has) = backend.parse_output(r#"{"output":"value"}"#);
        assert_eq!(text, "");
        assert!(sid.is_none());
        assert!(!has);
    }

    #[test]
    fn test_generic_parse_output_json_nested_field() {
        let mut def = test_definition();
        def.output = OutputConfig {
            format: OutputFormat::Json,
            result_field: Some("data.text".into()),
            session_id_field: None,
        };
        let backend = GenericBackend::new(def);
        let (text, _sid, has) = backend.parse_output(r#"{"data":{"text":"nested value"}}"#);
        assert_eq!(text, "nested value");
        assert!(has);
    }

    #[test]
    fn test_generic_parse_output_jsonl() {
        let mut def = test_definition();
        def.output = OutputConfig {
            format: OutputFormat::Jsonl,
            result_field: Some("text".into()),
            session_id_field: Some("session".into()),
        };
        let backend = GenericBackend::new(def);
        let stdout = r#"{"type":"progress","pct":50}
{"text":"final answer","session":"s456"}"#;
        let (text, sid, has) = backend.parse_output(stdout);
        assert_eq!(text, "final answer");
        assert_eq!(sid, Some("s456".to_string()));
        assert!(has);
    }

    #[test]
    fn test_generic_parse_output_jsonl_no_valid_json() {
        let mut def = test_definition();
        def.output = OutputConfig {
            format: OutputFormat::Jsonl,
            result_field: Some("text".into()),
            session_id_field: None,
        };
        let backend = GenericBackend::new(def);
        let (text, sid, has) = backend.parse_output("not json\nalso not json\n");
        assert_eq!(text, "not json\nalso not json\n");
        assert!(sid.is_none());
        assert!(!has);
    }

    #[test]
    fn test_generic_parse_output_json_invalid_json() {
        let mut def = test_definition();
        def.output = OutputConfig {
            format: OutputFormat::Json,
            result_field: Some("result".into()),
            session_id_field: None,
        };
        let backend = GenericBackend::new(def);
        let (text, sid, has) = backend.parse_output("not valid json");
        assert_eq!(text, "not valid json");
        assert!(sid.is_none());
        assert!(!has);
    }

    // ── trigger() with echo ──

    #[tokio::test]
    async fn test_generic_trigger_plaintext() {
        let backend = GenericBackend::new(test_definition());
        let agent = test_agent();
        let session = backend.start_session(&agent).await.unwrap();
        let output = backend
            .trigger(&agent, &session, Some("hello world"))
            .await
            .unwrap();
        assert!(output.success);
        assert!(output.result_text.contains("hello world"));
        assert!(output.pid.is_some());
    }

    #[tokio::test]
    async fn test_generic_trigger_json_output() {
        let def = BackendDefinition {
            name: "json-tool".into(),
            command: "echo".into(),
            args: vec![r#"{"result":"ok","sid":"s1"}"#.into()],
            resume: None,
            output: OutputConfig {
                format: OutputFormat::Json,
                result_field: Some("result".into()),
                session_id_field: Some("sid".into()),
            },
            ping: None,
            env_remove: None,
        };
        let backend = GenericBackend::new(def);
        let agent = test_agent();
        let session = backend.start_session(&agent).await.unwrap();
        let output = backend
            .trigger(&agent, &session, Some("test"))
            .await
            .unwrap();
        assert!(output.success);
        assert_eq!(output.result_text, "ok");
        assert_eq!(output.session_id, Some("s1".to_string()));
    }

    #[tokio::test]
    async fn test_generic_trigger_failure_classifies_error() {
        let def = BackendDefinition {
            name: "fail-tool".into(),
            command: "sh".into(),
            args: vec!["-c".into(), "echo 'connection refused'; exit 1".into()],
            resume: None,
            output: OutputConfig::default(),
            ping: None,
            env_remove: None,
        };
        let backend = GenericBackend::new(def);
        let agent = test_agent();
        let session = backend.start_session(&agent).await.unwrap();
        let output = backend
            .trigger(&agent, &session, Some("test"))
            .await
            .unwrap();
        assert!(!output.success);
        assert!(output.error_category.is_some());
    }

    // ── kill_session() ──

    #[tokio::test]
    async fn test_generic_kill_session_no_pid() {
        let backend = GenericBackend::new(test_definition());
        let agent = test_agent();
        let session = backend.start_session(&agent).await.unwrap();
        // No process running — kill_session should succeed silently.
        let result = backend.kill_session(&agent, &session, "test").await;
        assert!(result.is_ok());
    }

    // ── ping() ──

    #[tokio::test]
    async fn test_generic_ping_default() {
        // echo --version should succeed
        let backend = GenericBackend::new(test_definition());
        let agent = test_agent();
        let result = backend.ping(&agent, 5).await;
        assert!(result.alive);
    }

    #[tokio::test]
    async fn test_generic_ping_custom() {
        let mut def = test_definition();
        def.ping = Some(PingConfig {
            command: "echo".into(),
            args: vec!["pong".into()],
        });
        let backend = GenericBackend::new(def);
        let agent = test_agent();
        let result = backend.ping(&agent, 5).await;
        assert!(result.alive);
    }

    #[tokio::test]
    async fn test_generic_ping_failing_command() {
        let mut def = test_definition();
        def.ping = Some(PingConfig {
            command: "sh".into(),
            args: vec!["-c".into(), "exit 1".into()],
        });
        let backend = GenericBackend::new(def);
        let agent = test_agent();
        let result = backend.ping(&agent, 5).await;
        assert!(!result.alive);
        assert!(result.detail.is_some());
    }

    // ── env_remove ──

    #[tokio::test]
    async fn test_generic_trigger_env_remove() {
        let def = BackendDefinition {
            name: "env-tool".into(),
            command: "sh".into(),
            args: vec!["-c".into(), "echo ${TEST_VAR:-unset}".into()],
            resume: None,
            output: OutputConfig::default(),
            ping: None,
            env_remove: Some(vec!["TEST_VAR".into()]),
        };
        let backend = GenericBackend::new(def);
        let mut agent = test_agent();
        agent.env = Some(
            [("TEST_VAR".to_string(), "should_be_removed".to_string())]
                .into_iter()
                .collect(),
        );
        let session = backend.start_session(&agent).await.unwrap();
        let output = backend
            .trigger(&agent, &session, Some("test"))
            .await
            .unwrap();
        assert!(output.success);
        // TEST_VAR was removed by env_remove, so the shell prints "unset".
        assert!(output.result_text.contains("unset"));
    }

    // ── session_status() ──

    #[tokio::test]
    async fn test_generic_session_status_returns_none() {
        let backend = GenericBackend::new(test_definition());
        let agent = test_agent();
        let status = backend.session_status(&agent).await.unwrap();
        assert!(status.is_none());
    }

    // ── extract_field() ──

    #[test]
    fn test_generic_extract_field_top_level() {
        let backend = GenericBackend::new(test_definition());
        let val: serde_json::Value = serde_json::from_str(r#"{"foo":"bar"}"#).unwrap();
        assert_eq!(
            backend.extract_field(&val, Some("foo")),
            Some("bar".to_string())
        );
    }

    #[test]
    fn test_generic_extract_field_nested() {
        let backend = GenericBackend::new(test_definition());
        let val: serde_json::Value = serde_json::from_str(r#"{"a":{"b":"deep"}}"#).unwrap();
        assert_eq!(
            backend.extract_field(&val, Some("a.b")),
            Some("deep".to_string())
        );
    }

    #[test]
    fn test_generic_extract_field_none_field() {
        let backend = GenericBackend::new(test_definition());
        let val: serde_json::Value = serde_json::from_str(r#"{"foo":"bar"}"#).unwrap();
        assert_eq!(backend.extract_field(&val, None), None);
    }

    #[test]
    fn test_generic_extract_field_missing() {
        let backend = GenericBackend::new(test_definition());
        let val: serde_json::Value = serde_json::from_str(r#"{"foo":"bar"}"#).unwrap();
        assert_eq!(backend.extract_field(&val, Some("missing")), None);
    }
}
