use super::types::{AgentRole, OrchestratorConfig};
use crate::error::{OrchestratorError, Result};
use std::collections::HashSet;

/// Validate an orchestrator configuration.
pub fn validate_config(config: &OrchestratorConfig) -> Result<()> {
    if config.agents.is_empty() {
        return Err(OrchestratorError::Config(
            "at least one agent must be configured".into(),
        ));
    }
    if config.project_root.as_os_str().is_empty() {
        return Err(OrchestratorError::Config(
            "project_root must not be empty".into(),
        ));
    }
    if !config.project_root.exists() {
        return Err(OrchestratorError::Config(format!(
            "project_root does not exist: {}",
            config.project_root.display()
        )));
    }
    if !config.project_root.is_dir() {
        return Err(OrchestratorError::Config(format!(
            "project_root must be a directory: {}",
            config.project_root.display()
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
        // ORCH-MODELS-1: validate model pool
        if agent.model.is_some() && agent.models.is_some() {
            tracing::warn!(
                "agent '{}': both model and models set; models takes precedence",
                agent.alias
            );
        }
        if let Some(ref models) = agent.models {
            if models.is_empty() {
                return Err(OrchestratorError::Config(format!(
                    "agent '{}' models list must not be empty when set",
                    agent.alias
                )));
            }
        }
        // ORCH-MODELS-2b: validate preferred_models against global registry
        if let Some(ref preferred) = agent.preferred_models {
            if preferred.is_empty() {
                return Err(OrchestratorError::Config(format!(
                    "agent '{}' preferred_models list must not be empty when set",
                    agent.alias
                )));
            }
            if let Some(ref global) = config.models {
                for model_id in preferred {
                    if !global.iter().any(|m| m.id == *model_id) {
                        return Err(OrchestratorError::Config(format!(
                            "agent '{}' preferred_models references unknown model '{}'",
                            agent.alias, model_id
                        )));
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
    if config.db_path.as_os_str().is_empty() {
        return Err(OrchestratorError::Config(
            "db_path must not be empty".into(),
        ));
    }

    if config.orchestration.stale_active_secs < 60
        || config.orchestration.stale_active_secs > 604_800
    {
        return Err(OrchestratorError::Config(
            "orchestration.stale_active_secs must be 60..604800".into(),
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

    Ok(())
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
    use crate::config::types::{AgentConfig, AgentRole, OrchestratorConfig};
    use std::path::PathBuf;

    fn minimal_config() -> OrchestratorConfig {
        OrchestratorConfig {
            project_root: PathBuf::from("/tmp"),
            state_dir: PathBuf::from("/tmp/test-mail"),
            db_path: PathBuf::from(".aster-orch/jobs.sqlite"),
            poll_interval_secs: 5,
            models: None,
            agents: vec![AgentConfig {
                alias: "focused".into(),
                role: AgentRole::Worker,
                backend: "stub".into(),

                model: None,
                models: None,
                preferred_models: None,
                prompt: None,
                prompt_file: None,
                timeout_secs: None,

                backend_args: None,
                env: None,
            }],
            orchestration: Default::default(),
            database: Default::default(),
        }
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
    fn test_config_validation_missing_project_root() {
        let mut config = minimal_config();
        config.project_root = PathBuf::new();
        let err = validate_config(&config).unwrap_err();
        assert!(err.to_string().contains("project_root must not be empty"));
    }

    #[test]
    fn test_config_validation_nonexistent_project_root() {
        let mut config = minimal_config();
        config.project_root = PathBuf::from("/definitely/nonexistent/project-root");
        let err = validate_config(&config).unwrap_err();
        assert!(err.to_string().contains("project_root does not exist"));
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
            models: None,
            preferred_models: None,
            prompt: None,
            prompt_file: None,
            timeout_secs: None,

            backend_args: None,
            env: None,
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
project_root: /tmp
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
project_root: /tmp
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
            models: None,
            preferred_models: None,
            prompt: None,
            prompt_file: None,
            timeout_secs: None,

            backend_args: None,
            env: None,
        });
        assert_eq!(config.effective_max_concurrent_triggers(), 2);

        // Operator agents don't count
        config.agents.push(AgentConfig {
            alias: "operator".into(),
            backend: "stub".into(),
            role: AgentRole::Operator,
            model: None,
            models: None,
            preferred_models: None,
            prompt: None,
            prompt_file: None,
            timeout_secs: None,

            backend_args: None,
            env: None,
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
    fn test_model_pool_from_model_only() {
        use crate::config::types::ModelEntry;
        let mut config = minimal_config();
        config.agents[0].model = Some("claude-opus-4-6".into());
        config.agents[0].models = None;
        let pool = config.agents[0].model_pool(&[]);
        assert_eq!(
            pool,
            vec![ModelEntry {
                id: "claude-opus-4-6".into(),
                backend: None,
                description: None,
                timeout_secs: None
            }]
        );
    }

    #[test]
    fn test_model_pool_from_models_only() {
        use crate::config::types::ModelEntry;
        let mut config = minimal_config();
        config.agents[0].model = None;
        config.agents[0].models = Some(vec![
            ModelEntry {
                id: "opus".into(),
                backend: None,
                description: Some("fast".into()),
                timeout_secs: None,
            },
            ModelEntry {
                id: "sonnet".into(),
                backend: None,
                description: None,
                timeout_secs: None,
            },
        ]);
        let pool = config.agents[0].model_pool(&[]);
        assert_eq!(pool.len(), 2);
        assert_eq!(pool[0].id, "opus");
        assert_eq!(pool[0].description.as_deref(), Some("fast"));
        assert_eq!(pool[1].id, "sonnet");
    }

    #[test]
    fn test_model_pool_models_takes_precedence() {
        use crate::config::types::ModelEntry;
        let mut config = minimal_config();
        config.agents[0].model = Some("old-model".into());
        config.agents[0].models = Some(vec![ModelEntry {
            id: "new-model".into(),
            backend: None,
            description: None,
            timeout_secs: None,
        }]);
        let pool = config.agents[0].model_pool(&[]);
        assert_eq!(pool.len(), 1);
        assert_eq!(pool[0].id, "new-model");
    }

    #[test]
    fn test_model_pool_empty_when_neither_set() {
        let mut config = minimal_config();
        config.agents[0].model = None;
        config.agents[0].models = None;
        assert!(config.agents[0].model_pool(&[]).is_empty());
    }

    #[test]
    fn test_config_validation_empty_models_rejected() {
        let mut config = minimal_config();
        config.agents[0].models = Some(vec![]);
        let err = validate_config(&config).unwrap_err();
        assert!(err.to_string().contains("models list must not be empty"));
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
    fn test_model_pool_from_preferred_models() {
        use crate::config::types::ModelEntry;
        let mut config = minimal_config();
        config.models = Some(vec![
            ModelEntry {
                id: "opus".into(),
                backend: Some("claude".into()),
                description: None,
                timeout_secs: None,
            },
            ModelEntry {
                id: "sonnet".into(),
                backend: Some("claude".into()),
                description: None,
                timeout_secs: None,
            },
            ModelEntry {
                id: "glm-5".into(),
                backend: Some("opencode".into()),
                description: None,
                timeout_secs: None,
            },
        ]);
        config.agents[0].preferred_models = Some(vec!["opus".into(), "glm-5".into()]);
        let global = config.models.as_deref().unwrap();
        let pool = config.agents[0].model_pool(global);
        assert_eq!(pool.len(), 2);
        assert_eq!(pool[0].id, "opus");
        assert_eq!(pool[0].backend.as_deref(), Some("claude"));
        assert_eq!(pool[1].id, "glm-5");
        assert_eq!(pool[1].backend.as_deref(), Some("opencode"));
    }

    #[test]
    fn test_preferred_models_unknown_id_rejected() {
        use crate::config::types::ModelEntry;
        let mut config = minimal_config();
        config.models = Some(vec![ModelEntry {
            id: "opus".into(),
            backend: Some("claude".into()),
            description: None,
            timeout_secs: None,
        }]);
        config.agents[0].preferred_models = Some(vec!["nonexistent".into()]);
        let err = validate_config(&config).unwrap_err();
        assert!(err.to_string().contains("unknown model 'nonexistent'"));
    }

    #[test]
    fn test_preferred_models_empty_rejected() {
        let mut config = minimal_config();
        config.agents[0].preferred_models = Some(vec![]);
        let err = validate_config(&config).unwrap_err();
        assert!(err
            .to_string()
            .contains("preferred_models list must not be empty"));
    }

    #[test]
    fn test_normalization_legacy_models_to_global() {
        let yaml = r#"
project_root: /tmp
state_dir: /tmp/test
agents:
  - alias: a1
    backend: claude
    models:
      - id: opus
        description: "Best"
      - id: sonnet
"#;
        let config = crate::config::load_config_from_str(yaml).unwrap();
        // Global registry should be synthesized
        let global = config.models.unwrap();
        assert_eq!(global.len(), 2);
        assert_eq!(global[0].id, "opus");
        assert_eq!(global[0].backend.as_deref(), Some("claude"));
        assert_eq!(global[1].id, "sonnet");
        assert_eq!(global[1].backend.as_deref(), Some("claude"));
        // preferred_models should be populated
        let preferred = config.agents[0].preferred_models.as_ref().unwrap();
        assert_eq!(preferred, &vec!["opus".to_string(), "sonnet".to_string()]);
    }

    #[test]
    fn test_normalization_model_only_to_preferred() {
        let yaml = r#"
project_root: /tmp
state_dir: /tmp/test
agents:
  - alias: a1
    backend: claude
    model: opus
"#;
        let config = crate::config::load_config_from_str(yaml).unwrap();
        let preferred = config.agents[0].preferred_models.as_ref().unwrap();
        assert_eq!(preferred, &vec!["opus".to_string()]);
    }

    #[test]
    fn test_global_models_config_roundtrip() {
        let yaml = r#"
project_root: /tmp
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
    preferred_models:
      - opus
      - glm-5
"#;
        let config = crate::config::load_config_from_str(yaml).unwrap();
        let global = config.models.as_ref().unwrap();
        assert_eq!(global.len(), 2);
        assert_eq!(global[0].backend.as_deref(), Some("claude"));
        assert_eq!(global[1].backend.as_deref(), Some("opencode"));
        let pool = config.agents[0].model_pool(global);
        assert_eq!(pool.len(), 2);
        assert_eq!(pool[0].id, "opus");
        assert_eq!(pool[0].backend.as_deref(), Some("claude"));
        assert_eq!(pool[1].id, "glm-5");
        assert_eq!(pool[1].backend.as_deref(), Some("opencode"));
    }
}
