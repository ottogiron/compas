//! orch_wait MCP tool — delegates to the shared wait logic.

use std::time::Duration;

use rmcp::model::CallToolResult;
use serde::Serialize;

use super::params::WaitParams;
use super::server::{err_text, json_text, OrchestratorMcpServer};
use crate::store;
use crate::wait::{self, WaitOutcome, WaitRequest};

impl OrchestratorMcpServer {
    pub async fn wait_impl(&self, params: WaitParams) -> Result<CallToolResult, rmcp::Error> {
        let req = WaitRequest {
            thread_id: params.thread_id,
            intent: params.intent,
            since_reference: params.since_reference,
            strict_new: params.strict_new.unwrap_or(false),
            timeout: Duration::from_secs(params.timeout_secs.unwrap_or(15)),
            trigger_intents: self.config.orchestration.trigger_intents.clone(),
        };

        match wait::wait_for_message(&self.store, &req).await {
            Ok(WaitOutcome::Found(msg)) => {
                #[derive(Serialize)]
                struct WaitResult {
                    found: bool,
                    message_id: i64,
                    reference: String,
                    from: String,
                    to: String,
                    intent: String,
                    body: String,
                    thread_id: String,
                    created_at: i64,
                }

                Ok(json_text(&WaitResult {
                    found: true,
                    message_id: msg.id,
                    reference: store::message_ref(msg.id),
                    from: msg.from_alias,
                    to: msg.to_alias,
                    intent: msg.intent,
                    body: msg.body,
                    thread_id: msg.thread_id,
                    created_at: msg.created_at,
                }))
            }
            Ok(WaitOutcome::Timeout {
                thread_id,
                timeout_secs,
                intent_filter,
            }) => {
                #[derive(Serialize)]
                struct WaitTimeout {
                    found: bool,
                    thread_id: String,
                    timeout_secs: u64,
                    intent_filter: Option<String>,
                }

                Ok(json_text(&WaitTimeout {
                    found: false,
                    thread_id,
                    timeout_secs,
                    intent_filter,
                }))
            }
            Err(e) => Ok(err_text(e)),
        }
    }
}
