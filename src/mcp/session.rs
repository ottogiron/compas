//! orch_session_info and orch_list_agents implementations.

use rmcp::model::CallToolResult;
use serde::Serialize;

use super::server::{json_text, OrchestratorMcpServer};

impl OrchestratorMcpServer {
    // ── orch_session_info ────────────────────────────────────────────────

    pub fn session_info_impl(&self) -> Result<CallToolResult, rmcp::Error> {
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
            server: "aster-orch".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            agent_count: config.agents.len(),
            db_path: config.db_path.display().to_string(),
        }))
    }

    // ── orch_list_agents ─────────────────────────────────────────────────

    pub fn list_agents_impl(&self) -> Result<CallToolResult, rmcp::Error> {
        // Snapshot live config for this request.
        let config = self.config.load();

        #[derive(Serialize)]
        struct AgentInfo {
            alias: String,
            backend: String,
            role: String,
            model: Option<String>,
            timeout_secs: Option<u64>,
        }

        let agents: Vec<AgentInfo> = config
            .agents
            .iter()
            .map(|a| AgentInfo {
                alias: a.alias.clone(),
                backend: a.backend.clone(),
                role: format!("{:?}", a.role).to_lowercase(),
                model: a.model.clone(),
                timeout_secs: a.timeout_secs,
            })
            .collect();

        Ok(json_text(&agents))
    }
}
