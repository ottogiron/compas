//! Tab view renderers for the compas TUI dashboard.

pub mod activity;
pub mod agents;
pub mod conversation;
pub mod executions;
pub mod log_viewer;
pub mod payload;

use ratatui::style::Color;

/// Pass-through: thread status values are already human-readable.
pub fn humanize_thread_status(raw: &str) -> &str {
    raw
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

/// Check if an execution status string represents a running execution.
pub fn is_running_exec_status(status: &str) -> bool {
    matches!(status, "executing" | "picked_up" | "queued")
}

// ── Shared duration formatters ─────────────────────────────────────────────────

/// Format a duration in seconds to human-readable tiers.
///
/// * `< 60`    → `"Ns"`
/// * `< 3600`  → `"Nm Ss"`
/// * `< 86400` → `"Nh Mm"`
/// * `≥ 86400` → `"Nd Hh"`
pub fn format_duration_secs(secs: i64) -> String {
    let secs = secs.max(0);
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3_600 {
        let m = secs / 60;
        let s = secs % 60;
        format!("{}m {}s", m, s)
    } else if secs < 86_400 {
        let h = secs / 3_600;
        let m = (secs % 3_600) / 60;
        format!("{}h {}m", h, m)
    } else {
        let d = secs / 86_400;
        let h = (secs % 86_400) / 3_600;
        format!("{}d {}h", d, h)
    }
}

/// Format a duration in milliseconds to human-readable tiers.
///
/// * `< 0`    → `"-"`
/// * `< 1000` → `"Nms"`
/// * otherwise delegates to [`format_duration_secs`] with `ms / 1000`.
pub fn format_duration_ms(ms: i64) -> String {
    if ms < 0 {
        return "-".to_string();
    }
    if ms < 1_000 {
        format!("{}ms", ms)
    } else {
        format_duration_secs(ms / 1_000)
    }
}

/// Format a token count to a compact human-readable string.
///
/// * `< 1_000`       → `"N"` (plain number)
/// * `< 1_000_000`   → `"N.NK"` (thousands)
/// * `≥ 1_000_000`   → `"N.NM"` (millions)
pub fn format_tokens(n: i64) -> String {
    let n = n.max(0);
    if n < 1_000 {
        format!("{}", n)
    } else if n < 1_000_000 {
        let k = n as f64 / 1_000.0;
        // Promote to M if rounding would produce "1000.0K"
        if k >= 999.95 {
            format!("{:.1}M", n as f64 / 1_000_000.0)
        } else {
            format!("{:.1}K", k)
        }
    } else {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    }
}

/// Format a USD cost value to a compact display string.
///
/// Always two decimal places: `"$0.00"`, `"$0.12"`, `"$1.23"`, etc.
pub fn format_cost_usd(cost: f64) -> String {
    format!("${:.2}", cost.max(0.0))
}

// ── Shared string helpers ──────────────────────────────────────────────────────

/// Truncate a string to `max_chars` Unicode scalars, appending "…" if truncated.
pub fn truncate(s: &str, max_chars: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max_chars {
        s.to_string()
    } else if max_chars <= 1 {
        "…".to_string()
    } else {
        let truncated: String = s.chars().take(max_chars - 1).collect();
        format!("{truncated}…")
    }
}

// ── Shared colour helpers ──────────────────────────────────────────────────────

/// Color for thread status values.
pub fn thread_status_color(status: &str) -> Color {
    match status {
        "Active" | "active" => Color::Yellow,
        "Completed" | "completed" => Color::Green,
        "Failed" | "failed" => Color::Red,
        "Abandoned" | "abandoned" => Color::DarkGray,
        _ => Color::White,
    }
}

/// Color for execution status values.
pub fn exec_status_color(status: &str) -> Color {
    match status {
        "completed" => Color::Green,
        "failed" | "crashed" | "timed_out" => Color::Red,
        "executing" | "picked_up" => Color::Yellow,
        "queued" => Color::Cyan,
        "cancelled" => Color::DarkGray,
        _ => Color::White,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // humanize_thread_status

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

    // format_duration_secs

    #[test]
    fn test_format_duration_secs_zero() {
        assert_eq!(format_duration_secs(0), "0s");
    }

    #[test]
    fn test_format_duration_secs_seconds() {
        assert_eq!(format_duration_secs(30), "30s");
        assert_eq!(format_duration_secs(59), "59s");
    }

    #[test]
    fn test_format_duration_secs_minutes() {
        // 90s → 1m 30s
        assert_eq!(format_duration_secs(90), "1m 30s");
        assert_eq!(format_duration_secs(3599), "59m 59s");
    }

    #[test]
    fn test_format_duration_secs_hours() {
        // 3661s → 1h 1m
        assert_eq!(format_duration_secs(3_661), "1h 1m");
        assert_eq!(format_duration_secs(86_399), "23h 59m");
    }

    #[test]
    fn test_format_duration_secs_days() {
        // 90000s = 1d 1h
        assert_eq!(format_duration_secs(90_000), "1d 1h");
        assert_eq!(format_duration_secs(172_800), "2d 0h");
    }

    #[test]
    fn test_format_duration_secs_negative_clamps_to_zero() {
        assert_eq!(format_duration_secs(-5), "0s");
    }

    // format_duration_ms

    #[test]
    fn test_format_duration_ms_zero() {
        assert_eq!(format_duration_ms(0), "0ms");
    }

    #[test]
    fn test_format_duration_ms_millis() {
        assert_eq!(format_duration_ms(500), "500ms");
        assert_eq!(format_duration_ms(999), "999ms");
    }

    #[test]
    fn test_format_duration_ms_delegates_to_secs() {
        // 1500ms → format_duration_secs(1) → "1s"
        assert_eq!(format_duration_ms(1_500), "1s");
        // 174929ms → format_duration_secs(174) → "2m 54s"
        assert_eq!(format_duration_ms(174_929), "2m 54s");
    }

    #[test]
    fn test_format_duration_ms_negative() {
        assert_eq!(format_duration_ms(-1), "-");
    }

    // thread_status_color

    #[test]
    fn test_thread_status_color_active() {
        assert_eq!(thread_status_color("Active"), Color::Yellow);
        assert_eq!(thread_status_color("active"), Color::Yellow);
    }

    #[test]
    fn test_thread_status_color_completed() {
        assert_eq!(thread_status_color("Completed"), Color::Green);
    }

    #[test]
    fn test_thread_status_color_failed() {
        assert_eq!(thread_status_color("Failed"), Color::Red);
    }

    #[test]
    fn test_thread_status_color_abandoned() {
        assert_eq!(thread_status_color("Abandoned"), Color::DarkGray);
    }

    #[test]
    fn test_thread_status_color_unknown() {
        assert_eq!(thread_status_color("SomeFuture"), Color::White);
    }

    // exec_status_color

    #[test]
    fn test_exec_status_color_completed() {
        assert_eq!(exec_status_color("completed"), Color::Green);
    }

    #[test]
    fn test_exec_status_color_failed() {
        assert_eq!(exec_status_color("failed"), Color::Red);
    }

    #[test]
    fn test_exec_status_color_crashed() {
        assert_eq!(exec_status_color("crashed"), Color::Red);
    }

    #[test]
    fn test_exec_status_color_timed_out() {
        assert_eq!(exec_status_color("timed_out"), Color::Red);
    }

    #[test]
    fn test_exec_status_color_executing() {
        assert_eq!(exec_status_color("executing"), Color::Yellow);
    }

    #[test]
    fn test_exec_status_color_picked_up() {
        assert_eq!(exec_status_color("picked_up"), Color::Yellow);
    }

    #[test]
    fn test_exec_status_color_queued() {
        assert_eq!(exec_status_color("queued"), Color::Cyan);
    }

    #[test]
    fn test_exec_status_color_cancelled() {
        assert_eq!(exec_status_color("cancelled"), Color::DarkGray);
    }

    #[test]
    fn test_exec_status_color_unknown() {
        assert_eq!(exec_status_color("other"), Color::White);
    }

    // truncate

    #[test]
    fn test_truncate_short_string_passes_through() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_exact_length_passes_through() {
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn test_truncate_long_string_appends_ellipsis() {
        let s = "a".repeat(50);
        let result = truncate(&s, 10);
        assert!(result.ends_with('…'), "expected trailing ellipsis");
        // 9 'a' chars + "…" (3 UTF-8 bytes but 1 char)
        assert_eq!(result, format!("{}…", "a".repeat(9)));
    }

    #[test]
    fn test_truncate_empty_string() {
        assert_eq!(truncate("", 10), "");
    }

    #[test]
    fn test_truncate_max_chars_zero_returns_ellipsis() {
        // max_chars == 0 ≤ 1 → "…"
        assert_eq!(truncate("hello", 0), "…");
    }

    #[test]
    fn test_truncate_max_chars_one_returns_ellipsis() {
        // max_chars == 1 ≤ 1 → "…"
        assert_eq!(truncate("hello", 1), "…");
    }

    #[test]
    fn test_truncate_max_chars_two_truncates_to_one_char_plus_ellipsis() {
        assert_eq!(truncate("hello", 2), "h…");
    }

    #[test]
    fn test_truncate_non_ascii_chars() {
        assert_eq!(truncate("héllo", 4), "hél…");
        assert_eq!(truncate("日本語テスト", 4), "日本語…");
        assert_eq!(truncate("café", 4), "café"); // exactly 4 chars
        assert_eq!(truncate("café", 3), "ca…");
    }

    // format_tokens

    #[test]
    fn test_format_tokens_zero() {
        assert_eq!(format_tokens(0), "0");
    }

    #[test]
    fn test_format_tokens_below_thousand() {
        assert_eq!(format_tokens(999), "999");
    }

    #[test]
    fn test_format_tokens_one_thousand() {
        assert_eq!(format_tokens(1_000), "1.0K");
    }

    #[test]
    fn test_format_tokens_thousands() {
        assert_eq!(format_tokens(12_345), "12.3K");
    }

    #[test]
    fn test_format_tokens_below_million() {
        // 999_999 / 1000.0 = 999.999 ≥ 999.95, promotes to M
        assert_eq!(format_tokens(999_999), "1.0M");
    }

    #[test]
    fn test_format_tokens_one_million() {
        assert_eq!(format_tokens(1_000_000), "1.0M");
    }

    #[test]
    fn test_format_tokens_millions() {
        assert_eq!(format_tokens(1_234_567), "1.2M");
    }

    #[test]
    fn test_format_tokens_negative_clamps_to_zero() {
        assert_eq!(format_tokens(-5), "0");
    }

    // format_cost_usd

    #[test]
    fn test_format_cost_usd_zero() {
        assert_eq!(format_cost_usd(0.0), "$0.00");
    }

    #[test]
    fn test_format_cost_usd_small() {
        assert_eq!(format_cost_usd(0.001), "$0.00");
    }

    #[test]
    fn test_format_cost_usd_cents() {
        assert_eq!(format_cost_usd(0.12), "$0.12");
    }

    #[test]
    fn test_format_cost_usd_dollars() {
        assert_eq!(format_cost_usd(1.23), "$1.23");
    }

    #[test]
    fn test_format_cost_usd_tens() {
        assert_eq!(format_cost_usd(12.34), "$12.34");
    }

    #[test]
    fn test_format_cost_usd_negative_clamps_to_zero() {
        assert_eq!(format_cost_usd(-1.0), "$0.00");
    }
}
