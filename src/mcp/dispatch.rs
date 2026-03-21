//! orch_dispatch implementation.
//!
//! Dispatch is a pure message-insertion operation. It validates the target
//! agent alias and inserts the message into the store. Trigger eligibility
//! (whether the message should spawn an execution) is determined by the
//! worker poll loop, not here.

use rmcp::model::CallToolResult;
use serde::Serialize;

use super::params::DispatchParams;
use super::server::{err_text, json_text, OrchestratorMcpServer};

#[derive(Serialize)]
struct DispatchResult {
    thread_id: String,
    message_id: i64,
    /// Concrete CLI command to wait for the agent's response.
    next_step: String,
}

impl OrchestratorMcpServer {
    pub async fn dispatch_impl(
        &self,
        params: DispatchParams,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        // Snapshot live config for alias validation.
        let config = self.config.load();

        // Validate target agent exists
        if !config.agents.iter().any(|a| a.alias == params.to) {
            return Ok(err_text(format!(
                "unknown agent alias: '{}'. available: {}",
                params.to,
                config
                    .agents
                    .iter()
                    .map(|a| a.alias.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }

        // Generate thread_id if not provided
        let thread_id = params
            .thread_id
            .unwrap_or_else(|| ulid::Ulid::new().to_string());

        // Insert message — trigger eligibility is determined by the worker.
        let message_id = match self
            .store
            .insert_message(
                &thread_id,
                &params.from,
                &params.to,
                &params.intent,
                &params.body,
                params.batch.as_deref(),
                params.summary.as_deref(),
            )
            .await
        {
            Ok(id) => id,
            Err(e) => return Ok(err_text(format!("failed to insert message: {}", e))),
        };

        let next_step = format!(
            "compas wait --thread-id {} --since db:{} --timeout 900",
            thread_id, message_id
        );
        Ok(json_text(&DispatchResult {
            thread_id,
            message_id,
            next_step,
        }))
    }
}
