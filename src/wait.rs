//! Transport-agnostic wait logic.
//!
//! Polls the store at 200ms intervals for a matching message on a thread.
//! Used by the `compas wait` CLI subcommand.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
    Found {
        message: MessageRow,
        /// Number of fan-out child threads that were awaited.
        /// Present only when `await_chain=true` and fan-out children existed.
        fanout_children_awaited: Option<u32>,
        /// Wall-clock Unix timestamp (seconds) when the wait settled.
        /// Present only when `await_chain=true` and settlement required blocking.
        settled_at: Option<i64>,
    },
    /// Timed out without finding a match.
    Timeout {
        thread_id: String,
        timeout_secs: u64,
        intent_filter: Option<String>,
        chain_pending: bool,
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
    // Tracks whether we observed pending chain work in a prior poll iteration.
    // When pending drops to 0, we re-fetch messages once to capture any that
    // arrived between the messages query and the pending-work check (TOCTOU fix).
    let mut chain_was_pending = false;

    // Settlement metadata captured when fan-out settles (pending drops to 0
    // after chain_was_pending was true). Carried across the re-fetch iteration.
    let mut settled_metadata: Option<(u32, i64)> = None;

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
                    chain_was_pending = true;
                    // Chain still has work in progress or pending handoffs — keep polling.
                    if tokio::time::Instant::now() >= deadline {
                        return Ok(WaitOutcome::Timeout {
                            thread_id: req.thread_id.clone(),
                            timeout_secs: req.timeout.as_secs(),
                            intent_filter: req.intent.clone(),
                            chain_pending: true,
                        });
                    }
                    tokio::time::sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;
                    continue;
                }
                if chain_was_pending {
                    // Chain just settled — capture settlement metadata, then
                    // re-fetch to capture messages that arrived between the
                    // messages query and the pending check.
                    let fanout_count = store
                        .count_fanout_children(&req.thread_id)
                        .await
                        .map_err(|e| format!("fanout child count failed: {}", e))?;
                    let now_ts = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_secs() as i64;
                    settled_metadata = Some((fanout_count, now_ts));
                    chain_was_pending = false;
                    continue;
                }
            }
            let (fanout_children_awaited, settled_at) = match settled_metadata {
                Some((count, ts)) => (Some(count), Some(ts)),
                None => (None, None),
            };
            return Ok(WaitOutcome::Found {
                message: msg.clone(),
                fanout_children_awaited,
                settled_at,
            });
        }

        if tokio::time::Instant::now() >= deadline {
            return Ok(WaitOutcome::Timeout {
                thread_id: req.thread_id.clone(),
                timeout_secs: req.timeout.as_secs(),
                intent_filter: req.intent.clone(),
                chain_pending: false,
            });
        }

        tokio::time::sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;
    }
}
