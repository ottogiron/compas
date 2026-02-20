//! orch_wait implementation — polls DB at 200ms intervals.

use std::time::Duration;

use rmcp::model::CallToolResult;
use serde::Serialize;

use super::params::WaitParams;
use super::server::{err_text, json_text, OrchestratorMcpServer};
use crate::store;

const POLL_INTERVAL_MS: u64 = 200;

impl OrchestratorMcpServer {
    pub async fn wait_impl(&self, params: WaitParams) -> Result<CallToolResult, rmcp::Error> {
        let timeout = Duration::from_secs(params.timeout_secs.unwrap_or(15));
        let strict_new = params.strict_new.unwrap_or(false);

        // Resolve starting cursor
        let since_id = match params.since_reference.as_deref() {
            Some(r) => match store::parse_message_ref(r) {
                Ok(id) => id,
                Err(e) => return Ok(err_text(e)),
            },
            None => {
                if strict_new {
                    // Use current latest message ID as baseline
                    self.store
                        .latest_message_id(&params.thread_id)
                        .await
                        .unwrap_or(None)
                        .unwrap_or(0)
                } else {
                    0
                }
            }
        };

        let deadline = tokio::time::Instant::now() + timeout;

        loop {
            // Check for matching messages
            let messages = match self
                .store
                .get_messages_since(&params.thread_id, since_id)
                .await
            {
                Ok(m) => m,
                Err(e) => return Ok(err_text(format!("wait query failed: {}", e))),
            };

            // Filter by intent if specified, or auto-exclude trigger intents.
            //
            // When neither `intent` nor `since_reference` is provided, trigger
            // intents (dispatch, handoff, changes-requested) are auto-excluded
            // so the caller gets the agent's response, not their own dispatch.
            let matching: Vec<_> = if let Some(ref intent) = params.intent {
                messages.iter().filter(|m| m.intent == *intent).collect()
            } else if params.since_reference.is_none() {
                let trigger_intents = &self.config.orchestration.trigger_intents;
                messages
                    .iter()
                    .filter(|m| !trigger_intents.contains(&m.intent))
                    .collect()
            } else {
                messages.iter().collect()
            };

            if !matching.is_empty() {
                let msg = matching.last().unwrap();

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

                return Ok(json_text(&WaitResult {
                    found: true,
                    message_id: msg.id,
                    reference: store::message_ref(msg.id),
                    from: msg.from_alias.clone(),
                    to: msg.to_alias.clone(),
                    intent: msg.intent.clone(),
                    body: msg.body.clone(),
                    thread_id: msg.thread_id.clone(),
                    created_at: msg.created_at,
                }));
            }

            // Check timeout
            if tokio::time::Instant::now() >= deadline {
                #[derive(Serialize)]
                struct WaitTimeout {
                    found: bool,
                    thread_id: String,
                    timeout_secs: u64,
                    intent_filter: Option<String>,
                }

                return Ok(json_text(&WaitTimeout {
                    found: false,
                    thread_id: params.thread_id,
                    timeout_secs: timeout.as_secs(),
                    intent_filter: params.intent,
                }));
            }

            // Sleep before next poll
            tokio::time::sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;
        }
    }
}
