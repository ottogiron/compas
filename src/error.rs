use thiserror::Error;

#[derive(Debug, Error)]
pub enum OrchestratorError {
    #[error("config error: {0}")]
    Config(String),

    #[error("store error: {0}")]
    Store(String),

    #[error("workflow error: {0}")]
    Workflow(String),

    #[error("backend error: {0}")]
    Backend(String),

    #[error("notification error: {0}")]
    Notification(String),

    #[error("audit error: {0}")]
    Audit(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("yaml parse error: {0}")]
    Yaml(#[from] serde_yaml::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("sqlite error: {0}")]
    Sqlite(#[from] sqlx::Error),

    #[error("unknown agent alias: {0}")]
    UnknownAlias(String),

    #[error("invalid intent transition: {from} -> {to}")]
    InvalidTransition { from: String, to: String },

    #[error("invalid review token: {0}")]
    InvalidReviewToken(String),

    #[error("daemon lock held by another process")]
    DaemonLockHeld,

    #[error("timeout: {0}")]
    Timeout(String),

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, OrchestratorError>;
