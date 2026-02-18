use crate::error::Result;
use crate::model::session::Session;
use std::collections::HashMap;
use std::fs::File;
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;
use uuid::Uuid;

fn try_flock(file: &File) -> std::io::Result<()> {
    let fd = file.as_raw_fd();
    let ret = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
    if ret != 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Persists backend sessions by agent alias so runs can resume.
#[derive(Debug, Default)]
pub struct SessionLedger {
    sessions: HashMap<String, Session>,
    file_path: Option<PathBuf>,
}

impl SessionLedger {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_file(path: PathBuf) -> Result<Self> {
        let sessions = load_sessions(&path)?;
        Ok(Self {
            sessions,
            file_path: Some(path),
        })
    }

    pub fn get(&self, alias: &str) -> Option<&Session> {
        self.sessions.get(alias)
    }

    pub fn upsert(&mut self, alias: &str, session: Session) -> Result<()> {
        self.sessions.insert(alias.to_string(), session);
        self.persist()
    }

    fn persist(&self) -> Result<()> {
        let Some(path) = &self.file_path else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Acquire exclusive lock on the ledger file to prevent concurrent writes
        let lock_path = PathBuf::from(format!("{}.lock", path.display()));
        let lock_file = File::create(&lock_path)?;
        try_flock(&lock_file).map_err(|e| {
            crate::error::OrchestratorError::Workflow(format!(
                "failed to acquire session ledger lock: {}",
                e
            ))
        })?;

        // Atomic write: write to temp file then rename to avoid partial-write races
        let tmp_path = PathBuf::from(format!("{}.tmp.{}", path.display(), Uuid::new_v4()));
        let content = serde_json::to_string_pretty(&self.sessions)?;
        std::fs::write(&tmp_path, content)?;
        std::fs::rename(&tmp_path, path)?;

        // Lock is released when lock_file is dropped
        drop(lock_file);
        let _ = std::fs::remove_file(&lock_path);

        Ok(())
    }
}

fn load_sessions(path: &PathBuf) -> Result<HashMap<String, Session>> {
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let content = std::fs::read_to_string(path)?;
    if content.trim().is_empty() {
        return Ok(HashMap::new());
    }
    Ok(serde_json::from_str(&content)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn test_session_ledger_persist_and_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".session-ledger.json");
        let mut ledger = SessionLedger::with_file(path.clone()).unwrap();
        let session = Session {
            id: "s1".into(),
            agent_alias: "focused".into(),
            backend: "stub".into(),
            started_at: Utc::now(),
        };
        ledger.upsert("focused", session).unwrap();

        let reloaded = SessionLedger::with_file(path).unwrap();
        assert_eq!(reloaded.get("focused").unwrap().id, "s1");
    }
}
