//! TriggerContext — shared state passed to the apalis worker handler via `Data<TriggerContext>`.
//!
//! Holds config, backend registry, session cache, and store so the handler can
//! resolve agents, call backends, and update thread state.
//!
//! Job routing (pushing follow-up trigger jobs) is NOT handled here — that belongs
//! to the handler/driver layer which has direct access to the apalis storage handle.

use crate::backend::registry::BackendRegistry;
use crate::backend::Backend;
use crate::config::types::OrchestratorConfig;
use crate::error::Result;
use crate::model::agent::Agent;
use crate::model::session::Session;
use crate::store::Store;
use crate::workflow::alias::resolve_alias;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Shared state for the trigger worker pipeline.
///
/// Passed as `Data<TriggerContext>` to handler functions.
/// All fields are cheaply cloneable (Arc/Clone).
#[derive(Clone, Debug)]
pub struct TriggerContext {
    pub config: Arc<OrchestratorConfig>,
    pub backend_registry: Arc<BackendRegistry>,
    /// In-memory session cache: alias → Session.
    /// Cheap to lose on restart — sessions are re-created on next trigger.
    session_cache: Arc<Mutex<HashMap<String, Session>>>,
    pub store: Store,
}

impl TriggerContext {
    pub fn new(
        config: OrchestratorConfig,
        backend_registry: BackendRegistry,
        store: Store,
    ) -> Self {
        Self {
            config: Arc::new(config),
            backend_registry: Arc::new(backend_registry),
            session_cache: Arc::new(Mutex::new(HashMap::new())),
            store,
        }
    }

    /// Resolve agent alias → (Agent, Backend, Option<Session>).
    ///
    /// Mirrors the old `Driver::resolve_trigger_context()` logic:
    /// - Resolves alias to AgentConfig
    /// - Resolves backend: model-level (global registry) > agent-level
    /// - Resolves timeout: model-level > agent-level > global default
    /// - Looks up cached session
    pub fn resolve(
        &self,
        alias: &str,
        model_override: Option<&str>,
    ) -> Result<(Agent, Arc<dyn Backend>, Option<Session>)> {
        let agent_config = resolve_alias(alias, &self.config.agents)?.clone();

        // Resolve backend: model-level backend (from global registry) > agent-level backend
        let effective_model = model_override.or(agent_config.model.as_deref());
        let backend_name = effective_model
            .and_then(|m| {
                self.config
                    .models
                    .as_ref()
                    .and_then(|models| models.iter().find(|entry| entry.id == m))
                    .and_then(|entry| entry.backend.as_deref())
            })
            .unwrap_or(&agent_config.backend);
        let backend = self.backend_registry.get_by_name(backend_name)?;

        // Resolve timeout: model-level > agent-level > global default
        let model_timeout = effective_model.and_then(|m| {
            self.config
                .models
                .as_ref()
                .and_then(|models| models.iter().find(|entry| entry.id == m))
                .and_then(|entry| entry.timeout_secs)
        });

        let agent = Agent {
            alias: agent_config.alias.clone(),
            identity: agent_config.identity.clone(),
            backend: backend_name.to_string(),
            model: model_override
                .map(String::from)
                .or(agent_config.model.clone()),
            prompt: agent_config.prompt.clone(),
            prompt_file: agent_config.prompt_file.clone(),
            timeout_secs: Some(
                model_timeout
                    .or(agent_config.timeout_secs)
                    .unwrap_or(self.config.orchestration.execution_timeout_secs),
            ),
            backend_args: agent_config.backend_args.clone(),
            env: agent_config.env.clone(),
        };

        let session = self.session_cache.lock().unwrap().get(alias).cloned();

        Ok((agent, backend, session))
    }

    /// Cache a session for an agent alias.
    pub fn cache_session(&self, alias: &str, session: Session) {
        self.session_cache
            .lock()
            .unwrap()
            .insert(alias.to_string(), session);
    }
}

/// Build the backend registry from config — register only backends that are actually needed.
pub fn build_backend_registry(config: &OrchestratorConfig) -> BackendRegistry {
    use crate::backend::claude::ClaudeCodeBackend;
    use crate::backend::codex::CodexBackend;
    use crate::backend::gemini::GeminiBackend;
    use crate::backend::opencode::OpenCodeBackend;

    let mut registry = BackendRegistry::new();

    // Collect which backends are needed: from agents + global model registry
    let mut needed: std::collections::HashSet<&str> =
        config.agents.iter().map(|a| a.backend.as_str()).collect();
    if let Some(ref models) = config.models {
        for model in models {
            if let Some(ref backend) = model.backend {
                needed.insert(backend.as_str());
            }
        }
    }

    if needed.contains("claude") {
        registry.register("claude", Arc::new(ClaudeCodeBackend::new()));
    }
    if needed.contains("opencode") {
        registry.register("opencode", Arc::new(OpenCodeBackend::new()));
    }
    if needed.contains("codex") {
        registry.register("codex", Arc::new(CodexBackend::default()));
    }
    if needed.contains("gemini") {
        registry.register("gemini", Arc::new(GeminiBackend::new()));
    }
    if needed.contains("stub") {
        registry.register("stub", Arc::new(crate::testing::StubBackend::default()));
    }

    registry
}
