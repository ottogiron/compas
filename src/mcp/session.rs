//! orch_session_info and orch_list_agents implementations.

use std::collections::HashMap;

use rmcp::model::CallToolResult;
use serde::Serialize;

use super::server::{json_text, OrchestratorMcpServer};
use crate::config::types::{AgentConfig, HandoffTarget};

/// Serialization wrapper for `handoff_to` — emits a JSON string for a single
/// target and a JSON array for fan-out, matching the config semantics.
#[derive(Debug, Clone)]
enum HandoffToValue {
    Single(String),
    FanOut(Vec<String>),
}

impl Serialize for HandoffToValue {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            HandoffToValue::Single(s) => serializer.serialize_str(s),
            HandoffToValue::FanOut(v) => v.serialize(serializer),
        }
    }
}

/// Derive handoff metadata fields from an agent's config.
fn handoff_metadata(agent: &AgentConfig) -> (Option<HandoffToValue>, bool, Option<u32>) {
    let handoff = match &agent.handoff {
        Some(h) => h,
        None => return (None, false, None),
    };
    match &handoff.on_response {
        None => (None, false, None),
        Some(HandoffTarget::Single(target)) if target == "operator" => (None, false, None),
        Some(HandoffTarget::Single(target)) => {
            let depth = Some(handoff.max_chain_depth.unwrap_or(3));
            (Some(HandoffToValue::Single(target.clone())), true, depth)
        }
        Some(HandoffTarget::FanOut(targets)) => {
            let depth = Some(handoff.max_chain_depth.unwrap_or(3));
            (Some(HandoffToValue::FanOut(targets.clone())), true, depth)
        }
    }
}

impl OrchestratorMcpServer {
    // ── orch_session_info ────────────────────────────────────────────────

    pub fn session_info_impl(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        // Snapshot live config for this request.
        let config = self.config.load();

        #[derive(Serialize)]
        struct SessionInfo {
            server: String,
            version: String,
            agent_count: usize,
            db_path: String,
        }

        Ok(json_text(&SessionInfo {
            server: "compas".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            agent_count: config.agents.len(),
            db_path: config.db_path().display().to_string(),
        }))
    }

    // ── orch_list_agents ─────────────────────────────────────────────────

    pub async fn list_agents_impl(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        // Snapshot live config for this request.
        let config = self.config.load();

        let (active_res, queued_res) = tokio::join!(
            self.store.active_executions_by_agent(),
            self.store.queued_executions_by_agent(),
        );

        let active_map: HashMap<String, i64> = active_res.unwrap_or_default().into_iter().collect();
        let queued_map: HashMap<String, i64> = queued_res.unwrap_or_default().into_iter().collect();

        let max_per_agent = config.orchestration.max_triggers_per_agent as i64;
        let global_max = config.effective_max_concurrent_triggers() as i64;

        #[derive(Serialize)]
        struct AgentInfo {
            alias: String,
            backend: String,
            role: String,
            model: Option<String>,
            timeout_secs: Option<u64>,
            max_concurrent: i64,
            active: i64,
            queued: i64,
            available: i64,
            #[serde(skip_serializing_if = "Option::is_none")]
            handoff_to: Option<HandoffToValue>,
            await_chain_recommended: bool,
            #[serde(skip_serializing_if = "Option::is_none")]
            max_chain_depth: Option<u32>,
        }

        #[derive(Serialize)]
        struct ListAgentsResponse {
            agents: Vec<AgentInfo>,
            global_max_concurrent: i64,
            global_active: i64,
            global_available: i64,
        }

        let mut global_active: i64 = 0;

        let agents: Vec<AgentInfo> = config
            .agents
            .iter()
            .map(|a| {
                let active = *active_map.get(&a.alias).unwrap_or(&0);
                let queued = *queued_map.get(&a.alias).unwrap_or(&0);
                let available = max_per_agent.saturating_sub(active);
                global_active += active;
                let (handoff_to, await_chain_recommended, max_chain_depth) = handoff_metadata(a);
                AgentInfo {
                    alias: a.alias.clone(),
                    backend: a.backend().to_string(),
                    role: format!("{:?}", a.role).to_lowercase(),
                    model: a.model.clone(),
                    timeout_secs: a.timeout_secs,
                    max_concurrent: max_per_agent,
                    active,
                    queued,
                    available,
                    handoff_to,
                    await_chain_recommended,
                    max_chain_depth,
                }
            })
            .collect();

        let global_available = global_max.saturating_sub(global_active);

        Ok(json_text(&ListAgentsResponse {
            agents,
            global_max_concurrent: global_max,
            global_active,
            global_available,
        }))
    }
}
