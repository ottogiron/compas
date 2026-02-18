use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Intent of a message in the orchestration workflow.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum Intent {
    Dispatch,
    Handoff,
    ReviewRequest,
    Approved,
    ChangesRequested,
    Completion,
    StatusUpdate,
    DecisionNeeded,
}

impl std::str::FromStr for Intent {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "dispatch" => Ok(Intent::Dispatch),
            "handoff" => Ok(Intent::Handoff),
            "review-request" => Ok(Intent::ReviewRequest),
            "approved" => Ok(Intent::Approved),
            "changes-requested" => Ok(Intent::ChangesRequested),
            "completion" => Ok(Intent::Completion),
            "status-update" => Ok(Intent::StatusUpdate),
            "decision-needed" => Ok(Intent::DecisionNeeded),
            other => Err(format!("unknown intent '{}'", other)),
        }
    }
}

impl std::fmt::Display for Intent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Intent::Dispatch => write!(f, "dispatch"),
            Intent::Handoff => write!(f, "handoff"),
            Intent::ReviewRequest => write!(f, "review-request"),
            Intent::Approved => write!(f, "approved"),
            Intent::ChangesRequested => write!(f, "changes-requested"),
            Intent::Completion => write!(f, "completion"),
            Intent::StatusUpdate => write!(f, "status-update"),
            Intent::DecisionNeeded => write!(f, "decision-needed"),
        }
    }
}

/// Status of a message in the mailbox.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum MessageStatus {
    New,
    InProgress,
    Resolved,
    Closed,
}

impl std::fmt::Display for MessageStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MessageStatus::New => write!(f, "new"),
            MessageStatus::InProgress => write!(f, "in-progress"),
            MessageStatus::Resolved => write!(f, "resolved"),
            MessageStatus::Closed => write!(f, "closed"),
        }
    }
}

/// YAML frontmatter metadata for a message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageFrontmatter {
    pub session_namespace_id: String,
    pub from_alias: String,
    pub to_alias: String,
    pub intent: Intent,
    #[serde(default)]
    pub task_batch: Option<String>,
    #[serde(default)]
    pub thread_id: Option<String>,
    #[serde(default)]
    pub blocking: bool,
    #[serde(default = "default_status")]
    pub status: MessageStatus,
    #[serde(default)]
    pub review_token: Option<String>,
}

fn default_status() -> MessageStatus {
    MessageStatus::New
}

/// A complete message with frontmatter and body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub frontmatter: MessageFrontmatter,
    pub body: String,
    pub file_path: Option<std::path::PathBuf>,
    pub timestamp: Option<DateTime<Utc>>,
}

/// Thread lifecycle status in the orchestrator.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ThreadStatus {
    Active,
    Completed,
    Failed,
    Abandoned,
}

impl ThreadStatus {
    /// Returns true if this status is terminal (thread will not receive further messages).
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            ThreadStatus::Completed | ThreadStatus::Failed | ThreadStatus::Abandoned
        )
    }
}

impl std::str::FromStr for ThreadStatus {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "Active" => Ok(ThreadStatus::Active),
            "Completed" => Ok(ThreadStatus::Completed),
            "Failed" => Ok(ThreadStatus::Failed),
            "Abandoned" => Ok(ThreadStatus::Abandoned),
            other => Err(format!("unknown thread status '{}'", other)),
        }
    }
}

impl std::fmt::Display for ThreadStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ThreadStatus::Active => write!(f, "Active"),
            ThreadStatus::Completed => write!(f, "Completed"),
            ThreadStatus::Failed => write!(f, "Failed"),
            ThreadStatus::Abandoned => write!(f, "Abandoned"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_intent_from_str_roundtrip() {
        let cases = [
            ("dispatch", Intent::Dispatch),
            ("handoff", Intent::Handoff),
            ("review-request", Intent::ReviewRequest),
            ("approved", Intent::Approved),
            ("changes-requested", Intent::ChangesRequested),
            ("completion", Intent::Completion),
            ("status-update", Intent::StatusUpdate),
            ("decision-needed", Intent::DecisionNeeded),
        ];
        for (s, expected) in &cases {
            let parsed: Intent = s.parse().unwrap();
            assert_eq!(&parsed, expected);
            assert_eq!(&parsed.to_string(), s);
        }
    }

    #[test]
    fn test_intent_from_str_unknown() {
        let result: Result<Intent, _> = "bogus".parse();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown intent"));
    }

    #[test]
    fn test_thread_status_from_str_roundtrip() {
        let cases = [
            ("Active", ThreadStatus::Active),
            ("Completed", ThreadStatus::Completed),
            ("Failed", ThreadStatus::Failed),
            ("Abandoned", ThreadStatus::Abandoned),
        ];
        for (s, expected) in &cases {
            let parsed: ThreadStatus = s.parse().unwrap();
            assert_eq!(&parsed, expected);
            assert_eq!(&parsed.to_string(), s);
        }
    }

    #[test]
    fn test_thread_status_from_str_unknown() {
        let result: Result<ThreadStatus, _> = "bogus".parse();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown thread status"));
    }

    #[test]
    fn test_thread_status_is_terminal() {
        assert!(!ThreadStatus::Active.is_terminal());
        assert!(ThreadStatus::Completed.is_terminal());
        assert!(ThreadStatus::Failed.is_terminal());
        assert!(ThreadStatus::Abandoned.is_terminal());
    }
}
