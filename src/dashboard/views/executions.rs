//! Executions tab — recent execution records across all agents.
//!
//! Layout (within the content pane):
//!   ┌ Executions ────────────────────────────────────────────────────────────┐
//!   │  Agent          Thread ID     Status       Duration   Exit   Error      │
//!   │  ─────────────  ────────────  ───────────  ────────   ────   ────────   │
//!   │  focused        abc123def456  completed    1234ms     0      -           │
//!   │  chill          def456abc789  failed       -          1      timeout…    │
//!   │  …                                                                      │
//!   └────────────────────────────────────────────────────────────────────────┘
//!
//! Thread IDs are truncated to 12 characters.
//! Duration is displayed as "Nms", "Ns", or "Nm" depending on magnitude.
//! Error detail is truncated to 40 characters.
//! Status is colour-coded (green/red/yellow/cyan).
//! Up to 50 rows are shown, sorted newest first by `queued_at`.

use ratatui::{
    layout::{Constraint, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table},
    Frame,
};

use crate::dashboard::app::App;
use crate::dashboard::views::humanize_exec_status;

// ── Entry point ───────────────────────────────────────────────────────────────

/// Render the Executions tab into `area`.
pub fn render_executions(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default().borders(Borders::ALL).title(" Executions ");

    // ── No data yet ──────────────────────────────────────────────────────────
    let Some(data) = &app.executions_data else {
        let p = Paragraph::new(Line::from(Span::styled(
            "  Fetching…",
            Style::default().fg(Color::DarkGray),
        )))
        .block(block);
        f.render_widget(p, area);
        return;
    };

    // ── Empty state ──────────────────────────────────────────────────────────
    if data.executions.is_empty() {
        let p = Paragraph::new(Line::from(Span::styled(
            "  No executions recorded",
            Style::default().fg(Color::DarkGray),
        )))
        .block(block);
        f.render_widget(p, area);
        return;
    }

    // ── Build table ───────────────────────────────────────────────────────────
    let header = Row::new([
        Cell::from("Agent").style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Cell::from("Thread ID").style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Cell::from("Status").style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Cell::from("Duration").style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Cell::from("Exit").style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Cell::from("Error").style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
    ])
    .height(1)
    .bottom_margin(1);

    let rows: Vec<Row> = data
        .executions
        .iter()
        .map(|e| {
            // Agent alias.
            let agent = e.agent_alias.clone();

            // Thread ID — first 12 chars with ellipsis when truncated.
            let thread_id: String = if e.thread_id.len() > 12 {
                format!("{}…", &e.thread_id[..12])
            } else {
                e.thread_id.clone()
            };

            // Status — humanized and colour-coded cell.
            let status_color = exec_status_color(&e.status);
            let status_cell = Cell::from(humanize_exec_status(&e.status))
                .style(Style::default().fg(status_color));

            // Duration — human-readable.
            let duration = e
                .duration_ms
                .map(format_duration_ms)
                .unwrap_or_else(|| "-".to_string());

            // Exit code — dash if absent.
            let exit_code = e
                .exit_code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "-".to_string());

            // Error preview — truncated to 40 chars, dash if absent.
            let error_preview = e
                .error_detail
                .as_deref()
                .map(|s| truncate(s, 40))
                .unwrap_or_else(|| "-".to_string());

            Row::new(vec![
                Cell::from(agent),
                Cell::from(thread_id),
                status_cell,
                Cell::from(duration),
                Cell::from(exit_code),
                Cell::from(error_preview),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(14), // Agent (flexible alias)
        Constraint::Length(15), // Thread ID (12 chars + ellipsis + padding)
        Constraint::Length(13), // Status
        Constraint::Length(10), // Duration
        Constraint::Length(6),  // Exit code
        Constraint::Min(10),    // Error preview (fills remaining width)
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(block)
        .style(Style::default().fg(Color::White));

    f.render_widget(table, area);
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Map an execution status string to a display colour.
fn exec_status_color(status: &str) -> Color {
    match status {
        "completed" => Color::Green,
        "failed" | "crashed" | "timed_out" => Color::Red,
        "executing" | "picked_up" => Color::Yellow,
        "queued" => Color::Cyan,
        "cancelled" => Color::DarkGray,
        _ => Color::White,
    }
}

/// Format milliseconds as a compact human-readable label.
///
/// * < 10 000 ms → "1234ms"
/// * < 600 000 ms → "45s"
/// * otherwise → "12m"
fn format_duration_ms(ms: i64) -> String {
    if ms < 0 {
        return "-".to_string();
    }
    if ms < 10_000 {
        format!("{}ms", ms)
    } else if ms < 600_000 {
        format!("{}s", ms / 1_000)
    } else {
        format!("{}m", ms / 60_000)
    }
}

/// Truncate `s` to at most `max_chars` Unicode scalar values, appending "…"
/// if truncated.
fn truncate(s: &str, max_chars: usize) -> String {
    let mut chars = s.chars();
    let collected: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{}…", collected)
    } else {
        collected
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // exec_status_color

    #[test]
    fn test_executions_status_color_completed() {
        assert_eq!(exec_status_color("completed"), Color::Green);
    }

    #[test]
    fn test_executions_status_color_failed() {
        assert_eq!(exec_status_color("failed"), Color::Red);
    }

    #[test]
    fn test_executions_status_color_crashed() {
        assert_eq!(exec_status_color("crashed"), Color::Red);
    }

    #[test]
    fn test_executions_status_color_timed_out() {
        assert_eq!(exec_status_color("timed_out"), Color::Red);
    }

    #[test]
    fn test_executions_status_color_executing() {
        assert_eq!(exec_status_color("executing"), Color::Yellow);
    }

    #[test]
    fn test_executions_status_color_picked_up() {
        assert_eq!(exec_status_color("picked_up"), Color::Yellow);
    }

    #[test]
    fn test_executions_status_color_queued() {
        assert_eq!(exec_status_color("queued"), Color::Cyan);
    }

    #[test]
    fn test_executions_status_color_cancelled() {
        assert_eq!(exec_status_color("cancelled"), Color::DarkGray);
    }

    #[test]
    fn test_executions_status_color_unknown() {
        assert_eq!(exec_status_color("other"), Color::White);
    }

    // format_duration_ms

    #[test]
    fn test_executions_format_duration_ms_zero() {
        assert_eq!(format_duration_ms(0), "0ms");
    }

    #[test]
    fn test_executions_format_duration_ms_millis() {
        assert_eq!(format_duration_ms(1234), "1234ms");
        assert_eq!(format_duration_ms(9999), "9999ms");
    }

    #[test]
    fn test_executions_format_duration_ms_seconds() {
        assert_eq!(format_duration_ms(10_000), "10s");
        assert_eq!(format_duration_ms(45_000), "45s");
        assert_eq!(format_duration_ms(599_999), "599s");
    }

    #[test]
    fn test_executions_format_duration_ms_minutes() {
        assert_eq!(format_duration_ms(600_000), "10m");
        assert_eq!(format_duration_ms(3_600_000), "60m");
    }

    #[test]
    fn test_executions_format_duration_ms_negative() {
        assert_eq!(format_duration_ms(-1), "-");
    }

    // truncate

    #[test]
    fn test_executions_truncate_short() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn test_executions_truncate_exact() {
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn test_executions_truncate_long() {
        let s = "a".repeat(50);
        let result = truncate(&s, 40);
        assert!(result.ends_with('…'));
        // 40 chars + ellipsis
        assert_eq!(result.chars().count(), 41);
    }

    #[test]
    fn test_executions_truncate_empty() {
        assert_eq!(truncate("", 40), "");
    }
}
