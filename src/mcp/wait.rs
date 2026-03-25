//! `orch_wait` MCP tool handler and progress notification logic.
//!
//! Registered as `orch_wait` in `server.rs`. Blocks until a matching message
//! arrives on a thread, or timeout. Sends MCP progress notifications at
//! regular intervals to prevent transport timeouts.

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

/// Padding added to `execution_timeout_secs` when deriving the wait ceiling.
const CEILING_PADDING_SECS: u64 = 30;

/// Derive the maximum wait timeout from `execution_timeout_secs`.
///
/// - Non-chain: `exec_timeout + 30`
/// - Chain (`await_chain=true`): `exec_timeout * 3 + 30`
pub(crate) fn compute_wait_ceiling(exec_timeout_secs: u64, await_chain: bool) -> u64 {
    if await_chain {
        exec_timeout_secs * 3 + CEILING_PADDING_SECS
    } else {
        exec_timeout_secs + CEILING_PADDING_SECS
    }
}

/// Resolve the effective timeout for an `orch_wait` call.
///
/// Returns `(effective_timeout_secs, clamped)`.
pub(crate) fn resolve_wait_timeout(requested: Option<u64>, ceiling: u64) -> (u64, bool) {
    let effective = requested.unwrap_or(ceiling).min(ceiling);
    let clamped = requested.is_some_and(|r| r > ceiling);
    (effective, clamped)
}

impl OrchestratorMcpServer {
    pub async fn wait_impl(
        &self,
        params: WaitParams,
        peer: Option<Peer<RoleServer>>,
        client_progress_token: Option<ProgressToken>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        // Snapshot live config for this request.
        let config = self.config.load();
        let await_chain = params.await_chain.unwrap_or(false);
        let ceiling =
            compute_wait_ceiling(config.orchestration.execution_timeout_secs, await_chain);
        let requested = params.timeout_secs;
        let (timeout_secs, clamped) = resolve_wait_timeout(requested, ceiling);
        let thread_id_for_progress = params.thread_id.clone();

        let req = WaitRequest {
            thread_id: params.thread_id,
            intent: params.intent,
            since_reference: params.since_reference,
            strict_new: params.strict_new.unwrap_or(false),
            timeout: Duration::from_secs(timeout_secs),
            trigger_intents: config.orchestration.trigger_intents.clone(),
            await_chain,
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
            Ok(WaitOutcome::Found {
                message: msg,
                fanout_children_awaited,
                settled_at,
            }) => {
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
                    #[serde(skip_serializing_if = "Option::is_none")]
                    fanout_children_awaited: Option<u32>,
                    #[serde(skip_serializing_if = "Option::is_none")]
                    settled_at: Option<i64>,
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
                    fanout_children_awaited,
                    settled_at,
                }))
            }
            Ok(WaitOutcome::Timeout {
                thread_id,
                timeout_secs,
                intent_filter,
                chain_pending,
            }) => {
                #[derive(Serialize)]
                struct WaitTimeout {
                    found: bool,
                    thread_id: String,
                    timeout_secs: u64,
                    intent_filter: Option<String>,
                    chain_pending: bool,
                    #[serde(skip_serializing_if = "Option::is_none")]
                    effective_timeout_secs: Option<u64>,
                    #[serde(skip_serializing_if = "Option::is_none")]
                    requested_timeout_secs: Option<u64>,
                    #[serde(skip_serializing_if = "std::ops::Not::not")]
                    clamped: bool,
                    #[serde(skip_serializing_if = "Option::is_none")]
                    hint: Option<String>,
                }

                Ok(json_text(&WaitTimeout {
                    found: false,
                    thread_id,
                    timeout_secs,
                    intent_filter,
                    chain_pending,
                    effective_timeout_secs: if clamped { Some(timeout_secs) } else { None },
                    requested_timeout_secs: if clamped { requested } else { None },
                    clamped,
                    hint: if clamped {
                        Some(format!(
                            "Requested timeout {}s was clamped to server ceiling {}s \
                             (derived from execution_timeout_secs). \
                             Re-issue orch_wait to continue waiting.",
                            requested.unwrap_or(0),
                            ceiling
                        ))
                    } else {
                        None
                    },
                }))
            }
            Err(e) => Ok(err_text(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_wait_ceiling_non_chain() {
        assert_eq!(compute_wait_ceiling(600, false), 630);
        assert_eq!(compute_wait_ceiling(0, false), 30);
        assert_eq!(compute_wait_ceiling(1, false), 31);
        assert_eq!(compute_wait_ceiling(1800, false), 1830);
    }

    #[test]
    fn test_compute_wait_ceiling_chain() {
        assert_eq!(compute_wait_ceiling(600, true), 1830);
        assert_eq!(compute_wait_ceiling(0, true), 30);
        assert_eq!(compute_wait_ceiling(1, true), 33);
        assert_eq!(compute_wait_ceiling(1800, true), 5430);
    }

    #[test]
    fn test_resolve_wait_timeout_under_ceiling() {
        let (effective, clamped) = resolve_wait_timeout(Some(300), 630);
        assert_eq!(effective, 300);
        assert!(!clamped);
    }

    #[test]
    fn test_resolve_wait_timeout_over_ceiling() {
        let (effective, clamped) = resolve_wait_timeout(Some(5000), 630);
        assert_eq!(effective, 630);
        assert!(clamped);
    }

    #[test]
    fn test_resolve_wait_timeout_at_ceiling() {
        let (effective, clamped) = resolve_wait_timeout(Some(630), 630);
        assert_eq!(effective, 630);
        assert!(!clamped);
    }

    #[test]
    fn test_resolve_wait_timeout_none_defaults_to_ceiling() {
        let (effective, clamped) = resolve_wait_timeout(None, 630);
        assert_eq!(effective, 630);
        assert!(!clamped);
    }
}
