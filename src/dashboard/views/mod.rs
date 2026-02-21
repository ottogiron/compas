//! Tab view renderers for the aster-orch TUI dashboard.

pub mod agents;
pub mod executions;
pub mod overview;
pub mod threads;

/// Convert a raw thread-status string to a human-readable label.
///
/// Known snake_case / PascalCase variants are normalised to title-case words;
/// already-readable values (Active, Completed, Failed, Abandoned) pass through
/// unchanged.
pub fn humanize_thread_status(raw: &str) -> &str {
    match raw {
        "ReviewPending" | "review_pending" => "Review Pending",
        other => other,
    }
}

/// Convert a raw execution-status string to a human-readable label.
///
/// All snake_case storage values are capitalised for display; other values
/// pass through unchanged.
pub fn humanize_exec_status(raw: &str) -> &str {
    match raw {
        "picked_up" => "Picked Up",
        "timed_out" => "Timed Out",
        "queued" => "Queued",
        "executing" => "Executing",
        "completed" => "Completed",
        "failed" => "Failed",
        "crashed" => "Crashed",
        "cancelled" => "Cancelled",
        other => other,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // humanize_thread_status

    #[test]
    fn test_humanize_thread_status_review_pending_pascal() {
        assert_eq!(humanize_thread_status("ReviewPending"), "Review Pending");
    }

    #[test]
    fn test_humanize_thread_status_review_pending_snake() {
        assert_eq!(humanize_thread_status("review_pending"), "Review Pending");
    }

    #[test]
    fn test_humanize_thread_status_passthrough_active() {
        assert_eq!(humanize_thread_status("Active"), "Active");
    }

    #[test]
    fn test_humanize_thread_status_passthrough_completed() {
        assert_eq!(humanize_thread_status("Completed"), "Completed");
    }

    #[test]
    fn test_humanize_thread_status_passthrough_failed() {
        assert_eq!(humanize_thread_status("Failed"), "Failed");
    }

    #[test]
    fn test_humanize_thread_status_passthrough_abandoned() {
        assert_eq!(humanize_thread_status("Abandoned"), "Abandoned");
    }

    #[test]
    fn test_humanize_thread_status_passthrough_unknown() {
        assert_eq!(
            humanize_thread_status("SomeFutureStatus"),
            "SomeFutureStatus"
        );
    }

    // humanize_exec_status

    #[test]
    fn test_humanize_exec_status_picked_up() {
        assert_eq!(humanize_exec_status("picked_up"), "Picked Up");
    }

    #[test]
    fn test_humanize_exec_status_timed_out() {
        assert_eq!(humanize_exec_status("timed_out"), "Timed Out");
    }

    #[test]
    fn test_humanize_exec_status_queued() {
        assert_eq!(humanize_exec_status("queued"), "Queued");
    }

    #[test]
    fn test_humanize_exec_status_executing() {
        assert_eq!(humanize_exec_status("executing"), "Executing");
    }

    #[test]
    fn test_humanize_exec_status_completed() {
        assert_eq!(humanize_exec_status("completed"), "Completed");
    }

    #[test]
    fn test_humanize_exec_status_failed() {
        assert_eq!(humanize_exec_status("failed"), "Failed");
    }

    #[test]
    fn test_humanize_exec_status_crashed() {
        assert_eq!(humanize_exec_status("crashed"), "Crashed");
    }

    #[test]
    fn test_humanize_exec_status_cancelled() {
        assert_eq!(humanize_exec_status("cancelled"), "Cancelled");
    }

    #[test]
    fn test_humanize_exec_status_passthrough_unknown() {
        assert_eq!(
            humanize_exec_status("some_future_status"),
            "some_future_status"
        );
    }
}
