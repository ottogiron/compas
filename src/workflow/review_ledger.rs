use crate::error::{OrchestratorError, Result};
use crate::model::review::ReviewToken;
use chrono::Utc;
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{ErrorKind, Write};
use std::path::Path;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

const LOCK_WAIT_TIMEOUT: Duration = Duration::from_secs(2);
const LOCK_STALE_TTL: Duration = Duration::from_secs(30);
const LOCK_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Manages review tokens for the approval workflow.
/// When a file path is provided, tokens are persisted to disk as JSON
/// so they survive across separate CLI invocations.
#[derive(Debug)]
pub struct ReviewLedger {
    tokens: HashMap<String, ReviewToken>,
    file_path: Option<PathBuf>,
}

impl Default for ReviewLedger {
    fn default() -> Self {
        Self {
            tokens: HashMap::new(),
            file_path: None,
        }
    }
}

impl ReviewLedger {
    /// Create an in-memory-only ledger (for tests).
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a file-backed ledger that persists tokens to disk.
    pub fn with_file(path: PathBuf) -> Result<Self> {
        let tokens = load_tokens(&path)?;
        Ok(Self {
            tokens,
            file_path: Some(path),
        })
    }

    fn with_tokens_mut<R, F>(&mut self, f: F) -> Result<R>
    where
        F: FnOnce(&mut HashMap<String, ReviewToken>) -> Result<R>,
    {
        if let Some(path) = self.file_path.clone() {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let lock_path = PathBuf::from(format!("{}.lock", path.display()));
            let _lock = acquire_lock(&lock_path)?;

            let mut tokens = load_tokens(&path)?;
            match f(&mut tokens) {
                Ok(value) => {
                    persist_tokens(&path, &tokens)?;
                    self.tokens = tokens;
                    Ok(value)
                }
                Err(e) => {
                    self.tokens = tokens;
                    Err(e)
                }
            }
        } else {
            f(&mut self.tokens)
        }
    }

    /// Issue a new review token for a thread.
    pub fn issue_token(
        &mut self,
        thread_id: &str,
        issued_by: &str,
        issued_to: &str,
    ) -> Result<ReviewToken> {
        let token = ReviewToken {
            token: Uuid::new_v4().to_string(),
            thread_id: thread_id.to_string(),
            issued_by: issued_by.to_string(),
            issued_to: issued_to.to_string(),
            issued_at: Utc::now(),
            used: false,
        };

        let result = token.clone();
        self.with_tokens_mut(|tokens| {
            tokens.insert(token.token.clone(), token);
            Ok(())
        })?;
        Ok(result)
    }

    /// Validate and consume a review token.
    /// Returns the token if valid; errors if not found or already used.
    pub fn validate_token(&mut self, token_str: &str) -> Result<ReviewToken> {
        self.with_tokens_mut(|tokens| consume_token(tokens, token_str, None))
    }

    /// Validate thread ownership and consume a token atomically.
    pub fn consume_token_for_thread(
        &mut self,
        token_str: &str,
        thread_id: &str,
    ) -> Result<ReviewToken> {
        self.with_tokens_mut(|tokens| consume_token(tokens, token_str, Some(thread_id)))
    }

    /// Look up a token without consuming it.
    pub fn get_token(&self, token_str: &str) -> Option<&ReviewToken> {
        self.tokens.get(token_str)
    }

    /// Validate that a token exists, is unused, and belongs to the thread.
    /// Re-reads from file to avoid stale-token false negatives across processes.
    pub fn check_token_for_thread(&mut self, token_str: &str, thread_id: &str) -> Result<()> {
        // Refresh from file to pick up tokens written by other processes
        if let Some(ref path) = self.file_path {
            self.tokens = load_tokens(path)?;
        }
        let token = self
            .tokens
            .get(token_str)
            .ok_or_else(|| OrchestratorError::InvalidReviewToken(token_str.to_string()))?;
        if token.thread_id != thread_id {
            return Err(OrchestratorError::InvalidReviewToken(format!(
                "token was issued for thread '{}', not '{}'",
                token.thread_id, thread_id
            )));
        }
        if token.used {
            return Err(OrchestratorError::InvalidReviewToken(format!(
                "token already used: {}",
                token_str
            )));
        }
        Ok(())
    }

    /// Get all tokens for a thread.
    pub fn tokens_for_thread(&self, thread_id: &str) -> Vec<&ReviewToken> {
        self.tokens
            .values()
            .filter(|t| t.thread_id == thread_id)
            .collect()
    }
}

fn consume_token(
    tokens: &mut HashMap<String, ReviewToken>,
    token_str: &str,
    expected_thread_id: Option<&str>,
) -> Result<ReviewToken> {
    let token = tokens
        .get_mut(token_str)
        .ok_or_else(|| OrchestratorError::InvalidReviewToken(token_str.to_string()))?;

    if let Some(thread_id) = expected_thread_id {
        if token.thread_id != thread_id {
            return Err(OrchestratorError::InvalidReviewToken(format!(
                "token was issued for thread '{}', not '{}'",
                token.thread_id, thread_id
            )));
        }
    }

    if token.used {
        return Err(OrchestratorError::InvalidReviewToken(format!(
            "token already used: {}",
            token_str
        )));
    }

    token.used = true;
    Ok(token.clone())
}

fn load_tokens(path: &Path) -> Result<HashMap<String, ReviewToken>> {
    if path.exists() {
        let content = std::fs::read_to_string(path)?;
        if content.trim().is_empty() {
            Ok(HashMap::new())
        } else {
            Ok(serde_json::from_str(&content)?)
        }
    } else {
        Ok(HashMap::new())
    }
}

fn persist_tokens(path: &Path, tokens: &HashMap<String, ReviewToken>) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp_path = PathBuf::from(format!("{}.tmp.{}", path.display(), Uuid::new_v4()));
    let content = serde_json::to_string_pretty(tokens)?;
    std::fs::write(&tmp_path, content)?;
    std::fs::rename(&tmp_path, path)?;
    Ok(())
}

#[derive(Debug)]
struct LedgerFileLock {
    path: PathBuf,
}

impl Drop for LedgerFileLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn acquire_lock(path: &Path) -> Result<LedgerFileLock> {
    let start = Instant::now();

    loop {
        match OpenOptions::new().create_new(true).write(true).open(path) {
            Ok(mut file) => {
                write_lock_metadata(&mut file)?;
                return Ok(LedgerFileLock {
                    path: path.to_path_buf(),
                });
            }
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                if maybe_reclaim_stale_lock(path)? {
                    continue;
                }
                if start.elapsed() >= LOCK_WAIT_TIMEOUT {
                    let age = lock_age(path)
                        .map(|d| format!("{}s", d.as_secs()))
                        .unwrap_or_else(|| "unknown".to_string());
                    return Err(OrchestratorError::Workflow(format!(
                        "timed out acquiring review ledger lock: {} (age={})",
                        path.display(),
                        age
                    )));
                }
                thread::sleep(LOCK_POLL_INTERVAL);
            }
            Err(e) => return Err(e.into()),
        }
    }
}

fn write_lock_metadata(file: &mut std::fs::File) -> Result<()> {
    writeln!(file, "created_unix_secs={}", now_unix_secs())?;
    writeln!(file, "pid={}", std::process::id())?;
    file.sync_all()?;
    Ok(())
}

fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn parse_created_unix_secs(content: &str) -> Option<i64> {
    content.lines().find_map(|line| {
        let value = line.strip_prefix("created_unix_secs=")?;
        value.trim().parse::<i64>().ok()
    })
}

fn lock_age(path: &Path) -> Option<Duration> {
    if let Ok(content) = std::fs::read_to_string(path) {
        if let Some(created_unix_secs) = parse_created_unix_secs(&content) {
            let now = now_unix_secs();
            if now >= created_unix_secs {
                return Some(Duration::from_secs((now - created_unix_secs) as u64));
            }
        }
    }
    std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|modified| modified.elapsed().ok())
}

fn maybe_reclaim_stale_lock(path: &Path) -> Result<bool> {
    let Some(age) = lock_age(path) else {
        return Ok(false);
    };

    if age <= LOCK_STALE_TTL {
        return Ok(false);
    }

    match std::fs::remove_file(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(true),
        Err(e) => Err(OrchestratorError::Workflow(format!(
            "failed to reclaim stale review ledger lock {} (age={}s): {}",
            path.display(),
            age.as_secs(),
            e
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_issue_token() {
        let mut ledger = ReviewLedger::new();
        let token = ledger
            .issue_token("thread-1", "operator", "focused")
            .unwrap();
        assert_eq!(token.thread_id, "thread-1");
        assert_eq!(token.issued_by, "operator");
        assert_eq!(token.issued_to, "focused");
        assert!(!token.used);
        assert!(!token.token.is_empty());
    }

    #[test]
    fn test_validate_token_success() {
        let mut ledger = ReviewLedger::new();
        let token = ledger
            .issue_token("thread-1", "operator", "focused")
            .unwrap();
        let token_str = token.token.clone();

        let validated = ledger.validate_token(&token_str).unwrap();
        assert_eq!(validated.thread_id, "thread-1");
        assert!(validated.used);
    }

    #[test]
    fn test_validate_token_not_found() {
        let mut ledger = ReviewLedger::new();
        let result = ledger.validate_token("nonexistent");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("nonexistent"));
    }

    #[test]
    fn test_validate_token_already_used() {
        let mut ledger = ReviewLedger::new();
        let token = ledger
            .issue_token("thread-1", "operator", "focused")
            .unwrap();
        let token_str = token.token.clone();

        ledger.validate_token(&token_str).unwrap();
        let result = ledger.validate_token(&token_str);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already used"));
    }

    #[test]
    fn test_get_token() {
        let mut ledger = ReviewLedger::new();
        let token = ledger
            .issue_token("thread-1", "operator", "focused")
            .unwrap();
        let token_str = token.token.clone();

        let found = ledger.get_token(&token_str);
        assert!(found.is_some());
        assert_eq!(found.unwrap().thread_id, "thread-1");

        assert!(ledger.get_token("nonexistent").is_none());
    }

    #[test]
    fn test_file_backed_ledger_persists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.json");

        let token_str;
        // Issue a token with one ledger instance
        {
            let mut ledger = ReviewLedger::with_file(path.clone()).unwrap();
            let token = ledger
                .issue_token("thread-1", "operator", "focused")
                .unwrap();
            token_str = token.token;
        }

        // Load a new ledger from the same file and validate the token
        {
            let mut ledger = ReviewLedger::with_file(path.clone()).unwrap();
            let found = ledger.get_token(&token_str);
            assert!(found.is_some());
            assert_eq!(found.unwrap().thread_id, "thread-1");

            let validated = ledger.validate_token(&token_str).unwrap();
            assert!(validated.used);
        }

        // Load again — token should be marked used
        {
            let ledger = ReviewLedger::with_file(path).unwrap();
            let found = ledger.get_token(&token_str).unwrap();
            assert!(found.used);
        }
    }

    #[test]
    fn test_file_backed_ledger_merges_stale_writers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.json");

        // Simulate two long-lived driver instances that loaded the ledger early.
        let mut ledger1 = ReviewLedger::with_file(path.clone()).unwrap();
        let mut ledger2 = ReviewLedger::with_file(path.clone()).unwrap();

        let t1 = ledger1
            .issue_token("thread-1", "operator", "focused")
            .unwrap();
        let t2 = ledger2
            .issue_token("thread-2", "operator", "spark")
            .unwrap();

        // A fresh reader should see both tokens (no lost update).
        let ledger3 = ReviewLedger::with_file(path).unwrap();
        assert!(ledger3.get_token(&t1.token).is_some());
        assert!(ledger3.get_token(&t2.token).is_some());
    }

    #[test]
    fn test_file_backed_ledger_prevents_double_consume_from_stale_readers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.json");

        let token_str = {
            let mut issuer = ReviewLedger::with_file(path.clone()).unwrap();
            issuer
                .issue_token("thread-1", "operator", "focused")
                .unwrap()
                .token
        };

        // Two stale readers loaded before either consumes.
        let mut reader_a = ReviewLedger::with_file(path.clone()).unwrap();
        let mut reader_b = ReviewLedger::with_file(path).unwrap();

        let first = reader_a.validate_token(&token_str);
        assert!(first.is_ok());

        let second = reader_b.validate_token(&token_str);
        assert!(second.is_err());
        assert!(second.unwrap_err().to_string().contains("already used"));
    }

    #[test]
    fn test_file_backed_ledger_reclaims_stale_lock() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.json");
        let lock_path = PathBuf::from(format!("{}.lock", path.display()));
        let stale_created = now_unix_secs() - (LOCK_STALE_TTL.as_secs() as i64 + 5);
        std::fs::write(
            &lock_path,
            format!("created_unix_secs={}\npid=999999\n", stale_created),
        )
        .unwrap();

        let mut ledger = ReviewLedger::with_file(path).unwrap();
        let token = ledger
            .issue_token("thread-1", "operator", "focused")
            .unwrap();
        assert!(!token.token.is_empty());
        assert!(!lock_path.exists());
    }

    #[test]
    fn test_tokens_for_thread() {
        let mut ledger = ReviewLedger::new();
        ledger
            .issue_token("thread-1", "operator", "focused")
            .unwrap();
        ledger.issue_token("thread-1", "operator", "spark").unwrap();
        ledger.issue_token("thread-2", "operator", "chill").unwrap();

        let tokens = ledger.tokens_for_thread("thread-1");
        assert_eq!(tokens.len(), 2);

        let tokens = ledger.tokens_for_thread("thread-2");
        assert_eq!(tokens.len(), 1);

        let tokens = ledger.tokens_for_thread("nonexistent");
        assert!(tokens.is_empty());
    }
}
