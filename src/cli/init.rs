use crate::backend::process::command_exists;
use std::path::{Path, PathBuf};

/// Known backends and their CLI binary names.
const KNOWN_BACKENDS: &[&str] = &["claude", "codex", "gemini", "opencode"];

/// Produce a YAML-safe quoted string suitable for inline value interpolation.
/// Uses serde_yaml to handle special characters (`:`, `{`, `}`, `[`, `]`, etc.).
fn yaml_quote(value: &str) -> String {
    serde_yaml::to_string(value)
        .unwrap_or_else(|_| format!("\"{}\"", value))
        .trim_end()
        .to_string()
}

/// Run the `compas init` command.
#[allow(clippy::too_many_arguments)]
pub fn run(
    config_path: PathBuf,
    force: bool,
    non_interactive: bool,
    repo: Option<PathBuf>,
    backend: Option<String>,
    agent_alias: Option<String>,
    model: Option<String>,
    minimal: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // 1. Check overwrite
    if config_path.exists() && !force {
        return Err(format!(
            "config file already exists at {}. Use `compas init --force` to overwrite.",
            config_path.display()
        )
        .into());
    }

    // 2. Detect installed backends
    let detected: Vec<&str> = KNOWN_BACKENDS
        .iter()
        .filter(|b| command_exists(b))
        .copied()
        .collect();

    // 3. Gather values (interactive or flags/defaults)
    let target_repo_root: String;
    let chosen_backend: String;
    let alias: String;
    let chosen_model: Option<String>;

    if non_interactive {
        target_repo_root = match repo {
            Some(p) => p.to_string_lossy().to_string(),
            None => ".".to_string(),
        };
        chosen_backend = match backend {
            Some(b) => {
                validate_backend_name(&b)?;
                b
            }
            None => detected.first().map(|s| s.to_string()).ok_or(
                "no backends detected and --backend not specified. \
                 Install one of: claude, codex, gemini, opencode",
            )?,
        };
        alias = agent_alias.unwrap_or_else(|| "dev".to_string());
        chosen_model = model;
    } else {
        // Interactive prompts
        target_repo_root = prompt_repo_root(repo.as_deref())?;
        chosen_backend = prompt_backend(&detected, backend.as_deref())?;
        alias = prompt_alias(agent_alias.as_deref())?;
        chosen_model = prompt_model(model.as_deref())?;
    }

    // 4. Generate config
    let yaml = generate_config(
        &target_repo_root,
        &chosen_backend,
        &alias,
        chosen_model.as_deref(),
        minimal,
    );

    // 5. Create parent directory if needed
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // 6. Write config
    std::fs::write(&config_path, &yaml)?;

    // 7. Print summary
    println!("Created config at {}", config_path.display());
    println!();
    println!("  default_workdir:  {}", target_repo_root);
    println!("  backend:          {}", chosen_backend);
    println!("  agent alias:      {}", alias);
    if let Some(ref m) = chosen_model {
        println!("  model:            {}", m);
    }
    println!();
    println!("Next steps:");
    println!("  1. Review the config: {}", config_path.display());
    println!("  2. Run `compas setup mcp` to register the MCP server");

    Ok(())
}

fn validate_backend_name(name: &str) -> Result<(), Box<dyn std::error::Error>> {
    if !KNOWN_BACKENDS.contains(&name) {
        return Err(format!(
            "unknown backend '{}'. Supported backends: {}",
            name,
            KNOWN_BACKENDS.join(", ")
        )
        .into());
    }
    Ok(())
}

/// Determine a reasonable default repo root. Returns CWD unless it is $HOME or `/`.
fn default_repo_root() -> String {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let cwd_str = cwd.to_string_lossy().to_string();
    let home = std::env::var("HOME").unwrap_or_default();
    if cwd_str == home || cwd_str == "/" {
        ".".to_string()
    } else {
        cwd_str
    }
}

fn prompt_repo_root(flag: Option<&Path>) -> Result<String, Box<dyn std::error::Error>> {
    let default = match flag {
        Some(p) => p.to_string_lossy().to_string(),
        None => default_repo_root(),
    };
    let result: String = dialoguer::Input::new()
        .with_prompt("Target repository root")
        .default(default)
        .interact_text()?;
    Ok(result)
}

fn prompt_backend(
    detected: &[&str],
    flag: Option<&str>,
) -> Result<String, Box<dyn std::error::Error>> {
    if let Some(b) = flag {
        validate_backend_name(b)?;
        return Ok(b.to_string());
    }

    if detected.is_empty() {
        eprintln!(
            "Warning: no backends detected in PATH. \
             Install one of: claude, codex, gemini, opencode"
        );
        let result: String = dialoguer::Input::new()
            .with_prompt("Backend (claude, codex, gemini, opencode)")
            .interact_text()?;
        validate_backend_name(&result)?;
        return Ok(result);
    }

    let items: Vec<String> = detected.iter().map(|s| s.to_string()).collect();
    let selection = dialoguer::Select::new()
        .with_prompt("Backend")
        .items(&items)
        .default(0)
        .interact()?;
    Ok(items[selection].clone())
}

fn prompt_alias(flag: Option<&str>) -> Result<String, Box<dyn std::error::Error>> {
    let default = flag.unwrap_or("dev").to_string();
    let result: String = dialoguer::Input::new()
        .with_prompt("Agent alias")
        .default(default)
        .interact_text()?;
    Ok(result)
}

fn prompt_model(flag: Option<&str>) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let default = flag.unwrap_or("").to_string();
    let result: String = dialoguer::Input::new()
        .with_prompt("Model (optional, press Enter to skip)")
        .default(default)
        .allow_empty(true)
        .interact_text()?;
    if result.trim().is_empty() {
        Ok(None)
    } else {
        Ok(Some(result))
    }
}

/// Generate a YAML config string from user inputs.
///
/// When `minimal` is true, generates bare YAML without comments.
pub fn generate_config(
    target_repo_root: &str,
    backend: &str,
    alias: &str,
    model: Option<&str>,
    minimal: bool,
) -> String {
    if minimal {
        generate_minimal_config(target_repo_root, backend, alias, model)
    } else {
        generate_commented_config(target_repo_root, backend, alias, model)
    }
}

fn generate_minimal_config(
    target_repo_root: &str,
    backend: &str,
    alias: &str,
    model: Option<&str>,
) -> String {
    let mut yaml = String::new();
    yaml.push_str(&format!(
        "default_workdir: {}\n",
        yaml_quote(target_repo_root)
    ));
    yaml.push_str("state_dir: ~/.compas/state\n");
    yaml.push_str("poll_interval_secs: 1\n");
    yaml.push('\n');
    yaml.push_str("orchestration:\n");
    yaml.push_str("  execution_timeout_secs: 600\n");
    yaml.push('\n');
    yaml.push_str("agents:\n");
    yaml.push_str(&format!("  - alias: {}\n", yaml_quote(alias)));
    yaml.push_str(&format!("    backend: {}\n", backend));
    if crate::config::validation::BUILTIN_BACKEND_NAMES.contains(&backend) {
        yaml.push_str("    safety_mode: auto_approve\n");
    }
    if let Some(m) = model {
        yaml.push_str(&format!("    model: {}\n", yaml_quote(m)));
    }
    yaml
}

fn generate_commented_config(
    target_repo_root: &str,
    backend: &str,
    alias: &str,
    model: Option<&str>,
) -> String {
    let mut yaml = String::new();

    yaml.push_str("# Compas orchestrator configuration\n");
    yaml.push_str("# Docs: https://github.com/ottogiron/compas\n");
    yaml.push('\n');

    // default_workdir
    yaml.push_str(
        "# Default working directory for agents that do not specify a per-agent workdir.\n",
    );
    yaml.push_str(&format!(
        "default_workdir: {}\n",
        yaml_quote(target_repo_root)
    ));
    yaml.push('\n');

    // state_dir
    yaml.push_str("# Runtime directory for SQLite DB, logs, and state files.\n");
    yaml.push_str("state_dir: ~/.compas/state\n");
    yaml.push('\n');

    // poll_interval_secs
    yaml.push_str("# How often (seconds) the worker checks for new triggers.\n");
    yaml.push_str("poll_interval_secs: 1\n");
    yaml.push('\n');

    // orchestration
    yaml.push_str("orchestration:\n");
    yaml.push_str("  # Intents that trigger agent execution.\n");
    yaml.push_str("  # trigger_intents:\n");
    yaml.push_str("  #   - dispatch\n");
    yaml.push_str("  #   - handoff\n");
    yaml.push_str("  #   - changes-requested\n");
    yaml.push('\n');
    yaml.push_str("  # Max time (seconds) an agent execution can run before timeout.\n");
    yaml.push_str("  execution_timeout_secs: 600\n");
    yaml.push('\n');
    yaml.push_str("  # Max concurrent agent executions (default: worker agent count).\n");
    yaml.push_str("  # max_concurrent_triggers: 4\n");
    yaml.push('\n');

    // agents
    yaml.push_str("agents:\n");
    yaml.push_str(&format!("  - alias: {}\n", yaml_quote(alias)));
    yaml.push_str(&format!("    backend: {}\n", backend));
    if crate::config::validation::BUILTIN_BACKEND_NAMES.contains(&backend) {
        yaml.push_str("    safety_mode: auto_approve\n");
    }
    if let Some(m) = model {
        yaml.push_str(&format!("    model: {}\n", yaml_quote(m)));
    }
    yaml.push_str("    # prompt: >\n");
    yaml.push_str("    #   You implement changes in this repository.\n");
    yaml.push_str(
        "    # workspace: worktree        # \"worktree\" for git isolation, \"shared\" (default)\n",
    );
    yaml.push_str("    # timeout_secs: 600          # Per-agent timeout override\n");
    yaml.push_str("    # handoff:\n");
    yaml.push_str("    #   on_response: reviewer    # Auto-chain to another agent on completion\n");

    yaml
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_config_minimal_produces_valid_yaml() {
        let yaml = generate_config("/tmp", "claude", "dev", None, true);
        let config = crate::config::load_config_from_str(&yaml).unwrap();
        assert_eq!(config.agents.len(), 1);
        assert_eq!(config.agents[0].alias, "dev");
        assert_eq!(config.agents[0].backend, "claude");
    }

    #[test]
    fn test_generate_config_commented_produces_valid_yaml() {
        let yaml = generate_config("/tmp", "claude", "dev", None, false);
        let config = crate::config::load_config_from_str(&yaml).unwrap();
        assert_eq!(config.agents.len(), 1);
        assert_eq!(config.agents[0].alias, "dev");
        assert_eq!(config.agents[0].backend, "claude");
    }

    #[test]
    fn test_generate_config_with_model() {
        let yaml = generate_config("/tmp", "claude", "coder", Some("claude-opus-4-6"), false);
        let config = crate::config::load_config_from_str(&yaml).unwrap();
        assert_eq!(config.agents[0].model.as_deref(), Some("claude-opus-4-6"));
        assert_eq!(config.agents[0].alias, "coder");
    }

    #[test]
    fn test_generate_config_minimal_with_model() {
        let yaml = generate_config("/tmp", "codex", "dev", Some("gpt-5"), true);
        let config = crate::config::load_config_from_str(&yaml).unwrap();
        assert_eq!(config.agents[0].backend, "codex");
        assert_eq!(config.agents[0].model.as_deref(), Some("gpt-5"));
    }

    #[test]
    fn test_overwrite_protection() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.yaml");
        std::fs::write(&config_path, "existing").unwrap();

        let result = run(
            config_path.clone(),
            false, // no force
            true,  // non-interactive
            Some(PathBuf::from("/tmp")),
            Some("claude".to_string()),
            None,
            None,
            true,
        );
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("already exists"), "error was: {}", err);
        assert!(err.contains("--force"), "error was: {}", err);

        // File should not have been changed
        assert_eq!(std::fs::read_to_string(&config_path).unwrap(), "existing");
    }

    #[test]
    fn test_overwrite_with_force() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.yaml");
        std::fs::write(&config_path, "existing").unwrap();

        let result = run(
            config_path.clone(),
            true, // force
            true, // non-interactive
            Some(PathBuf::from("/tmp")),
            Some("claude".to_string()),
            None,
            None,
            true,
        );
        assert!(result.is_ok());

        // File should now contain valid YAML config
        let content = std::fs::read_to_string(&config_path).unwrap();
        let config = crate::config::load_config_from_str(&content).unwrap();
        assert_eq!(config.agents[0].backend, "claude");
    }

    #[test]
    fn test_non_interactive_with_explicit_flags() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.yaml");

        let result = run(
            config_path.clone(),
            false,
            true, // non-interactive
            Some(PathBuf::from("/tmp")),
            Some("gemini".to_string()),
            Some("my-agent".to_string()),
            Some("gemini-2.5-pro".to_string()),
            false,
        );
        assert!(result.is_ok());

        let content = std::fs::read_to_string(&config_path).unwrap();
        let config = crate::config::load_config_from_str(&content).unwrap();
        assert_eq!(config.agents[0].alias, "my-agent");
        assert_eq!(config.agents[0].backend, "gemini");
        assert_eq!(config.agents[0].model.as_deref(), Some("gemini-2.5-pro"));
    }

    #[test]
    fn test_non_interactive_uses_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.yaml");

        // This test may fail in CI if no backends are installed,
        // but we need at least `claude` or similar. Use explicit backend.
        let result = run(
            config_path.clone(),
            false,
            true, // non-interactive
            None,
            Some("claude".to_string()),
            None, // default alias "dev"
            None, // no model
            true,
        );
        assert!(result.is_ok());

        let content = std::fs::read_to_string(&config_path).unwrap();
        let config = crate::config::load_config_from_str(&content).unwrap();
        assert_eq!(config.agents[0].alias, "dev");
        assert_eq!(config.agents[0].backend, "claude");
        assert!(config.agents[0].model.is_none());
    }

    #[test]
    fn test_non_interactive_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("deep").join("nested").join("config.yaml");

        let result = run(
            config_path.clone(),
            false,
            true,
            Some(PathBuf::from("/tmp")),
            Some("claude".to_string()),
            None,
            None,
            true,
        );
        assert!(result.is_ok());
        assert!(config_path.exists());
    }

    #[test]
    fn test_load_config_error_includes_path_and_init_hint() {
        let missing_path = PathBuf::from("/nonexistent/path/config.yaml");
        let err = crate::config::load_config(&missing_path).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("/nonexistent/path/config.yaml"),
            "error should include path: {}",
            msg
        );
        assert!(
            msg.contains("compas init"),
            "error should mention compas init: {}",
            msg
        );
    }

    #[test]
    fn test_generate_config_yaml_special_chars_roundtrip() {
        // Paths and aliases with YAML-sensitive characters must survive round-trip.
        let edge_cases = [
            ("/home/user/my: project", "dev: v2", Some("model: test")),
            ("/path/with {braces}", "alias[0]", Some("m{o}del")),
            ("/path/with #comment", "plain", None),
        ];
        for (repo, alias, model) in &edge_cases {
            for minimal in [true, false] {
                let yaml = generate_config(repo, "claude", alias, *model, minimal);
                let parsed: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap_or_else(|e| {
                    panic!(
                        "invalid YAML for repo={}, alias={}, model={:?}, minimal={}: {}\n---\n{}",
                        repo, alias, model, minimal, e, yaml
                    )
                });
                let agents = parsed["agents"].as_sequence().unwrap();
                assert_eq!(agents[0]["alias"].as_str().unwrap(), *alias);
                assert_eq!(parsed["default_workdir"].as_str().unwrap(), *repo);
                if let Some(m) = model {
                    assert_eq!(agents[0]["model"].as_str().unwrap(), *m);
                }
            }
        }
    }

    #[test]
    fn test_validate_backend_name_valid() {
        assert!(validate_backend_name("claude").is_ok());
        assert!(validate_backend_name("codex").is_ok());
        assert!(validate_backend_name("gemini").is_ok());
        assert!(validate_backend_name("opencode").is_ok());
    }

    #[test]
    fn test_validate_backend_name_invalid() {
        let err = validate_backend_name("unknown").unwrap_err();
        assert!(err.to_string().contains("unknown backend"));
    }
}
