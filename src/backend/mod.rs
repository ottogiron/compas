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
use crate::model::session::{Session, SessionStatus, TriggerResult};

/// Result of a backend liveness ping.
#[derive(Debug, Clone, Serialize)]
pub struct PingResult {
    pub alive: bool,
    pub latency_ms: u64,
    pub detail: Option<String>,
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
    ) -> Result<TriggerResult>;
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
