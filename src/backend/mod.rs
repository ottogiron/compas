pub mod claude;
pub mod codex;
pub mod gemini;
pub mod opencode;
pub mod process;
pub mod registry;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::fmt::Debug;

use crate::error::Result;
use crate::model::agent::Agent;
use crate::model::session::{Session, SessionStatus};

/// Error category for classifying execution failures.
///
/// Used to determine whether an execution is retryable. The classification
/// uses a deny-list strategy: known non-retryable categories are identified
/// explicitly, and Unknown defaults to non-retryable (safe default).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCategory {
    /// Transient infrastructure error (network, rate limit, server error).
    /// Retryable.
    Transient,
    /// Quota/billing exhausted. Not retryable.
    QuotaExhausted,
    /// Authentication/authorization failure. Not retryable.
    AuthFailure,
    /// Agent-level error (bad output, crash in agent logic). Not retryable.
    AgentError,
    /// Unclassified error. Not retryable (safe default).
    Unknown,
}

impl ErrorCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Transient => "transient",
            Self::QuotaExhausted => "quota_exhausted",
            Self::AuthFailure => "auth_failure",
            Self::AgentError => "agent_error",
            Self::Unknown => "unknown",
        }
    }

    /// Whether this error category is eligible for retry.
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Transient)
    }
}

impl std::fmt::Display for ErrorCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for ErrorCategory {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, String> {
        match s {
            "transient" => Ok(Self::Transient),
            "quota_exhausted" => Ok(Self::QuotaExhausted),
            "auth_failure" => Ok(Self::AuthFailure),
            "agent_error" => Ok(Self::AgentError),
            "unknown" => Ok(Self::Unknown),
            other => Err(format!("unknown error category: '{}'", other)),
        }
    }
}

/// Classify an execution error into an `ErrorCategory`.
///
/// Uses a deny-list strategy: known non-retryable patterns are matched first,
/// then known transient patterns. Anything unmatched is `Unknown` (non-retryable).
///
/// - `success`: whether the backend reported success
/// - `has_result_output`: whether the backend produced parseable result output
/// - `error_text`: the error message or output text to classify
pub fn classify_error(success: bool, has_result_output: bool, error_text: &str) -> ErrorCategory {
    // Successful executions should not be classified
    if success {
        return ErrorCategory::Unknown;
    }

    let lower = error_text.to_lowercase();

    // ── Non-retryable: Auth failures ──
    if lower.contains("unauthorized")
        || lower.contains("authentication")
        || lower.contains("auth_error")
        || lower.contains("invalid api key")
        || lower.contains("invalid_api_key")
        || lower.contains("permission denied")
        || lower.contains("403 ")
        || lower.contains("http 403")
        || lower.contains("status: 403")
    {
        return ErrorCategory::AuthFailure;
    }

    // ── Non-retryable: Quota/billing (hard limits, not transient rate limits) ──
    if lower.contains("quota")
        || lower.contains("billing")
        || lower.contains("insufficient_quota")
        || lower.contains("spending limit")
        || lower.contains("credit")
    {
        return ErrorCategory::QuotaExhausted;
    }

    // ── Non-retryable: Agent-level errors ──
    // Agent produced output but it was a failure — the agent itself had an issue
    if has_result_output {
        return ErrorCategory::AgentError;
    }

    // ── Retryable: Transient infrastructure errors ──
    if lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("connection refused")
        || lower.contains("connection reset")
        || lower.contains("503")
        || lower.contains("502")
        || lower.contains("500")
        || lower.contains("server error")
        || lower.contains("overloaded")
        || lower.contains("temporarily unavailable")
        || lower.contains("econnreset")
        || lower.contains("econnrefused")
        || lower.contains("network")
        || lower.contains("dns")
        || lower.contains("rate_limit_exceeded")
        || lower.contains("too_many_requests")
        || lower.contains("429")
    {
        return ErrorCategory::Transient;
    }

    ErrorCategory::Unknown
}

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
    /// Parsed intent from the agent's response (e.g., "review-request", "response").
    /// `None` if no intent could be parsed.
    pub parsed_intent: Option<String>,
    /// Backend-specific session ID for resume (Claude session_id, Codex thread_id,
    /// OpenCode sessionID). `None` when unavailable or non-resumable.
    pub session_id: Option<String>,
    /// Raw output text (full stdout for logging/debugging).
    pub raw_output: String,
    /// Classified error category for failure cases.
    /// `None` on success or when classification hasn't been performed.
    pub error_category: Option<ErrorCategory>,
}

/// A structured event extracted from backend JSONL output during execution.
#[derive(Debug, Clone)]
pub struct ExecutionEvent {
    /// Event type: "tool_call", "tool_result", "message", "turn_complete", "error"
    pub event_type: String,
    /// Human-readable summary: "Write to src/events.rs", "cargo test (pass)"
    pub summary: String,
    /// Full JSON of the source event (for debugging). Optional.
    pub detail: Option<String>,
    /// Unix epoch milliseconds
    pub timestamp_ms: i64,
    /// Monotonic index within this execution (for stable ordering)
    pub event_index: i32,
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

    // ── classify_error tests ──

    #[test]
    fn test_classify_error_success_returns_unknown() {
        let cat = classify_error(true, true, "some text");
        assert_eq!(cat, ErrorCategory::Unknown);
    }

    #[test]
    fn test_classify_error_auth_failure() {
        assert_eq!(
            classify_error(false, false, "Error: unauthorized access"),
            ErrorCategory::AuthFailure
        );
        assert_eq!(
            classify_error(false, false, "invalid api key provided"),
            ErrorCategory::AuthFailure
        );
        assert_eq!(
            classify_error(false, false, "HTTP 403 Forbidden"),
            ErrorCategory::AuthFailure
        );
    }

    #[test]
    fn test_classify_error_quota_exhausted() {
        assert_eq!(
            classify_error(false, false, "quota exceeded for this billing period"),
            ErrorCategory::QuotaExhausted
        );
        assert_eq!(
            classify_error(false, false, "insufficient_quota"),
            ErrorCategory::QuotaExhausted
        );
        assert_eq!(
            classify_error(false, false, "spending limit reached"),
            ErrorCategory::QuotaExhausted
        );
    }

    #[test]
    fn test_classify_error_rate_limit_is_transient() {
        assert_eq!(
            classify_error(false, false, "rate_limit_exceeded"),
            ErrorCategory::Transient
        );
        assert_eq!(
            classify_error(false, false, "too_many_requests"),
            ErrorCategory::Transient
        );
        assert_eq!(
            classify_error(false, false, "HTTP 429 Too Many Requests"),
            ErrorCategory::Transient
        );
    }

    #[test]
    fn test_classify_error_403_narrow_matching() {
        // "403 " with trailing space matches (e.g. "403 Forbidden")
        assert_eq!(
            classify_error(false, false, "HTTP 403 Forbidden"),
            ErrorCategory::AuthFailure
        );
        // Bare "403" in a port number should NOT match
        assert_eq!(
            classify_error(false, false, "listening on port 4030"),
            ErrorCategory::Unknown
        );
    }

    #[test]
    fn test_classify_error_agent_error_with_result_output() {
        // Has result output but failed — agent-level error
        assert_eq!(
            classify_error(false, true, "agent produced bad output"),
            ErrorCategory::AgentError
        );
    }

    #[test]
    fn test_classify_error_transient() {
        assert_eq!(
            classify_error(false, false, "connection refused"),
            ErrorCategory::Transient
        );
        assert_eq!(
            classify_error(false, false, "HTTP 503 Service Unavailable"),
            ErrorCategory::Transient
        );
        assert_eq!(
            classify_error(false, false, "server is overloaded"),
            ErrorCategory::Transient
        );
        assert_eq!(
            classify_error(false, false, "DNS resolution failed"),
            ErrorCategory::Transient
        );
    }

    #[test]
    fn test_classify_error_unknown_default() {
        assert_eq!(
            classify_error(false, false, "something completely unexpected"),
            ErrorCategory::Unknown
        );
    }

    #[test]
    fn test_error_category_retryable() {
        assert!(ErrorCategory::Transient.is_retryable());
        assert!(!ErrorCategory::QuotaExhausted.is_retryable());
        assert!(!ErrorCategory::AuthFailure.is_retryable());
        assert!(!ErrorCategory::AgentError.is_retryable());
        assert!(!ErrorCategory::Unknown.is_retryable());
    }

    #[test]
    fn test_error_category_roundtrip() {
        for cat in &[
            ErrorCategory::Transient,
            ErrorCategory::QuotaExhausted,
            ErrorCategory::AuthFailure,
            ErrorCategory::AgentError,
            ErrorCategory::Unknown,
        ] {
            let s = cat.as_str();
            let parsed: ErrorCategory = s.parse().unwrap();
            assert_eq!(&parsed, cat);
        }
    }
}
