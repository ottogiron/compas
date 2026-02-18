pub mod types;
pub mod validation;

pub use types::OrchestratorConfig;

use crate::error::Result;
use std::path::Path;

/// Load and validate configuration from a YAML file.
pub fn load_config(path: &Path) -> Result<OrchestratorConfig> {
    let content = std::fs::read_to_string(path)?;
    load_config_from_str(&content)
}

/// Load and validate configuration from a YAML string.
pub fn load_config_from_str(yaml: &str) -> Result<OrchestratorConfig> {
    let mut config: OrchestratorConfig = serde_yaml::from_str(yaml)?;
    normalize_config(&mut config);
    validation::validate_config(&config)?;
    Ok(config)
}

/// Normalize legacy per-agent model lists into the global model registry.
///
/// Backward compatibility: if agents use `models:` (per-agent) without a global
/// `models:` section, synthesize global entries. If agents lack `preferred_models:`,
/// populate from their `models:` or `model:` fields.
fn normalize_config(config: &mut OrchestratorConfig) {
    use types::ModelEntry;

    // Phase 1: Synthesize global registry from per-agent models if needed.
    if config.models.is_none() {
        let mut global = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for agent in &config.agents {
            if let Some(ref models) = agent.models {
                for entry in models {
                    if seen.insert(entry.id.clone()) {
                        global.push(ModelEntry {
                            id: entry.id.clone(),
                            backend: entry
                                .backend
                                .clone()
                                .or_else(|| Some(agent.backend.clone())),
                            description: entry.description.clone(),
                            timeout_secs: entry.timeout_secs,
                        });
                    }
                }
            }
        }
        if !global.is_empty() {
            config.models = Some(global);
        }
    }

    // Phase 2: Populate preferred_models from legacy fields.
    for agent in &mut config.agents {
        if agent.preferred_models.is_none() {
            if let Some(ref models) = agent.models {
                agent.preferred_models = Some(models.iter().map(|e| e.id.clone()).collect());
            } else if let Some(ref model) = agent.model {
                agent.preferred_models = Some(vec![model.clone()]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_config_from_str() {
        let yaml = r#"
state_dir: /tmp/test-mail
agents:
  - alias: focused
    identity: Claude
    backend: stub
"#;
        let config = load_config_from_str(yaml).unwrap();
        assert_eq!(config.agents.len(), 1);
        assert_eq!(config.poll_interval_secs, 1); // default (ORCH-REL-19)
    }

    #[test]
    fn test_load_config_from_str_invalid() {
        let yaml = r#"
state_dir: /tmp/test-mail
agents: []
"#;
        let err = load_config_from_str(yaml).unwrap_err();
        assert!(err.to_string().contains("at least one agent"));
    }

    #[test]
    fn test_load_config_from_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(
            &path,
            r#"
state_dir: /tmp/test-mail
agents:
  - alias: focused
    identity: Claude
    backend: stub
"#,
        )
        .unwrap();
        let config = load_config(&path).unwrap();
        assert_eq!(config.agents.len(), 1);
    }
}
