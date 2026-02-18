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

        let since_id = if let Some(ref raw) = params.since_reference {
            match store::parse_message_ref(raw) {
                Ok(id) => id,
                Err(e) => return Ok(err_text(e)),
            }
        } else {
            // Default: use latest message ID so we only find new messages
            match self.store.latest_thread_message_id(&params.thread_id).await {
                Ok(Some(id)) => id.saturating_sub(1),
                Ok(None) => 0,
                Err(e) => return Ok(err_text(e)),
            }
        };

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
        let matching = if let Some(ref target) = intent_filter {
            messages
                .iter()
                .rev()
                .find(|m| m.intent == *target && m.id > since_id)
        } else {
            messages.last()
        };

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
