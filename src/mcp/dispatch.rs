//! orch_dispatch implementation.

use rmcp::model::CallToolResult;
use serde::Serialize;

use super::params::DispatchParams;
use super::server::{err_text, json_text, OrchestratorMcpServer};
use crate::config::types::AgentRole;

#[derive(Serialize)]
struct DispatchResult {
    thread_id: String,
    message_id: i64,
    execution_id: Option<String>,
    triggered: bool,
}

impl OrchestratorMcpServer {
    pub(crate) async fn dispatch_impl(
        &self,
        params: DispatchParams,
    ) -> Result<CallToolResult, rmcp::Error> {
        // Validate target agent exists
        let target = match self.config.agents.iter().find(|a| a.alias == params.to) {
            Some(a) => a,
            None => {
                return Ok(err_text(format!(
                    "unknown agent alias: '{}'. available: {}",
                    params.to,
                    self.config
                        .agents
                        .iter()
                        .map(|a| a.alias.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
            }
        };

        // Generate thread_id if not provided
        let thread_id = params
            .thread_id
            .unwrap_or_else(|| ulid::Ulid::new().to_string());

        // Insert message
        let message_id = match self
            .store
            .insert_message(
                &thread_id,
                &params.from,
                &params.to,
                &params.intent,
                &params.body,
                params.batch.as_deref(),
            )
            .await
        {
            Ok(id) => id,
            Err(e) => return Ok(err_text(format!("failed to insert message: {}", e))),
        };

        // Check if we should trigger the agent (worker role + matching intent)
        let should_trigger = target.role == AgentRole::Worker
            && self
                .config
                .orchestration
                .trigger_intents
                .iter()
                .any(|i| i == &params.intent);

        let execution_id = if should_trigger {
            match self.store.insert_execution(&thread_id, &params.to).await {
                Ok(id) => Some(id),
                Err(e) => {
                    tracing::error!(error = %e, "failed to insert execution");
                    None
                }
            }
        } else {
            None
        };

        let triggered = execution_id.is_some();
        Ok(json_text(&DispatchResult {
            thread_id,
            message_id,
            execution_id,
            triggered,
        }))
    }
}
