//! orch_dispatch implementation.

use rmcp::model::CallToolResult;

use super::params::DispatchParams;
use super::server::{err_text, json_text, parse_intent, OrchestratorMcpServer};
use crate::config::types::AgentRole;
use crate::store;
use crate::worker::trigger::TRIGGER_QUEUE;
use crate::worker::TriggerJob;

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

        // Push a TriggerJob if the intent is trigger-worthy and target is a worker agent
        let triggered = self
            .maybe_push_trigger(
                &thread_id,
                &params.from,
                &params.to,
                &params.intent,
                &params.body,
                params.batch.as_deref(),
            )
            .await;

        let mut val = serde_json::json!({
            "status": "dispatched",
            "thread_id": thread_id,
            "message_ref": store::message_ref(message_id),
            "from": params.from,
            "to": params.to,
            "intent": params.intent,
        });
        if let Some(job_id) = triggered {
            val["trigger_job_id"] = serde_json::Value::String(job_id);
        }
        Ok(json_text(&val))
    }

    /// Check if this dispatch should trigger a worker job and push it if so.
    ///
    /// Returns the job ID if a trigger was pushed, None otherwise.
    pub(crate) async fn maybe_push_trigger(
        &self,
        thread_id: &str,
        from_alias: &str,
        to_alias: &str,
        intent: &str,
        body: &str,
        batch_id: Option<&str>,
    ) -> Option<String> {
        // Check if intent is in trigger_intents
        let trigger_intents = &self.config.orchestration.trigger_intents;
        if !trigger_intents.iter().any(|i| i == intent) {
            return None;
        }

        // Check if target agent exists and is a Worker
        let agent = self.config.agents.iter().find(|a| a.alias == to_alias)?;
        if agent.role != AgentRole::Worker {
            return None;
        }

        let job = TriggerJob {
            thread_id: thread_id.to_string(),
            agent_alias: to_alias.to_string(),
            message_body: body.to_string(),
            from_alias: from_alias.to_string(),
            intent: intent.to_string(),
            batch_id: batch_id.map(|s| s.to_string()),
        };

        match self.store.push_trigger_job(&job, TRIGGER_QUEUE).await {
            Ok(job_id) => {
                tracing::info!(
                    thread = %thread_id,
                    agent = %to_alias,
                    intent = %intent,
                    job_id = %job_id,
                    "trigger job pushed to queue"
                );
                Some(job_id)
            }
            Err(e) => {
                tracing::error!(
                    thread = %thread_id,
                    agent = %to_alias,
                    error = %e,
                    "failed to push trigger job"
                );
                None
            }
        }
    }
}
