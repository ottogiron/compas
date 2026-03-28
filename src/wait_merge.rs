//! Blocking wait for merge operation completion.
//!
//! Polls the `merge_operations` table at 200ms intervals until the operation
//! reaches a terminal status (`completed`, `failed`, `cancelled`) or timeout.
//! Used by the `compas wait merge` CLI subcommand.

use std::time::Duration;

use crate::store::{MergeOperation, MergeOperationStatus, Store};

const POLL_INTERVAL_MS: u64 = 200;

/// Parameters for a wait-merge operation.
pub struct WaitMergeRequest {
    /// The merge operation ULID to wait on.
    pub op_id: String,
    /// Maximum time to wait before returning a timeout outcome.
    pub timeout: Duration,
}

/// Outcome of a wait-merge polling loop.
#[derive(Debug)]
pub enum WaitMergeOutcome {
    /// The operation reached a terminal status.
    Found(Box<MergeOperation>),
    /// Timed out without reaching a terminal status.
    Timeout {
        op_id: String,
        timeout_secs: u64,
        /// Last observed status from the most recent poll before timeout.
        last_status: Option<String>,
    },
}

/// Run the wait-merge polling loop against the store.
///
/// Returns `Ok(WaitMergeOutcome)` for normal found/timeout results,
/// or `Err(String)` for unrecoverable errors (DB failure, op_id not found).
pub async fn wait_for_merge_op(
    store: &Store,
    req: &WaitMergeRequest,
) -> Result<WaitMergeOutcome, String> {
    let deadline = tokio::time::Instant::now() + req.timeout;

    loop {
        let op = store
            .get_merge_op(&req.op_id)
            .await
            .map_err(|e| format!("wait_merge query failed: {}", e))?
            .ok_or_else(|| format!("merge operation not found: {}", req.op_id))?;

        let last_status = op.status.clone();
        let is_terminal = op
            .status
            .parse::<MergeOperationStatus>()
            .map(|s| s.is_terminal())
            .unwrap_or(false);
        if is_terminal {
            return Ok(WaitMergeOutcome::Found(Box::new(op)));
        }

        if tokio::time::Instant::now() >= deadline {
            return Ok(WaitMergeOutcome::Timeout {
                op_id: req.op_id.clone(),
                timeout_secs: req.timeout.as_secs(),
                last_status: Some(last_status),
            });
        }

        tokio::time::sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{MergeOperation, Store};
    use sqlx::SqlitePool;

    async fn test_store() -> Store {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        let store = Store::new(pool);
        store.setup().await.unwrap();
        store
    }

    fn make_merge_op(id: &str, thread_id: &str, status: &str) -> MergeOperation {
        MergeOperation {
            id: id.to_string(),
            thread_id: thread_id.to_string(),
            source_branch: format!("compas/{}", thread_id),
            target_branch: "main".to_string(),
            merge_strategy: "merge".to_string(),
            requested_by: "test".to_string(),
            status: status.to_string(),
            push_requested: false,
            queued_at: 0,
            claimed_at: None,
            started_at: None,
            finished_at: None,
            duration_ms: None,
            result_summary: None,
            error_detail: None,
            conflict_files: None,
            commit_message: None,
        }
    }

    #[tokio::test]
    async fn test_wait_merge_finds_completed_op() {
        let store = test_store().await;
        let op = make_merge_op(
            "01ABC000000000000000000000",
            "01THR000000000000000000000",
            "completed",
        );
        store.insert_merge_op(&op).await.unwrap();

        let req = WaitMergeRequest {
            op_id: "01ABC000000000000000000000".to_string(),
            timeout: Duration::from_secs(5),
        };

        let outcome = wait_for_merge_op(&store, &req).await.unwrap();
        match outcome {
            WaitMergeOutcome::Found(found) => {
                assert_eq!(found.id, "01ABC000000000000000000000");
                assert_eq!(found.status, "completed");
            }
            WaitMergeOutcome::Timeout { .. } => panic!("expected Found, got Timeout"),
        }
    }

    #[tokio::test]
    async fn test_wait_merge_times_out() {
        let store = test_store().await;
        let op = make_merge_op(
            "01DEF000000000000000000000",
            "01THR100000000000000000000",
            "queued",
        );
        store.insert_merge_op(&op).await.unwrap();

        let req = WaitMergeRequest {
            op_id: "01DEF000000000000000000000".to_string(),
            timeout: Duration::from_millis(300),
        };

        let outcome = wait_for_merge_op(&store, &req).await.unwrap();
        match outcome {
            WaitMergeOutcome::Timeout {
                op_id, last_status, ..
            } => {
                assert_eq!(op_id, "01DEF000000000000000000000");
                assert_eq!(last_status.as_deref(), Some("queued"));
            }
            WaitMergeOutcome::Found(_) => panic!("expected Timeout, got Found"),
        }
    }

    #[tokio::test]
    async fn test_wait_merge_reports_conflict_files() {
        let store = test_store().await;
        let mut op = make_merge_op(
            "01GHI000000000000000000000",
            "01THR200000000000000000000",
            "failed",
        );
        op.error_detail = Some("Merge conflict detected".to_string());
        op.conflict_files = Some("src/store/mod.rs,src/config/types.rs".to_string());
        store.insert_merge_op(&op).await.unwrap();

        let req = WaitMergeRequest {
            op_id: "01GHI000000000000000000000".to_string(),
            timeout: Duration::from_secs(5),
        };

        let outcome = wait_for_merge_op(&store, &req).await.unwrap();
        match outcome {
            WaitMergeOutcome::Found(found) => {
                assert_eq!(found.status, "failed");
                assert_eq!(
                    found.conflict_files.as_deref(),
                    Some("src/store/mod.rs,src/config/types.rs")
                );
                assert_eq!(
                    found.error_detail.as_deref(),
                    Some("Merge conflict detected")
                );
            }
            WaitMergeOutcome::Timeout { .. } => panic!("expected Found, got Timeout"),
        }
    }

    #[tokio::test]
    async fn test_wait_merge_unknown_op_id_returns_error() {
        let store = test_store().await;

        let req = WaitMergeRequest {
            op_id: "nonexistent-op-id".to_string(),
            timeout: Duration::from_secs(120),
        };

        let result = wait_for_merge_op(&store, &req).await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .contains("merge operation not found: nonexistent-op-id"));
    }
}
