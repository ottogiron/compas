pub mod claude;
pub mod codex;
pub mod gemini;
pub mod opencode;
pub mod process;
pub mod registry;

use async_trait::async_trait;
use serde::Serialize;
use std::fmt::Debug;

use crate::error::Result;
use crate::model::agent::Agent;
use crate::model::session::{Session, SessionStatus};

/// Result of a backend liveness ping.
#[derive(Debug, Clone, Serialize)]
pub struct PingResult {
    pub alive: bool,
    pub latency_ms: u64,
    pub detail: Option<String>,
}

/// Unified output envelope produced by all backends after trigger execution.
///
/// Replaces the former ad-hoc per-backend output parsing with a single contract.
/// Each backend maps its native CLI output format into this struct before returning.
#[derive(Debug, Clone)]
pub struct BackendOutput {
    /// Whether the execution succeeded (backend exited cleanly with valid output).
    pub success: bool,
    /// The agent's response text (extracted from backend-specific format).
    pub result_text: String,
    /// Parsed intent from the agent's response (e.g., "review-request", "status-update").
    /// `None` if no intent could be parsed.
    pub parsed_intent: Option<String>,
    /// Backend-specific session ID for resume (Claude session_id, Codex thread_id,
    /// OpenCode sessionID). `None` when unavailable or non-resumable.
    pub session_id: Option<String>,
    /// Raw output text (full stdout for logging/debugging).
    pub raw_output: String,
}

/// Parse an intent JSON line from agent output text.
///
/// Looks for a JSON object containing `{"intent": "..."}` in the text.
/// Searches lines from the end (intent is typically the last line).
/// Returns the intent string if found.
pub fn parse_intent_from_text(text: &str) -> Option<String> {
    // Try parsing the entire text as JSON first (single-line output)
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(text) {
        if let Some(intent) = val.get("intent").and_then(|i| i.as_str()) {
            return Some(intent.to_string());
        }
    }
    // Scan lines from the end — intent JSON is typically the last line
    for line in text.lines().rev() {
        let trimmed = line.trim();
        if trimmed.starts_with('{') && trimmed.ends_with('}') {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) {
                if let Some(intent) = val.get("intent").and_then(|i| i.as_str()) {
                    return Some(intent.to_string());
                }
            }
        }
    }
    None
}

/// Backend trait for agent session management.
#[async_trait]
pub trait Backend: Send + Sync + Debug {
    fn name(&self) -> &str;
    async fn start_session(&self, agent: &Agent) -> Result<Session>;
    async fn trigger(
        &self,
        agent: &Agent,
        session: &Session,
        instruction: Option<&str>,
    ) -> Result<BackendOutput>;
    async fn session_status(&self, agent: &Agent) -> Result<Option<SessionStatus>>;
    async fn kill_session(&self, agent: &Agent, session: &Session, reason: &str) -> Result<()>;

    /// Liveness probe: send a minimal prompt to verify the backend can execute.
    /// Default implementation returns alive for stub-like backends.
    async fn ping(&self, _agent: &Agent, _timeout_secs: u64) -> PingResult {
        PingResult {
            alive: true,
            latency_ms: 0,
            detail: Some("default ping (no probe)".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_intent_full_json() {
        let text = r#"{"intent": "status-update", "to": "operator", "body": "Done"}"#;
        let intent = parse_intent_from_text(text);
        assert_eq!(intent, Some("status-update".to_string()));
    }

    #[test]
    fn test_parse_intent_embedded_json_not_supported() {
        // Intent embedded in the middle of a line is NOT supported.
        // The reply protocol requires intent JSON on its own line.
        let text = r#"I finished the task. {"intent": "completion", "to": "lead"}"#;
        let intent = parse_intent_from_text(text);
        assert_eq!(intent, None);
    }

    #[test]
    fn test_parse_intent_on_own_line_after_text() {
        // Intent JSON on its own line after text — this IS supported.
        let text = "I finished the task.\n{\"intent\": \"completion\", \"to\": \"lead\"}";
        let intent = parse_intent_from_text(text);
        assert_eq!(intent, Some("completion".to_string()));
    }

    #[test]
    fn test_parse_intent_no_to_field() {
        let text = r#"{"intent": "status-update"}"#;
        let intent = parse_intent_from_text(text);
        assert_eq!(intent, Some("status-update".to_string()));
    }

    #[test]
    fn test_parse_intent_plain_text() {
        let intent = parse_intent_from_text("just plain text");
        assert_eq!(intent, None);
    }

    #[test]
    fn test_parse_intent_json_without_intent_field() {
        let text = r#"{"result": "ok", "status": "done"}"#;
        let intent = parse_intent_from_text(text);
        assert_eq!(intent, None);
    }

    #[test]
    fn test_parse_intent_multiline_intent_at_end() {
        let text =
            "Some output text\nMore output\n{\"intent\": \"review-request\", \"to\": \"operator\"}";
        let intent = parse_intent_from_text(text);
        assert_eq!(intent, Some("review-request".to_string()));
    }

    #[test]
    fn test_parse_intent_empty_text() {
        let intent = parse_intent_from_text("");
        assert_eq!(intent, None);
    }
}
