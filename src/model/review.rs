use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A review token issued upon approval.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewToken {
    pub token: String,
    pub thread_id: String,
    pub issued_by: String,
    pub issued_to: String,
    pub issued_at: DateTime<Utc>,
    pub used: bool,
}

/// Outcome of a review decision.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ReviewOutcome {
    Approved { token: String },
    ChangesRequested { feedback: String },
}
