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
use crate::dashboard::views::{exec_status_color, format_duration_ms, humanize_exec_status};

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
