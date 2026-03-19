//! Transport-agnostic wait logic.
//!
//! Polls the store at 200ms intervals for a matching message on a thread.
//! Used by the `compas wait` CLI subcommand.

use std::time::Duration;

use crate::store::{self, MessageRow, Store};

const POLL_INTERVAL_MS: u64 = 200;

/// Parameters for a wait operation (transport-independent).
pub struct WaitRequest {
    pub thread_id: String,
    pub intent: Option<String>,
    pub since_reference: Option<String>,
    pub strict_new: bool,
    pub timeout: Duration,
    /// Trigger intents to auto-exclude when no explicit intent or cursor is set.
    pub trigger_intents: Vec<String>,
    /// When true, keep polling until the entire handoff chain settles
    /// (no active executions AND no untriggered handoff messages on the thread).
    pub await_chain: bool,
}

/// Outcome of a wait operation.
pub enum WaitOutcome {
    /// A matching message was found.
    Found(MessageRow),
    /// Timed out without finding a match.
    Timeout {
        thread_id: String,
        timeout_secs: u64,
        intent_filter: Option<String>,
    },
}

/// Run the wait polling loop against the store.
///
/// Returns `Ok(WaitOutcome)` for normal found/timeout results,
/// or `Err(String)` for unrecoverable errors (bad cursor, DB failure).
pub async fn wait_for_message(store: &Store, req: &WaitRequest) -> Result<WaitOutcome, String> {
    // Resolve starting cursor
    let since_id = match req.since_reference.as_deref() {
        Some(r) => store::parse_message_ref(r)?,
        None => {
            if req.strict_new {
                store
                    .latest_message_id(&req.thread_id)
                    .await
                    .unwrap_or(None)
                    .unwrap_or(0)
            } else {
                0
            }
        }
    };

    let deadline = tokio::time::Instant::now() + req.timeout;

    loop {
        let messages = store
            .get_messages_since(&req.thread_id, since_id)
            .await
            .map_err(|e| format!("wait query failed: {}", e))?;

        // Filter by intent if specified, or auto-exclude trigger intents.
        let matching: Vec<&MessageRow> = if let Some(ref intent) = req.intent {
            messages.iter().filter(|m| m.intent == *intent).collect()
        } else if req.since_reference.is_none() {
            messages
                .iter()
                .filter(|m| !req.trigger_intents.contains(&m.intent))
                .collect()
        } else {
            messages.iter().collect()
        };

        if let Some(&msg) = matching.last() {
            if req.await_chain {
                let pending = store
                    .count_pending_chain_and_fanout_work(&req.thread_id)
                    .await
                    .map_err(|e| format!("chain settlement check failed: {}", e))?;
                if pending > 0 {
                    // Chain still has work in progress or pending handoffs — keep polling.
                    if tokio::time::Instant::now() >= deadline {
                        return Ok(WaitOutcome::Timeout {
                            thread_id: req.thread_id.clone(),
                            timeout_secs: req.timeout.as_secs(),
                            intent_filter: req.intent.clone(),
                        });
                    }
                    tokio::time::sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;
                    continue;
                }
            }
            return Ok(WaitOutcome::Found(msg.clone()));
        }

        if tokio::time::Instant::now() >= deadline {
            return Ok(WaitOutcome::Timeout {
                thread_id: req.thread_id.clone(),
                timeout_secs: req.timeout.as_secs(),
                intent_filter: req.intent.clone(),
            });
        }

        tokio::time::sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;
    }
}
