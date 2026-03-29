use super::types::{AgentRole, HandoffTarget, OrchestratorConfig, OutputFormat};
use crate::error::{OrchestratorError, Result};
use std::collections::HashSet;

/// Built-in backend names that cannot be used in `backend_definitions`.
const BUILTIN_BACKEND_NAMES: &[&str] = &["claude", "codex", "gemini", "opencode"];

/// Validate an orchestrator configuration.
pub fn validate_config(config: &OrchestratorConfig) -> Result<()> {
    if config.agents.is_empty() {
        return Err(OrchestratorError::Config(
            "at least one agent must be configured".into(),
        ));
    }
    if config.default_workdir.as_os_str().is_empty() {
        return Err(OrchestratorError::Config(
            "default_workdir must not be empty".into(),
        ));
    }
    if !config.default_workdir.exists() {
        return Err(OrchestratorError::Config(format!(
            "default_workdir does not exist: {}",
            config.default_workdir.display()
        )));
    }
    if !config.default_workdir.is_dir() {
        return Err(OrchestratorError::Config(format!(
            "default_workdir must be a directory: {}",
            config.default_workdir.display()
        )));
    }

    let mut aliases = HashSet::new();
    for agent in &config.agents {
        if agent.alias.is_empty() {
            return Err(OrchestratorError::Config(
                "agent alias must not be empty".into(),
            ));
        }
        if agent.backend.is_empty() {
            return Err(OrchestratorError::Config(format!(
                "agent '{}' backend must not be empty",
                agent.alias
            )));
        }
        if !aliases.insert(&agent.alias) {
            return Err(OrchestratorError::Config(format!(
                "duplicate agent alias: '{}'",
                agent.alias
            )));
        }
        if agent.prompt.is_some() && agent.prompt_file.is_some() {
            tracing::warn!(
                "agent '{}': both prompt and prompt_file set; prompt_file takes precedence",
                agent.alias
            );
        }
        if let Some(ref pf) = agent.prompt_file {
            if !pf.exists() {
                return Err(OrchestratorError::Config(format!(
                    "agent '{}' prompt_file does not exist: {}",
                    agent.alias,
                    pf.display()
                )));
            }
        }
        if let Some(timeout) = agent.timeout_secs {
            if timeout == 0 {
                return Err(OrchestratorError::Config(format!(
                    "agent '{}' timeout_secs must be > 0",
                    agent.alias
                )));
            }
        }
        if let Some(ref backend_args) = agent.backend_args {
            if backend_args.iter().any(|a| a.trim().is_empty()) {
                return Err(OrchestratorError::Config(format!(
                    "agent '{}' backend_args must not contain empty entries",
                    agent.alias
                )));
            }
        }
        if let Some(ref ws) = agent.workspace {
            if ws != "worktree" && ws != "shared" {
                return Err(OrchestratorError::Config(format!(
                    "agent '{}' workspace must be \"worktree\" or \"shared\", got \"{}\"",
                    agent.alias, ws
                )));
            }
        }
    }

    // ── Handoff validation (ORCH-CHAIN-1) ──
    // Collect all valid aliases for target resolution.
    let all_aliases: HashSet<&str> = config.agents.iter().map(|a| a.alias.as_str()).collect();

    for agent in &config.agents {
        if let Some(ref handoff) = agent.handoff {
            // Validate max_chain_depth bounds (1..=20)
            if let Some(depth) = handoff.max_chain_depth {
                if !(1..=20).contains(&depth) {
                    return Err(OrchestratorError::Config(format!(
                        "agent '{}' handoff.max_chain_depth must be 1..=20, got {}",
                        agent.alias, depth
                    )));
                }
            }

            // Validate on_response target(s).
            if let Some(ref target) = handoff.on_response {
                match target {
                    HandoffTarget::Single(alias) => {
                        if alias == &agent.alias {
                            return Err(OrchestratorError::Config(format!(
                                "agent '{}' handoff.on_response points to itself (self-loop not allowed)",
                                agent.alias
                            )));
                        }
                        if alias != "operator" && !all_aliases.contains(alias.as_str()) {
                            return Err(OrchestratorError::Config(format!(
                                "agent '{}' handoff.on_response references unknown agent alias '{}'",
                                agent.alias, alias
                            )));
                        }
                    }
                    HandoffTarget::FanOut(aliases) => {
                        if aliases.is_empty() {
                            return Err(OrchestratorError::Config(format!(
                                "agent '{}' handoff.on_response fan-out list must not be empty",
                                agent.alias
                            )));
                        }
                        let mut seen = HashSet::new();
                        for alias in aliases {
                            if alias == "operator" {
                                return Err(OrchestratorError::Config(format!(
                                    "agent '{}' handoff.on_response fan-out must not contain 'operator' \
                                     (operator is a chain-stop target, not a dispatch target)",
                                    agent.alias
                                )));
                            }
                            if !seen.insert(alias.as_str()) {
                                return Err(OrchestratorError::Config(format!(
                                    "agent '{}' handoff.on_response fan-out contains duplicate target '{}'",
                                    agent.alias, alias
                                )));
                            }
                            if alias == &agent.alias {
                                return Err(OrchestratorError::Config(format!(
                                    "agent '{}' handoff.on_response points to itself (self-loop not allowed)",
                                    agent.alias
                                )));
                            }
                            if !all_aliases.contains(alias.as_str()) {
                                return Err(OrchestratorError::Config(format!(
                                    "agent '{}' handoff.on_response references unknown agent alias '{}'",
                                    agent.alias, alias
                                )));
                            }
                        }
                    }
                }
            }
        }
    }

    // ORCHV3-15: validate poll_interval bounds
    if config.poll_interval_secs < 1 || config.poll_interval_secs > 3600 {
        return Err(OrchestratorError::Config(
            "poll_interval_secs must be 1..3600".into(),
        ));
    }

    if config.orchestration.stale_active_secs < 60
        || config.orchestration.stale_active_secs > 604_800
    {
        return Err(OrchestratorError::Config(
            "orchestration.stale_active_secs must be 60..604800".into(),
        ));
    }

    const VALID_MERGE_STRATEGIES: &[&str] = &["merge", "rebase", "squash"];
    if !VALID_MERGE_STRATEGIES.contains(&config.orchestration.default_merge_strategy.as_str()) {
        return Err(OrchestratorError::Config(format!(
            "orchestration.default_merge_strategy must be \"merge\", \"rebase\", or \"squash\", got \"{}\"",
            config.orchestration.default_merge_strategy
        )));
    }

    if config.orchestration.default_merge_target.trim().is_empty() {
        return Err(OrchestratorError::Config(
            "orchestration.default_merge_target must not be empty".into(),
        ));
    }

    // ORCHV3-15: ensure state_dir is writable (create if needed)
    if !config.state_dir.exists() {
        std::fs::create_dir_all(&config.state_dir).map_err(|e| {
            OrchestratorError::Config(format!(
                "cannot create state_dir '{}': {}",
                config.state_dir.display(),
                e
            ))
        })?;
        // SEC-4: restrict state directory to owner-only on Unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&config.state_dir, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| {
                    OrchestratorError::Config(format!(
                        "cannot set permissions on state_dir '{}': {}",
                        config.state_dir.display(),
                        e
                    ))
                })?;
        }
    }

    // Database pool bounds
    if config.database.max_connections < 1 {
        return Err(OrchestratorError::Config(
            "database.max_connections must be >= 1".into(),
        ));
    }
    if config.database.min_connections < 1 {
        return Err(OrchestratorError::Config(
            "database.min_connections must be >= 1".into(),
        ));
    }
    if config.database.min_connections > config.database.max_connections {
        return Err(OrchestratorError::Config(
            "database.min_connections must be <= database.max_connections".into(),
        ));
    }
    if config.database.acquire_timeout_ms < 100 {
        return Err(OrchestratorError::Config(
            "database.acquire_timeout_ms must be >= 100".into(),
        ));
    }

    // Validate max_concurrent_triggers
    if let Some(max) = config.orchestration.max_concurrent_triggers {
        if max < 1 {
            return Err(OrchestratorError::Config(
                "orchestration.max_concurrent_triggers must be >= 1".into(),
            ));
        }
        let worker_count = config
            .agents
            .iter()
            .filter(|a| a.role == AgentRole::Worker)
            .count();
        if worker_count > 0 && max > worker_count {
            tracing::warn!(
                "max_concurrent_triggers ({}) exceeds worker agent count ({}); \
                 effective parallelism is limited by agent count",
                max,
                worker_count
            );
        }
    }

    for intent in &config.orchestration.trigger_intents {
        if !is_valid_intent_slug(intent) {
            return Err(OrchestratorError::Config(format!(
                "invalid trigger intent '{}': expected lowercase slug format",
                intent
            )));
        }
    }

    // ── Schedule validation (CRON-1) ──
    if let Some(ref schedules) = config.schedules {
        let mut seen_names = HashSet::new();
        for sched in schedules {
            if sched.name.is_empty() {
                return Err(OrchestratorError::Config(
                    "schedules: name must not be empty".into(),
                ));
            }
            if !seen_names.insert(&sched.name) {
                return Err(OrchestratorError::Config(format!(
                    "schedules: duplicate schedule name '{}'",
                    sched.name
                )));
            }
            if sched.agent.is_empty() {
                return Err(OrchestratorError::Config(format!(
                    "schedules: schedule '{}' agent must not be empty",
                    sched.name
                )));
            }
            if sched.body.is_empty() {
                return Err(OrchestratorError::Config(format!(
                    "schedules: schedule '{}' body must not be empty",
                    sched.name
                )));
            }
            if !all_aliases.contains(sched.agent.as_str()) {
                return Err(OrchestratorError::Config(format!(
                    "schedules: schedule '{}' references unknown agent alias '{}'",
                    sched.name, sched.agent
                )));
            }
            if sched.max_runs == 0 {
                return Err(OrchestratorError::Config(format!(
                    "schedules: schedule '{}' max_runs must be > 0",
                    sched.name
                )));
            }
            // Validate cron expression syntax via croner
            if let Err(e) = sched.cron.parse::<croner::Cron>() {
                return Err(OrchestratorError::Config(format!(
                    "schedules: schedule '{}' has invalid cron expression '{}': {}",
                    sched.name, sched.cron, e
                )));
            }
        }
    }

    // ── Backend definitions validation (GBE-1) ──
    if let Some(ref defs) = config.backend_definitions {
        let mut seen_names = HashSet::new();
        for def in defs {
            if def.name.is_empty() {
                return Err(OrchestratorError::Config(
                    "backend_definitions: name must not be empty".into(),
                ));
            }
            if BUILTIN_BACKEND_NAMES.contains(&def.name.as_str()) {
                return Err(OrchestratorError::Config(format!(
                    "backend_definitions: name '{}' conflicts with built-in backend",
                    def.name
                )));
            }
            if !seen_names.insert(&def.name) {
                return Err(OrchestratorError::Config(format!(
                    "backend_definitions: duplicate name '{}'",
                    def.name
                )));
            }
            if def.command.is_empty() {
                return Err(OrchestratorError::Config(format!(
                    "backend_definitions: '{}' command must not be empty",
                    def.name
                )));
            }
            if def.args.iter().any(|a| a.trim().is_empty()) {
                return Err(OrchestratorError::Config(format!(
                    "backend_definitions: '{}' args must not contain empty entries",
                    def.name
                )));
            }
            if let Some(ref env_remove) = def.env_remove {
                if env_remove.iter().any(|v| v.trim().is_empty()) {
                    return Err(OrchestratorError::Config(format!(
                        "backend_definitions: '{}' env_remove must not contain empty entries",
                        def.name
                    )));
                }
            }
            if let Some(ref resume) = def.resume {
                if resume.session_id_arg.is_empty() {
                    return Err(OrchestratorError::Config(format!(
                        "backend_definitions: '{}' resume.session_id_arg must not be empty",
                        def.name
                    )));
                }
            }
            // result_field / session_id_field only make sense for json/jsonl formats
            if def.output.format == OutputFormat::Plaintext {
                if def.output.result_field.is_some() {
                    return Err(OrchestratorError::Config(format!(
                        "backend_definitions: '{}' output.result_field is only valid for json/jsonl format",
                        def.name
                    )));
                }
                if def.output.session_id_field.is_some() {
                    return Err(OrchestratorError::Config(format!(
                        "backend_definitions: '{}' output.session_id_field is only valid for json/jsonl format",
                        def.name
                    )));
                }
            }
        }
    }

    // ── Advisory warnings (SEC-3, SEC-5) ──
    for warning in collect_config_warnings(config) {
        tracing::warn!("{}", warning);
    }

    Ok(())
}

/// Known bypass/dangerous flags and their associated built-in backend.
///
/// The backend field indicates which backend **auto-injects** the flag. A sentinel
/// value `"_none"` means no backend injects this flag — it is always user-supplied
/// and should always produce a "dangerous flag" warning, never a "duplicate" warning.
const KNOWN_BYPASS_FLAGS: &[(&str, &str)] = &[
    ("--dangerously-skip-permissions", "claude"),
    // codex detects this flag to suppress --full-auto but never injects it
    ("--dangerously-bypass-approvals-and-sandbox", "_none"),
    ("--yolo", "gemini"),
    ("--full-auto", "codex"),
];

/// Environment variable names that are security-sensitive.
const SENSITIVE_ENV_VARS: &[&str] = &[
    "PATH",
    "HOME",
    "SHELL",
    "USER",
    "LD_PRELOAD",
    "LD_LIBRARY_PATH",
    "DYLD_INSERT_LIBRARIES",
    "DYLD_LIBRARY_PATH",
];

/// Collect advisory config warnings without rejecting the config.
///
/// Called by [`validate_config`] to emit `tracing::warn!` messages. Tests can
/// call this directly and assert on the returned `Vec<String>`.
pub(crate) fn collect_config_warnings(config: &OrchestratorConfig) -> Vec<String> {
    let mut warnings = Vec::new();

    for agent in &config.agents {
        // SEC-3: Warn on dangerous flags in backend_args
        if let Some(ref backend_args) = agent.backend_args {
            for arg in backend_args {
                for &(flag, flag_backend) in KNOWN_BYPASS_FLAGS {
                    if arg == flag {
                        if agent.backend == flag_backend {
                            warnings.push(format!(
                                "agent '{}' backend_args includes '{}' which is already applied by the {} backend (duplicate flag)",
                                agent.alias, flag, agent.backend
                            ));
                        } else {
                            warnings.push(format!(
                                "agent '{}' backend_args includes dangerous flag '{}'",
                                agent.alias, flag
                            ));
                        }
                    }
                }
            }
        }

        // SEC-5: Warn on security-sensitive env var overrides
        if let Some(ref env) = agent.env {
            let mut keys: Vec<&String> = env.keys().collect();
            keys.sort();
            for key in keys {
                if SENSITIVE_ENV_VARS.contains(&key.as_str()) {
                    warnings.push(format!(
                        "agent '{}' env overrides security-sensitive variable '{}'",
                        agent.alias, key
                    ));
                }
            }
        }
    }

    warnings
}

fn is_valid_intent_slug(intent: &str) -> bool {
    if intent.is_empty() || intent.starts_with('-') || intent.ends_with('-') {
        return false;
    }
    let mut prev_dash = false;
    for ch in intent.chars() {
        let ok = ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-';
        if !ok {
            return false;
        }
        if ch == '-' {
            if prev_dash {
                return false;
            }
            prev_dash = true;
        } else {
            prev_dash = false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::types::{AgentConfig, AgentRole, HandoffTarget, OrchestratorConfig};
    use std::path::PathBuf;

    fn minimal_config() -> OrchestratorConfig {
        OrchestratorConfig {
            default_workdir: PathBuf::from("/tmp"),
            state_dir: PathBuf::from("/tmp/test-mail"),
            poll_interval_secs: 5,
            models: None,
            agents: vec![AgentConfig {
                alias: "focused".into(),
                role: AgentRole::Worker,
                backend: "stub".into(),

                model: None,
                prompt: None,
                prompt_file: None,
                timeout_secs: None,

                backend_args: None,
                env: None,
                workdir: None,
                workspace: None,
                max_retries: 0,
                retry_backoff_secs: 30,
                handoff: None,
            }],
            worktree_dir: None,
            orchestration: Default::default(),
            database: Default::default(),
            notifications: Default::default(),
            backend_definitions: None,
            hooks: None,
            schedules: None,
        }
    }

    #[test]
    fn test_db_path_is_derived_from_state_dir() {
        let config = minimal_config();
        assert_eq!(
            config.db_path(),
            PathBuf::from("/tmp/test-mail").join("jobs.sqlite")
        );
    }

    #[test]
    fn test_config_validation_minimal_valid() {
        let config = minimal_config();
        assert!(validate_config(&config).is_ok());
    }

    #[test]
    fn test_config_validation_no_agents() {
        let mut config = minimal_config();
        config.agents.clear();
        let err = validate_config(&config).unwrap_err();
        assert!(err.to_string().contains("at least one agent"));
    }

    #[test]
    fn test_config_validation_missing_default_workdir() {
        let mut config = minimal_config();
        config.default_workdir = PathBuf::new();
        let err = validate_config(&config).unwrap_err();
        assert!(err
            .to_string()
            .contains("default_workdir must not be empty"));
    }

    #[test]
    fn test_config_validation_nonexistent_default_workdir() {
        let mut config = minimal_config();
        config.default_workdir = PathBuf::from("/definitely/nonexistent/project-root");
        let err = validate_config(&config).unwrap_err();
        assert!(err.to_string().contains("default_workdir does not exist"));
    }

    #[test]
    fn test_config_validation_empty_alias() {
        let mut config = minimal_config();
        config.agents[0].alias = String::new();
        let err = validate_config(&config).unwrap_err();
        assert!(err.to_string().contains("alias must not be empty"));
    }

    #[test]
    fn test_config_validation_empty_backend() {
        let mut config = minimal_config();
        config.agents[0].backend = String::new();
        let err = validate_config(&config).unwrap_err();
        assert!(err.to_string().contains("backend must not be empty"));
    }

    #[test]
    fn test_config_validation_duplicate_alias() {
        let mut config = minimal_config();
        config.agents.push(AgentConfig {
            alias: "focused".into(),
            backend: "stub".into(),
            role: AgentRole::Worker,
            model: None,
            prompt: None,
            prompt_file: None,
            timeout_secs: None,

            backend_args: None,
            env: None,
            workdir: None,
            workspace: None,
            max_retries: 0,
            retry_backoff_secs: 30,
            handoff: None,
        });
        let err = validate_config(&config).unwrap_err();
        assert!(err.to_string().contains("duplicate agent alias"));
    }

    #[test]
    fn test_config_validation_zero_poll_interval() {
        let mut config = minimal_config();
        config.poll_interval_secs = 0;
        let err = validate_config(&config).unwrap_err();
        assert!(err.to_string().contains("poll_interval_secs"));
    }

    #[test]
    fn test_config_yaml_deserialization() {
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/test-mail
poll_interval_secs: 10
agents:
  - alias: focused
    backend: stub
  - alias: chill
    backend: opencode
"#;
        let config: OrchestratorConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.agents.len(), 2);
        assert_eq!(config.poll_interval_secs, 10);
        assert!(validate_config(&config).is_ok());
    }

    #[test]
    fn test_hooks_config_yaml_roundtrip() {
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/test
agents:
  - alias: a1
    backend: stub
hooks:
  on_execution_started:
    - command: notify.sh
      args: ["--event"]
      timeout_secs: 5
      env:
        WEBHOOK_URL: https://example.com
  on_execution_completed: []
"#;
        let config: OrchestratorConfig = serde_yaml::from_str(yaml).unwrap();
        let hooks = config.hooks.as_ref().unwrap();
        assert_eq!(hooks.on_execution_started.len(), 1);
        assert_eq!(hooks.on_execution_started[0].command, "notify.sh");
        assert_eq!(hooks.on_execution_started[0].timeout_secs, 5);
        assert_eq!(
            hooks.on_execution_started[0].env.as_ref().unwrap()["WEBHOOK_URL"],
            "https://example.com"
        );
        assert!(hooks.on_execution_completed.is_empty());
        assert!(hooks.on_thread_closed.is_empty());
        assert!(hooks.on_thread_failed.is_empty());
        assert!(validate_config(&config).is_ok());
    }

    #[test]
    fn test_hooks_config_absent_is_backward_compatible() {
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/test
agents:
  - alias: a1
    backend: stub
"#;
        let config: OrchestratorConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(config.hooks.is_none());
        assert!(validate_config(&config).is_ok());
    }

    #[test]
    fn test_config_validation_zero_timeout() {
        let mut config = minimal_config();
        config.agents[0].timeout_secs = Some(0);
        let err = validate_config(&config).unwrap_err();
        assert!(err.to_string().contains("timeout_secs must be > 0"));
    }

    #[test]
    fn test_config_validation_db_pool_bounds() {
        let mut config = minimal_config();
        config.database.min_connections = 8;
        config.database.max_connections = 4;
        let err = validate_config(&config).unwrap_err();
        assert!(err.to_string().contains("min_connections"));
    }

    #[test]
    fn test_config_validation_db_acquire_timeout_too_low() {
        let mut config = minimal_config();
        config.database.acquire_timeout_ms = 50;
        let err = validate_config(&config).unwrap_err();
        assert!(err.to_string().contains("acquire_timeout_ms"));
    }

    #[test]
    fn test_config_validation_nonexistent_prompt_file() {
        let mut config = minimal_config();
        config.agents[0].prompt_file = Some(PathBuf::from("/nonexistent/prompt.txt"));
        let err = validate_config(&config).unwrap_err();
        assert!(err.to_string().contains("prompt_file does not exist"));
    }

    #[test]
    fn test_config_yaml_with_new_fields() {
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/test-mail
poll_interval_secs: 5
agents:
  - alias: focused
    backend: claude
    model: claude-opus-4-6
    prompt: "You are the compiler engineer."
    timeout_secs: 300
    env:
      ANTHROPIC_API_KEY: test-key
"#;
        let config: OrchestratorConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.agents[0].model.as_deref(), Some("claude-opus-4-6"));
        assert!(config.agents[0].prompt.is_some());
        assert_eq!(config.agents[0].timeout_secs, Some(300));
        let env = config.agents[0].env.as_ref().unwrap();
        assert_eq!(env.get("ANTHROPIC_API_KEY").unwrap(), "test-key");
    }

    #[test]
    fn test_config_validation_invalid_trigger_intent() {
        let mut config = minimal_config();
        config.orchestration.trigger_intents = vec!["Invalid Intent".into()];
        let err = validate_config(&config).unwrap_err();
        assert!(err.to_string().contains("invalid trigger intent"));
    }

    #[test]
    fn test_config_validation_poll_interval_too_high() {
        let mut config = minimal_config();
        config.poll_interval_secs = 3601;
        let err = validate_config(&config).unwrap_err();
        assert!(err.to_string().contains("poll_interval_secs"));
    }

    #[test]
    fn test_config_validation_poll_interval_at_bounds() {
        let mut config = minimal_config();
        config.poll_interval_secs = 1;
        assert!(validate_config(&config).is_ok());
        config.poll_interval_secs = 3600;
        assert!(validate_config(&config).is_ok());
    }

    #[test]
    fn test_config_validation_stale_active_secs_out_of_bounds() {
        let mut config = minimal_config();
        config.orchestration.stale_active_secs = 59;
        let err = validate_config(&config).unwrap_err();
        assert!(err.to_string().contains("stale_active_secs"));

        config.orchestration.stale_active_secs = 604_801;
        let err = validate_config(&config).unwrap_err();
        assert!(err.to_string().contains("stale_active_secs"));
    }

    #[test]
    fn test_config_validation_stale_active_secs_valid() {
        let mut config = minimal_config();
        config.orchestration.stale_active_secs = 3_600;
        assert!(validate_config(&config).is_ok());
    }

    #[test]
    fn test_config_validation_state_dir_created() {
        let dir = tempfile::tempdir().unwrap();
        let new_path = dir.path().join("new-state");
        let mut config = minimal_config();
        config.state_dir = new_path.clone();
        assert!(validate_config(&config).is_ok());
        assert!(new_path.exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&new_path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "state_dir must be created with mode 0700");
        }
    }

    #[test]
    fn test_config_validation_max_concurrent_triggers_zero() {
        let mut config = minimal_config();
        config.orchestration.max_concurrent_triggers = Some(0);
        let err = validate_config(&config).unwrap_err();
        assert!(err
            .to_string()
            .contains("max_concurrent_triggers must be >= 1"));
    }

    #[test]
    fn test_config_validation_max_concurrent_triggers_valid() {
        let mut config = minimal_config();
        config.orchestration.max_concurrent_triggers = Some(1);
        assert!(validate_config(&config).is_ok());
        config.orchestration.max_concurrent_triggers = Some(5);
        assert!(validate_config(&config).is_ok());
    }

    #[test]
    fn test_effective_max_concurrent_defaults_to_worker_count() {
        let mut config = minimal_config();
        // minimal_config has 1 worker agent
        assert_eq!(config.effective_max_concurrent_triggers(), 1);

        // Add a second worker
        config.agents.push(AgentConfig {
            alias: "spark".into(),
            backend: "stub".into(),
            role: AgentRole::Worker,
            model: None,
            prompt: None,
            prompt_file: None,
            timeout_secs: None,

            backend_args: None,
            env: None,
            workdir: None,
            workspace: None,
            max_retries: 0,
            retry_backoff_secs: 30,
            handoff: None,
        });
        assert_eq!(config.effective_max_concurrent_triggers(), 2);

        // Operator agents don't count
        config.agents.push(AgentConfig {
            alias: "operator".into(),
            backend: "stub".into(),
            role: AgentRole::Operator,
            model: None,
            prompt: None,
            prompt_file: None,
            timeout_secs: None,

            backend_args: None,
            env: None,
            workdir: None,
            workspace: None,
            max_retries: 0,
            retry_backoff_secs: 30,
            handoff: None,
        });
        assert_eq!(config.effective_max_concurrent_triggers(), 2);

        // Explicit override takes precedence
        config.orchestration.max_concurrent_triggers = Some(1);
        assert_eq!(config.effective_max_concurrent_triggers(), 1);
    }

    #[test]
    fn test_config_validation_backend_args_no_empty_entries() {
        let mut config = minimal_config();
        config.agents[0].backend_args = Some(vec!["--ok".into(), "  ".into()]);
        let err = validate_config(&config).unwrap_err();
        assert!(err.to_string().contains("backend_args"));
    }

    #[test]
    fn test_agent_model_field_is_allowed() {
        let mut config = minimal_config();
        config.agents[0].model = Some("claude-opus-4-6".into());
        assert!(validate_config(&config).is_ok());
    }

    #[test]
    fn test_model_entry_deserialize_plain_string() {
        use crate::config::types::ModelEntry;
        let entry: ModelEntry = serde_yaml::from_str("claude-opus-4-6").unwrap();
        assert_eq!(entry.id, "claude-opus-4-6");
        assert_eq!(entry.description, None);
    }

    #[test]
    fn test_model_entry_deserialize_object_with_description() {
        use crate::config::types::ModelEntry;
        let entry: ModelEntry =
            serde_yaml::from_str("id: opus\ndescription: Very capable").unwrap();
        assert_eq!(entry.id, "opus");
        assert_eq!(entry.description.as_deref(), Some("Very capable"));
    }

    #[test]
    fn test_model_entry_deserialize_object_without_description() {
        use crate::config::types::ModelEntry;
        let entry: ModelEntry = serde_yaml::from_str("id: sonnet").unwrap();
        assert_eq!(entry.id, "sonnet");
        assert_eq!(entry.description, None);
    }

    #[test]
    fn test_model_entry_deserialize_with_timeout_secs() {
        use crate::config::types::ModelEntry;
        let entry: ModelEntry =
            serde_yaml::from_str("id: glm-5\nbackend: opencode\ntimeout_secs: 300").unwrap();
        assert_eq!(entry.id, "glm-5");
        assert_eq!(entry.backend.as_deref(), Some("opencode"));
        assert_eq!(entry.timeout_secs, Some(300));
    }

    #[test]
    fn test_model_entry_plain_string_has_no_timeout() {
        use crate::config::types::ModelEntry;
        let entry: ModelEntry = serde_yaml::from_str("claude-opus-4-6").unwrap();
        assert_eq!(entry.timeout_secs, None);
    }

    #[test]
    fn test_model_entry_deserialize_with_backend() {
        use crate::config::types::ModelEntry;
        let entry: ModelEntry =
            serde_yaml::from_str("id: opus\nbackend: claude\ndescription: Best").unwrap();
        assert_eq!(entry.id, "opus");
        assert_eq!(entry.backend.as_deref(), Some("claude"));
        assert_eq!(entry.description.as_deref(), Some("Best"));
    }

    #[test]
    fn test_global_models_catalog_roundtrip() {
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/test
models:
  - id: opus
    backend: claude
    description: "Deep reasoning"
  - id: glm-5
    backend: opencode
agents:
  - alias: a1
    backend: claude
    model: opus
"#;
        let config = crate::config::load_config_from_str(yaml).unwrap();
        let global = config.models.unwrap();
        assert_eq!(global.len(), 2);
        assert_eq!(global[0].id, "opus");
        assert_eq!(global[0].backend.as_deref(), Some("claude"));
        assert_eq!(global[1].id, "glm-5");
        assert_eq!(global[1].backend.as_deref(), Some("opencode"));
        assert_eq!(config.agents[0].model.as_deref(), Some("opus"));
    }

    #[test]
    fn test_legacy_agent_model_fields_are_rejected() {
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/test
models:
  - id: opus
    backend: claude
    description: "Deep reasoning"
  - id: glm-5
    backend: opencode
agents:
  - alias: a1
    backend: claude
    model: opus
    preferred_models: [opus, glm-5]
    models:
      - id: sonnet
"#;
        let err = crate::config::load_config_from_str(yaml).unwrap_err();
        assert!(err.to_string().contains("unknown field"));
        assert!(err.to_string().contains("preferred_models"));
    }

    #[test]
    fn test_legacy_db_path_field_is_rejected() {
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/test
db_path: /tmp/custom.sqlite
agents:
  - alias: a1
    backend: claude
"#;
        let err = crate::config::load_config_from_str(yaml).unwrap_err();
        assert!(err.to_string().contains("unknown field"));
        assert!(err.to_string().contains("db_path"));
    }

    #[test]
    fn test_legacy_project_root_field_is_rejected() {
        let yaml = r#"
project_root: /tmp
state_dir: /tmp/test
agents:
  - alias: a1
    backend: claude
"#;
        let err = crate::config::load_config_from_str(yaml).unwrap_err();
        assert!(err.to_string().contains("unknown field"));
        assert!(err.to_string().contains("project_root"));
    }

    // ── Handoff config validation tests (ORCH-CHAIN-1) ──

    #[test]
    fn test_handoff_valid_simple_targets() {
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/test
agents:
  - alias: coder
    backend: stub
    handoff:
      on_response: reviewer
  - alias: reviewer
    backend: stub
"#;
        let config = crate::config::load_config_from_str(yaml).unwrap();
        assert!(config.agents[0].handoff.is_some());
        assert!(matches!(
            config.agents[0].handoff.as_ref().unwrap().on_response,
            Some(HandoffTarget::Single(ref s)) if s == "reviewer"
        ));
    }

    #[test]
    fn test_handoff_invalid_target_alias_rejected() {
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/test
agents:
  - alias: coder
    backend: stub
    handoff:
      on_response: nonexistent
"#;
        let err = crate::config::load_config_from_str(yaml).unwrap_err();
        assert!(err
            .to_string()
            .contains("unknown agent alias 'nonexistent'"));
    }

    #[test]
    fn test_handoff_self_loop_rejected() {
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/test
agents:
  - alias: coder
    backend: stub
    handoff:
      on_response: coder
"#;
        let err = crate::config::load_config_from_str(yaml).unwrap_err();
        assert!(err.to_string().contains("self-loop"));
    }

    #[test]
    fn test_handoff_max_chain_depth_zero_rejected() {
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/test
agents:
  - alias: coder
    backend: stub
    handoff:
      max_chain_depth: 0
"#;
        let err = crate::config::load_config_from_str(yaml).unwrap_err();
        assert!(err.to_string().contains("max_chain_depth"));
    }

    #[test]
    fn test_handoff_max_chain_depth_bounds() {
        // Depth 1 is valid
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/test
agents:
  - alias: coder
    backend: stub
    handoff:
      max_chain_depth: 1
"#;
        assert!(crate::config::load_config_from_str(yaml).is_ok());

        // Depth 20 is valid
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/test
agents:
  - alias: coder
    backend: stub
    handoff:
      max_chain_depth: 20
"#;
        assert!(crate::config::load_config_from_str(yaml).is_ok());

        // Depth 21 is rejected
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/test
agents:
  - alias: coder
    backend: stub
    handoff:
      max_chain_depth: 21
"#;
        let err = crate::config::load_config_from_str(yaml).unwrap_err();
        assert!(err.to_string().contains("max_chain_depth"));
    }

    #[test]
    fn test_handoff_operator_target_is_valid() {
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/test
agents:
  - alias: coder
    backend: stub
    handoff:
      on_response: operator
"#;
        assert!(crate::config::load_config_from_str(yaml).is_ok());
    }

    #[test]
    fn test_handoff_no_config_preserves_behavior() {
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/test
agents:
  - alias: coder
    backend: stub
"#;
        let config = crate::config::load_config_from_str(yaml).unwrap();
        assert!(config.agents[0].handoff.is_none());
    }

    // ── Fan-out validation tests (ORCH-HANDOFF-2) ──

    #[test]
    fn test_handoff_fanout_valid() {
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/test
agents:
  - alias: coder
    backend: stub
    handoff:
      on_response:
        - reviewer
        - reviewer-2
  - alias: reviewer
    backend: stub
  - alias: reviewer-2
    backend: stub
"#;
        assert!(crate::config::load_config_from_str(yaml).is_ok());
    }

    #[test]
    fn test_handoff_fanout_single_element_valid() {
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/test
agents:
  - alias: coder
    backend: stub
    handoff:
      on_response:
        - reviewer
  - alias: reviewer
    backend: stub
"#;
        assert!(crate::config::load_config_from_str(yaml).is_ok());
    }

    #[test]
    fn test_handoff_fanout_duplicates_rejected() {
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/test
agents:
  - alias: coder
    backend: stub
    handoff:
      on_response:
        - reviewer
        - reviewer
  - alias: reviewer
    backend: stub
"#;
        let err = crate::config::load_config_from_str(yaml).unwrap_err();
        assert!(err.to_string().contains("duplicate target"));
    }

    #[test]
    fn test_handoff_fanout_self_loop_rejected() {
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/test
agents:
  - alias: coder
    backend: stub
    handoff:
      on_response:
        - coder
  - alias: reviewer
    backend: stub
"#;
        let err = crate::config::load_config_from_str(yaml).unwrap_err();
        assert!(err.to_string().contains("self-loop"));
    }

    #[test]
    fn test_handoff_fanout_operator_rejected() {
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/test
agents:
  - alias: coder
    backend: stub
    handoff:
      on_response:
        - reviewer
        - operator
  - alias: reviewer
    backend: stub
"#;
        let err = crate::config::load_config_from_str(yaml).unwrap_err();
        assert!(err.to_string().contains("must not contain 'operator'"));
    }

    #[test]
    fn test_handoff_fanout_empty_rejected() {
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/test
agents:
  - alias: coder
    backend: stub
    handoff:
      on_response: []
"#;
        let err = crate::config::load_config_from_str(yaml).unwrap_err();
        assert!(err.to_string().contains("must not be empty"));
    }

    #[test]
    fn test_handoff_fanout_yaml_roundtrip() {
        // String form
        let yaml_single = r#"
default_workdir: /tmp
state_dir: /tmp/test
agents:
  - alias: coder
    backend: stub
    handoff:
      on_response: reviewer
  - alias: reviewer
    backend: stub
"#;
        let config = crate::config::load_config_from_str(yaml_single).unwrap();
        let handoff = config.agents[0].handoff.as_ref().unwrap();
        assert!(matches!(
            handoff.on_response,
            Some(HandoffTarget::Single(ref s)) if s == "reviewer"
        ));

        // List form
        let yaml_list = r#"
default_workdir: /tmp
state_dir: /tmp/test
agents:
  - alias: coder
    backend: stub
    handoff:
      on_response:
        - reviewer
        - reviewer-2
  - alias: reviewer
    backend: stub
  - alias: reviewer-2
    backend: stub
"#;
        let config = crate::config::load_config_from_str(yaml_list).unwrap();
        let handoff = config.agents[0].handoff.as_ref().unwrap();
        assert!(matches!(&handoff.on_response, Some(HandoffTarget::FanOut(v)) if v.len() == 2));
    }

    #[test]
    fn test_config_backward_compat_target_repo_root_alias() {
        let yaml = r#"
target_repo_root: /tmp
state_dir: /tmp/test-state
agents:
  - alias: a
    backend: stub
"#;
        let config: OrchestratorConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.default_workdir, PathBuf::from("/tmp"));
        assert!(validate_config(&config).is_ok());
    }

    // ── Backend definitions validation tests (GBE-1) ──

    #[test]
    fn test_backend_definitions_valid_minimal() {
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/test
agents:
  - alias: a1
    backend: aider
backend_definitions:
  - name: aider
    command: aider
"#;
        let config = crate::config::load_config_from_str(yaml).unwrap();
        let defs = config.backend_definitions.unwrap();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "aider");
        assert_eq!(defs[0].command, "aider");
        assert!(defs[0].args.is_empty());
        assert!(defs[0].resume.is_none());
        assert!(defs[0].ping.is_none());
        assert!(defs[0].env_remove.is_none());
    }

    #[test]
    fn test_backend_definitions_valid_full() {
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/test
agents:
  - alias: a1
    backend: my-tool
backend_definitions:
  - name: my-tool
    command: /usr/local/bin/my-tool
    args:
      - "--model"
      - "{{model}}"
      - "--prompt"
      - "{{instruction}}"
    resume:
      flag: "--resume"
      session_id_arg: "{{session_id}}"
    output:
      format: json
      result_field: response
      session_id_field: sid
    ping:
      command: my-tool
      args: ["--version"]
    env_remove:
      - SOME_VAR
"#;
        let config = crate::config::load_config_from_str(yaml).unwrap();
        let defs = config.backend_definitions.unwrap();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "my-tool");
        assert_eq!(defs[0].args.len(), 4);
        assert!(defs[0].resume.is_some());
        let resume = defs[0].resume.as_ref().unwrap();
        assert_eq!(resume.flag, "--resume");
        assert_eq!(resume.session_id_arg, "{{session_id}}");
        assert_eq!(
            defs[0].output.format,
            crate::config::types::OutputFormat::Json
        );
        assert_eq!(defs[0].output.result_field.as_deref(), Some("response"));
        assert_eq!(defs[0].output.session_id_field.as_deref(), Some("sid"));
        let ping = defs[0].ping.as_ref().unwrap();
        assert_eq!(ping.command, "my-tool");
        assert_eq!(ping.args, vec!["--version"]);
        assert_eq!(
            defs[0].env_remove.as_ref().unwrap(),
            &vec!["SOME_VAR".to_string()]
        );
    }

    #[test]
    fn test_backend_definitions_empty_name_rejected() {
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/test
agents:
  - alias: a1
    backend: stub
backend_definitions:
  - name: ""
    command: some-cmd
"#;
        let err = crate::config::load_config_from_str(yaml).unwrap_err();
        assert!(err.to_string().contains("name must not be empty"));
    }

    #[test]
    fn test_backend_definitions_duplicate_name_rejected() {
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/test
agents:
  - alias: a1
    backend: stub
backend_definitions:
  - name: aider
    command: aider
  - name: aider
    command: aider2
"#;
        let err = crate::config::load_config_from_str(yaml).unwrap_err();
        assert!(err.to_string().contains("duplicate name 'aider'"));
    }

    #[test]
    fn test_backend_definitions_builtin_name_conflict_rejected() {
        for builtin in &["claude", "codex", "gemini", "opencode"] {
            let yaml = format!(
                r#"
default_workdir: /tmp
state_dir: /tmp/test
agents:
  - alias: a1
    backend: stub
backend_definitions:
  - name: {builtin}
    command: some-cmd
"#
            );
            let err = crate::config::load_config_from_str(&yaml).unwrap_err();
            assert!(
                err.to_string().contains("conflicts with built-in backend"),
                "expected built-in conflict error for '{builtin}', got: {err}"
            );
        }
    }

    #[test]
    fn test_backend_definitions_empty_command_rejected() {
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/test
agents:
  - alias: a1
    backend: stub
backend_definitions:
  - name: aider
    command: ""
"#;
        let err = crate::config::load_config_from_str(yaml).unwrap_err();
        assert!(err.to_string().contains("command must not be empty"));
    }

    #[test]
    fn test_backend_definitions_absent_is_backward_compatible() {
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/test
agents:
  - alias: a1
    backend: stub
"#;
        let config = crate::config::load_config_from_str(yaml).unwrap();
        assert!(config.backend_definitions.is_none());
    }

    #[test]
    fn test_backend_definitions_empty_vec_is_valid() {
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/test
agents:
  - alias: a1
    backend: stub
backend_definitions: []
"#;
        let config = crate::config::load_config_from_str(yaml).unwrap();
        assert_eq!(config.backend_definitions.unwrap().len(), 0);
    }

    #[test]
    fn test_backend_definitions_output_format_jsonl() {
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/test
agents:
  - alias: a1
    backend: my-tool
backend_definitions:
  - name: my-tool
    command: my-tool
    output:
      format: jsonl
      result_field: text
"#;
        let config = crate::config::load_config_from_str(yaml).unwrap();
        let defs = config.backend_definitions.unwrap();
        assert_eq!(
            defs[0].output.format,
            crate::config::types::OutputFormat::Jsonl
        );
        assert_eq!(defs[0].output.result_field.as_deref(), Some("text"));
    }

    #[test]
    fn test_backend_definitions_plaintext_with_result_field_rejected() {
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/test
agents:
  - alias: a1
    backend: stub
backend_definitions:
  - name: my-tool
    command: my-tool
    output:
      format: plaintext
      result_field: text
"#;
        let err = crate::config::load_config_from_str(yaml).unwrap_err();
        assert!(err
            .to_string()
            .contains("result_field is only valid for json/jsonl"));
    }

    #[test]
    fn test_backend_definitions_plaintext_with_session_id_field_rejected() {
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/test
agents:
  - alias: a1
    backend: stub
backend_definitions:
  - name: my-tool
    command: my-tool
    output:
      format: plaintext
      session_id_field: sid
"#;
        let err = crate::config::load_config_from_str(yaml).unwrap_err();
        assert!(err
            .to_string()
            .contains("session_id_field is only valid for json/jsonl"));
    }

    #[test]
    fn test_backend_definitions_multiple_valid() {
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/test
agents:
  - alias: a1
    backend: aider
  - alias: a2
    backend: custom
backend_definitions:
  - name: aider
    command: aider
  - name: custom
    command: /opt/bin/custom-tool
    args: ["--prompt", "{{instruction}}"]
"#;
        let config = crate::config::load_config_from_str(yaml).unwrap();
        let defs = config.backend_definitions.unwrap();
        assert_eq!(defs.len(), 2);
        assert_eq!(defs[0].name, "aider");
        assert_eq!(defs[1].name, "custom");
    }

    #[test]
    fn test_backend_definitions_unknown_field_rejected() {
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/test
agents:
  - alias: a1
    backend: stub
backend_definitions:
  - name: aider
    command: aider
    model_flag: "--model"
"#;
        let err = crate::config::load_config_from_str(yaml).unwrap_err();
        assert!(err.to_string().contains("unknown field"));
    }

    #[test]
    fn test_backend_definitions_output_default_is_plaintext() {
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/test
agents:
  - alias: a1
    backend: aider
backend_definitions:
  - name: aider
    command: aider
"#;
        let config = crate::config::load_config_from_str(yaml).unwrap();
        let defs = config.backend_definitions.unwrap();
        assert_eq!(
            defs[0].output.format,
            crate::config::types::OutputFormat::Plaintext
        );
        assert!(defs[0].output.result_field.is_none());
        assert!(defs[0].output.session_id_field.is_none());
    }

    // ── Merge config validation tests (MERGE-7) ──

    #[test]
    fn test_config_merge_fields_default_when_absent() {
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/test
agents:
  - alias: a1
    backend: stub
"#;
        let config: OrchestratorConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.orchestration.merge_timeout_secs, 30);
        assert_eq!(config.orchestration.default_merge_strategy, "merge");
        assert_eq!(config.orchestration.default_merge_target, "main");
        assert!(validate_config(&config).is_ok());
    }

    #[test]
    fn test_config_merge_fields_explicit_values_round_trip() {
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/test
agents:
  - alias: a1
    backend: stub
orchestration:
  merge_timeout_secs: 60
  default_merge_strategy: rebase
"#;
        let config: OrchestratorConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.orchestration.merge_timeout_secs, 60);
        assert_eq!(config.orchestration.default_merge_strategy, "rebase");
        assert!(validate_config(&config).is_ok());
    }

    #[test]
    fn test_config_merge_strategy_invalid_rejected() {
        let mut config = minimal_config();
        config.orchestration.default_merge_strategy = "bogus".into();
        let err = validate_config(&config).unwrap_err();
        assert!(err.to_string().contains("default_merge_strategy must be"));
        assert!(err.to_string().contains("bogus"));
    }

    #[test]
    fn test_config_merge_target_default_when_absent() {
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/test
agents:
  - alias: a1
    backend: stub
"#;
        let config: OrchestratorConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.orchestration.default_merge_target, "main");
    }

    #[test]
    fn test_config_merge_target_explicit_value() {
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/test
agents:
  - alias: a1
    backend: stub
orchestration:
  default_merge_target: develop
"#;
        let config: OrchestratorConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.orchestration.default_merge_target, "develop");
        assert!(validate_config(&config).is_ok());
    }

    #[test]
    fn test_config_merge_target_empty_rejected() {
        let mut config = minimal_config();
        config.orchestration.default_merge_target = "".into();
        let err = validate_config(&config).unwrap_err();
        assert!(err
            .to_string()
            .contains("default_merge_target must not be empty"));
    }

    #[test]
    fn test_config_merge_target_whitespace_rejected() {
        let mut config = minimal_config();
        config.orchestration.default_merge_target = "  ".into();
        let err = validate_config(&config).unwrap_err();
        assert!(err
            .to_string()
            .contains("default_merge_target must not be empty"));
    }

    // ── Schedule config validation tests (CRON-1) ──

    #[test]
    fn test_schedule_config_valid_minimal() {
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/test
agents:
  - alias: coder
    backend: stub
schedules:
  - name: ci-check
    agent: coder
    cron: "*/5 * * * *"
    body: "Run CI checks"
"#;
        let config = crate::config::load_config_from_str(yaml).unwrap();
        let schedules = config.schedules.unwrap();
        assert_eq!(schedules.len(), 1);
        assert_eq!(schedules[0].name, "ci-check");
        assert_eq!(schedules[0].agent, "coder");
        assert_eq!(schedules[0].cron, "*/5 * * * *");
        assert_eq!(schedules[0].body, "Run CI checks");
        assert!(schedules[0].batch.is_none());
        assert_eq!(schedules[0].max_runs, 100); // default
        assert!(schedules[0].enabled); // default true
    }

    #[test]
    fn test_schedule_config_valid_full() {
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/test
agents:
  - alias: coder
    backend: stub
schedules:
  - name: nightly-build
    agent: coder
    cron: "0 2 * * *"
    body: "Run nightly build"
    batch: "NIGHTLY-001"
    max_runs: 50
    enabled: false
"#;
        let config = crate::config::load_config_from_str(yaml).unwrap();
        let schedules = config.schedules.unwrap();
        assert_eq!(schedules[0].name, "nightly-build");
        assert_eq!(schedules[0].batch.as_deref(), Some("NIGHTLY-001"));
        assert_eq!(schedules[0].max_runs, 50);
        assert!(!schedules[0].enabled);
    }

    #[test]
    fn test_schedule_config_absent_is_backward_compatible() {
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/test
agents:
  - alias: a1
    backend: stub
"#;
        let config = crate::config::load_config_from_str(yaml).unwrap();
        assert!(config.schedules.is_none());
    }

    #[test]
    fn test_schedule_config_empty_vec_is_valid() {
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/test
agents:
  - alias: a1
    backend: stub
schedules: []
"#;
        let config = crate::config::load_config_from_str(yaml).unwrap();
        assert_eq!(config.schedules.unwrap().len(), 0);
    }

    #[test]
    fn test_schedule_config_empty_name_rejected() {
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/test
agents:
  - alias: coder
    backend: stub
schedules:
  - name: ""
    agent: coder
    cron: "*/5 * * * *"
    body: "test"
"#;
        let err = crate::config::load_config_from_str(yaml).unwrap_err();
        assert!(err.to_string().contains("name must not be empty"));
    }

    #[test]
    fn test_schedule_config_duplicate_name_rejected() {
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/test
agents:
  - alias: coder
    backend: stub
schedules:
  - name: ci-check
    agent: coder
    cron: "*/5 * * * *"
    body: "first"
  - name: ci-check
    agent: coder
    cron: "*/10 * * * *"
    body: "second"
"#;
        let err = crate::config::load_config_from_str(yaml).unwrap_err();
        assert!(err
            .to_string()
            .contains("duplicate schedule name 'ci-check'"));
    }

    #[test]
    fn test_schedule_config_unknown_agent_rejected() {
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/test
agents:
  - alias: coder
    backend: stub
schedules:
  - name: ci-check
    agent: nonexistent
    cron: "*/5 * * * *"
    body: "test"
"#;
        let err = crate::config::load_config_from_str(yaml).unwrap_err();
        assert!(err
            .to_string()
            .contains("unknown agent alias 'nonexistent'"));
    }

    #[test]
    fn test_schedule_config_invalid_cron_rejected() {
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/test
agents:
  - alias: coder
    backend: stub
schedules:
  - name: bad-cron
    agent: coder
    cron: "not-a-cron"
    body: "test"
"#;
        let err = crate::config::load_config_from_str(yaml).unwrap_err();
        assert!(err.to_string().contains("invalid cron expression"));
    }

    #[test]
    fn test_schedule_config_max_runs_zero_rejected() {
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/test
agents:
  - alias: coder
    backend: stub
schedules:
  - name: ci-check
    agent: coder
    cron: "*/5 * * * *"
    body: "test"
    max_runs: 0
"#;
        let err = crate::config::load_config_from_str(yaml).unwrap_err();
        assert!(err.to_string().contains("max_runs must be > 0"));
    }

    #[test]
    fn test_schedule_config_multiple_valid() {
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/test
agents:
  - alias: coder
    backend: stub
  - alias: reviewer
    backend: stub
schedules:
  - name: ci-check
    agent: coder
    cron: "*/5 * * * *"
    body: "Run CI"
  - name: review-sweep
    agent: reviewer
    cron: "0 9 * * *"
    body: "Sweep pending reviews"
    batch: "REVIEW-BATCH"
"#;
        let config = crate::config::load_config_from_str(yaml).unwrap();
        let schedules = config.schedules.unwrap();
        assert_eq!(schedules.len(), 2);
        assert_eq!(schedules[0].name, "ci-check");
        assert_eq!(schedules[1].name, "review-sweep");
        assert_eq!(schedules[1].batch.as_deref(), Some("REVIEW-BATCH"));
    }

    #[test]
    fn test_schedule_config_various_cron_expressions() {
        // All of these should be valid cron expressions
        let expressions = [
            "* * * * *",     // every minute
            "*/5 * * * *",   // every 5 minutes
            "0 * * * *",     // every hour
            "0 0 * * *",     // daily at midnight
            "0 9 * * 1-5",   // weekdays at 9am
            "0 0 1 * *",     // first of month
            "30 4 1,15 * *", // 4:30am on 1st and 15th
        ];
        for expr in &expressions {
            let yaml = format!(
                r#"
default_workdir: /tmp
state_dir: /tmp/test
agents:
  - alias: coder
    backend: stub
schedules:
  - name: test-schedule
    agent: coder
    cron: "{expr}"
    body: "test"
"#
            );
            let result = crate::config::load_config_from_str(&yaml);
            assert!(
                result.is_ok(),
                "expected cron expression '{}' to be valid, got: {}",
                expr,
                result.unwrap_err()
            );
        }
    }

    #[test]
    fn test_schedule_config_unknown_field_rejected() {
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/test
agents:
  - alias: coder
    backend: stub
schedules:
  - name: ci-check
    agent: coder
    cron: "*/5 * * * *"
    body: "test"
    unknown_field: true
"#;
        let err = crate::config::load_config_from_str(yaml).unwrap_err();
        assert!(err.to_string().contains("unknown field"));
    }

    #[test]
    fn test_schedule_config_empty_agent_rejected() {
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/test
agents:
  - alias: coder
    backend: stub
schedules:
  - name: ci-check
    agent: ""
    cron: "*/5 * * * *"
    body: "Run CI checks"
"#;
        let err = crate::config::load_config_from_str(yaml).unwrap_err();
        assert!(
            err.to_string().contains("agent must not be empty"),
            "expected 'agent must not be empty', got: {}",
            err
        );
    }

    #[test]
    fn test_schedule_config_empty_body_rejected() {
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/test
agents:
  - alias: coder
    backend: stub
schedules:
  - name: ci-check
    agent: coder
    cron: "*/5 * * * *"
    body: ""
"#;
        let err = crate::config::load_config_from_str(yaml).unwrap_err();
        assert!(
            err.to_string().contains("body must not be empty"),
            "expected 'body must not be empty', got: {}",
            err
        );
    }

    // ── SEC-3: backend_args bypass-flag warnings ──

    #[test]
    fn test_warning_duplicate_bypass_flag_claude() {
        let mut config = minimal_config();
        config.agents[0].backend = "claude".into();
        config.agents[0].backend_args = Some(vec!["--dangerously-skip-permissions".into()]);
        let warnings = collect_config_warnings(&config);
        assert_eq!(warnings.len(), 1);
        assert!(
            warnings[0].contains("already applied by the claude backend"),
            "expected duplicate-flag warning, got: {}",
            warnings[0]
        );
        // Config still loads
        assert!(validate_config(&config).is_ok());
    }

    #[test]
    fn test_warning_duplicate_bypass_flag_codex() {
        let mut config = minimal_config();
        config.agents[0].backend = "codex".into();
        config.agents[0].backend_args = Some(vec!["--full-auto".into()]);
        let warnings = collect_config_warnings(&config);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("already applied by the codex backend"));
    }

    #[test]
    fn test_warning_duplicate_bypass_flag_gemini() {
        let mut config = minimal_config();
        config.agents[0].backend = "gemini".into();
        config.agents[0].backend_args = Some(vec!["--yolo".into()]);
        let warnings = collect_config_warnings(&config);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("already applied by the gemini backend"));
    }

    #[test]
    fn test_warning_dangerous_flag_cross_backend() {
        let mut config = minimal_config();
        config.agents[0].backend = "claude".into();
        config.agents[0].backend_args = Some(vec!["--yolo".into()]);
        let warnings = collect_config_warnings(&config);
        assert_eq!(warnings.len(), 1);
        assert!(
            warnings[0].contains("dangerous flag '--yolo'"),
            "expected cross-backend dangerous warning, got: {}",
            warnings[0]
        );
    }

    #[test]
    fn test_warning_multiple_bypass_flags() {
        let mut config = minimal_config();
        config.agents[0].backend = "codex".into();
        config.agents[0].backend_args = Some(vec![
            "--full-auto".into(),
            "--dangerously-bypass-approvals-and-sandbox".into(),
            "--yolo".into(), // cross-backend
        ]);
        let warnings = collect_config_warnings(&config);
        assert_eq!(warnings.len(), 3);
        // --full-auto is auto-injected by codex → duplicate warning
        assert!(warnings[0].contains("already applied by the codex backend"));
        // --dangerously-bypass-approvals-and-sandbox is never auto-injected → dangerous flag
        assert!(
            warnings[1].contains("dangerous flag '--dangerously-bypass-approvals-and-sandbox'"),
            "expected dangerous-flag warning, got: {}",
            warnings[1]
        );
        // --yolo is gemini's flag → dangerous flag on codex agent
        assert!(warnings[2].contains("dangerous flag '--yolo'"));
    }

    #[test]
    fn test_no_warning_for_safe_backend_args() {
        let mut config = minimal_config();
        config.agents[0].backend = "claude".into();
        config.agents[0].backend_args = Some(vec!["--model".into(), "opus".into()]);
        let warnings = collect_config_warnings(&config);
        assert!(warnings.is_empty());
    }

    // ── SEC-5: sensitive env var warnings ──

    #[test]
    fn test_warning_sensitive_env_path() {
        let mut config = minimal_config();
        let mut env = std::collections::HashMap::new();
        env.insert("PATH".into(), "/custom/bin".into());
        config.agents[0].env = Some(env);
        let warnings = collect_config_warnings(&config);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("security-sensitive variable 'PATH'"));
        // Config still loads
        assert!(validate_config(&config).is_ok());
    }

    #[test]
    fn test_warning_sensitive_env_ld_preload() {
        let mut config = minimal_config();
        let mut env = std::collections::HashMap::new();
        env.insert("LD_PRELOAD".into(), "/evil.so".into());
        config.agents[0].env = Some(env);
        let warnings = collect_config_warnings(&config);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("security-sensitive variable 'LD_PRELOAD'"));
    }

    #[test]
    fn test_warning_sensitive_env_dyld() {
        let mut config = minimal_config();
        let mut env = std::collections::HashMap::new();
        env.insert("DYLD_INSERT_LIBRARIES".into(), "/evil.dylib".into());
        config.agents[0].env = Some(env);
        let warnings = collect_config_warnings(&config);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("security-sensitive variable 'DYLD_INSERT_LIBRARIES'"));
    }

    #[test]
    fn test_warning_multiple_sensitive_env_vars() {
        let mut config = minimal_config();
        let mut env = std::collections::HashMap::new();
        env.insert("PATH".into(), "/custom".into());
        env.insert("HOME".into(), "/evil".into());
        env.insert("SAFE_VAR".into(), "ok".into());
        config.agents[0].env = Some(env);
        let warnings = collect_config_warnings(&config);
        assert_eq!(warnings.len(), 2);
        // Sorted order: HOME before PATH
        assert!(warnings[0].contains("'HOME'"));
        assert!(warnings[1].contains("'PATH'"));
    }

    #[test]
    fn test_no_warning_for_safe_env_vars() {
        let mut config = minimal_config();
        let mut env = std::collections::HashMap::new();
        env.insert("ANTHROPIC_API_KEY".into(), "key".into());
        env.insert("MY_CUSTOM_VAR".into(), "value".into());
        config.agents[0].env = Some(env);
        let warnings = collect_config_warnings(&config);
        assert!(warnings.is_empty());
    }

    #[test]
    fn test_no_warning_when_env_absent() {
        let config = minimal_config();
        assert!(config.agents[0].env.is_none());
        let warnings = collect_config_warnings(&config);
        assert!(warnings.is_empty());
    }

    #[test]
    fn test_combined_bypass_and_env_warnings() {
        let mut config = minimal_config();
        config.agents[0].backend = "claude".into();
        config.agents[0].backend_args = Some(vec!["--dangerously-skip-permissions".into()]);
        let mut env = std::collections::HashMap::new();
        env.insert("LD_PRELOAD".into(), "/evil.so".into());
        config.agents[0].env = Some(env);
        let warnings = collect_config_warnings(&config);
        assert_eq!(warnings.len(), 2);
        let combined = warnings.join(" ");
        assert!(combined.contains("already applied"));
        assert!(combined.contains("LD_PRELOAD"));
        // Config still loads
        assert!(validate_config(&config).is_ok());
    }
}
