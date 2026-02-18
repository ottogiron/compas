//! Session info tool implementation.

use rmcp::model::CallToolResult;

use super::server::{json_text, OrchestratorMcpServer};

impl OrchestratorMcpServer {
    pub(crate) fn session_info_impl(&self) -> Result<CallToolResult, rmcp::Error> {
        let val = serde_json::json!({
            "version": env!("CARGO_PKG_VERSION"),
            "agent_count": self.config.agents.len(),
            "agents": self.config.agents.iter().map(|a| &a.alias).collect::<Vec<_>>(),
        });
        Ok(json_text(&val))
    }

    pub(crate) fn list_agents_impl(&self) -> Result<CallToolResult, rmcp::Error> {
        let agents: Vec<serde_json::Value> = self
            .config
            .agents
            .iter()
            .map(|a| {
                serde_json::json!({
                    "alias": a.alias,
                    "identity": a.identity,
                    "backend": a.backend,
                    "role": format!("{:?}", a.role).to_lowercase(),
                    "model": a.model,
                    "timeout_secs": a.timeout_secs,
                })
            })
            .collect();
        Ok(json_text(&agents))
    }
}
