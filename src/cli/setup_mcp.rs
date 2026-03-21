//! `compas setup-mcp` — register compas as an MCP server in coding tools.
//!
//! Detects installed coding tools (Claude Code, Codex, OpenCode, Gemini)
//! and registers/unregisters compas in each via their respective config
//! mechanisms (CLI commands or JSON config file editing).

use super::detection::{self, Tool};
use std::path::Path;
use std::process::Command;

/// Outcome of a single tool registration attempt.
#[derive(Debug, PartialEq)]
pub enum RegistrationResult {
    /// Successfully registered.
    Registered(String),
    /// Successfully unregistered.
    Unregistered(String),
    /// Already registered (idempotent skip).
    AlreadyRegistered,
    /// Already unregistered (idempotent skip).
    AlreadyUnregistered,
    /// Tool not installed, skipped.
    NotInstalled,
    /// Would have performed action (dry-run).
    DryRun(String),
    /// Error during registration.
    Error(String),
}

/// Run the `setup-mcp` command.
pub fn run(
    tool_filter: Option<&str>,
    remove: bool,
    dry_run: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Validate --tool filter if provided.
    let filter: Option<Tool> = match tool_filter {
        Some(name) => {
            let t = Tool::from_name(name).ok_or_else(|| {
                format!(
                    "unknown tool '{}'. Supported tools: claude, codex, opencode, gemini",
                    name
                )
            })?;
            Some(t)
        }
        None => None,
    };

    println!("Detecting installed coding tools...");
    println!();

    let tools_to_process: Vec<Tool> = match filter {
        Some(t) => vec![t],
        None => Tool::all().to_vec(),
    };

    let installed = detection::detect_installed_tools();

    // When --tool explicitly targets an uninstalled tool, exit with an error.
    if let Some(t) = filter {
        if !installed.contains(&t) {
            return Err(format!("'{}' is not installed", t).into());
        }
    }

    let mut changed_count = 0u32;
    let mut up_to_date_count = 0u32;

    for tool in &tools_to_process {
        let is_installed = installed.contains(tool);
        let result = process_tool(tool, is_installed, remove, dry_run);

        let (icon, status) = format_result(&result);
        println!(" {} {:<10} {}", icon, tool.binary_name(), status);

        match result {
            RegistrationResult::Registered(_) | RegistrationResult::Unregistered(_) => {
                changed_count += 1;
            }
            RegistrationResult::AlreadyRegistered | RegistrationResult::AlreadyUnregistered => {
                up_to_date_count += 1;
            }
            RegistrationResult::DryRun(_) => {
                changed_count += 1;
            }
            RegistrationResult::NotInstalled | RegistrationResult::Error(_) => {}
        }
    }

    println!();

    if dry_run {
        println!("Dry run complete. No changes were made.");
    } else {
        let action = if remove { "unregistered" } else { "registered" };
        let mut parts = Vec::new();
        if changed_count > 0 {
            parts.push(format!(
                "{} in {} tool{}",
                action,
                changed_count,
                if changed_count == 1 { "" } else { "s" }
            ));
        }
        if up_to_date_count > 0 {
            parts.push(format!(
                "already up to date in {} tool{}",
                up_to_date_count,
                if up_to_date_count == 1 { "" } else { "s" }
            ));
        }
        if parts.is_empty() {
            println!("All done. No tools to process.");
        } else {
            println!("All done. MCP server {}.", parts.join(", "));
        }
    }

    Ok(())
}

/// Process a single tool: register, unregister, or dry-run.
fn process_tool(
    tool: &Tool,
    is_installed: bool,
    remove: bool,
    dry_run: bool,
) -> RegistrationResult {
    if !is_installed {
        return RegistrationResult::NotInstalled;
    }

    let already_registered = detection::is_compas_registered(tool);

    if dry_run {
        return dry_run_result(tool, already_registered, remove);
    }

    if remove {
        if !already_registered {
            return RegistrationResult::AlreadyUnregistered;
        }
        unregister_tool(tool)
    } else {
        if already_registered {
            return RegistrationResult::AlreadyRegistered;
        }
        register_tool(tool)
    }
}

fn dry_run_result(tool: &Tool, already_registered: bool, remove: bool) -> RegistrationResult {
    if remove {
        if !already_registered {
            RegistrationResult::DryRun("already unregistered, would skip".to_string())
        } else {
            let desc = unregister_description(tool);
            RegistrationResult::DryRun(format!("would unregister ({})", desc))
        }
    } else if already_registered {
        RegistrationResult::DryRun("already registered, would skip".to_string())
    } else {
        let desc = register_description(tool);
        RegistrationResult::DryRun(format!("would register ({})", desc))
    }
}

fn format_result(result: &RegistrationResult) -> (&'static str, String) {
    match result {
        RegistrationResult::Registered(desc) => ("\u{2713}", format!("registered ({})", desc)),
        RegistrationResult::Unregistered(desc) => ("\u{2713}", format!("unregistered ({})", desc)),
        RegistrationResult::AlreadyRegistered => ("\u{2713}", "already registered".to_string()),
        RegistrationResult::AlreadyUnregistered => ("\u{2713}", "already unregistered".to_string()),
        RegistrationResult::NotInstalled => ("-", "not installed, skipping".to_string()),
        RegistrationResult::DryRun(desc) => ("~", desc.clone()),
        RegistrationResult::Error(msg) => ("\u{2717}", format!("error: {}", msg)),
    }
}

fn register_description(tool: &Tool) -> String {
    match tool {
        Tool::Claude => "claude mcp add --scope user ...".to_string(),
        Tool::Codex => "codex mcp add compas ...".to_string(),
        Tool::OpenCode => "edit ~/.config/opencode/opencode.json".to_string(),
        Tool::Gemini => "edit ~/.gemini/settings.json".to_string(),
    }
}

fn unregister_description(tool: &Tool) -> String {
    match tool {
        Tool::Claude => "claude mcp remove compas -s user".to_string(),
        Tool::Codex => "codex mcp remove compas".to_string(),
        Tool::OpenCode => "edit ~/.config/opencode/opencode.json".to_string(),
        Tool::Gemini => "edit ~/.gemini/settings.json".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

fn register_tool(tool: &Tool) -> RegistrationResult {
    match tool {
        Tool::Claude => register_claude(),
        Tool::Codex => register_codex(),
        Tool::OpenCode => register_opencode(),
        Tool::Gemini => register_gemini(),
    }
}

fn register_claude() -> RegistrationResult {
    let args = claude_register_args();
    match run_command("claude", &args) {
        Ok(_) => RegistrationResult::Registered("claude mcp add --scope user ...".to_string()),
        Err(e) => RegistrationResult::Error(e),
    }
}

fn register_codex() -> RegistrationResult {
    let args = codex_register_args();
    match run_command("codex", &args) {
        Ok(_) => RegistrationResult::Registered("codex mcp add compas ...".to_string()),
        Err(e) => RegistrationResult::Error(e),
    }
}

fn register_opencode() -> RegistrationResult {
    let path = match detection::opencode_config_path() {
        Some(p) => p,
        None => return RegistrationResult::Error("could not determine HOME directory".to_string()),
    };
    let entry = opencode_compas_entry();
    match upsert_json_entry(&path, &["mcp"], "compas", entry) {
        Ok(_) => RegistrationResult::Registered(format!("edited {}", path.display())),
        Err(e) => RegistrationResult::Error(e),
    }
}

fn register_gemini() -> RegistrationResult {
    let path = match detection::gemini_config_path() {
        Some(p) => p,
        None => return RegistrationResult::Error("could not determine HOME directory".to_string()),
    };
    let entry = gemini_compas_entry();
    match upsert_json_entry(&path, &["mcpServers"], "compas", entry) {
        Ok(_) => RegistrationResult::Registered(format!("edited {}", path.display())),
        Err(e) => RegistrationResult::Error(e),
    }
}

// ---------------------------------------------------------------------------
// Unregistration
// ---------------------------------------------------------------------------

fn unregister_tool(tool: &Tool) -> RegistrationResult {
    match tool {
        Tool::Claude => unregister_claude(),
        Tool::Codex => unregister_codex(),
        Tool::OpenCode => unregister_opencode(),
        Tool::Gemini => unregister_gemini(),
    }
}

fn unregister_claude() -> RegistrationResult {
    let args = claude_unregister_args();
    match run_command("claude", &args) {
        Ok(_) => RegistrationResult::Unregistered("claude mcp remove compas -s user".to_string()),
        Err(e) => RegistrationResult::Error(e),
    }
}

fn unregister_codex() -> RegistrationResult {
    let args = codex_unregister_args();
    match run_command("codex", &args) {
        Ok(_) => RegistrationResult::Unregistered("codex mcp remove compas".to_string()),
        Err(e) => RegistrationResult::Error(e),
    }
}

fn unregister_opencode() -> RegistrationResult {
    let path = match detection::opencode_config_path() {
        Some(p) => p,
        None => return RegistrationResult::Error("could not determine HOME directory".to_string()),
    };
    match remove_json_entry(&path, &["mcp"], "compas") {
        Ok(_) => RegistrationResult::Unregistered(format!("edited {}", path.display())),
        Err(e) => RegistrationResult::Error(e),
    }
}

fn unregister_gemini() -> RegistrationResult {
    let path = match detection::gemini_config_path() {
        Some(p) => p,
        None => return RegistrationResult::Error("could not determine HOME directory".to_string()),
    };
    match remove_json_entry(&path, &["mcpServers"], "compas") {
        Ok(_) => RegistrationResult::Unregistered(format!("edited {}", path.display())),
        Err(e) => RegistrationResult::Error(e),
    }
}

// ---------------------------------------------------------------------------
// CLI command arguments
// ---------------------------------------------------------------------------

/// Build `claude mcp add` args.
pub fn claude_register_args() -> Vec<&'static str> {
    vec![
        "mcp",
        "add",
        "--scope",
        "user",
        "--transport",
        "stdio",
        "compas",
        "--",
        "compas",
        "mcp-server",
    ]
}

/// Build `claude mcp remove` args.
pub fn claude_unregister_args() -> Vec<&'static str> {
    vec!["mcp", "remove", "compas", "-s", "user"]
}

/// Build `codex mcp add` args.
pub fn codex_register_args() -> Vec<&'static str> {
    vec!["mcp", "add", "compas", "--", "compas", "mcp-server"]
}

/// Build `codex mcp remove` args.
pub fn codex_unregister_args() -> Vec<&'static str> {
    vec!["mcp", "remove", "compas"]
}

// ---------------------------------------------------------------------------
// JSON config entries
// ---------------------------------------------------------------------------

/// The JSON value for the compas entry in OpenCode config.
pub fn opencode_compas_entry() -> serde_json::Value {
    serde_json::json!({
        "type": "local",
        "command": ["compas", "mcp-server"],
        "enabled": true
    })
}

/// The JSON value for the compas entry in Gemini config.
pub fn gemini_compas_entry() -> serde_json::Value {
    serde_json::json!({
        "command": "compas",
        "args": ["mcp-server"]
    })
}

// ---------------------------------------------------------------------------
// JSON file editing
// ---------------------------------------------------------------------------

/// Insert or update a key inside a nested JSON object.
///
/// `parent_keys` is the path to the parent object (created if missing).
/// `entry_key` is the key to insert/overwrite in that parent.
///
/// Example: `upsert_json_entry(path, &["mcp"], "compas", value)` ensures
/// `{"mcp": {"compas": <value>, ...existing...}}`.
pub fn upsert_json_entry(
    path: &Path,
    parent_keys: &[&str],
    entry_key: &str,
    entry_value: serde_json::Value,
) -> Result<(), String> {
    let mut root = read_or_create_json(path)?;

    // Navigate to or create the parent object.
    let mut current = &mut root;
    for key in parent_keys {
        if !current.is_object() {
            return Err(format!(
                "expected JSON object at key path, found {}",
                json_type_name(current)
            ));
        }
        // Safe: we verified is_object() above.
        let obj = current.as_object_mut().expect("verified is_object");
        if !obj.contains_key(*key) {
            obj.insert(key.to_string(), serde_json::json!({}));
        }
        current = current.get_mut(*key).expect("key was just inserted");
    }

    if !current.is_object() {
        return Err(format!(
            "expected JSON object for parent, found {}",
            json_type_name(current)
        ));
    }

    // Safe: we verified is_object() above.
    current
        .as_object_mut()
        .expect("verified is_object")
        .insert(entry_key.to_string(), entry_value);

    write_json(path, &root)
}

/// Remove a key from a nested JSON object.
///
/// `parent_keys` is the path to the parent object.
/// `entry_key` is the key to remove from that parent.
pub fn remove_json_entry(path: &Path, parent_keys: &[&str], entry_key: &str) -> Result<(), String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("failed to read {}: {}", path.display(), e))?;
    let mut root: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| format!("failed to parse {}: {}", path.display(), e))?;

    let mut current = &mut root;
    for key in parent_keys {
        match current.get_mut(*key) {
            Some(v) => current = v,
            None => return Ok(()), // parent doesn't exist, nothing to remove
        }
    }

    if let Some(obj) = current.as_object_mut() {
        obj.remove(entry_key);
    }

    write_json(path, &root)
}

/// Read a JSON file, or return an empty object `{}` if the file doesn't exist.
fn read_or_create_json(path: &Path) -> Result<serde_json::Value, String> {
    match std::fs::read_to_string(path) {
        Ok(content) => {
            if content.trim().is_empty() {
                return Ok(serde_json::json!({}));
            }
            serde_json::from_str(&content)
                .map_err(|e| format!("failed to parse {}: {}", path.display(), e))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(serde_json::json!({})),
        Err(e) => Err(format!("failed to read {}: {}", path.display(), e)),
    }
}

/// Write a JSON value to a file, creating parent directories if needed.
fn write_json(path: &Path, value: &serde_json::Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create directory {}: {}", parent.display(), e))?;
    }
    let formatted = serde_json::to_string_pretty(value)
        .map_err(|e| format!("failed to serialize JSON: {}", e))?;
    std::fs::write(path, formatted.as_bytes())
        .map_err(|e| format!("failed to write {}: {}", path.display(), e))?;
    Ok(())
}

fn json_type_name(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// Run a CLI command and return Ok(stdout) on success or Err(message) on failure.
fn run_command(binary: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(binary)
        .args(args)
        .output()
        .map_err(|e| format!("failed to run {} {}: {}", binary, args.join(" "), e))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let detail = if !stderr.is_empty() {
            stderr.trim().to_string()
        } else {
            stdout.trim().to_string()
        };
        Err(format!(
            "{} {} failed (exit {}): {}",
            binary,
            args.join(" "),
            output.status.code().unwrap_or(-1),
            detail
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // ── CLI arg construction ─────────────────────────────────────────────

    #[test]
    fn test_claude_register_args() {
        let args = claude_register_args();
        assert_eq!(
            args,
            vec![
                "mcp",
                "add",
                "--scope",
                "user",
                "--transport",
                "stdio",
                "compas",
                "--",
                "compas",
                "mcp-server"
            ]
        );
    }

    #[test]
    fn test_claude_unregister_args() {
        let args = claude_unregister_args();
        assert_eq!(args, vec!["mcp", "remove", "compas", "-s", "user"]);
    }

    #[test]
    fn test_codex_register_args() {
        let args = codex_register_args();
        assert_eq!(
            args,
            vec!["mcp", "add", "compas", "--", "compas", "mcp-server"]
        );
    }

    #[test]
    fn test_codex_unregister_args() {
        let args = codex_unregister_args();
        assert_eq!(args, vec!["mcp", "remove", "compas"]);
    }

    // ── JSON entry values ────────────────────────────────────────────────

    #[test]
    fn test_opencode_compas_entry() {
        let entry = opencode_compas_entry();
        assert_eq!(entry["type"], "local");
        assert_eq!(
            entry["command"],
            serde_json::json!(["compas", "mcp-server"])
        );
        assert_eq!(entry["enabled"], true);
    }

    #[test]
    fn test_gemini_compas_entry() {
        let entry = gemini_compas_entry();
        assert_eq!(entry["command"], "compas");
        assert_eq!(entry["args"], serde_json::json!(["mcp-server"]));
    }

    // ── JSON file editing ────────────────────────────────────────────────

    #[test]
    fn test_upsert_json_creates_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("new.json");

        upsert_json_entry(
            &path,
            &["mcp"],
            "compas",
            serde_json::json!({"type": "local"}),
        )
        .unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["mcp"]["compas"]["type"], "local");
    }

    #[test]
    fn test_upsert_json_preserves_existing_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("existing.json");
        std::fs::write(
            &path,
            r#"{"mcp": {"other-server": {"url": "http://example.com"}}, "version": 1}"#,
        )
        .unwrap();

        upsert_json_entry(
            &path,
            &["mcp"],
            "compas",
            serde_json::json!({"type": "local"}),
        )
        .unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["mcp"]["compas"]["type"], "local");
        assert_eq!(parsed["mcp"]["other-server"]["url"], "http://example.com");
        assert_eq!(parsed["version"], 1);
    }

    #[test]
    fn test_upsert_json_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("idem.json");

        let entry = serde_json::json!({"type": "local"});

        upsert_json_entry(&path, &["mcp"], "compas", entry.clone()).unwrap();
        upsert_json_entry(&path, &["mcp"], "compas", entry.clone()).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["mcp"]["compas"]["type"], "local");
    }

    #[test]
    fn test_upsert_json_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deep").join("nested").join("config.json");

        upsert_json_entry(
            &path,
            &["mcp"],
            "compas",
            serde_json::json!({"type": "local"}),
        )
        .unwrap();

        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["mcp"]["compas"]["type"], "local");
    }

    #[test]
    fn test_upsert_json_creates_intermediate_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.json");
        std::fs::write(&path, "{}").unwrap();

        upsert_json_entry(
            &path,
            &["mcpServers"],
            "compas",
            serde_json::json!({"command": "compas"}),
        )
        .unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["mcpServers"]["compas"]["command"], "compas");
    }

    #[test]
    fn test_upsert_json_errors_on_non_object_parent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.json");
        std::fs::write(&path, r#"{"mcp": "not-an-object"}"#).unwrap();

        let result = upsert_json_entry(
            &path,
            &["mcp"],
            "compas",
            serde_json::json!({"type": "local"}),
        );
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("expected JSON object"), "error was: {}", err);
    }

    #[test]
    fn test_remove_json_entry_removes_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("remove.json");
        std::fs::write(
            &path,
            r#"{"mcp": {"compas": {"type": "local"}, "other": {}}}"#,
        )
        .unwrap();

        remove_json_entry(&path, &["mcp"], "compas").unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(parsed["mcp"]["compas"].is_null());
        assert!(parsed["mcp"]["other"].is_object());
    }

    #[test]
    fn test_remove_json_entry_noop_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("noop.json");
        std::fs::write(&path, r#"{"mcp": {"other": {}}}"#).unwrap();

        remove_json_entry(&path, &["mcp"], "compas").unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(parsed["mcp"]["other"].is_object());
    }

    #[test]
    fn test_remove_json_entry_noop_when_parent_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("noop2.json");
        std::fs::write(&path, r#"{"version": 1}"#).unwrap();

        remove_json_entry(&path, &["mcp"], "compas").unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["version"], 1);
    }

    #[test]
    fn test_remove_json_errors_on_missing_file() {
        let path = PathBuf::from("/nonexistent/path/config.json");
        let result = remove_json_entry(&path, &["mcp"], "compas");
        assert!(result.is_err());
    }

    #[test]
    fn test_remove_json_errors_on_malformed_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.json");
        std::fs::write(&path, "not json").unwrap();

        let result = remove_json_entry(&path, &["mcp"], "compas");
        assert!(result.is_err());
    }

    #[test]
    fn test_upsert_json_empty_file_treated_as_empty_object() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.json");
        std::fs::write(&path, "").unwrap();

        upsert_json_entry(
            &path,
            &["mcp"],
            "compas",
            serde_json::json!({"type": "local"}),
        )
        .unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["mcp"]["compas"]["type"], "local");
    }

    // ── Full OpenCode/Gemini round-trip ──────────────────────────────────

    #[test]
    fn test_opencode_register_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.json");

        // Register into new file.
        upsert_json_entry(&path, &["mcp"], "compas", opencode_compas_entry()).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["mcp"]["compas"]["type"], "local");
        assert_eq!(
            parsed["mcp"]["compas"]["command"],
            serde_json::json!(["compas", "mcp-server"])
        );
        assert_eq!(parsed["mcp"]["compas"]["enabled"], true);

        // Unregister.
        remove_json_entry(&path, &["mcp"], "compas").unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(parsed["mcp"]["compas"].is_null());
    }

    #[test]
    fn test_gemini_register_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");

        // Register into new file.
        upsert_json_entry(&path, &["mcpServers"], "compas", gemini_compas_entry()).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["mcpServers"]["compas"]["command"], "compas");
        assert_eq!(
            parsed["mcpServers"]["compas"]["args"],
            serde_json::json!(["mcp-server"])
        );

        // Unregister.
        remove_json_entry(&path, &["mcpServers"], "compas").unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(parsed["mcpServers"]["compas"].is_null());
    }

    // ── process_tool / dry-run logic ─────────────────────────────────────

    #[test]
    fn test_process_tool_not_installed() {
        let result = process_tool(&Tool::Claude, false, false, false);
        assert_eq!(result, RegistrationResult::NotInstalled);
    }

    #[test]
    fn test_dry_run_register_not_installed() {
        let result = process_tool(&Tool::Claude, false, false, true);
        assert_eq!(result, RegistrationResult::NotInstalled);
    }

    #[test]
    fn test_dry_run_result_register() {
        let result = dry_run_result(&Tool::Claude, false, false);
        match result {
            RegistrationResult::DryRun(msg) => {
                assert!(msg.contains("would register"), "msg was: {}", msg);
            }
            _ => panic!("expected DryRun, got {:?}", result),
        }
    }

    #[test]
    fn test_dry_run_result_already_registered() {
        let result = dry_run_result(&Tool::Claude, true, false);
        match result {
            RegistrationResult::DryRun(msg) => {
                assert!(msg.contains("already registered"), "msg was: {}", msg);
            }
            _ => panic!("expected DryRun, got {:?}", result),
        }
    }

    #[test]
    fn test_dry_run_result_unregister() {
        let result = dry_run_result(&Tool::Gemini, true, true);
        match result {
            RegistrationResult::DryRun(msg) => {
                assert!(msg.contains("would unregister"), "msg was: {}", msg);
            }
            _ => panic!("expected DryRun, got {:?}", result),
        }
    }

    #[test]
    fn test_dry_run_result_already_unregistered() {
        let result = dry_run_result(&Tool::Codex, false, true);
        match result {
            RegistrationResult::DryRun(msg) => {
                assert!(msg.contains("already unregistered"), "msg was: {}", msg);
            }
            _ => panic!("expected DryRun, got {:?}", result),
        }
    }

    #[test]
    fn test_format_result_registered() {
        let (icon, msg) = format_result(&RegistrationResult::Registered("via CLI".to_string()));
        assert_eq!(icon, "\u{2713}");
        assert!(msg.contains("registered"));
    }

    #[test]
    fn test_format_result_not_installed() {
        let (icon, msg) = format_result(&RegistrationResult::NotInstalled);
        assert_eq!(icon, "-");
        assert!(msg.contains("not installed"));
    }

    #[test]
    fn test_format_result_error() {
        let (icon, msg) = format_result(&RegistrationResult::Error("boom".to_string()));
        assert_eq!(icon, "\u{2717}");
        assert!(msg.contains("error: boom"));
    }

    // ── Tool filter validation (via run) ─────────────────────────────────

    #[test]
    fn test_run_rejects_unknown_tool() {
        let result = run(Some("unknown-tool"), false, true);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("unknown tool"), "error was: {}", err);
    }
}
