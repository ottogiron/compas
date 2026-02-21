//! Threads tab — table of all threads with status and latest execution info.
//!
//! Layout (within the content pane):
//!   ┌ Threads ───────────────────────────────────────────────────────────────┐
//!   │  Thread ID     Status           Agent          Batch      Age          │
//!   │  ────────────  ───────────────  ─────────────  ────────   ───          │
//!   │  abc123def456  Active           focused        ba12cd34   5s           │
//!   │  …                                                                     │
//!   └────────────────────────────────────────────────────────────────────────┘
//!
//! Thread IDs are truncated to 12 characters.
//! Batch IDs are truncated to 8 characters.
//! Age is derived from `thread_updated_at` (unix seconds).
//! Status is colour-coded using the shared `status_color` helper.

use ratatui::{
    layout::{Constraint, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table},
    Frame,
};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::dashboard::app::App;
use crate::dashboard::views::humanize_thread_status;
use crate::dashboard::views::overview::status_color;

// ── Entry point ───────────────────────────────────────────────────────────────

/// Render the Threads tab into `area`.
pub fn render_threads(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default().borders(Borders::ALL).title(" Threads ");

    // ── No data yet ──────────────────────────────────────────────────────────
    let Some(data) = &app.threads_data else {
        let p = Paragraph::new(Line::from(Span::styled(
            "  Fetching…",
            Style::default().fg(Color::DarkGray),
        )))
        .block(block);
        f.render_widget(p, area);
        return;
    };

    // ── Empty state ──────────────────────────────────────────────────────────
    if data.threads.is_empty() {
        let p = Paragraph::new(Line::from(Span::styled(
            "  No threads",
            Style::default().fg(Color::DarkGray),
        )))
        .block(block);
        f.render_widget(p, area);
        return;
    }

    // ── Build table ───────────────────────────────────────────────────────────
    let now_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    let header = Row::new([
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
        Cell::from("Agent").style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Cell::from("Batch").style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Cell::from("Age").style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
    ])
    .height(1)
    .bottom_margin(1);

    let rows: Vec<Row> = data
        .threads
        .iter()
        .map(|t| {
            // Thread ID — first 12 chars with ellipsis when truncated.
            let thread_id: String = if t.thread_id.len() > 12 {
                format!("{}…", &t.thread_id[..12])
            } else {
                t.thread_id.clone()
            };

            // Status — humanized and colour-coded.
            let status = &t.thread_status;
            let status_cell = Cell::from(humanize_thread_status(status))
                .style(Style::default().fg(status_color(status)));

            // Agent alias — dash if absent.
            let agent = t.agent_alias.as_deref().unwrap_or("-").to_string();

            // Batch ID — first 8 chars with ellipsis when truncated, or dash.
            let batch: String = match &t.batch_id {
                Some(b) if !b.is_empty() => {
                    if b.len() > 8 {
                        format!("{}…", &b[..8])
                    } else {
                        b.clone()
                    }
                }
                _ => "-".to_string(),
            };

            // Age — elapsed since last update.
            let age_secs = (now_unix - t.thread_updated_at).max(0) as u64;
            let age = format_age(age_secs);

            Row::new(vec![
                Cell::from(thread_id),
                status_cell,
                Cell::from(agent),
                Cell::from(batch),
                Cell::from(age),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(15), // Thread ID (12 chars + ellipsis + padding)
        Constraint::Length(16), // Status
        Constraint::Length(14), // Agent
        Constraint::Length(11), // Batch (8 chars + ellipsis + padding)
        Constraint::Length(6),  // Age
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(block)
        .style(Style::default().fg(Color::White));

    f.render_widget(table, area);
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Format elapsed seconds as a compact label: "5s", "3m", "2h", "4d".
fn format_age(secs: u64) -> String {
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86400)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_threads_format_age_seconds() {
        assert_eq!(format_age(0), "0s");
        assert_eq!(format_age(1), "1s");
        assert_eq!(format_age(59), "59s");
    }

    #[test]
    fn test_threads_format_age_minutes() {
        assert_eq!(format_age(60), "1m");
        assert_eq!(format_age(120), "2m");
        assert_eq!(format_age(3599), "59m");
    }

    #[test]
    fn test_threads_format_age_hours() {
        assert_eq!(format_age(3600), "1h");
        assert_eq!(format_age(7200), "2h");
        assert_eq!(format_age(86399), "23h");
    }

    #[test]
    fn test_threads_format_age_days() {
        assert_eq!(format_age(86400), "1d");
        assert_eq!(format_age(172800), "2d");
    }
}
