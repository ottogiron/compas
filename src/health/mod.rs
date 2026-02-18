use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Health status of an agent based on ping probe.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum HealthStatus {
    /// Ping succeeded — backend can execute work.
    Healthy,
    /// Agent is currently busy; ping skipped.
    Busy,
    /// Ping failed — backend cannot execute work.
    Unhealthy,
    /// Ping skipped (stub backend or other reason).
    Skipped,
}

/// Health report for an individual agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentHealth {
    pub alias: String,
    pub backend: String,
    pub status: HealthStatus,
    pub latency_ms: Option<u64>,
    /// Runtime state: idle / busy:<thread> / failed:<n>
    pub state: String,
    pub detail: Option<String>,
}

/// Overall health report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthReport {
    pub agents: Vec<AgentHealth>,
    pub checked_at: DateTime<Utc>,
}
