//! WaitRegistry broadcast + orch_wait / orch_poll implementations.

use rmcp::model::CallToolResult;
use tokio::sync::broadcast;

use super::params::{PollParams, WaitParams};
use super::server::{err_text, json_text, parse_intent, OrchestratorMcpServer};
use crate::store;

// ---------------------------------------------------------------------------
// WaitRegistry — broadcast notifications for orch_wait wake-ups
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct WaitNotification {
    pub thread_id: String,
    pub message_id: i64,
}

#[derive(Clone)]
pub struct WaitRegistry {
    sender: broadcast::Sender<WaitNotification>,
}

impl WaitRegistry {
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(256);
        Self { sender }
    }

    pub fn notify(&self, thread_id: String, message_id: i64) {
        let _ = self.sender.send(WaitNotification {
            thread_id,
            message_id,
        });
    }

    pub fn subscribe(&self) -> broadcast::Receiver<WaitNotification> {
        self.sender.subscribe()
    }
}

impl Default for WaitRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for WaitRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WaitRegistry").finish()
    }
}

fn effective_since_id(
    parsed_since_id: Option<i64>,
    call_start_latest_id: i64,
    strict_new: bool,
) -> i64 {
    let base = parsed_since_id.unwrap_or(call_start_latest_id);
    if strict_new {
        std::cmp::max(base, call_start_latest_id)
    } else {
        base
    }
}

fn find_poll_match<'a>(
    messages: &'a [store::MessageRow],
    intent_filter: Option<&str>,
    since_id: i64,
) -> Option<&'a store::MessageRow> {
    messages.iter().rev().find(|m| {
        if m.id <= since_id {
            return false;
        }
        if let Some(intent) = intent_filter {
            m.intent == intent
        } else {
            true
        }
    })
}

// ---------------------------------------------------------------------------
// Tool implementations
// ---------------------------------------------------------------------------

impl OrchestratorMcpServer {
    pub(crate) async fn wait_impl(
        &self,
        params: WaitParams,
    ) -> Result<CallToolResult, rmcp::Error> {
        let intent_filter = match params.intent.as_deref() {
            Some(s) => match parse_intent(s) {
                Ok(i) => Some(i.to_string()),
                Err(e) => return Ok(err_text(e)),
            },
            None => None,
        };
        let timeout = params.timeout_secs.unwrap_or(15);
        let strict_new = params.strict_new.unwrap_or(false);

        let parsed_since_id = if let Some(ref raw) = params.since_reference {
            match store::parse_message_ref(raw) {
                Ok(id) => Some(id),
                Err(e) => return Ok(err_text(e)),
            }
        } else {
            None
        };
        let call_start_latest_id = match self.store.latest_thread_message_id(&params.thread_id).await {
            Ok(Some(id)) => id,
            Ok(None) => 0,
            Err(e) => return Ok(err_text(e)),
        };
        let since_id = effective_since_id(parsed_since_id, call_start_latest_id, strict_new);

        let start = std::time::Instant::now();
        let deadline = std::time::Duration::from_secs(timeout);
        let mut wait_receiver = self.wait_registry.subscribe();

        // Auto-exclude trigger intents when no intent filter and no explicit cursor
        let auto_exclude = if intent_filter.is_none() && params.since_reference.is_none() {
            vec!["dispatch", "handoff", "changes-requested"]
        } else {
            vec![]
        };

        loop {
            // Check for matching messages
            let messages = match self
                .store
                .get_thread_messages_since(&params.thread_id, since_id)
                .await
            {
                Ok(msgs) => msgs,
                Err(e) => return Ok(err_text(e)),
            };

            for msg in &messages {
                // Apply auto-exclude
                if auto_exclude.contains(&msg.intent.as_str()) {
                    continue;
                }
                // Apply intent filter
                if let Some(ref target) = intent_filter {
                    if &msg.intent != target {
                        continue;
                    }
                }
                // Found a match
                let val = serde_json::json!({
                    "found": true,
                    "from": msg.from_alias,
                    "to": msg.to_alias,
                    "intent": msg.intent,
                    "body": msg.body,
                    "message_ref": store::message_ref(msg.id),
                });
                return Ok(json_text(&val));
            }

            // Check timeout
            if start.elapsed() >= deadline {
                let thread_status = self
                    .store
                    .get_thread_status(&params.thread_id)
                    .await
                    .ok()
                    .flatten();
                let all_messages = self
                    .store
                    .get_thread_messages(&params.thread_id)
                    .await
                    .unwrap_or_default();
                let last_msg = all_messages.last();

                let val = serde_json::json!({
                    "found": false,
                    "timeout_secs": timeout,
                    "thread_status": thread_status,
                    "total_messages": all_messages.len(),
                    "last_message_intent": last_msg.map(|m| &m.intent),
                    "last_message_from": last_msg.map(|m| &m.from_alias),
                    "searched_since_id": since_id,
                });
                return Ok(json_text(&val));
            }

            // Wait for notification or poll interval
            let remaining = deadline.saturating_sub(start.elapsed());
            let poll_interval = std::time::Duration::from_millis(500);
            let wait_duration = std::cmp::min(remaining, poll_interval);

            tokio::select! {
                result = wait_receiver.recv() => {
                    if let Ok(notif) = result {
                        if notif.thread_id == params.thread_id {
                            continue;
                        }
                    }
                }
                _ = tokio::time::sleep(wait_duration) => {}
            }
        }
    }

    pub(crate) async fn poll_impl(
        &self,
        params: PollParams,
    ) -> Result<CallToolResult, rmcp::Error> {
        let intent_filter = match params.intent.as_deref() {
            Some(s) => match parse_intent(s) {
                Ok(i) => Some(i.to_string()),
                Err(e) => return Ok(err_text(e)),
            },
            None => None,
        };

        let since_id = if let Some(ref raw) = params.since_reference {
            match store::parse_message_ref(raw) {
                Ok(id) => id,
                Err(e) => return Ok(err_text(e)),
            }
        } else {
            0
        };

        let thread_status = self
            .store
            .get_thread_status(&params.thread_id)
            .await
            .ok()
            .flatten();
        let messages = self
            .store
            .get_thread_messages(&params.thread_id)
            .await
            .unwrap_or_default();
        let total_messages = messages.len();
        let thread_exists = thread_status.is_some() || total_messages > 0;

        // Find matching message
        let matching = find_poll_match(&messages, intent_filter.as_deref(), since_id);

        let last_msg = messages.last();

        let mut val = serde_json::json!({
            "thread_id": params.thread_id,
            "thread_exists": thread_exists,
            "thread_status": thread_status.unwrap_or_else(|| {
                if thread_exists { "Active".into() } else { "NotFound".into() }
            }),
            "total_messages": total_messages,
            "last_message_intent": last_msg.map(|m| &m.intent),
            "last_message_from": last_msg.map(|m| &m.from_alias),
            "has_match": matching.is_some(),
        });

        if let Some(msg) = matching {
            val["match"] = serde_json::json!({
                "from": msg.from_alias,
                "to": msg.to_alias,
                "intent": msg.intent,
                "body": msg.body,
                "message_ref": store::message_ref(msg.id),
            });
        }

        Ok(json_text(&val))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MessageRow;

    fn msg(id: i64, intent: &str) -> MessageRow {
        MessageRow {
            id,
            thread_id: "t-1".into(),
            from_alias: "focused".into(),
            to_alias: "operator".into(),
            intent: intent.into(),
            body: String::new(),
            status: "new".into(),
            batch_id: None,
            review_token: None,
            created_at: 0,
        }
    }

    #[test]
    fn test_effective_since_id_defaults_to_call_start_latest() {
        assert_eq!(effective_since_id(None, 42, false), 42);
    }

    #[test]
    fn test_effective_since_id_uses_explicit_cursor_when_not_strict() {
        assert_eq!(effective_since_id(Some(7), 42, false), 7);
    }

    #[test]
    fn test_effective_since_id_strict_new_uses_max_of_cursor_and_call_start() {
        assert_eq!(effective_since_id(Some(7), 42, true), 42);
        assert_eq!(effective_since_id(Some(77), 42, true), 77);
    }

    #[test]
    fn test_find_poll_match_without_intent_respects_since_id() {
        let messages = vec![msg(1, "dispatch"), msg(2, "review-request"), msg(3, "status-update")];
        let found = find_poll_match(&messages, None, 2).expect("message newer than cursor expected");
        assert_eq!(found.id, 3);
    }

    #[test]
    fn test_find_poll_match_with_intent_respects_since_id() {
        let messages = vec![
            msg(1, "review-request"),
            msg(2, "status-update"),
            msg(3, "review-request"),
        ];
        let found = find_poll_match(&messages, Some("review-request"), 1)
            .expect("matching review-request expected");
        assert_eq!(found.id, 3);

        let not_found = find_poll_match(&messages, Some("review-request"), 3);
        assert!(not_found.is_none());
    }
}
