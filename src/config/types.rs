use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Top-level orchestrator configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrchestratorConfig {
    /// Default working directory for agents without a per-agent `workdir`.
    #[serde(alias = "target_repo_root")]
    pub default_workdir: PathBuf,
    /// Orchestrator-owned runtime directory (SQLite DB, logs, and state).
    pub state_dir: PathBuf,
    #[serde(default = "default_poll_interval_secs")]
    pub poll_interval_secs: u64,
    /// Optional model catalog for operator reference.
    ///
    /// This registry is informational only. Runtime model selection uses each
    /// agent's `model` field directly.
    #[serde(default)]
    pub models: Option<Vec<ModelEntry>>,
    pub agents: Vec<AgentConfig>,
    /// Optional override for worktree parent directory.
    /// Default: `{repo_root}/.compas-worktrees/`
    #[serde(default)]
    pub worktree_dir: Option<PathBuf>,
    #[serde(default)]
    pub orchestration: OrchestrationConfig,
    /// SQLite connection pool settings for MCP + worker.
    #[serde(default, alias = "apalis")]
    pub database: DatabaseConfig,
    /// Desktop notification settings.
    #[serde(default)]
    pub notifications: NotificationConfig,
    /// Config-driven backend definitions (generic backends).
    /// Each entry defines a CLI-based backend that can be referenced by agent `backend:` fields.
    #[serde(default)]
    pub backend_definitions: Option<Vec<BackendDefinition>>,
    /// Lifecycle hook commands fired at named execution events.
    #[serde(default)]
    pub hooks: Option<HooksConfig>,
    /// Config-declared recurring schedules (cron-based dispatch).
    #[serde(default)]
    pub schedules: Option<Vec<ScheduleConfig>>,
}

impl OrchestratorConfig {
    /// SQLite database file used by MCP + worker.
    ///
    /// This path is derived from `state_dir` and is always
    /// `{state_dir}/jobs.sqlite`.
    pub fn db_path(&self) -> PathBuf {
        self.state_dir.join("jobs.sqlite")
    }

    /// Resolved concurrency limit: explicit config or worker agent count (min 1).
    pub fn effective_max_concurrent_triggers(&self) -> usize {
        self.orchestration
            .max_concurrent_triggers
            .unwrap_or_else(|| {
                self.agents
                    .iter()
                    .filter(|a| a.role == AgentRole::Worker)
                    .count()
                    .max(1)
            })
    }

    /// Directory where per-execution log files are written: `{state_dir}/logs/`.
    pub fn log_dir(&self) -> std::path::PathBuf {
        self.state_dir.join("logs")
    }
}

fn default_poll_interval_secs() -> u64 {
    1
}

/// SQLite connection pool configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseConfig {
    /// SQLite pool max connections for MCP + worker.
    #[serde(default = "default_db_max_connections")]
    pub max_connections: u32,
    /// SQLite pool min idle connections.
    #[serde(default = "default_db_min_connections")]
    pub min_connections: u32,
    /// SQLite pool acquire timeout in milliseconds.
    #[serde(default = "default_db_acquire_timeout_ms")]
    pub acquire_timeout_ms: u64,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            max_connections: default_db_max_connections(),
            min_connections: default_db_min_connections(),
            acquire_timeout_ms: default_db_acquire_timeout_ms(),
        }
    }
}

/// Desktop notification configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NotificationConfig {
    /// Enable macOS desktop notifications for execution completion/failure.
    #[serde(default)]
    pub desktop: bool,
}

/// A single hook command to execute at a lifecycle event.
///
/// The subprocess receives event JSON on stdin and is killed after `timeout_secs`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookEntry {
    /// Command to invoke (must be on PATH or an absolute path).
    pub command: String,
    /// Optional positional arguments passed after the command.
    #[serde(default)]
    pub args: Option<Vec<String>>,
    /// Maximum seconds to wait before sending SIGTERM; a 5-second grace period
    /// follows before SIGKILL (effective ceiling: timeout_secs + 5s). Default 10.
    #[serde(default = "default_hook_timeout_secs")]
    pub timeout_secs: u64,
    /// Additional environment variables injected into the hook process.
    #[serde(default)]
    pub env: Option<HashMap<String, String>>,
}

fn default_hook_timeout_secs() -> u64 {
    10
}

/// Named hook points for execution lifecycle events.
///
/// Each hook point accepts a list of `HookEntry` values. Hooks within a point
/// run sequentially in declaration order. An empty vec (the default) means no
/// hooks are registered for that event.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HooksConfig {
    /// Fired when an execution starts (agent process is spawned).
    #[serde(default)]
    pub on_execution_started: Vec<HookEntry>,
    /// Fired when an execution reaches a terminal state (success or failure).
    #[serde(default)]
    pub on_execution_completed: Vec<HookEntry>,
    /// Fired when a thread transitions to Completed status.
    #[serde(default)]
    pub on_thread_closed: Vec<HookEntry>,
    /// Fired when a thread transitions to a failed state.
    #[serde(default)]
    pub on_thread_failed: Vec<HookEntry>,
}

fn default_db_max_connections() -> u32 {
    32
}

fn default_db_min_connections() -> u32 {
    4
}

fn default_db_acquire_timeout_ms() -> u64 {
    30000
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestrationConfig {
    #[serde(default = "default_trigger_intents")]
    pub trigger_intents: Vec<String>,
    #[serde(
        default = "default_execution_timeout_secs",
        alias = "trigger_timeout_secs",
        alias = "default_timeout_secs"
    )]
    pub execution_timeout_secs: u64,
    /// Maximum number of concurrent agent triggers. Defaults to worker agent count.
    #[serde(default)]
    pub max_concurrent_triggers: Option<usize>,
    /// Maximum concurrent triggers per individual agent. Defaults to 1.
    #[serde(default = "default_max_triggers_per_agent")]
    pub max_triggers_per_agent: usize,
    /// Timeout in seconds for backend ping liveness probes (default 15).
    #[serde(default = "default_ping_timeout_secs")]
    pub ping_timeout_secs: u64,
    /// TTL in seconds for cached ping results (default 60).
    /// Subsequent `orch_health` calls within this window return cached results
    /// instead of re-pinging backends.
    #[serde(default = "default_ping_cache_ttl_secs")]
    pub ping_cache_ttl_secs: u64,
    /// Number of execution log files to retain under `{state_dir}/logs/` (default 100).
    /// Oldest files (by ULID-sorted name) are pruned on worker startup.
    #[serde(default = "default_log_retention_count")]
    pub log_retention_count: usize,
    /// Age threshold (seconds) after which non-running Active threads are considered stale.
    #[serde(default = "default_stale_active_secs")]
    pub stale_active_secs: u64,
    /// Timeout in seconds for merge operations (default 30).
    #[serde(default = "default_merge_timeout_secs")]
    pub merge_timeout_secs: u64,
    /// Default merge strategy: "merge", "rebase", or "squash" (default "merge").
    #[serde(default = "default_merge_strategy")]
    pub default_merge_strategy: String,
    /// Default target branch for auto-merge on close (default: "main").
    #[serde(default = "default_merge_target")]
    pub default_merge_target: String,
    /// Maximum timeout (seconds) for orch_wait MCP tool calls (default 120).
    #[serde(default = "default_mcp_wait_max_timeout_secs")]
    pub mcp_wait_max_timeout_secs: u64,
}

impl Default for OrchestrationConfig {
    fn default() -> Self {
        Self {
            trigger_intents: default_trigger_intents(),
            execution_timeout_secs: default_execution_timeout_secs(),
            max_concurrent_triggers: None,
            max_triggers_per_agent: default_max_triggers_per_agent(),
            ping_timeout_secs: default_ping_timeout_secs(),
            ping_cache_ttl_secs: default_ping_cache_ttl_secs(),
            log_retention_count: default_log_retention_count(),
            stale_active_secs: default_stale_active_secs(),
            merge_timeout_secs: default_merge_timeout_secs(),
            default_merge_strategy: default_merge_strategy(),
            default_merge_target: default_merge_target(),
            mcp_wait_max_timeout_secs: default_mcp_wait_max_timeout_secs(),
        }
    }
}

fn default_trigger_intents() -> Vec<String> {
    vec![
        "dispatch".to_string(),
        "handoff".to_string(),
        "changes-requested".to_string(),
    ]
}

fn default_max_triggers_per_agent() -> usize {
    1
}

fn default_ping_timeout_secs() -> u64 {
    15
}

fn default_ping_cache_ttl_secs() -> u64 {
    60
}

fn default_log_retention_count() -> usize {
    100
}

fn default_stale_active_secs() -> u64 {
    3600
}

fn default_execution_timeout_secs() -> u64 {
    600
}

fn default_merge_timeout_secs() -> u64 {
    30
}

fn default_merge_strategy() -> String {
    "merge".to_string()
}

fn default_merge_target() -> String {
    "main".to_string()
}

fn default_mcp_wait_max_timeout_secs() -> u64 {
    120
}

/// Agent role determines worker behavior.
/// - `Worker`: triggered on matching intents (default).
/// - `Operator`: coordinator driven via MCP tools, never triggered.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AgentRole {
    #[default]
    Worker,
    Operator,
}

/// A model entry in the optional model catalog.
/// Accepts either a plain string or an object with backend/description metadata.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ModelEntry {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
}

impl<'de> serde::Deserialize<'de> for ModelEntry {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Plain(String),
            Full {
                id: String,
                backend: Option<String>,
                description: Option<String>,
                timeout_secs: Option<u64>,
            },
        }
        match Raw::deserialize(deserializer)? {
            Raw::Plain(id) => Ok(ModelEntry {
                id,
                backend: None,
                description: None,
                timeout_secs: None,
            }),
            Raw::Full {
                id,
                backend,
                description,
                timeout_secs,
            } => Ok(ModelEntry {
                id,
                backend,
                description,
                timeout_secs,
            }),
        }
    }
}

/// Configuration for a single agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConfig {
    pub alias: String,
    pub backend: String,
    #[serde(default)]
    pub role: AgentRole,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub prompt_file: Option<PathBuf>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    /// Extra backend CLI flags/args appended before instruction text.
    #[serde(default)]
    pub backend_args: Option<Vec<String>>,
    #[serde(default)]
    pub env: Option<HashMap<String, String>>,
    /// Optional per-agent working directory. If omitted, uses global `default_workdir`.
    #[serde(default)]
    pub workdir: Option<PathBuf>,
    /// Workspace isolation mode: `"worktree"` for git worktree isolation, `"shared"` (default).
    #[serde(default)]
    pub workspace: Option<String>,
    /// Maximum number of retry attempts for transient failures (default 0 = no retry).
    #[serde(default)]
    pub max_retries: u32,
    /// Backoff base in seconds between retries (exponential: base * 2^attempt). Default 30.
    #[serde(default = "default_retry_backoff_secs")]
    pub retry_backoff_secs: u64,
    /// Handoff routing: auto-chain to another agent based on reply intent.
    #[serde(default)]
    pub handoff: Option<HandoffConfig>,
}

fn default_retry_backoff_secs() -> u64 {
    30
}

/// Target for auto-handoff routing.
/// Deserializes from either a single string or a list of strings.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum HandoffTarget {
    /// Route to a single agent (or "operator" to stop chain).
    Single(String),
    /// Fan-out: create separate threads per target, linked by batch.
    FanOut(Vec<String>),
}

/// Handoff routing configuration for automatic agent chaining.
///
/// When an agent completes successfully, the worker checks `on_response` and
/// auto-dispatches to the target agent regardless of reply intent.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct HandoffConfig {
    /// Agent alias (or "operator") to route to when agent completes successfully.
    /// Accepts a single string or a list of strings for fan-out.
    #[serde(default)]
    pub on_response: Option<HandoffTarget>,
    /// Custom prompt prepended to the auto-generated handoff context.
    #[serde(default)]
    pub handoff_prompt: Option<String>,
    /// Maximum consecutive auto-handoffs before forcing operator review (default: 3).
    #[serde(default)]
    pub max_chain_depth: Option<u32>,
}

// ── Config-declared recurring schedules (CRON-1) ──

/// A config-declared recurring schedule.
///
/// Defines a cron-triggered dispatch that the worker evaluates on each tick.
/// When the cron expression is due, the worker creates a dispatch message
/// targeting the configured agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScheduleConfig {
    /// Unique schedule name (used for dedup and display).
    pub name: String,
    /// Target agent alias — must exist in the `agents` list.
    pub agent: String,
    /// Cron expression (e.g., `"*/5 * * * *"`). Parsed by the `croner` crate.
    pub cron: String,
    /// Dispatch message body sent to the agent.
    pub body: String,
    /// Optional batch/ticket ID attached to each dispatch.
    #[serde(default)]
    pub batch: Option<String>,
    /// Safety cap on total dispatches for this schedule (default 100).
    #[serde(default = "default_schedule_max_runs")]
    pub max_runs: u64,
    /// Whether this schedule is active (default true).
    #[serde(default = "default_schedule_enabled")]
    pub enabled: bool,
}

fn default_schedule_max_runs() -> u64 {
    100
}

fn default_schedule_enabled() -> bool {
    true
}

// ── Config-driven generic backend definitions (GBE-1) ──

/// A config-driven backend definition.
///
/// Allows backends to be defined entirely in YAML without Rust code.
/// Each definition describes a CLI command, its arguments (with template
/// variables), output parsing, and optional session resume / ping behavior.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackendDefinition {
    /// Backend name, referenced by agent `backend:` field.
    pub name: String,
    /// CLI command to invoke (e.g. `aider`, `/usr/local/bin/my-tool`).
    pub command: String,
    /// Arguments with template variables: `{{instruction}}`, `{{model}}`, `{{session_id}}`.
    #[serde(default)]
    pub args: Vec<String>,
    /// Optional session resume configuration.
    #[serde(default)]
    pub resume: Option<ResumeConfig>,
    /// Output format and extraction configuration.
    #[serde(default)]
    pub output: OutputConfig,
    /// Optional custom ping/liveness check command.
    #[serde(default)]
    pub ping: Option<PingConfig>,
    /// Environment variables to strip before spawning the backend process.
    /// Composes with per-agent `env`: agent `env` adds vars, backend `env_remove` strips vars.
    #[serde(default)]
    pub env_remove: Option<Vec<String>>,
}

/// Session resume configuration for a generic backend.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResumeConfig {
    /// CLI flag to enable session resume (e.g. `--resume`, `-r`).
    pub flag: String,
    /// Template for the session ID argument (e.g. `{{session_id}}`).
    pub session_id_arg: String,
}

/// Output format for generic backend result parsing.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    /// Raw stdout is the result text.
    #[default]
    Plaintext,
    /// Parse stdout as a single JSON object.
    Json,
    /// Parse last line of stdout as JSON.
    Jsonl,
}

/// Output parsing configuration for a generic backend.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputConfig {
    /// Output format: plaintext, json, or jsonl.
    #[serde(default)]
    pub format: OutputFormat,
    /// JSON field path to extract the result text (for json/jsonl formats).
    #[serde(default)]
    pub result_field: Option<String>,
    /// JSON field path to extract the session ID (for json/jsonl formats).
    #[serde(default)]
    pub session_id_field: Option<String>,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            format: OutputFormat::Plaintext,
            result_field: None,
            session_id_field: None,
        }
    }
}

/// Custom ping/liveness check configuration for a generic backend.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PingConfig {
    /// Command to run for the liveness check.
    pub command: String,
    /// Arguments for the ping command.
    #[serde(default)]
    pub args: Vec<String>,
}
