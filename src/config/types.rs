use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Top-level orchestrator configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrchestratorConfig {
    /// Root directory of the target repository where agent backends execute.
    pub target_repo_root: PathBuf,
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
    #[serde(default)]
    pub orchestration: OrchestrationConfig,
    /// SQLite connection pool settings for MCP + worker.
    #[serde(default, alias = "apalis")]
    pub database: DatabaseConfig,
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
    /// Number of execution log files to retain under `{state_dir}/logs/` (default 100).
    /// Oldest files (by ULID-sorted name) are pruned on worker startup.
    #[serde(default = "default_log_retention_count")]
    pub log_retention_count: usize,
    /// Age threshold (seconds) after which non-running Active threads are considered stale.
    #[serde(default = "default_stale_active_secs")]
    pub stale_active_secs: u64,
}

impl Default for OrchestrationConfig {
    fn default() -> Self {
        Self {
            trigger_intents: default_trigger_intents(),
            execution_timeout_secs: default_execution_timeout_secs(),
            max_concurrent_triggers: None,
            max_triggers_per_agent: default_max_triggers_per_agent(),
            ping_timeout_secs: default_ping_timeout_secs(),
            log_retention_count: default_log_retention_count(),
            stale_active_secs: default_stale_active_secs(),
        }
    }
}

fn default_trigger_intents() -> Vec<String> {
    vec!["dispatch".to_string(), "handoff".to_string()]
}

fn default_max_triggers_per_agent() -> usize {
    1
}

fn default_ping_timeout_secs() -> u64 {
    15
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
}
