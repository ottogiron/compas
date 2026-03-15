pub mod types;
pub mod validation;
pub mod watcher;

pub use types::OrchestratorConfig;
pub use watcher::ConfigHandle;

use crate::error::Result;
use std::path::{Path, PathBuf};

/// Load and validate configuration from a YAML file.
pub fn load_config(path: &Path) -> Result<OrchestratorConfig> {
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
    resolve_paths(&mut config, base_dir);
    validation::validate_config(&config)?;
    Ok(config)
}

fn resolve_paths(config: &mut OrchestratorConfig, base_dir: &Path) {
    let base = absolutize_base(base_dir);

    config.target_repo_root = resolve_path(&base, &config.target_repo_root);
    config.state_dir = resolve_path(&base, &config.state_dir);

    if let Some(ref worktree_dir) = config.worktree_dir {
        config.worktree_dir = Some(resolve_path(&base, worktree_dir));
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

fn expand_tilde(path: &Path) -> PathBuf {
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
target_repo_root: /tmp
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
target_repo_root: /tmp
state_dir: /tmp/test-mail
agents: []
"#;
        let err = load_config_from_str(yaml).unwrap_err();
        assert!(err.to_string().contains("at least one agent"));
    }

    #[test]
    fn test_load_config_from_file() {
        let dir = tempfile::tempdir().unwrap();
        let target_repo_root = dir.path().join("repo");
        std::fs::create_dir_all(&target_repo_root).unwrap();
        let prompt_file = dir.path().join("prompts").join("focused.txt");
        std::fs::create_dir_all(prompt_file.parent().unwrap()).unwrap();
        std::fs::write(&prompt_file, "You are focused.").unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(
            &path,
            r#"
target_repo_root: ./repo
state_dir: ./.aster-orch/state
agents:
  - alias: focused
    backend: stub
    prompt_file: prompts/focused.txt
"#,
        )
        .unwrap();
        let config = load_config(&path).unwrap();
        assert_eq!(config.agents.len(), 1);
        assert_eq!(config.target_repo_root, target_repo_root);
        assert_eq!(
            config.state_dir,
            dir.path().join(".aster-orch").join("state")
        );
        assert_eq!(
            config.db_path(),
            dir.path()
                .join(".aster-orch")
                .join("state")
                .join("jobs.sqlite")
        );
        assert_eq!(
            config.agents[0].prompt_file.as_deref(),
            Some(prompt_file.as_path())
        );
    }
}
