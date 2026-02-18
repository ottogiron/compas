use super::AuditEvent;
use crate::error::Result;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Append-only JSON-line audit logger with size-based rotation (ORCHV3-11).
#[derive(Debug, Clone)]
pub struct AuditLogger {
    path: PathBuf,
    max_file_bytes: usize,
    max_archive_files: usize,
}

impl AuditLogger {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            max_file_bytes: 10_485_760, // 10MB default
            max_archive_files: 10,
        }
    }

    pub fn with_rotation(path: PathBuf, max_file_bytes: usize, max_archive_files: usize) -> Self {
        Self {
            path,
            max_file_bytes,
            max_archive_files,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append an audit event as a JSON line. Rotates if file exceeds size limit.
    pub fn log(&self, event: &AuditEvent) -> Result<()> {
        self.maybe_rotate();
        let line = serde_json::to_string(event)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        writeln!(file, "{}", line)?;
        Ok(())
    }

    /// Check file size and rotate if necessary.
    fn maybe_rotate(&self) {
        let metadata = match std::fs::metadata(&self.path) {
            Ok(m) => m,
            Err(_) => return,
        };
        if (metadata.len() as usize) <= self.max_file_bytes {
            return;
        }
        let archive_name = format!(
            "audit-archive-{}.jsonl",
            chrono::Utc::now().format("%Y%m%dT%H%M%SZ")
        );
        let archive_dir = self.path.parent().unwrap_or(Path::new("."));
        let archive_path = archive_dir.join(archive_name);
        if let Err(e) = std::fs::rename(&self.path, &archive_path) {
            tracing::warn!("audit log rotation failed: {}", e);
            return;
        }
        self.cleanup_archives(archive_dir);
    }

    /// Remove oldest archive files if count exceeds the limit.
    fn cleanup_archives(&self, dir: &Path) {
        let mut archives: Vec<PathBuf> = match std::fs::read_dir(dir) {
            Ok(entries) => entries
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .map(|n| n.starts_with("audit-archive-") && n.ends_with(".jsonl"))
                        .unwrap_or(false)
                })
                .collect(),
            Err(_) => return,
        };
        if archives.len() <= self.max_archive_files {
            return;
        }
        archives.sort();
        let to_remove = archives.len() - self.max_archive_files;
        for path in archives.iter().take(to_remove) {
            if let Err(e) = std::fs::remove_file(path) {
                tracing::warn!(
                    "failed to remove old audit archive {}: {}",
                    path.display(),
                    e
                );
            }
        }
    }

    /// Read the last N events from the log.
    pub fn tail(&self, n: usize) -> Result<Vec<AuditEvent>> {
        let content = match std::fs::read_to_string(&self.path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(e.into()),
        };
        let events: Vec<AuditEvent> = content
            .lines()
            .rev()
            .take(n)
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        Ok(events)
    }

    /// Read all events in order.
    pub fn all(&self) -> Result<Vec<AuditEvent>> {
        let content = match std::fs::read_to_string(&self.path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(e.into()),
        };

        Ok(content
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn test_audit_logger_write_and_read() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("audit.jsonl");
        let logger = AuditLogger::new(log_path);

        let event = AuditEvent::Dispatch {
            from: "operator".into(),
            to: "focused".into(),
            thread_id: "test-thread".into(),
            batch: Some("B1".into()),
            timestamp: Utc::now(),
        };

        logger.log(&event).unwrap();
        logger.log(&event).unwrap();

        let events = logger.tail(10).unwrap();
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn test_audit_logger_tail_empty() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("nonexistent.jsonl");
        let logger = AuditLogger::new(log_path);

        let events = logger.tail(10).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn test_audit_logger_rotation_on_size() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("audit.jsonl");
        let logger = AuditLogger::with_rotation(log_path.clone(), 100, 10);

        let event = AuditEvent::Dispatch {
            from: "operator".into(),
            to: "focused".into(),
            thread_id: "test-thread".into(),
            batch: Some("B1".into()),
            timestamp: Utc::now(),
        };

        for _ in 0..5 {
            logger.log(&event).unwrap();
        }

        let archives: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .map(|n| n.starts_with("audit-archive-"))
                    .unwrap_or(false)
            })
            .collect();
        assert!(
            !archives.is_empty(),
            "expected at least one archive file after rotation"
        );
    }

    #[test]
    fn test_audit_logger_archive_pruning() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("audit.jsonl");
        let logger = AuditLogger::with_rotation(log_path.clone(), 50, 2);

        let event = AuditEvent::Dispatch {
            from: "operator".into(),
            to: "focused".into(),
            thread_id: "test-thread".into(),
            batch: Some("B1".into()),
            timestamp: Utc::now(),
        };

        for _ in 0..30 {
            logger.log(&event).unwrap();
        }

        let archives: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .map(|n| n.starts_with("audit-archive-"))
                    .unwrap_or(false)
            })
            .collect();
        assert!(
            archives.len() <= 2,
            "expected at most 2 archive files, got {}",
            archives.len()
        );
    }
}
