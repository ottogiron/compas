use super::Backend;
use crate::config::types::AgentConfig;
use crate::error::{OrchestratorError, Result};
use std::collections::HashMap;
use std::sync::Arc;

/// Registry mapping backend names to implementations.
#[derive(Debug, Default)]
pub struct BackendRegistry {
    backends: HashMap<String, Arc<dyn Backend>>,
}

impl BackendRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a backend implementation.
    pub fn register(&mut self, name: &str, backend: Arc<dyn Backend>) {
        self.backends.insert(name.to_string(), backend);
    }

    /// Look up the backend for an agent configuration.
    pub fn get(&self, agent: &AgentConfig) -> Result<Arc<dyn Backend>> {
        self.backends.get(&agent.backend).cloned().ok_or_else(|| {
            OrchestratorError::Backend(format!("no backend registered for '{}'", agent.backend))
        })
    }

    /// Look up a backend by name directly.
    pub fn get_by_name(&self, name: &str) -> Result<Arc<dyn Backend>> {
        self.backends.get(name).cloned().ok_or_else(|| {
            OrchestratorError::Backend(format!("no backend registered for '{}'", name))
        })
    }
}
