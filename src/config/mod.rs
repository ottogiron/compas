pub mod types;
pub mod validation;
pub mod watcher;

pub use types::OrchestratorConfig;
pub use watcher::ConfigHandle;

use crate::error::Result;
use std::path::{Path, PathBuf};

/// Load and validate configuration from a YAML file.
pub fn load_config(path: &Path) -> Result<OrchestratorConfig> {
    if !path.exists() {
        return Err(crate::error::OrchestratorError::Config(format!(
            "config file not found at {}. Run `compas init` to create one, \
             or use `--config <path>` to specify a different location.",
            path.display()
        )));
    }
    let content = std::fs::read_to_string(path)?;
    let config_path = absolutize_base(path);
    let base_dir = config_path.parent().unwrap_or_else(|| Path::new("."));
    load_config_from_str_with_base(&content, base_dir)
}

/// Load and validate configuration from a YAML string.
pub fn load_config_from_str(yaml: &str) -> Result<OrchestratorConfig> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    load_config_from_str_with_base(yaml, &cwd)
}

/// Load config from YAML and resolve relative paths against a base directory.
fn load_config_from_str_with_base(yaml: &str, base_dir: &Path) -> Result<OrchestratorConfig> {
    let mut config: OrchestratorConfig = serde_yaml::from_str(yaml)?;
    apply_agent_defaults(&mut config);
    resolve_paths(&mut config, base_dir);
    validation::validate_config(&config)?;
    Ok(config)
}

/// Apply `agent_defaults` to all agents. Per-agent values take precedence.
pub(crate) fn apply_agent_defaults(config: &mut OrchestratorConfig) {
    let Some(ref defaults) = config.agent_defaults else {
        return;
    };
    let defaults = defaults.clone();
    for agent in &mut config.agents {
        if agent.backend.is_none() {
            agent.backend = defaults.backend.clone();
        }
        if agent.model.is_none() {
            agent.model = defaults.model.clone();
        }
        if agent.safety_mode.is_none() {
            agent.safety_mode = defaults.safety_mode.clone();
        }
        if agent.workspace.is_none() {
            agent.workspace = defaults.workspace.clone();
        }
        if agent.timeout_secs.is_none() {
            agent.timeout_secs = defaults.timeout_secs;
        }
        if agent.workdir.is_none() {
            agent.workdir = defaults.workdir.clone();
        }
        if agent.prompt.is_none() {
            agent.prompt = defaults.prompt.clone();
        }
        if agent.prompt_file.is_none() {
            agent.prompt_file = defaults.prompt_file.clone();
        }
        if agent.max_retries.is_none() {
            agent.max_retries = defaults.max_retries;
        }
        if agent.retry_backoff_secs.is_none() {
            agent.retry_backoff_secs = defaults.retry_backoff_secs;
        }
        // handoff: replace — only inherit if agent has no handoff block
        if agent.handoff.is_none() {
            agent.handoff = defaults.handoff.clone();
        }
        // backend_args: replace — only inherit if agent has no backend_args
        if agent.backend_args.is_none() {
            agent.backend_args = defaults.backend_args.clone();
        }
        // env: shallow merge — defaults as base, agent keys override
        if let Some(ref default_env) = defaults.env {
            let mut merged = default_env.clone();
            if let Some(ref agent_env) = agent.env {
                merged.extend(agent_env.iter().map(|(k, v)| (k.clone(), v.clone())));
            }
            agent.env = Some(merged);
        }
    }
}

fn resolve_paths(config: &mut OrchestratorConfig, base_dir: &Path) {
    let base = absolutize_base(base_dir);

    config.default_workdir = resolve_path(&base, &config.default_workdir);
    config.state_dir = resolve_path(&base, &config.state_dir);

    if let Some(ref worktree_dir) = config.worktree_dir {
        config.worktree_dir = Some(resolve_path(&base, worktree_dir));
    }

    if let Some(ref mut defaults) = config.agent_defaults {
        if let Some(ref prompt_file) = defaults.prompt_file {
            defaults.prompt_file = Some(resolve_path(&base, prompt_file));
        }
        if let Some(ref workdir) = defaults.workdir {
            defaults.workdir = Some(resolve_path(&base, workdir));
        }
    }

    for agent in &mut config.agents {
        if let Some(ref prompt_file) = agent.prompt_file {
            agent.prompt_file = Some(resolve_path(&base, prompt_file));
        }
        if let Some(ref workdir) = agent.workdir {
            agent.workdir = Some(resolve_path(&base, workdir));
        }
    }
}

fn resolve_path(base_dir: &Path, path: &Path) -> PathBuf {
    let expanded = expand_tilde(path);
    if expanded.is_absolute() {
        expanded
    } else {
        base_dir.join(expanded)
    }
}

pub fn expand_tilde(path: &Path) -> PathBuf {
    let Some(raw) = path.to_str() else {
        return path.to_path_buf();
    };

    if raw == "~" {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home);
        }
    }

    if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }

    path.to_path_buf()
}

fn absolutize_base(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_config_from_str() {
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/test-mail
agents:
  - alias: focused
    backend: stub
"#;
        let config = load_config_from_str(yaml).unwrap();
        assert_eq!(config.agents.len(), 1);
        assert_eq!(config.poll_interval_secs, 1); // default (ORCH-REL-19)
        assert_eq!(config.database.max_connections, 32); // default
        assert_eq!(config.database.min_connections, 4); // default
    }

    #[test]
    fn test_load_config_from_str_invalid() {
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/test-mail
agents: []
"#;
        let err = load_config_from_str(yaml).unwrap_err();
        assert!(err.to_string().contains("at least one agent"));
    }

    #[test]
    fn test_load_config_from_file() {
        let dir = tempfile::tempdir().unwrap();
        let default_workdir = dir.path().join("repo");
        std::fs::create_dir_all(&default_workdir).unwrap();
        let prompt_file = dir.path().join("prompts").join("focused.txt");
        std::fs::create_dir_all(prompt_file.parent().unwrap()).unwrap();
        std::fs::write(&prompt_file, "You are focused.").unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(
            &path,
            r#"
default_workdir: ./repo
state_dir: ./.compas/state
agents:
  - alias: focused
    backend: stub
    prompt_file: prompts/focused.txt
"#,
        )
        .unwrap();
        let config = load_config(&path).unwrap();
        assert_eq!(config.agents.len(), 1);
        assert_eq!(config.default_workdir, default_workdir);
        assert_eq!(config.state_dir, dir.path().join(".compas").join("state"));
        assert_eq!(
            config.db_path(),
            dir.path().join(".compas").join("state").join("jobs.sqlite")
        );
        assert_eq!(
            config.agents[0].prompt_file.as_deref(),
            Some(prompt_file.as_path())
        );
    }

    // ── agent_defaults tests (CFG-DEFAULTS) ──

    #[test]
    fn test_agent_defaults_basic_merge() {
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/state
agent_defaults:
  backend: claude
  model: claude-sonnet-4-6
  safety_mode: auto_approve
  workspace: worktree
agents:
  - alias: dev
    model: claude-opus-4-6
  - alias: reviewer
"#;
        let config = load_config_from_str(yaml).unwrap();
        // Both agents inherit backend and workspace from defaults.
        assert_eq!(config.agents[0].backend(), "claude");
        assert_eq!(config.agents[1].backend(), "claude");
        // dev overrides model; reviewer inherits.
        assert_eq!(config.agents[0].model.as_deref(), Some("claude-opus-4-6"));
        assert_eq!(config.agents[1].model.as_deref(), Some("claude-sonnet-4-6"));
        // Both inherit workspace.
        assert_eq!(config.agents[0].workspace.as_deref(), Some("worktree"));
        assert_eq!(config.agents[1].workspace.as_deref(), Some("worktree"));
    }

    #[test]
    fn test_agent_defaults_env_shallow_merge() {
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/state
agent_defaults:
  backend: stub
  env:
    SHARED_VAR: from_defaults
    OVERRIDE_ME: default_val
agents:
  - alias: dev
    env:
      OVERRIDE_ME: agent_val
      AGENT_ONLY: extra
"#;
        let config = load_config_from_str(yaml).unwrap();
        let env = config.agents[0].env.as_ref().unwrap();
        assert_eq!(env.get("SHARED_VAR").unwrap(), "from_defaults");
        assert_eq!(env.get("OVERRIDE_ME").unwrap(), "agent_val");
        assert_eq!(env.get("AGENT_ONLY").unwrap(), "extra");
    }

    #[test]
    fn test_agent_defaults_backend_args_replace() {
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/state
agent_defaults:
  backend: stub
  backend_args: ["--default-flag"]
agents:
  - alias: dev
    backend_args: ["--agent-flag"]
  - alias: reviewer
"#;
        let config = load_config_from_str(yaml).unwrap();
        // dev's backend_args replaces entirely (not merged).
        assert_eq!(
            config.agents[0].backend_args.as_deref().unwrap(),
            &["--agent-flag"]
        );
        // reviewer inherits defaults.
        assert_eq!(
            config.agents[1].backend_args.as_deref().unwrap(),
            &["--default-flag"]
        );
    }

    #[test]
    fn test_agent_defaults_handoff_replace() {
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/state
agent_defaults:
  backend: stub
  handoff:
    on_response: reviewer
agents:
  - alias: dev
    handoff:
      on_response: operator
  - alias: reviewer
    handoff:
      on_response: dev
"#;
        let config = load_config_from_str(yaml).unwrap();
        // dev's handoff replaces entirely.
        let dev_handoff = config.agents[0].handoff.as_ref().unwrap();
        assert_eq!(
            dev_handoff.on_response,
            Some(types::HandoffTarget::Single("operator".into()))
        );
        // reviewer has its own handoff — defaults not inherited.
        let rev_handoff = config.agents[1].handoff.as_ref().unwrap();
        assert_eq!(
            rev_handoff.on_response,
            Some(types::HandoffTarget::Single("dev".into()))
        );
    }

    #[test]
    fn test_agent_defaults_no_backend_error() {
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/state
agents:
  - alias: dev
"#;
        let err = load_config_from_str(yaml).unwrap_err();
        assert!(
            err.to_string().contains("has no backend"),
            "expected backend error, got: {}",
            err
        );
    }

    #[test]
    fn test_agent_defaults_none() {
        // No agent_defaults block — existing config works unchanged.
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/state
agents:
  - alias: dev
    backend: stub
"#;
        let config = load_config_from_str(yaml).unwrap();
        assert!(config.agent_defaults.is_none());
        assert_eq!(config.agents[0].backend(), "stub");
    }

    #[test]
    fn test_agent_defaults_cross_backend() {
        // Gemini agent inherits workspace/safety_mode from claude-style defaults.
        let yaml = r#"
default_workdir: /tmp
state_dir: /tmp/state
agent_defaults:
  safety_mode: auto_approve
  workspace: worktree
agents:
  - alias: dev
    backend: claude
  - alias: gem
    backend: gemini
"#;
        let config = load_config_from_str(yaml).unwrap();
        assert_eq!(config.agents[0].workspace.as_deref(), Some("worktree"));
        assert_eq!(config.agents[1].workspace.as_deref(), Some("worktree"));
        assert_eq!(
            config.agents[0].safety_mode,
            Some(types::SafetyMode::AutoApprove)
        );
        assert_eq!(
            config.agents[1].safety_mode,
            Some(types::SafetyMode::AutoApprove)
        );
    }
}
