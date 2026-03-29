//! Worker singleton guard — prevents multiple worker processes from running
//! simultaneously via an exclusive lockfile + heartbeat/PID liveness check.

use crate::error::OrchestratorError;
use crate::store::Store;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};

/// RAII guard that holds an exclusive `flock` on `{state_dir}/worker.lock`.
///
/// The lock is held for the lifetime of this struct. When dropped, the file
/// descriptor is closed and the lock is released.
#[derive(Debug)]
pub struct WorkerLockGuard {
    /// Kept alive so the flock persists for the process lifetime.
    _lock_file: File,
    _lock_path: PathBuf,
}

/// Heartbeat recency threshold for the singleton guard (seconds).
pub const WORKER_HEARTBEAT_MAX_AGE_SECS: i64 = 30;

/// Attempt to acquire an exclusive worker lock.
///
/// 1. Opens/creates `{state_dir}/worker.lock`
/// 2. Tries `flock(LOCK_EX | LOCK_NB)` — fails immediately if already locked
/// 3. Checks `is_worker_alive()` via heartbeat freshness + PID liveness
/// 4. Returns a guard struct that holds the `File` (lock persists for process lifetime)
///
/// On failure, returns `OrchestratorError::DaemonLockHeld` with diagnostic info.
pub async fn acquire_worker_lock(
    state_dir: &Path,
    store: &Store,
) -> Result<WorkerLockGuard, OrchestratorError> {
    fs::create_dir_all(state_dir).map_err(OrchestratorError::Io)?;

    let lock_path = state_dir.join("worker.lock");
    let lock_file = {
        let mut opts = OpenOptions::new();
        opts.create(true).write(true).truncate(false);
        // SEC-4: restrict lock file to owner-only on Unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        opts.open(&lock_path).map_err(OrchestratorError::Io)?
    };

    // Try non-blocking exclusive lock.
    // On non-unix platforms, flock is unavailable — the guard falls through to the
    // heartbeat+PID check below, which provides weaker (TOCTOU-prone) protection.
    #[cfg(unix)]
    {
        use libc::{flock, LOCK_EX, LOCK_NB};
        use std::os::unix::io::AsRawFd;
        let ret = unsafe { flock(lock_file.as_raw_fd(), LOCK_EX | LOCK_NB) };
        if ret != 0 {
            // Lock is held by another process. Query heartbeat for diagnostics.
            let diag = fetch_worker_diagnostics(store).await;
            return Err(OrchestratorError::DaemonLockHeld {
                worker_id: diag.0,
                pid: diag.1,
                heartbeat_age_secs: diag.2,
            });
        }
    }

    // Lock acquired. Double-check via heartbeat + PID that no worker is alive.
    // This catches the edge case where a previous worker died without releasing
    // the flock (shouldn't happen with flock, but defense-in-depth).
    let heartbeat = store.latest_heartbeat().await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "heartbeat query failed during worker lock guard");
        None
    });

    if is_worker_alive(&heartbeat, WORKER_HEARTBEAT_MAX_AGE_SECS) {
        let diag = diagnostics_from_heartbeat(&heartbeat);
        return Err(OrchestratorError::DaemonLockHeld {
            worker_id: diag.0,
            pid: diag.1,
            heartbeat_age_secs: diag.2,
        });
    }

    Ok(WorkerLockGuard {
        _lock_file: lock_file,
        _lock_path: lock_path,
    })
}

/// Check whether a worker is actually running.
///
/// Two checks are performed (in this order):
/// 1. **Heartbeat is fresh**: `last_beat_at` is within `max_age_secs` of now
///    (tolerates up to 5s of forward clock skew).
/// 2. **Process exists**: extract the PID from `worker_id` (format: `worker-<pid>`)
///    and verify the process is alive via `kill(pid, 0)`.
///
/// Both must be true.
pub fn is_worker_alive(
    heartbeat: &Option<(String, i64, i64, Option<String>)>,
    max_age_secs: i64,
) -> bool {
    match heartbeat {
        Some((worker_id, last_beat_at, _, _)) => {
            let now_unix = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64;

            // Check 1: heartbeat is recent
            let heartbeat_fresh = *last_beat_at >= now_unix.saturating_sub(max_age_secs)
                && *last_beat_at <= now_unix + 5;

            if !heartbeat_fresh {
                return false;
            }

            // Check 2: process is actually alive
            // worker_id format: "worker-<pid>"
            let pid_alive = worker_id
                .strip_prefix("worker-")
                .and_then(|pid_str| pid_str.parse::<u32>().ok())
                .and_then(|pid| i32::try_from(pid).ok())
                .map(|pid| {
                    #[cfg(unix)]
                    {
                        let ret = unsafe { libc::kill(pid, 0) };
                        if ret == 0 {
                            true
                        } else {
                            let err = std::io::Error::last_os_error();
                            err.raw_os_error() != Some(libc::ESRCH)
                        }
                    }
                    #[cfg(not(unix))]
                    {
                        let _ = pid;
                        true // Can't check on non-unix, trust the heartbeat
                    }
                })
                .unwrap_or(false);

            if !pid_alive {
                tracing::info!(
                    worker_id = %worker_id,
                    "stale heartbeat: process no longer exists, clearing"
                );
            }

            pid_alive
        }
        None => false,
    }
}

/// Extract diagnostics from a heartbeat for error messages.
fn diagnostics_from_heartbeat(
    heartbeat: &Option<(String, i64, i64, Option<String>)>,
) -> (String, u32, i64) {
    match heartbeat {
        Some((worker_id, last_beat_at, _, _)) => {
            let now_unix = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64;
            let pid = worker_id
                .strip_prefix("worker-")
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(0);
            let age = now_unix.saturating_sub(*last_beat_at);
            (worker_id.clone(), pid, age)
        }
        None => ("unknown".to_string(), 0, 0),
    }
}

/// Fetch heartbeat diagnostics from the store.
async fn fetch_worker_diagnostics(store: &Store) -> (String, u32, i64) {
    let heartbeat = store.latest_heartbeat().await.unwrap_or(None);
    diagnostics_from_heartbeat(&heartbeat)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    fn now_unix() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }

    fn heartbeat_at(ts: i64) -> Option<(String, i64, i64, Option<String>)> {
        // PID 1 (init/launchd) is always alive on Unix
        heartbeat_at_pid(ts, 1)
    }

    fn heartbeat_at_pid(ts: i64, pid: u32) -> Option<(String, i64, i64, Option<String>)> {
        Some((
            format!("worker-{}", pid),
            ts,
            ts - 100,
            Some("0.2.0".to_string()),
        ))
    }

    #[test]
    fn test_guard_is_worker_alive_recent_heartbeat_returns_true() {
        let hb = heartbeat_at(now_unix() - 5);
        assert!(is_worker_alive(&hb, WORKER_HEARTBEAT_MAX_AGE_SECS));
    }

    #[test]
    fn test_guard_is_worker_alive_stale_heartbeat_returns_false() {
        let hb = heartbeat_at(now_unix() - 60);
        assert!(!is_worker_alive(&hb, WORKER_HEARTBEAT_MAX_AGE_SECS));
    }

    #[test]
    fn test_guard_is_worker_alive_no_heartbeat_returns_false() {
        assert!(!is_worker_alive(&None, WORKER_HEARTBEAT_MAX_AGE_SECS));
    }

    #[test]
    fn test_guard_is_worker_alive_at_exact_boundary_returns_true() {
        let hb = heartbeat_at(now_unix() - WORKER_HEARTBEAT_MAX_AGE_SECS);
        assert!(is_worker_alive(&hb, WORKER_HEARTBEAT_MAX_AGE_SECS));
    }

    #[test]
    fn test_guard_is_worker_alive_just_past_boundary_returns_false() {
        let hb = heartbeat_at(now_unix() - WORKER_HEARTBEAT_MAX_AGE_SECS - 1);
        assert!(!is_worker_alive(&hb, WORKER_HEARTBEAT_MAX_AGE_SECS));
    }

    #[test]
    fn test_guard_is_worker_alive_future_within_tolerance_returns_true() {
        let hb = heartbeat_at(now_unix() + 3);
        assert!(is_worker_alive(&hb, WORKER_HEARTBEAT_MAX_AGE_SECS));
    }

    #[test]
    fn test_guard_is_worker_alive_future_beyond_tolerance_returns_false() {
        let hb = heartbeat_at(now_unix() + 10);
        assert!(!is_worker_alive(&hb, WORKER_HEARTBEAT_MAX_AGE_SECS));
    }

    #[test]
    fn test_guard_is_worker_alive_fresh_heartbeat_but_dead_pid() {
        let hb = heartbeat_at_pid(now_unix() - 5, 99_999_999);
        assert!(!is_worker_alive(&hb, WORKER_HEARTBEAT_MAX_AGE_SECS));
    }

    #[test]
    fn test_guard_is_worker_alive_unparseable_worker_id() {
        let hb = Some((
            "bad-format".to_string(),
            now_unix() - 5,
            now_unix() - 105,
            Some("0.2.0".to_string()),
        ));
        assert!(!is_worker_alive(&hb, WORKER_HEARTBEAT_MAX_AGE_SECS));
    }

    #[test]
    fn test_guard_diagnostics_from_heartbeat_extracts_pid() {
        let hb = heartbeat_at_pid(now_unix() - 10, 42);
        let (worker_id, pid, age) = diagnostics_from_heartbeat(&hb);
        assert_eq!(worker_id, "worker-42");
        assert_eq!(pid, 42);
        assert!((10..=12).contains(&age)); // allow 2s of test execution
    }

    #[test]
    fn test_guard_diagnostics_from_no_heartbeat() {
        let (worker_id, pid, age) = diagnostics_from_heartbeat(&None);
        assert_eq!(worker_id, "unknown");
        assert_eq!(pid, 0);
        assert_eq!(age, 0);
    }

    #[test]
    fn test_guard_singleton_rejection_via_lockfile() {
        // Acquire a lock on a temp file, then verify a second attempt fails.
        let dir = tempfile::tempdir().unwrap();
        let lock_path = dir.path().join("worker.lock");

        let lock1 = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .unwrap();

        #[cfg(unix)]
        {
            use libc::{flock, LOCK_EX, LOCK_NB};
            use std::os::unix::io::AsRawFd;

            // First lock succeeds
            let ret = unsafe { flock(lock1.as_raw_fd(), LOCK_EX | LOCK_NB) };
            assert_eq!(ret, 0, "first lock should succeed");

            // Second lock on the same file fails
            let lock2 = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(false)
                .open(&lock_path)
                .unwrap();

            let ret = unsafe { flock(lock2.as_raw_fd(), LOCK_EX | LOCK_NB) };
            assert_ne!(ret, 0, "second lock should fail (EWOULDBLOCK)");
        }

        // Keep lock1 alive for the test duration
        drop(lock1);
    }

    async fn test_store() -> Store {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        let store = Store::new(pool);
        store.setup().await.unwrap();
        store
    }

    #[tokio::test]
    async fn test_acquire_worker_lock_succeeds_when_no_worker() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store().await;
        let guard = acquire_worker_lock(dir.path(), &store).await;
        assert!(guard.is_ok(), "first lock should succeed");
    }

    #[tokio::test]
    async fn test_acquire_worker_lock_second_call_returns_daemon_lock_held() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store().await;

        // First acquire succeeds (no heartbeat yet).
        let _guard = acquire_worker_lock(dir.path(), &store).await.unwrap();

        // Write a heartbeat *after* lock is held (simulates a running worker).
        let pid = std::process::id();
        let worker_id = format!("worker-{}", pid);
        store.write_heartbeat(&worker_id, "0.2.0").await.unwrap();

        // Second acquire should fail with DaemonLockHeld.
        let err = acquire_worker_lock(dir.path(), &store).await.unwrap_err();
        match &err {
            OrchestratorError::DaemonLockHeld {
                worker_id: wid,
                pid: p,
                heartbeat_age_secs: age,
            } => {
                assert_eq!(wid, &worker_id);
                assert_eq!(*p, pid);
                // Heartbeat was just written, age should be small.
                assert!(*age <= 5, "heartbeat age should be recent, got {}", age);
            }
            other => panic!("expected DaemonLockHeld, got: {:?}", other),
        }
        // Verify the error message includes the PID (not "kill 0").
        let msg = err.to_string();
        assert!(
            msg.contains(&format!("kill {}", pid)),
            "error message should include kill hint with actual PID: {}",
            msg
        );
    }

    #[tokio::test]
    async fn test_acquire_worker_lock_no_heartbeat_shows_safe_message() {
        let dir = tempfile::tempdir().unwrap();
        let store = test_store().await;

        // No heartbeat written — diagnostics will have pid=0.
        // First acquire succeeds.
        let _guard = acquire_worker_lock(dir.path(), &store).await.unwrap();

        // Second acquire fails.
        let err = acquire_worker_lock(dir.path(), &store).await.unwrap_err();
        match &err {
            OrchestratorError::DaemonLockHeld { pid, .. } => {
                assert_eq!(*pid, 0);
            }
            other => panic!("expected DaemonLockHeld, got: {:?}", other),
        }
        // Verify the error message does NOT say "kill 0".
        let msg = err.to_string();
        assert!(
            !msg.contains("kill 0"),
            "error message must not contain 'kill 0': {}",
            msg
        );
        assert!(
            msg.contains("pgrep"),
            "error message should suggest pgrep: {}",
            msg
        );
    }
}
