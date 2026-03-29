//! Action type system, state machine, and async execution engine for dashboard actions.
//!
//! Defines the context-sensitive actions operators can perform on selected items
//! in the activity view (threads, batches, merge ops). Rendering and app.rs
//! integration are separate workstreams.

use std::sync::Arc;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;

use crate::config::types::OrchestratorConfig;
use crate::dashboard::views::activity::OpsSelectable;
use crate::lifecycle::{CloseStatus, LifecycleService};
use crate::store::{MergeOperation, Store, ThreadStatusView};

// ── State machine ────────────────────────────────────────────────────────

/// Top-level action-menu state, driven by key events from the dashboard app.
pub enum ActionState {
    /// No menu visible.
    Idle,
    /// Action menu is open for a selected target.
    Menu {
        target: ActionTarget,
        actions: Vec<ActionEntry>,
    },
    /// Waiting for y/n confirmation before executing.
    Confirming {
        action: PendingAction,
        description: String,
    },
    /// Transient feedback shown after execution.
    Feedback {
        message: String,
        is_error: bool,
        shown_at: Instant,
    },
}

// ── Types ────────────────────────────────────────────────────────────────

/// A single entry in the action menu.
pub struct ActionEntry {
    pub key: char,
    pub label: &'static str,
}

/// The dashboard item an action targets.
pub enum ActionTarget {
    Thread {
        thread_id: String,
        status: String,
        summary: Option<String>,
        has_worktree: bool,
    },
    Batch {
        batch_id: String,
    },
    MergeOp {
        op_id: String,
        status: String,
    },
}

/// A concrete action ready for execution (possibly after confirmation).
pub enum PendingAction {
    AbandonThread {
        thread_id: String,
    },
    AbandonBatch {
        batch_id: String,
    },
    CloseThread {
        thread_id: String,
        status: CloseStatus,
    },
    ReopenThread {
        thread_id: String,
    },
    CancelMerge {
        op_id: String,
    },
    QueueMerge {
        thread_id: String,
    },
}

/// Result of executing an action.
pub struct ActionResult {
    pub message: String,
    pub is_error: bool,
}

// ── Helpers ──────────────────────────────────────────────────────────────

/// Truncate an ID to `max_len` chars, appending `…` if truncated.
/// Uses char boundaries to avoid panicking on multi-byte input.
fn truncate_id(id: &str, max_len: usize) -> String {
    if id.chars().count() <= max_len {
        id.to_string()
    } else {
        let end = id
            .char_indices()
            .nth(max_len)
            .map(|(i, _)| i)
            .unwrap_or(id.len());
        format!("{}…", &id[..end])
    }
}

fn is_terminal_status(status: &str) -> bool {
    matches!(
        status.to_lowercase().as_str(),
        "completed" | "failed" | "abandoned"
    )
}

// ── Core functions ───────────────────────────────────────────────────────

/// Returns the available actions for a given target, respecting its state.
pub fn available_actions(target: &ActionTarget) -> Vec<ActionEntry> {
    match target {
        ActionTarget::Thread {
            status,
            has_worktree,
            ..
        } => {
            if is_terminal_status(status) {
                vec![ActionEntry {
                    key: 'r',
                    label: "Reopen",
                }]
            } else {
                let mut actions = vec![
                    ActionEntry {
                        key: 'a',
                        label: "Abandon",
                    },
                    ActionEntry {
                        key: 'c',
                        label: "Close completed",
                    },
                    ActionEntry {
                        key: 'f',
                        label: "Close failed",
                    },
                ];
                if *has_worktree {
                    actions.push(ActionEntry {
                        key: 'm',
                        label: "Queue merge",
                    });
                }
                actions
            }
        }
        ActionTarget::Batch { .. } => {
            vec![ActionEntry {
                key: 'a',
                label: "Abandon batch",
            }]
        }
        ActionTarget::MergeOp { status, .. } => {
            if status == "queued" {
                vec![ActionEntry {
                    key: 'x',
                    label: "Cancel merge",
                }]
            } else {
                vec![]
            }
        }
    }
}

/// Maps a key press to a `PendingAction` within the context of a target.
/// Returns `None` if the key doesn't match any available action.
pub fn action_for_key(key: char, target: &ActionTarget) -> Option<PendingAction> {
    match target {
        ActionTarget::Thread {
            thread_id,
            status,
            has_worktree,
            ..
        } => {
            if is_terminal_status(status) {
                match key {
                    'r' => Some(PendingAction::ReopenThread {
                        thread_id: thread_id.clone(),
                    }),
                    _ => None,
                }
            } else {
                match key {
                    'a' => Some(PendingAction::AbandonThread {
                        thread_id: thread_id.clone(),
                    }),
                    'c' => Some(PendingAction::CloseThread {
                        thread_id: thread_id.clone(),
                        status: CloseStatus::Completed,
                    }),
                    'f' => Some(PendingAction::CloseThread {
                        thread_id: thread_id.clone(),
                        status: CloseStatus::Failed,
                    }),
                    'm' if *has_worktree => Some(PendingAction::QueueMerge {
                        thread_id: thread_id.clone(),
                    }),
                    _ => None,
                }
            }
        }
        ActionTarget::Batch { batch_id } => match key {
            'a' => Some(PendingAction::AbandonBatch {
                batch_id: batch_id.clone(),
            }),
            _ => None,
        },
        ActionTarget::MergeOp { op_id, status } => {
            if status == "queued" {
                match key {
                    'x' => Some(PendingAction::CancelMerge {
                        op_id: op_id.clone(),
                    }),
                    _ => None,
                }
            } else {
                None
            }
        }
    }
}

/// Returns `true` if the action requires y/n confirmation before execution.
/// Only `ReopenThread` skips confirmation.
pub fn needs_confirmation(action: &PendingAction) -> bool {
    !matches!(action, PendingAction::ReopenThread { .. })
}

/// Generates a human-readable confirmation prompt for the action.
/// Thread/op IDs are truncated to ~12 chars with ellipsis.
pub fn confirmation_prompt(action: &PendingAction) -> String {
    match action {
        PendingAction::AbandonThread { thread_id } => {
            format!(
                "Abandon thread {}? [y]es / [n]o",
                truncate_id(thread_id, 12)
            )
        }
        PendingAction::AbandonBatch { batch_id } => {
            format!("Abandon batch {}? [y]es / [n]o", truncate_id(batch_id, 12))
        }
        PendingAction::CloseThread { thread_id, status } => {
            let status_label = match status {
                CloseStatus::Completed => "completed",
                CloseStatus::Failed => "failed",
            };
            format!(
                "Close thread {} as {}? [y]es / [n]o",
                truncate_id(thread_id, 12),
                status_label
            )
        }
        // Unreachable in normal flow (needs_confirmation returns false for ReopenThread),
        // but kept for exhaustiveness and direct callers of confirmation_prompt.
        PendingAction::ReopenThread { thread_id } => {
            format!("Reopen thread {}? [y]es / [n]o", truncate_id(thread_id, 12))
        }
        PendingAction::CancelMerge { op_id } => {
            format!("Cancel merge op {}? [y]es / [n]o", truncate_id(op_id, 12))
        }
        PendingAction::QueueMerge { thread_id } => {
            format!(
                "Queue merge for thread {}? [y]es / [n]o",
                truncate_id(thread_id, 12)
            )
        }
    }
}

/// Resolves the currently selected dashboard item into an `ActionTarget`.
///
/// Returns `None` if the index is out of bounds or the item can't be resolved.
pub async fn resolve_action_target(
    store: &Store,
    selectable: &OpsSelectable,
    rows: &[ThreadStatusView],
    merge_ops: &[MergeOperation],
) -> Option<ActionTarget> {
    match selectable {
        OpsSelectable::Thread(idx) => {
            let row = rows.get(*idx)?;
            let has_worktree = store
                .get_thread_worktree_info(&row.thread_id)
                .await
                .ok()
                .flatten()
                .is_some();
            Some(ActionTarget::Thread {
                thread_id: row.thread_id.clone(),
                status: row.thread_status.clone(),
                summary: row.summary.clone(),
                has_worktree,
            })
        }
        OpsSelectable::Batch(id) => Some(ActionTarget::Batch {
            batch_id: id.clone(),
        }),
        OpsSelectable::MergeOp(id) => {
            let op = merge_ops.iter().find(|o| o.id == *id)?;
            Some(ActionTarget::MergeOp {
                op_id: op.id.clone(),
                status: op.status.clone(),
            })
        }
    }
}

/// Executes a `PendingAction` against the store/lifecycle services.
///
/// Each call is wrapped in a 5-second timeout. On timeout the result reports
/// an error. Callers should display the returned `ActionResult` as transient
/// feedback.
pub async fn execute_action(
    action: PendingAction,
    store: &Store,
    _config: &Arc<ArcSwap<OrchestratorConfig>>,
) -> ActionResult {
    let timeout = Duration::from_secs(5);

    match action {
        PendingAction::AbandonThread { thread_id } => {
            let lifecycle = LifecycleService::new(store.clone());
            match tokio::time::timeout(timeout, lifecycle.abandon(&thread_id)).await {
                Ok(Ok(outcome)) => ActionResult {
                    message: format!(
                        "Thread abandoned ({} cancelled, {} killed)",
                        outcome.executions_cancelled, outcome.processes_killed
                    ),
                    is_error: false,
                },
                Ok(Err(e)) => ActionResult {
                    message: e.to_string(),
                    is_error: true,
                },
                Err(_) => ActionResult {
                    message: "Action timed out".into(),
                    is_error: true,
                },
            }
        }

        PendingAction::AbandonBatch { batch_id } => {
            // TODO: uncomment when WS-2 lands (LifecycleService::abandon_batch)
            // For now, replicate the batch abandon logic from mcp/lifecycle.rs inline.
            let lifecycle = LifecycleService::new(store.clone());
            match tokio::time::timeout(timeout, async {
                let threads = store
                    .list_threads(Some(&batch_id), None, 500)
                    .await
                    .map_err(|e| format!("failed to list batch threads: {e}"))?;
                if threads.is_empty() {
                    return Err(format!("No threads found for batch '{batch_id}'"));
                }
                let mut threads_abandoned: u64 = 0;
                let mut total_cancelled: u64 = 0;
                let mut errors: Vec<String> = Vec::new();
                for thread in &threads {
                    let status: crate::store::ThreadStatus = thread
                        .status
                        .parse()
                        .unwrap_or(crate::store::ThreadStatus::Active);
                    if status.is_terminal() {
                        continue;
                    }
                    match lifecycle.abandon(&thread.thread_id).await {
                        Ok(outcome) => {
                            threads_abandoned += 1;
                            total_cancelled += outcome.executions_cancelled;
                        }
                        Err(e) => {
                            errors.push(format!("{}: {e}", thread.thread_id));
                        }
                    }
                }
                Ok((threads_abandoned, total_cancelled, errors))
            })
            .await
            {
                Ok(Ok((threads, execs, errors))) => {
                    if errors.is_empty() {
                        ActionResult {
                            message: format!(
                                "Batch abandoned ({threads} threads, {execs} executions cancelled)"
                            ),
                            is_error: false,
                        }
                    } else {
                        ActionResult {
                            message: format!(
                                "Batch partially abandoned ({threads} threads, {execs} cancelled); errors: {}",
                                errors.join(", ")
                            ),
                            is_error: true,
                        }
                    }
                }
                Ok(Err(e)) => ActionResult {
                    message: e,
                    is_error: true,
                },
                Err(_) => ActionResult {
                    message: "Action timed out".into(),
                    is_error: true,
                },
            }
        }

        PendingAction::CloseThread { thread_id, status } => {
            let lifecycle = LifecycleService::new(store.clone());
            let status_label = match &status {
                CloseStatus::Completed => "completed",
                CloseStatus::Failed => "failed",
            };
            match tokio::time::timeout(
                timeout,
                lifecycle.close(&thread_id, "dashboard", status, None),
            )
            .await
            {
                Ok(Ok(_)) => ActionResult {
                    message: format!("Thread closed as {status_label}"),
                    is_error: false,
                },
                Ok(Err(e)) => ActionResult {
                    message: e.to_string(),
                    is_error: true,
                },
                Err(_) => ActionResult {
                    message: "Action timed out".into(),
                    is_error: true,
                },
            }
        }

        PendingAction::ReopenThread { thread_id } => {
            let lifecycle = LifecycleService::new(store.clone());
            match tokio::time::timeout(timeout, lifecycle.reopen(&thread_id)).await {
                Ok(Ok(outcome)) => ActionResult {
                    message: format!(
                        "Thread reopened ({} → {})",
                        outcome.previous_status, outcome.new_status
                    ),
                    is_error: false,
                },
                Ok(Err(e)) => ActionResult {
                    message: e.to_string(),
                    is_error: true,
                },
                Err(_) => ActionResult {
                    message: "Action timed out".into(),
                    is_error: true,
                },
            }
        }

        PendingAction::CancelMerge { op_id } => {
            match tokio::time::timeout(timeout, store.cancel_merge_op(&op_id)).await {
                Ok(Ok(true)) => ActionResult {
                    message: "Merge cancelled".into(),
                    is_error: false,
                },
                Ok(Ok(false)) => ActionResult {
                    message: "Cannot cancel: merge is no longer queued".into(),
                    is_error: true,
                },
                Ok(Err(e)) => ActionResult {
                    message: e,
                    is_error: true,
                },
                Err(_) => ActionResult {
                    message: "Action timed out".into(),
                    is_error: true,
                },
            }
        }

        PendingAction::QueueMerge { thread_id: _ } => {
            // TODO: uncomment when WS-3 lands (MergeService::queue_merge)
            ActionResult {
                message: "Not yet implemented".into(),
                is_error: true,
            }
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn thread_target(status: &str, has_worktree: bool) -> ActionTarget {
        ActionTarget::Thread {
            thread_id: "01KMVTEST1234567".into(),
            status: status.into(),
            summary: Some("test thread".into()),
            has_worktree,
        }
    }

    #[test]
    fn test_available_actions_active_thread() {
        let target = thread_target("Active", false);
        let actions = available_actions(&target);
        assert_eq!(actions.len(), 3);
        assert_eq!(actions[0].key, 'a');
        assert_eq!(actions[1].key, 'c');
        assert_eq!(actions[2].key, 'f');
    }

    #[test]
    fn test_available_actions_active_worktree_thread() {
        let target = thread_target("Active", true);
        let actions = available_actions(&target);
        assert_eq!(actions.len(), 4);
        assert_eq!(actions[3].key, 'm');
        assert_eq!(actions[3].label, "Queue merge");
    }

    #[test]
    fn test_available_actions_terminal_thread() {
        for status in &["Completed", "Failed", "Abandoned"] {
            let target = thread_target(status, false);
            let actions = available_actions(&target);
            assert_eq!(actions.len(), 1, "expected 1 action for status {status}");
            assert_eq!(actions[0].key, 'r');
            assert_eq!(actions[0].label, "Reopen");
        }
    }

    #[test]
    fn test_available_actions_batch() {
        let target = ActionTarget::Batch {
            batch_id: "GAP-11".into(),
        };
        let actions = available_actions(&target);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].key, 'a');
        assert_eq!(actions[0].label, "Abandon batch");
    }

    #[test]
    fn test_available_actions_queued_merge() {
        let target = ActionTarget::MergeOp {
            op_id: "01ABC".into(),
            status: "queued".into(),
        };
        let actions = available_actions(&target);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].key, 'x');
        assert_eq!(actions[0].label, "Cancel merge");
    }

    #[test]
    fn test_available_actions_non_queued_merge() {
        for status in &["executing", "completed", "failed", "cancelled"] {
            let target = ActionTarget::MergeOp {
                op_id: "01ABC".into(),
                status: (*status).into(),
            };
            let actions = available_actions(&target);
            assert!(
                actions.is_empty(),
                "expected empty actions for merge status {status}"
            );
        }
    }

    #[test]
    fn test_needs_confirmation_reopen_false() {
        let action = PendingAction::ReopenThread {
            thread_id: "t1".into(),
        };
        assert!(!needs_confirmation(&action));
    }

    #[test]
    fn test_needs_confirmation_abandon_true() {
        let action = PendingAction::AbandonThread {
            thread_id: "t1".into(),
        };
        assert!(needs_confirmation(&action));
    }

    #[test]
    fn test_needs_confirmation_all_others_true() {
        let cases = vec![
            PendingAction::AbandonBatch {
                batch_id: "b1".into(),
            },
            PendingAction::CloseThread {
                thread_id: "t1".into(),
                status: CloseStatus::Completed,
            },
            PendingAction::CloseThread {
                thread_id: "t1".into(),
                status: CloseStatus::Failed,
            },
            PendingAction::CancelMerge { op_id: "m1".into() },
            PendingAction::QueueMerge {
                thread_id: "t1".into(),
            },
        ];
        for action in cases {
            assert!(needs_confirmation(&action));
        }
    }

    #[test]
    fn test_action_for_key_matches_available() {
        // Active thread without worktree
        let target = thread_target("Active", false);
        let actions = available_actions(&target);
        for entry in &actions {
            assert!(
                action_for_key(entry.key, &target).is_some(),
                "key '{}' should produce an action",
                entry.key
            );
        }
        assert!(action_for_key('z', &target).is_none());
        assert!(action_for_key('m', &target).is_none()); // no worktree

        // Active thread with worktree
        let target_wt = thread_target("Active", true);
        assert!(action_for_key('m', &target_wt).is_some());

        // Terminal thread
        let target_term = thread_target("Completed", false);
        assert!(action_for_key('r', &target_term).is_some());
        assert!(action_for_key('a', &target_term).is_none());

        // Batch
        let target_batch = ActionTarget::Batch {
            batch_id: "B1".into(),
        };
        assert!(action_for_key('a', &target_batch).is_some());
        assert!(action_for_key('x', &target_batch).is_none());

        // Queued merge
        let target_merge = ActionTarget::MergeOp {
            op_id: "M1".into(),
            status: "queued".into(),
        };
        assert!(action_for_key('x', &target_merge).is_some());
        assert!(action_for_key('a', &target_merge).is_none());

        // Non-queued merge
        let target_merge_done = ActionTarget::MergeOp {
            op_id: "M2".into(),
            status: "completed".into(),
        };
        assert!(action_for_key('x', &target_merge_done).is_none());
    }

    #[test]
    fn test_confirmation_prompt_truncates_long_ids() {
        let action = PendingAction::AbandonThread {
            thread_id: "01KMVABCDEFGHIJKLMNOP".into(),
        };
        let prompt = confirmation_prompt(&action);
        assert!(prompt.contains("01KMVABCDEFG…"));
        assert!(prompt.contains("[y]es / [n]o"));
    }

    #[test]
    fn test_confirmation_prompt_short_id_no_truncation() {
        let action = PendingAction::AbandonThread {
            thread_id: "SHORT".into(),
        };
        let prompt = confirmation_prompt(&action);
        assert!(prompt.contains("SHORT"));
        assert!(!prompt.contains('…'));
    }

    #[test]
    fn test_confirmation_prompt_variants() {
        let batch = PendingAction::AbandonBatch {
            batch_id: "GAP-11".into(),
        };
        assert!(confirmation_prompt(&batch).starts_with("Abandon batch GAP-11?"));

        let close = PendingAction::CloseThread {
            thread_id: "t1".into(),
            status: CloseStatus::Completed,
        };
        assert!(confirmation_prompt(&close).contains("as completed?"));

        let cancel = PendingAction::CancelMerge { op_id: "m1".into() };
        assert!(confirmation_prompt(&cancel).starts_with("Cancel merge op m1?"));

        let queue = PendingAction::QueueMerge {
            thread_id: "t1".into(),
        };
        assert!(confirmation_prompt(&queue).starts_with("Queue merge for thread t1?"));
    }
}
