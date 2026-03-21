//! Shared detection utilities for coding tools.
//!
//! Used by `compas setup-mcp` (and later `compas doctor`) to discover
//! which AI coding tools are installed and whether compas is registered
//! as an MCP server in each.

use crate::backend::process::command_exists;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Coding tools that compas can integrate with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Claude,
    Codex,
    OpenCode,
    Gemini,
}

impl Tool {
    /// CLI binary name used to detect the tool.
    pub fn binary_name(&self) -> &'static str {
        match self {
            Tool::Claude => "claude",
            Tool::Codex => "codex",
            Tool::OpenCode => "opencode",
            Tool::Gemini => "gemini",
        }
    }

    /// All known tools, in display order.
    pub fn all() -> &'static [Tool] {
        &[Tool::Claude, Tool::Codex, Tool::OpenCode, Tool::Gemini]
    }

    /// Parse a tool name from a user-provided string.
    pub fn from_name(name: &str) -> Option<Tool> {
        match name.to_lowercase().as_str() {
            "claude" => Some(Tool::Claude),
            "codex" => Some(Tool::Codex),
            "opencode" => Some(Tool::OpenCode),
            "gemini" => Some(Tool::Gemini),
            _ => None,
        }
    }
}

impl fmt::Display for Tool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.binary_name())
    }
}

/// Returns the list of tools currently installed (binary found in PATH).
pub fn detect_installed_tools() -> Vec<Tool> {
    Tool::all()
        .iter()
        .filter(|t| command_exists(t.binary_name()))
        .copied()
        .collect()
}

/// Check whether compas is registered as an MCP server in the given tool.
pub fn is_compas_registered(tool: &Tool) -> bool {
    match tool {
        Tool::Claude => is_registered_claude(),
        Tool::Codex => is_registered_codex(),
        Tool::OpenCode => is_registered_opencode(),
        Tool::Gemini => is_registered_gemini(),
    }
}

/// Claude Code: run `claude mcp list` and look for "compas:" in the output.
fn is_registered_claude() -> bool {
    let output = Command::new("claude").args(["mcp", "list"]).output().ok();
    match output {
        Some(o) if o.status.success() => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            stdout.contains("compas:")
        }
        _ => false,
    }
}

/// Codex: run `codex mcp list` and look for "compas:" in the output.
fn is_registered_codex() -> bool {
    let output = Command::new("codex").args(["mcp", "list"]).output().ok();
    match output {
        Some(o) if o.status.success() => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            stdout.contains("compas:")
        }
        _ => false,
    }
}

/// OpenCode: check `~/.config/opencode/opencode.json` for `mcp.compas`.
fn is_registered_opencode() -> bool {
    let Some(path) = opencode_config_path() else {
        return false;
    };
    is_compas_in_json_object(&path, &["mcp", "compas"])
}

/// Gemini: check `~/.gemini/settings.json` for `mcpServers.compas`.
fn is_registered_gemini() -> bool {
    let Some(path) = gemini_config_path() else {
        return false;
    };
    is_compas_in_json_object(&path, &["mcpServers", "compas"])
}

/// Resolve the OpenCode config path: `~/.config/opencode/opencode.json`.
pub fn opencode_config_path() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(".config/opencode/opencode.json"))
}

/// Resolve the Gemini config path: `~/.gemini/settings.json`.
pub fn gemini_config_path() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(".gemini/settings.json"))
}

/// Check whether a nested key path exists in a JSON file.
fn is_compas_in_json_object(path: &Path, keys: &[&str]) -> bool {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return false,
    };
    let json: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let mut current = &json;
    for key in keys {
        match current.get(key) {
            Some(v) => current = v,
            None => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_binary_names() {
        assert_eq!(Tool::Claude.binary_name(), "claude");
        assert_eq!(Tool::Codex.binary_name(), "codex");
        assert_eq!(Tool::OpenCode.binary_name(), "opencode");
        assert_eq!(Tool::Gemini.binary_name(), "gemini");
    }

    #[test]
    fn test_tool_from_name() {
        assert_eq!(Tool::from_name("claude"), Some(Tool::Claude));
        assert_eq!(Tool::from_name("Claude"), Some(Tool::Claude));
        assert_eq!(Tool::from_name("CODEX"), Some(Tool::Codex));
        assert_eq!(Tool::from_name("opencode"), Some(Tool::OpenCode));
        assert_eq!(Tool::from_name("gemini"), Some(Tool::Gemini));
        assert_eq!(Tool::from_name("unknown"), None);
    }

    #[test]
    fn test_tool_from_name_invalid() {
        assert_eq!(Tool::from_name("vscode"), None);
        assert_eq!(Tool::from_name(""), None);
    }

    #[test]
    fn test_tool_display() {
        assert_eq!(format!("{}", Tool::Claude), "claude");
        assert_eq!(format!("{}", Tool::OpenCode), "opencode");
    }

    #[test]
    fn test_all_tools_returns_four() {
        assert_eq!(Tool::all().len(), 4);
    }

    #[test]
    fn test_is_compas_in_json_present() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, r#"{"mcp": {"compas": {"type": "local"}}}"#).unwrap();
        assert!(is_compas_in_json_object(&path, &["mcp", "compas"]));
    }

    #[test]
    fn test_is_compas_in_json_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, r#"{"mcp": {"other": {}}}"#).unwrap();
        assert!(!is_compas_in_json_object(&path, &["mcp", "compas"]));
    }

    #[test]
    fn test_is_compas_in_json_missing_file() {
        let path = PathBuf::from("/nonexistent/config.json");
        assert!(!is_compas_in_json_object(&path, &["mcp", "compas"]));
    }

    #[test]
    fn test_is_compas_in_json_malformed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, "not json at all").unwrap();
        assert!(!is_compas_in_json_object(&path, &["mcp", "compas"]));
    }

    #[test]
    fn test_is_compas_in_json_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, "").unwrap();
        assert!(!is_compas_in_json_object(&path, &["mcp", "compas"]));
    }

    #[test]
    fn test_is_compas_in_json_nested_gemini_format() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(
            &path,
            r#"{"mcpServers": {"compas": {"command": "compas", "args": ["mcp-server"]}}}"#,
        )
        .unwrap();
        assert!(is_compas_in_json_object(&path, &["mcpServers", "compas"]));
    }
}
