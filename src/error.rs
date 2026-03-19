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

    #[error("{}", daemon_lock_held_message(.worker_id, *.pid, *.heartbeat_age_secs))]
    DaemonLockHeld {
        worker_id: String,
        pid: u32,
        heartbeat_age_secs: i64,
    },

    #[error("timeout: {0}")]
    Timeout(String),

    #[error("{0}")]
    Other(String),
}

fn daemon_lock_held_message(worker_id: &str, pid: u32, heartbeat_age_secs: i64) -> String {
    if pid == 0 {
        format!(
            "worker lock held by another process (worker_id: {worker_id}). \
             Another worker holds the lock but hasn't started heartbeating yet. \
             Check for running compas processes: pgrep -fl compas"
        )
    } else {
        format!(
            "worker lock held by another process (worker_id: {worker_id}, pid: {pid}, \
             heartbeat_age_secs: {heartbeat_age_secs}). \
             Kill the existing worker (kill {pid}) or wait for it to exit."
        )
    }
}

pub type Result<T> = std::result::Result<T, OrchestratorError>;
