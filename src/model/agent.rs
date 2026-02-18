use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// An agent participating in the orchestration system.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Agent {
    pub alias: String,
    pub identity: String,
    pub backend: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub prompt_file: Option<std::path::PathBuf>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub backend_args: Option<Vec<String>>,
    #[serde(default)]
    pub env: Option<HashMap<String, String>>,
}
