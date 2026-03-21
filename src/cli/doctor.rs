//! `compas doctor` — pre-flight validation with actionable fix suggestions.
//!
//! Runs an ordered checklist of health checks against the compas installation:
//! config, backends, worker, MCP registration. Collects all results before
//! reporting so the user sees every issue at once.

use crate::backend::process::command_exists;
use crate::backend::registry::BackendRegistry;
use crate::cli::detection::{self, Tool};
use crate::cli::setup_mcp;
use crate::config::types::OrchestratorConfig;
use crate::model::agent::Agent;
use crate::worker;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Check result types
// ---------------------------------------------------------------------------

/// Severity level for a single check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Pass,
    Warn,
    Fail,
}

/// A single check result.
#[derive(Debug, Clone)]
pub struct CheckResult {
    pub label: String,
    pub severity: Severity,
    pub detail: String,
    /// Optional fix suggestion (shown in the summary section).
    pub fix_hint: Option<String>,
}

impl CheckResult {
    fn pass(label: &str, detail: &str) -> Self {
        Self {
            label: label.to_string(),
            severity: Severity::Pass,
            detail: detail.to_string(),
            fix_hint: None,
        }
    }

    fn warn(label: &str, detail: &str, hint: &str) -> Self {
        Self {
            label: label.to_string(),
            severity: Severity::Warn,
            detail: detail.to_string(),
            fix_hint: Some(hint.to_string()),
        }
    }

    fn fail(label: &str, detail: &str, hint: &str) -> Self {
        Self {
            label: label.to_string(),
            severity: Severity::Fail,
            detail: detail.to_string(),
            fix_hint: Some(hint.to_string()),
        }
    }

    fn icon(&self) -> &'static str {
        match self.severity {
            Severity::Pass => "\u{2713}",
            Severity::Warn => "\u{26A0}",
            Severity::Fail => "\u{2717}",
        }
    }
}

/// Outcome of the full doctor run.
pub struct DoctorReport {
    pub results: Vec<CheckResult>,
    pub fixes_applied: Vec<String>,
}

impl DoctorReport {
    pub fn has_failures(&self) -> bool {
        self.results.iter().any(|r| r.severity == Severity::Fail)
    }

    pub fn failure_count(&self) -> usize {
        self.results
            .iter()
            .filter(|r| r.severity == Severity::Fail)
            .count()
    }

    pub fn warning_count(&self) -> usize {
        self.results
            .iter()
            .filter(|r| r.severity == Severity::Warn)
            .count()
    }
}

// ---------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------

/// Format a complete report to a string (for testability and display).
pub fn format_report(report: &DoctorReport) -> String {
    let mut out = String::new();
    out.push_str("Compas health check\n\n");

    for r in &report.results {
        out.push_str(&format!(" {} {:<18} {}\n", r.icon(), r.label, r.detail));
    }

    let fails: Vec<&CheckResult> = report
        .results
        .iter()
        .filter(|r| r.severity == Severity::Fail)
        .collect();
    let warns: Vec<&CheckResult> = report
        .results
        .iter()
        .filter(|r| r.severity == Severity::Warn)
        .collect();

    if !fails.is_empty() {
        out.push_str(&format!(
            "\n{} issue{} found:\n",
            fails.len(),
            if fails.len() == 1 { "" } else { "s" }
        ));
        for f in &fails {
            if let Some(ref hint) = f.fix_hint {
                out.push_str(&format!("  \u{2717} {} \u{2014} {}\n", f.detail, hint));
            }
        }
    }

    if !warns.is_empty() {
        out.push_str(&format!(
            "\n{} warning{}:\n",
            warns.len(),
            if warns.len() == 1 { "" } else { "s" }
        ));
        for w in &warns {
            if let Some(ref hint) = w.fix_hint {
                out.push_str(&format!("  \u{26A0} {} \u{2014} {}\n", w.detail, hint));
            }
        }
    }

    if !report.fixes_applied.is_empty() {
        out.push_str("\nFixes applied:\n");
        for fix in &report.fixes_applied {
            out.push_str(&format!("  \u{2713} {}\n", fix));
        }
    }

    if fails.is_empty() && warns.is_empty() {
        out.push_str("\nAll checks passed.\n");
    }

    out
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Run the doctor checks and return a report. This is async because it needs
/// to open the SQLite database for the worker heartbeat check and ping backends.
pub async fn run(config_path: PathBuf, fix: bool) -> DoctorReport {
    let mut results = Vec::new();
    let mut fixes_applied = Vec::new();

    // ── 1 & 2. Config file exists and validates ──────────────────────────
    let config = match check_config(&config_path) {
        Ok((config, check_results)) => {
            results.extend(check_results);
            Some(config)
        }
        Err(check_results) => {
            results.extend(check_results);
            // Without a valid config, remaining checks are skipped.
            return DoctorReport {
                results,
                fixes_applied,
            };
        }
    };

    let config = config.unwrap();

    // ── 3. Target repo root exists ───────────────────────────────────────
    results.push(check_target_repo(&config.default_workdir));

    // ── 4. State directory writable ──────────────────────────────────────
    results.push(check_state_dir(&config.state_dir));

    // ── 5. Backend CLIs installed ────────────────────────────────────────
    let unique_backends = unique_backends(&config);
    let mut installed_backends: HashSet<String> = HashSet::new();
    for backend_name in &unique_backends {
        let (check, is_installed) = check_backend_installed(backend_name);
        if is_installed {
            installed_backends.insert(backend_name.clone());
        }
        results.push(check);
    }

    // ── 6. Backend CLIs authenticated (ping) ─────────────────────────────
    // This is the most expensive check (~5s per backend). Print progress.
    if !installed_backends.is_empty() {
        println!("Pinging backends...");
        let _ = std::io::Write::flush(&mut std::io::stdout());
        let registry = build_doctor_registry(&config);
        let ping_results = check_backend_pings(&config, &registry, &installed_backends).await;
        results.extend(ping_results);
    }

    // ── 7. Worker running ────────────────────────────────────────────────
    let worker_check = check_worker(&config).await;
    results.push(worker_check);

    // ── 8. MCP registration ──────────────────────────────────────────────
    let mcp_results = check_mcp_registration();
    let unregistered_tools: Vec<Tool> = mcp_results
        .iter()
        .filter(|r| r.severity == Severity::Fail)
        .filter_map(|r| {
            // Extract tool name from the label: "MCP: <tool>"
            let tool_name = r.label.trim_start_matches("MCP: ");
            Tool::from_name(tool_name)
        })
        .collect();
    results.extend(mcp_results);

    // ── --fix: auto-register missing MCP servers ─────────────────────────
    if fix && !unregistered_tools.is_empty() {
        for tool in &unregistered_tools {
            match auto_register_mcp(tool) {
                Ok(msg) => {
                    fixes_applied.push(msg);
                    // Update the corresponding result from fail to pass.
                    for r in &mut results {
                        let expected_label = format!("MCP: {}", tool);
                        if r.label == expected_label && r.severity == Severity::Fail {
                            r.severity = Severity::Pass;
                            r.detail = "registered (auto-fixed)".to_string();
                            r.fix_hint = None;
                        }
                    }
                }
                Err(msg) => {
                    fixes_applied.push(format!("failed to register {}: {}", tool, msg));
                }
            }
        }
    }

    DoctorReport {
        results,
        fixes_applied,
    }
}

// ---------------------------------------------------------------------------
// Individual checks
// ---------------------------------------------------------------------------

/// Check 1 & 2: config file exists, parses, and validates.
/// Returns Ok((config, pass_results)) or Err(fail_results).
fn check_config(
    config_path: &Path,
) -> Result<(OrchestratorConfig, Vec<CheckResult>), Vec<CheckResult>> {
    let display_path = config_path.display().to_string();

    if !config_path.exists() {
        return Err(vec![CheckResult::fail(
            "Config file",
            &format!("{} not found", display_path),
            "run: compas init",
        )]);
    }

    match crate::config::load_config(config_path) {
        Ok(config) => {
            let agent_count = config.agents.len();
            let repo_display = config.default_workdir.display().to_string();
            Ok((
                config,
                vec![
                    CheckResult::pass("Config file", &display_path),
                    CheckResult::pass(
                        "Config valid",
                        &format!(
                            "{} agent{}, default_workdir: {}",
                            agent_count,
                            if agent_count == 1 { "" } else { "s" },
                            repo_display
                        ),
                    ),
                ],
            ))
        }
        Err(e) => {
            let err_msg = e.to_string();
            // Determine if this is a YAML parse error or a validation error.
            if err_msg.contains("yaml parse error") {
                Err(vec![
                    CheckResult::pass("Config file", &display_path),
                    CheckResult::fail("Config valid", &err_msg, "fix YAML syntax errors"),
                ])
            } else {
                Err(vec![
                    CheckResult::pass("Config file", &display_path),
                    CheckResult::fail("Config valid", &err_msg, "fix config validation errors"),
                ])
            }
        }
    }
}

/// Check 3: target repo root exists and is a directory.
fn check_target_repo(path: &Path) -> CheckResult {
    let display = path.display().to_string();
    if path.is_dir() {
        CheckResult::pass("Target repo", &format!("{} exists", display))
    } else if path.exists() {
        CheckResult::fail(
            "Target repo",
            &format!("{} is not a directory", display),
            "set default_workdir to a valid directory",
        )
    } else {
        CheckResult::fail(
            "Target repo",
            &format!("{} does not exist", display),
            "create the directory or update default_workdir in config",
        )
    }
}

/// Check 4: state directory is writable.
fn check_state_dir(state_dir: &Path) -> CheckResult {
    let display = state_dir.display().to_string();
    // Ensure the directory exists.
    if let Err(e) = std::fs::create_dir_all(state_dir) {
        return CheckResult::fail(
            "State directory",
            &format!("cannot create {}: {}", display, e),
            "check permissions or update state_dir in config",
        );
    }
    // Try creating a temp file.
    let probe = state_dir.join(".doctor-probe");
    match std::fs::write(&probe, b"probe") {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            CheckResult::pass("State directory", &format!("{} writable", display))
        }
        Err(e) => CheckResult::fail(
            "State directory",
            &format!("{} not writable: {}", display, e),
            "check permissions or update state_dir in config",
        ),
    }
}

/// Check 5: backend CLI installed. Returns (check_result, is_installed).
fn check_backend_installed(backend_name: &str) -> (CheckResult, bool) {
    let label = format!("Backend: {}", backend_name);
    if command_exists(backend_name) {
        // Try to get the full path via `which`.
        let path = Command::new("which")
            .arg(backend_name)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|| "found".to_string());
        (
            CheckResult::pass(&label, &format!("installed ({})", path)),
            true,
        )
    } else {
        let hint = install_hint(backend_name);
        (
            CheckResult::fail(
                &label,
                &format!("{} not found in PATH", backend_name),
                &format!("install: {}", hint),
            ),
            false,
        )
    }
}

/// Check 6: ping backends that are installed.
async fn check_backend_pings(
    config: &OrchestratorConfig,
    registry: &BackendRegistry,
    installed_backends: &HashSet<String>,
) -> Vec<CheckResult> {
    let mut results = Vec::new();

    // Pick one agent per backend to ping.
    let mut seen_backends: HashSet<String> = HashSet::new();
    for agent_config in &config.agents {
        if !installed_backends.contains(&agent_config.backend) {
            continue;
        }
        if seen_backends.contains(&agent_config.backend) {
            continue;
        }
        seen_backends.insert(agent_config.backend.clone());

        let label = format!("Ping: {}", agent_config.backend);
        let backend = match registry.get(agent_config) {
            Ok(b) => b,
            Err(_) => {
                results.push(CheckResult::warn(
                    &label,
                    "no backend registered",
                    "check config",
                ));
                continue;
            }
        };

        let agent = agent_from_config(agent_config);
        let ping_timeout = config.orchestration.ping_timeout_secs;
        let ping_result = backend.ping(&agent, ping_timeout).await;

        if ping_result.alive {
            results.push(CheckResult::pass(
                &label,
                &format!("alive ({}ms)", ping_result.latency_ms),
            ));
        } else {
            let detail_str = ping_result.detail.as_deref().unwrap_or("ping failed");
            results.push(CheckResult::fail(
                &label,
                detail_str,
                "check authentication and network connectivity",
            ));
        }
    }

    results
}

/// Check 7: worker heartbeat.
async fn check_worker(config: &OrchestratorConfig) -> CheckResult {
    let db_path = config.db_path();
    if !db_path.exists() {
        return CheckResult::warn(
            "Worker",
            "database not found (no worker has run yet)",
            "start with: compas dashboard",
        );
    }

    let db_url = format!("sqlite:{}?mode=ro", db_path.display());
    let pool = match sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&db_url)
        .await
    {
        Ok(p) => p,
        Err(e) => {
            return CheckResult::warn(
                "Worker",
                &format!("cannot open database: {}", e),
                "start with: compas dashboard",
            );
        }
    };

    // Intentionally skip store.setup() — DB opened read-only, table creation
    // would fail. Missing tables produce a Warn, which is correct for
    // first-time installs.
    let store = crate::store::Store::new(pool);
    let heartbeat = match store.latest_heartbeat().await {
        Ok(hb) => hb,
        Err(e) => {
            return CheckResult::warn(
                "Worker",
                &format!("heartbeat query failed: {}", e),
                "start with: compas dashboard",
            );
        }
    };

    let max_age = worker::WORKER_HEARTBEAT_MAX_AGE_SECS;
    if worker::is_worker_alive(&heartbeat, max_age) {
        if let Some((worker_id, _last_beat, started_at, _)) = &heartbeat {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64;
            let uptime_secs = now - started_at;
            let uptime_display = format_duration_human(uptime_secs);
            CheckResult::pass(
                "Worker",
                &format!("running ({}, uptime {})", worker_id, uptime_display),
            )
        } else {
            CheckResult::pass("Worker", "running")
        }
    } else {
        CheckResult::warn("Worker", "not running", "start with: compas dashboard")
    }
}

/// Check 8: MCP registration for installed tools.
fn check_mcp_registration() -> Vec<CheckResult> {
    let installed = detection::detect_installed_tools();
    let mut results = Vec::new();

    for tool in &installed {
        let label = format!("MCP: {}", tool);
        if detection::is_compas_registered(tool) {
            results.push(CheckResult::pass(&label, "registered"));
        } else {
            results.push(CheckResult::fail(
                &label,
                &format!("compas not registered in {}", tool),
                &format!("run: compas setup-mcp --tool {}", tool),
            ));
        }
    }

    results
}

// ---------------------------------------------------------------------------
// Auto-fix helpers
// ---------------------------------------------------------------------------

/// Attempt to auto-register compas in the given tool.
/// Reuses setup_mcp functions directly (no subprocess).
fn auto_register_mcp(tool: &Tool) -> Result<String, String> {
    match tool {
        Tool::Claude => {
            let args = setup_mcp::claude_register_args();
            run_fix_command("claude", &args)?;
            Ok("registered compas in claude".to_string())
        }
        Tool::Codex => {
            let args = setup_mcp::codex_register_args();
            run_fix_command("codex", &args)?;
            Ok("registered compas in codex".to_string())
        }
        Tool::OpenCode => {
            let path = detection::opencode_config_path()
                .ok_or_else(|| "could not determine HOME directory".to_string())?;
            let entry = setup_mcp::opencode_compas_entry();
            setup_mcp::upsert_json_entry(&path, &["mcp"], "compas", entry)?;
            Ok(format!(
                "registered compas in opencode ({})",
                path.display()
            ))
        }
        Tool::Gemini => {
            let path = detection::gemini_config_path()
                .ok_or_else(|| "could not determine HOME directory".to_string())?;
            let entry = setup_mcp::gemini_compas_entry();
            setup_mcp::upsert_json_entry(&path, &["mcpServers"], "compas", entry)?;
            Ok(format!("registered compas in gemini ({})", path.display()))
        }
    }
}

fn run_fix_command(binary: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(binary)
        .args(args)
        .output()
        .map_err(|e| format!("failed to run {} {}: {}", binary, args.join(" "), e))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!(
            "{} {} failed (exit {}): {}",
            binary,
            args.join(" "),
            output.status.code().unwrap_or(-1),
            stderr.trim()
        ))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Collect unique backend names from config agents.
fn unique_backends(config: &OrchestratorConfig) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut result = Vec::new();
    for agent in &config.agents {
        if seen.insert(agent.backend.clone()) {
            result.push(agent.backend.clone());
        }
    }
    result
}

/// Build a backend registry for doctor pings (mirrors compas.rs pattern).
fn build_doctor_registry(config: &OrchestratorConfig) -> BackendRegistry {
    use crate::backend::claude::ClaudeCodeBackend;
    use crate::backend::codex::CodexBackend;
    use crate::backend::gemini::GeminiBackend;
    use crate::backend::opencode::OpenCodeBackend;

    let mut registry = BackendRegistry::new();
    let workdir = Some(config.default_workdir.clone());

    registry.register(
        "claude",
        Arc::new(ClaudeCodeBackend::with_workdir(workdir.clone())),
    );
    registry.register("codex", Arc::new(CodexBackend::new(workdir)));
    registry.register(
        "opencode",
        Arc::new(OpenCodeBackend::with_workdir(Some(
            config.default_workdir.clone(),
        ))),
    );
    registry.register(
        "gemini",
        Arc::new(GeminiBackend::with_workdir(Some(
            config.default_workdir.clone(),
        ))),
    );

    registry
}

/// Convert an AgentConfig to the Agent model type needed by Backend::ping.
fn agent_from_config(ac: &crate::config::types::AgentConfig) -> Agent {
    Agent {
        alias: ac.alias.clone(),
        backend: ac.backend.clone(),
        model: ac.model.clone(),
        prompt: ac.prompt.clone(),
        prompt_file: ac.prompt_file.clone(),
        timeout_secs: ac.timeout_secs,
        backend_args: ac.backend_args.clone(),
        env: ac.env.clone(),
        log_path: None,
        execution_workdir: None,
    }
}

/// Return a human-readable install hint for a backend CLI.
fn install_hint(backend: &str) -> &'static str {
    match backend {
        "claude" => "npm install -g @anthropic-ai/claude-code",
        "codex" => "npm install -g @openai/codex",
        "opencode" => "see https://opencode.ai for installation",
        "gemini" => "npm install -g @anthropic-ai/gemini-cli or see https://github.com/google-gemini/gemini-cli",
        _ => "see project documentation",
    }
}

/// Format a duration in seconds to a human-readable string (e.g. "2h 15m").
fn format_duration_human(secs: i64) -> String {
    if secs < 0 {
        return "0s".to_string();
    }
    let s = secs as u64;
    let hours = s / 3600;
    let minutes = (s % 3600) / 60;
    let seconds = s % 60;

    if hours > 0 {
        format!("{}h {}m", hours, minutes)
    } else if minutes > 0 {
        format!("{}m {}s", minutes, seconds)
    } else {
        format!("{}s", seconds)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ── Result formatting ────────────────────────────────────────────────

    #[test]
    fn test_check_result_icons() {
        assert_eq!(CheckResult::pass("x", "ok").icon(), "\u{2713}");
        assert_eq!(CheckResult::warn("x", "hmm", "hint").icon(), "\u{26A0}");
        assert_eq!(CheckResult::fail("x", "bad", "hint").icon(), "\u{2717}");
    }

    #[test]
    fn test_format_report_all_pass() {
        let report = DoctorReport {
            results: vec![
                CheckResult::pass("Config file", "~/.compas/config.yaml"),
                CheckResult::pass("Config valid", "2 agents"),
            ],
            fixes_applied: vec![],
        };

        let output = format_report(&report);
        assert!(output.contains("Compas health check"));
        assert!(output.contains("\u{2713}"));
        assert!(output.contains("Config file"));
        assert!(output.contains("All checks passed."));
        assert!(!output.contains("issue"));
        assert!(!output.contains("warning"));
    }

    #[test]
    fn test_format_report_with_failures() {
        let report = DoctorReport {
            results: vec![
                CheckResult::pass("Config file", "found"),
                CheckResult::fail(
                    "Backend: codex",
                    "not found in PATH",
                    "install: npm install -g @openai/codex",
                ),
            ],
            fixes_applied: vec![],
        };

        let output = format_report(&report);
        assert!(output.contains("1 issue found:"));
        assert!(output.contains("\u{2717}"));
        assert!(output.contains("not found in PATH"));
    }

    #[test]
    fn test_format_report_with_warnings() {
        let report = DoctorReport {
            results: vec![
                CheckResult::pass("Config file", "found"),
                CheckResult::warn("Worker", "not running", "start with: compas dashboard"),
            ],
            fixes_applied: vec![],
        };

        let output = format_report(&report);
        assert!(output.contains("1 warning:"));
        assert!(output.contains("\u{26A0}"));
        assert!(output.contains("not running"));
    }

    #[test]
    fn test_format_report_with_fixes() {
        let report = DoctorReport {
            results: vec![CheckResult::pass("MCP: claude", "registered (auto-fixed)")],
            fixes_applied: vec!["registered compas in claude".to_string()],
        };

        let output = format_report(&report);
        assert!(output.contains("Fixes applied:"));
        assert!(output.contains("registered compas in claude"));
    }

    // ── Exit code logic ──────────────────────────────────────────────────

    #[test]
    fn test_exit_code_all_pass() {
        let report = DoctorReport {
            results: vec![CheckResult::pass("A", "ok"), CheckResult::pass("B", "ok")],
            fixes_applied: vec![],
        };
        assert!(!report.has_failures());
        assert_eq!(report.failure_count(), 0);
        assert_eq!(report.warning_count(), 0);
    }

    #[test]
    fn test_exit_code_with_failure() {
        let report = DoctorReport {
            results: vec![
                CheckResult::pass("A", "ok"),
                CheckResult::fail("B", "bad", "fix it"),
            ],
            fixes_applied: vec![],
        };
        assert!(report.has_failures());
        assert_eq!(report.failure_count(), 1);
    }

    #[test]
    fn test_exit_code_warnings_only_no_failure() {
        let report = DoctorReport {
            results: vec![
                CheckResult::pass("A", "ok"),
                CheckResult::warn("B", "hmm", "hint"),
            ],
            fixes_applied: vec![],
        };
        assert!(!report.has_failures());
        assert_eq!(report.failure_count(), 0);
        assert_eq!(report.warning_count(), 1);
    }

    // ── Config check ─────────────────────────────────────────────────────

    #[test]
    fn test_check_config_missing_file() {
        let path = PathBuf::from("/nonexistent/config.yaml");
        let result = check_config(&path);
        assert!(result.is_err());
        let checks = result.unwrap_err();
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].severity, Severity::Fail);
        assert!(checks[0].detail.contains("not found"));
        assert!(checks[0]
            .fix_hint
            .as_deref()
            .unwrap()
            .contains("compas init"));
    }

    #[test]
    fn test_check_config_valid() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let config_path = dir.path().join("config.yaml");
        std::fs::write(
            &config_path,
            format!(
                "default_workdir: {}\nstate_dir: {}\nagents:\n  - alias: dev\n    backend: stub\n",
                repo.display(),
                dir.path().join("state").display()
            ),
        )
        .unwrap();

        let result = check_config(&config_path);
        assert!(result.is_ok());
        let (config, checks) = result.unwrap();
        assert_eq!(checks.len(), 2);
        assert_eq!(checks[0].severity, Severity::Pass);
        assert_eq!(checks[1].severity, Severity::Pass);
        assert_eq!(config.agents.len(), 1);
    }

    #[test]
    fn test_check_config_invalid_yaml() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.yaml");
        std::fs::write(&config_path, "{{{{invalid yaml").unwrap();

        let result = check_config(&config_path);
        assert!(result.is_err());
        let checks = result.unwrap_err();
        assert_eq!(checks.len(), 2);
        assert_eq!(checks[0].severity, Severity::Pass); // file exists
        assert_eq!(checks[1].severity, Severity::Fail); // parse error
    }

    #[test]
    fn test_check_config_validation_error() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.yaml");
        std::fs::write(
            &config_path,
            "default_workdir: /tmp\nstate_dir: /tmp\nagents: []\n",
        )
        .unwrap();

        let result = check_config(&config_path);
        assert!(result.is_err());
        let checks = result.unwrap_err();
        assert_eq!(checks[1].severity, Severity::Fail);
        assert!(checks[1].detail.contains("at least one agent"));
    }

    // ── Target repo check ────────────────────────────────────────────────

    #[test]
    fn test_check_target_repo_exists() {
        let dir = tempfile::tempdir().unwrap();
        let result = check_target_repo(dir.path());
        assert_eq!(result.severity, Severity::Pass);
        assert!(result.detail.contains("exists"));
    }

    #[test]
    fn test_check_target_repo_missing() {
        let result = check_target_repo(Path::new("/nonexistent/repo/path"));
        assert_eq!(result.severity, Severity::Fail);
        assert!(result.detail.contains("does not exist"));
    }

    #[test]
    fn test_check_target_repo_not_directory() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("not-a-dir");
        std::fs::write(&file, "").unwrap();
        let result = check_target_repo(&file);
        assert_eq!(result.severity, Severity::Fail);
        assert!(result.detail.contains("not a directory"));
    }

    // ── State directory check ────────────────────────────────────────────

    #[test]
    fn test_check_state_dir_writable() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        let result = check_state_dir(&state);
        assert_eq!(result.severity, Severity::Pass);
        assert!(result.detail.contains("writable"));
    }

    // ── Backend installed check ──────────────────────────────────────────

    #[test]
    fn test_check_backend_installed_echo() {
        // "echo" should always exist
        let (result, installed) = check_backend_installed("echo");
        assert_eq!(result.severity, Severity::Pass);
        assert!(installed);
        assert!(result.detail.contains("installed"));
    }

    #[test]
    fn test_check_backend_installed_missing() {
        let (result, installed) = check_backend_installed("nonexistent-cli-xyz987");
        assert_eq!(result.severity, Severity::Fail);
        assert!(!installed);
        assert!(result.detail.contains("not found in PATH"));
    }

    // ── Install hints ────────────────────────────────────────────────────

    #[test]
    fn test_install_hints() {
        assert!(install_hint("claude").contains("claude-code"));
        assert!(install_hint("codex").contains("@openai/codex"));
        assert!(install_hint("opencode").contains("opencode"));
        assert!(install_hint("gemini").contains("gemini"));
        assert_eq!(install_hint("unknown"), "see project documentation");
    }

    // ── Duration formatting ──────────────────────────────────────────────

    #[test]
    fn test_format_duration_human() {
        assert_eq!(format_duration_human(0), "0s");
        assert_eq!(format_duration_human(45), "45s");
        assert_eq!(format_duration_human(65), "1m 5s");
        assert_eq!(format_duration_human(3661), "1h 1m");
        assert_eq!(format_duration_human(-5), "0s");
    }

    // ── MCP registration ─────────────────────────────────────────────────

    #[test]
    fn test_check_mcp_registration_returns_results() {
        // This test verifies the function runs without panicking.
        // Actual registration status depends on the environment.
        let results = check_mcp_registration();
        // Should return a result for each installed tool.
        for r in &results {
            assert!(
                r.severity == Severity::Pass || r.severity == Severity::Fail,
                "MCP check should be pass or fail, not warn"
            );
            assert!(r.label.starts_with("MCP: "));
        }
    }

    // ── Worker check without DB ──────────────────────────────────────────

    #[tokio::test]
    async fn test_check_worker_no_db() {
        // Point to a config that doesn't have a real DB.
        let dir = tempfile::tempdir().unwrap();
        let config = OrchestratorConfig {
            default_workdir: dir.path().to_path_buf(),
            state_dir: dir.path().join("state"),
            poll_interval_secs: 1,
            models: None,
            agents: vec![],
            worktree_dir: None,
            orchestration: Default::default(),
            database: Default::default(),
            notifications: Default::default(),
            backend_definitions: None,
        };

        let result = check_worker(&config).await;
        // No DB file = warn, not error.
        assert_eq!(result.severity, Severity::Warn);
    }

    // ── Full run without config ──────────────────────────────────────────

    #[tokio::test]
    async fn test_doctor_run_missing_config() {
        let report = run(PathBuf::from("/nonexistent/config.yaml"), false).await;
        assert!(report.has_failures());
        assert!(report.results[0].severity == Severity::Fail);
        assert!(report.results[0].detail.contains("not found"));
    }

    // ── Full run with valid config but no worker ─────────────────────────

    #[tokio::test]
    async fn test_doctor_run_valid_config_no_worker() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let config_path = dir.path().join("config.yaml");
        std::fs::write(
            &config_path,
            format!(
                "default_workdir: {}\nstate_dir: {}\nagents:\n  - alias: dev\n    backend: stub\n",
                repo.display(),
                dir.path().join("state").display()
            ),
        )
        .unwrap();

        let report = run(config_path, false).await;

        // Config checks should pass.
        assert!(report
            .results
            .iter()
            .any(|r| r.label == "Config file" && r.severity == Severity::Pass));
        assert!(report
            .results
            .iter()
            .any(|r| r.label == "Config valid" && r.severity == Severity::Pass));

        // Worker should be a warning (not running), not an error.
        let worker_check = report.results.iter().find(|r| r.label == "Worker");
        assert!(worker_check.is_some());
        assert_eq!(worker_check.unwrap().severity, Severity::Warn);

        // Warnings alone should not cause failure exit code.
        // The only failure would be the missing "stub" backend.
        let backend_fail = report.results.iter().find(|r| r.label == "Backend: stub");
        if let Some(bf) = backend_fail {
            assert_eq!(bf.severity, Severity::Fail);
        }
    }

    // ── Auto-fix MCP registration ─────────────────────────────────────

    #[test]
    fn test_fix_registers_mcp_via_upsert_json() {
        // Simulate the --fix flow for a JSON-config tool (OpenCode).
        // Write an opencode.json without a compas entry, call upsert_json_entry
        // (the same code path auto_register_mcp uses), and verify the entry appears.
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("opencode.json");
        std::fs::write(&config_path, r#"{"mcp": {}}"#).unwrap();

        let entry = setup_mcp::opencode_compas_entry();
        let result = setup_mcp::upsert_json_entry(&config_path, &["mcp"], "compas", entry);
        assert!(result.is_ok(), "upsert_json_entry failed: {:?}", result);

        // Verify the file now contains the compas entry.
        let content = std::fs::read_to_string(&config_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(
            parsed["mcp"]["compas"].is_object(),
            "compas entry should exist in mcp section"
        );
        assert_eq!(parsed["mcp"]["compas"]["enabled"], true);
    }

    #[test]
    fn test_fix_registers_mcp_creates_file_if_missing() {
        // upsert_json_entry should create the file if it doesn't exist.
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("subdir").join("opencode.json");

        let entry = setup_mcp::opencode_compas_entry();
        let result = setup_mcp::upsert_json_entry(&config_path, &["mcp"], "compas", entry);
        assert!(result.is_ok(), "upsert_json_entry failed: {:?}", result);

        let content = std::fs::read_to_string(&config_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(parsed["mcp"]["compas"].is_object());
    }

    #[test]
    fn test_fix_updates_report_results() {
        // Verify that when --fix succeeds, the report result is updated from
        // Fail to Pass and the fix is recorded.
        let mut results = vec![CheckResult::fail(
            "MCP: opencode",
            "compas not registered in opencode",
            "run: compas setup-mcp --tool opencode",
        )];
        let mut fixes_applied = Vec::new();

        // Simulate successful fix by directly updating results (same logic as run()).
        let fix_msg = "registered compas in opencode".to_string();
        fixes_applied.push(fix_msg);
        for r in &mut results {
            if r.label == "MCP: opencode" && r.severity == Severity::Fail {
                r.severity = Severity::Pass;
                r.detail = "registered (auto-fixed)".to_string();
                r.fix_hint = None;
            }
        }

        let report = DoctorReport {
            results,
            fixes_applied,
        };

        assert!(!report.has_failures());
        assert_eq!(report.results[0].severity, Severity::Pass);
        assert!(report.results[0].detail.contains("auto-fixed"));
        assert_eq!(report.fixes_applied.len(), 1);
    }

    // ── Unique backends ──────────────────────────────────────────────────

    #[test]
    fn test_unique_backends_deduplicates() {
        use crate::config::types::AgentConfig;
        let config = OrchestratorConfig {
            default_workdir: PathBuf::from("/tmp"),
            state_dir: PathBuf::from("/tmp/state"),
            poll_interval_secs: 1,
            models: None,
            agents: vec![
                AgentConfig {
                    alias: "a".to_string(),
                    backend: "claude".to_string(),
                    role: Default::default(),
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
                },
                AgentConfig {
                    alias: "b".to_string(),
                    backend: "claude".to_string(),
                    role: Default::default(),
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
                },
                AgentConfig {
                    alias: "c".to_string(),
                    backend: "codex".to_string(),
                    role: Default::default(),
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
                },
            ],
            worktree_dir: None,
            orchestration: Default::default(),
            database: Default::default(),
            notifications: Default::default(),
            backend_definitions: None,
        };

        let backends = unique_backends(&config);
        assert_eq!(backends, vec!["claude", "codex"]);
    }
}
