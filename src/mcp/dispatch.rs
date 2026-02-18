//! orch_dispatch implementation.

use rmcp::model::CallToolResult;

use super::params::DispatchParams;
use super::server::{err_text, json_text, parse_intent, OrchestratorMcpServer};
use crate::store;

impl OrchestratorMcpServer {
    pub(crate) async fn dispatch_impl(
        &self,
        params: DispatchParams,
    ) -> Result<CallToolResult, rmcp::Error> {
        // Validate intent
        if let Err(e) = parse_intent(&params.intent) {
            return Ok(err_text(e));
        }

        // Generate thread ID if not provided
        let thread_id = params
            .thread_id
            .unwrap_or_else(|| format!("t-{}", uuid::Uuid::new_v4().as_simple()));

        // Insert message into store
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
            Err(e) => return Ok(err_text(e)),
        };

        // Notify waiters
        self.wait_registry.notify(thread_id.clone(), message_id);

        let val = serde_json::json!({
            "status": "dispatched",
            "thread_id": thread_id,
            "message_ref": store::message_ref(message_id),
            "from": params.from,
            "to": params.to,
            "intent": params.intent,
        });
        Ok(json_text(&val))
    }
}
