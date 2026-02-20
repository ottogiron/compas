use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Top-level orchestrator configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestratorConfig {
    #[serde(alias = "mailbox_root")]
    pub state_dir: PathBuf,
    /// SQLite database file used by MCP + worker.
    #[serde(default = "default_db_path")]
    pub db_path: PathBuf,
    #[serde(default = "default_poll_interval_secs")]
    pub poll_interval_secs: u64,
    /// Global model registry. Each model declares its backend and description.
    #[serde(default)]
    pub models: Option<Vec<ModelEntry>>,
    pub agents: Vec<AgentConfig>,
    #[serde(default)]
    pub orchestration: OrchestrationConfig,
    /// SQLite connection pool settings for MCP + worker.
    #[serde(default, alias = "apalis")]
    pub database: DatabaseConfig,
    /// Telegram notification settings (flattened from NotificationConfig).
    #[serde(default)]
    pub telegram: Option<TelegramConfig>,
    #[serde(default)]
    pub audit_log_path: Option<PathBuf>,
}

impl OrchestratorConfig {
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
}

fn default_poll_interval_secs() -> u64 {
    1
}

fn default_db_path() -> PathBuf {
    PathBuf::from("~/.aster/orch/jobs.sqlite")
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
    #[serde(default)]
    pub auto_trigger_enabled: bool,
    #[serde(default = "default_trigger_intents")]
    pub trigger_intents: Vec<String>,
    #[serde(default = "default_max_output_capture_bytes")]
    pub max_output_capture_bytes: usize,
    #[serde(
        default = "default_execution_timeout_secs",
        alias = "trigger_timeout_secs",
        alias = "default_timeout_secs"
    )]
    pub execution_timeout_secs: u64,
    #[serde(default = "default_max_message_body_bytes")]
    pub max_message_body_bytes: usize,
    /// Maximum number of concurrent agent triggers. Defaults to worker agent count.
    #[serde(default)]
    pub max_concurrent_triggers: Option<usize>,
    /// Maximum concurrent triggers per individual agent. Defaults to 1.
    #[serde(default = "default_max_triggers_per_agent")]
    pub max_triggers_per_agent: usize,
    /// Maximum trigger execution history records to retain (default 1000).
    #[serde(default = "default_task_history_retention")]
    pub task_history_retention: usize,
    /// Timeout in seconds for backend ping liveness probes (default 15).
    #[serde(default = "default_ping_timeout_secs")]
    pub ping_timeout_secs: u64,
    // -- Daemon fields (flattened from DaemonConfig) --
    #[serde(default = "default_daemon_required")]
    pub daemon_required: bool,
    #[serde(default = "default_daemon_auto_start")]
    pub daemon_auto_start: bool,
    /// Staleness threshold in seconds.
    #[serde(default)]
    pub daemon_staleness_threshold_secs: u64,
    /// Path to daemon log file.
    #[serde(default)]
    pub daemon_log_file_path: Option<PathBuf>,
}

impl Default for OrchestrationConfig {
    fn default() -> Self {
        Self {
            auto_trigger_enabled: false,
            trigger_intents: default_trigger_intents(),
            max_output_capture_bytes: default_max_output_capture_bytes(),
            execution_timeout_secs: default_execution_timeout_secs(),
            max_message_body_bytes: default_max_message_body_bytes(),
            max_concurrent_triggers: None,
            max_triggers_per_agent: default_max_triggers_per_agent(),
            task_history_retention: default_task_history_retention(),
            ping_timeout_secs: default_ping_timeout_secs(),
            daemon_required: default_daemon_required(),
            daemon_auto_start: default_daemon_auto_start(),
            daemon_staleness_threshold_secs: 0,
            daemon_log_file_path: None,
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

fn default_task_history_retention() -> usize {
    1000
}

fn default_max_triggers_per_agent() -> usize {
    1
}

fn default_ping_timeout_secs() -> u64 {
    15
}

fn default_max_output_capture_bytes() -> usize {
    32768 // 32KB
}

fn default_execution_timeout_secs() -> u64 {
    30
}

fn default_daemon_required() -> bool {
    true
}

fn default_daemon_auto_start() -> bool {
    true
}

/// Agent role determines daemon behavior.
/// - `Worker`: daemon-triggered on matching intents (default).
/// - `Operator`: coordinator driven via MCP tools, never daemon-triggered.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AgentRole {
    Worker,
    Operator,
}

impl Default for AgentRole {
    fn default() -> Self {
        AgentRole::Worker
    }
}

/// A model entry in the model pool. Accepts either a plain string or an object with description/backend.
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
pub struct AgentConfig {
    pub alias: String,
    pub identity: String,
    pub backend: String,
    #[serde(default)]
    pub role: AgentRole,
    #[serde(default)]
    pub model: Option<String>,
    /// Model pool: first is primary, rest are fallbacks. Accepts plain strings or objects.
    /// Legacy: use `preferred_models` with a global `models` registry instead.
    #[serde(default)]
    pub models: Option<Vec<ModelEntry>>,
    /// Preferred model IDs referencing the global `models` registry.
    #[serde(default)]
    pub preferred_models: Option<Vec<String>>,
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

impl AgentConfig {
    /// Effective model pool resolved against the global model registry.
    /// Priority: `preferred_models` (global lookup) > `models` (legacy) > `model` (single).
    pub fn model_pool(&self, global_models: &[ModelEntry]) -> Vec<ModelEntry> {
        if let Some(ref preferred) = self.preferred_models {
            preferred
                .iter()
                .filter_map(|id| global_models.iter().find(|m| m.id == *id).cloned())
                .collect()
        } else if let Some(ref models) = self.models {
            models.clone()
        } else if let Some(ref model) = self.model {
            vec![ModelEntry {
                id: model.clone(),
                backend: None,
                description: None,
                timeout_secs: None,
            }]
        } else {
            vec![]
        }
    }
}

fn default_max_message_body_bytes() -> usize {
    1_048_576 // 1MB
}

/// Hardcoded audit log rotation constants.
pub const AUDIT_MAX_FILE_BYTES: usize = 10_485_760; // 10MB
pub const AUDIT_MAX_ARCHIVE_FILES: usize = 10;

/// Telegram notification configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramConfig {
    pub bot_token: String,
    pub chat_ids: Vec<String>,
}
