use async_trait::async_trait;
use std::sync::{Arc, Mutex};

use crate::backend::Backend;
use crate::error::Result;
use crate::model::agent::Agent;
use crate::model::message::Message;
use crate::model::session::{Session, SessionStatus, TriggerResult};
use crate::notification::Notifier;
use chrono::Utc;
use uuid::Uuid;

/// Stub backend for testing — returns configurable responses.
#[derive(Debug)]
pub struct StubBackend {
    pub trigger_success: bool,
    pub trigger_output: Option<String>,
}

impl Default for StubBackend {
    fn default() -> Self {
        Self {
            trigger_success: true,
            trigger_output: Some("stub output".into()),
        }
    }
}

#[async_trait]
impl Backend for StubBackend {
    fn name(&self) -> &str {
        "stub"
    }

    async fn start_session(&self, agent: &Agent) -> Result<Session> {
        Ok(Session {
            id: Uuid::new_v4().to_string(),
            agent_alias: agent.alias.clone(),
            backend: "stub".into(),
            started_at: Utc::now(),
        })
    }

    async fn trigger(
        &self,
        _agent: &Agent,
        session: &Session,
        _instruction: Option<&str>,
    ) -> Result<TriggerResult> {
        Ok(TriggerResult {
            session_id: session.id.clone(),
            success: self.trigger_success,
            output: self.trigger_output.clone(),
        })
    }

    async fn session_status(&self, _agent: &Agent) -> Result<Option<SessionStatus>> {
        Ok(Some(SessionStatus::Running))
    }

    async fn kill_session(&self, _agent: &Agent, _session: &Session, _reason: &str) -> Result<()> {
        Ok(())
    }
}

/// Stub notifier for testing — captures notifications.
#[derive(Debug, Default)]
pub struct StubNotifier {
    pub notifications: Arc<Mutex<Vec<String>>>,
}

impl StubNotifier {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn notification_count(&self) -> usize {
        self.notifications.lock().unwrap().len()
    }
}

#[async_trait]
impl Notifier for StubNotifier {
    fn name(&self) -> &str {
        "stub"
    }

    async fn notify(&self, message: &Message) -> Result<()> {
        let summary = format!(
            "{}: {} -> {}",
            message.frontmatter.intent,
            message.frontmatter.from_alias,
            message.frontmatter.to_alias
        );
        self.notifications.lock().unwrap().push(summary);
        Ok(())
    }
}
