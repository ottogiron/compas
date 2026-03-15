use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// An agent participating in the orchestration system.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Agent {
    pub alias: String,
    pub backend: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub prompt_file: Option<PathBuf>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub backend_args: Option<Vec<String>>,
    #[serde(default)]
    pub env: Option<HashMap<String, String>>,
    /// Per-execution log file path set by the worker at runtime.
    /// Not persisted; always `None` when deserialized.
    #[serde(skip)]
    pub log_path: Option<PathBuf>,
    /// Per-execution working directory set by the worker at runtime.
    /// Not persisted; always `None` when deserialized.
    #[serde(skip)]
    pub execution_workdir: Option<PathBuf>,
}
