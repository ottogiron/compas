//! orch_wait MCP tool — delegates to the shared wait logic.
//!
//! Sends MCP progress notifications at regular intervals to keep the
//! client connection alive during long waits.

use std::time::Duration;

use rmcp::model::{CallToolResult, NumberOrString, ProgressNotificationParam, ProgressToken};
use rmcp::{Peer, RoleServer};
use serde::Serialize;
use tracing::debug;

use super::params::WaitParams;
use super::server::{err_text, json_text, OrchestratorMcpServer};
use crate::store;
use crate::wait::{self, WaitOutcome, WaitRequest};

/// Interval between progress notifications sent to the MCP client.
const PROGRESS_INTERVAL: Duration = Duration::from_secs(10);

impl OrchestratorMcpServer {
    pub async fn wait_impl(
        &self,
        params: WaitParams,
        peer: Option<Peer<RoleServer>>,
        client_progress_token: Option<ProgressToken>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        // Snapshot live config for this request.
        let config = self.config.load();
        let timeout_secs = params.timeout_secs.unwrap_or(120);
        let thread_id_for_progress = params.thread_id.clone();

        let req = WaitRequest {
            thread_id: params.thread_id,
            intent: params.intent,
            since_reference: params.since_reference,
            strict_new: params.strict_new.unwrap_or(false),
            timeout: Duration::from_secs(timeout_secs),
            trigger_intents: config.orchestration.trigger_intents.clone(),
        };

        // Spawn a background task that sends progress notifications at regular
        // intervals. This keeps the MCP client from timing out the tool call
        // while we wait for the agent's response.
        //
        // Use the client's progress token if provided (spec-compliant), otherwise
        // generate a stable token for the duration of this wait.
        let progress_handle = if let Some(peer) = peer {
            let token = client_progress_token.unwrap_or_else(|| {
                ProgressToken(NumberOrString::String(
                    format!("orch-wait-{}", thread_id_for_progress).into(),
                ))
            });
            debug!(
                thread_id = %thread_id_for_progress,
                token = ?token,
                "starting progress reporter for orch_wait"
            );
            Some(tokio::spawn(async move {
                let mut elapsed = 0u64;
                loop {
                    tokio::time::sleep(PROGRESS_INTERVAL).await;
                    elapsed += PROGRESS_INTERVAL.as_secs();

                    let param = ProgressNotificationParam::new(token.clone(), elapsed as f64)
                        .with_total(timeout_secs as f64)
                        .with_message(format!(
                            "waiting for message on thread {}... {}/{}s",
                            thread_id_for_progress, elapsed, timeout_secs
                        ));

                    debug!(elapsed, "sending orch_wait progress notification");

                    // Best-effort — if the client disconnects, we'll stop on
                    // the next iteration when the wait itself completes.
                    match peer.notify_progress(param).await {
                        Ok(_) => debug!(elapsed, "progress notification sent"),
                        Err(e) => {
                            debug!(elapsed, error = %e, "progress notification failed, stopping reporter");
                            break;
                        }
                    }
                }
            }))
        } else {
            None
        };

        let result = wait::wait_for_message(&self.store, &req).await;

        // Cancel the progress reporter now that the wait is done.
        if let Some(handle) = progress_handle {
            handle.abort();
        }

        match result {
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
