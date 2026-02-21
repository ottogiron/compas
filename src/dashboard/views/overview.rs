//! Overview tab — live metrics from SQLite.
//!
//! Layout (top → bottom within the content pane):
//!   ┌ Metrics ────────────────────────────────────────────────┐
//!   │  ● Active: 2   ● Completed: 42  │ Queue: 3 │ Msgs: 120  │
//!   ├ Agent Utilization ──────────────────────────────────────┤
//!   │  focused     [=====>     ] 1/2                          │
//!   │  chill       [>          ] 0/2                          │
//!   ├ Worker ─────────────────────────────────────────────────┤
//!   │  ● Worker: worker-01JX… │ Last beat: 3s ago │ Up: 2h    │
//!   └─────────────────────────────────────────────────────────┘

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::dashboard::app::App;
use crate::dashboard::views::humanize_thread_status;

// ── Entry point ───────────────────────────────────────────────────────────────

/// Render the Overview tab into `area`.
pub fn render_overview(f: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // metrics row (border + 1 line + border)
            Constraint::Min(0),    // agent utilization (scales with agent count)
            Constraint::Length(3), // worker heartbeat
        ])
        .split(area);

    render_metrics(f, app, chunks[0]);
    render_agents(f, app, chunks[1]);
    render_heartbeat(f, app, chunks[2]);
}

// ── Metrics row ───────────────────────────────────────────────────────────────

fn render_metrics(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default().borders(Borders::ALL).title(" Metrics ");

    let Some(data) = &app.overview_data else {
        let p = Paragraph::new(Span::styled(
            " Fetching…",
            Style::default().fg(Color::DarkGray),
        ))
        .block(block);
        f.render_widget(p, area);
        return;
    };

    let mut spans: Vec<Span> = vec![Span::raw(" ")];

    // Thread count badges — one per status, coloured by severity.
    for (status, count) in &data.thread_counts {
        let color = status_color(status);
        spans.push(Span::styled("● ", Style::default().fg(color)));
        spans.push(Span::styled(
            format!("{}: {} ", humanize_thread_status(status), count),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw(" "));
    }

    // Separator + queue depth.
    spans.push(Span::styled("│ ", Style::default().fg(Color::DarkGray)));
    spans.push(Span::raw("Pending: "));
    spans.push(Span::styled(
        format!("{} ", data.queue_depth),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ));

    // Separator + total messages.
    spans.push(Span::styled("│ ", Style::default().fg(Color::DarkGray)));
    spans.push(Span::raw("Total messages: "));
    spans.push(Span::styled(
        format!("{}", data.total_messages),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ));

    let p = Paragraph::new(Line::from(spans)).block(block);
    f.render_widget(p, area);
}

// ── Agent utilization ─────────────────────────────────────────────────────────

fn render_agents(f: &mut Frame, app: &App, area: Rect) {
    let max = app.config.orchestration.max_triggers_per_agent;
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Agent Utilization ");

    let lines: Vec<Line> = if app.config.agents.is_empty() {
        vec![Line::from(Span::styled(
            "  No agents configured.",
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
        app.config
            .agents
            .iter()
            .map(|agent| {
                // Look up active count for this agent (0 if not found in snapshot).
                let active = app
                    .overview_data
                    .as_ref()
                    .and_then(|d| {
                        d.active_by_agent
                            .iter()
                            .find(|(a, _)| a == &agent.alias)
                            .map(|(_, c)| *c as usize)
                    })
                    .unwrap_or(0);

                let bar = gauge_bar(active, max);
                // Left-pad alias to 12 chars for column alignment.
                let alias_col = format!("{:<12}", agent.alias);

                let bar_color = if active == 0 {
                    Color::DarkGray
                } else if active >= max {
                    Color::Red
                } else {
                    Color::Yellow
                };

                Line::from(vec![
                    Span::raw("  "),
                    Span::styled(
                        alias_col,
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(bar, Style::default().fg(bar_color)),
                    Span::raw("  "),
                    Span::styled(
                        format!("{}/{}", active, max),
                        Style::default().fg(Color::White),
                    ),
                ])
            })
            .collect()
    };

    let p = Paragraph::new(lines).block(block);
    f.render_widget(p, area);
}

// ── Worker heartbeat ──────────────────────────────────────────────────────────

fn render_heartbeat(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default().borders(Borders::ALL).title(" Worker ");

    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    let line = match app
        .overview_data
        .as_ref()
        .and_then(|d| d.heartbeat.as_ref())
    {
        None => Line::from(Span::styled(
            "  ● No worker heartbeat recorded",
            Style::default().fg(Color::DarkGray),
        )),

        Some((worker_id, last_beat_at, started_at, _version)) => {
            let age_secs = (now_unix - last_beat_at).max(0) as u64;
            let health_color = if age_secs < 30 {
                Color::Green
            } else {
                Color::Red
            };

            let uptime_secs = (now_unix - started_at).max(0) as u64;
            let age_label = format_duration(age_secs);
            let uptime_label = format_duration(uptime_secs);

            // Truncate long worker IDs to keep the line tidy.
            let id_display = if worker_id.len() > 22 {
                format!("{}…", &worker_id[..21])
            } else {
                worker_id.clone()
            };

            Line::from(vec![
                Span::raw("  "),
                Span::styled("● ", Style::default().fg(health_color)),
                Span::styled(
                    format!("Worker: {} ", id_display),
                    Style::default().fg(Color::White),
                ),
                Span::styled("│ ", Style::default().fg(Color::DarkGray)),
                Span::raw(format!("Last beat: {} ago ", age_label)),
                Span::styled("│ ", Style::default().fg(Color::DarkGray)),
                Span::raw(format!("Started: {} ago", uptime_label)),
            ])
        }
    };

    let p = Paragraph::new(line).block(block);
    f.render_widget(p, area);
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Map a thread status string to a display colour.
pub fn status_color(status: &str) -> Color {
    match status {
        "Active" | "active" => Color::Yellow,
        "ReviewPending" | "review_pending" => Color::Blue,
        "Completed" | "completed" => Color::Green,
        "Failed" | "failed" => Color::Red,
        "Abandoned" | "abandoned" => Color::DarkGray,
        _ => Color::White,
    }
}

/// Build a fixed-width ASCII gauge bar: `[===>      ]`.
///
/// Bar width (between the brackets) is `BAR_WIDTH` characters.
/// The tip `>` marks the current fill position; `=` fills to the left.
fn gauge_bar(active: usize, max: usize) -> String {
    const BAR_WIDTH: usize = 10;

    if max == 0 {
        return format!("[{}]", " ".repeat(BAR_WIDTH));
    }

    let ratio = (active as f64 / max as f64).min(1.0);
    let filled = (ratio * BAR_WIDTH as f64).round() as usize;
    let filled = filled.min(BAR_WIDTH);

    if filled == 0 {
        // Show a lone `>` to indicate zero load.
        format!("[>{}]", " ".repeat(BAR_WIDTH - 1))
    } else {
        let bars = filled.saturating_sub(1);
        let empty = BAR_WIDTH - filled;
        format!("[{}>{}]", "=".repeat(bars), " ".repeat(empty))
    }
}

/// Format seconds as a short human-readable duration: "5s", "3m", "2h".
fn format_duration(secs: u64) -> String {
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h", secs / 3600)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_overview_gauge_bar_zero() {
        let bar = gauge_bar(0, 2);
        assert_eq!(bar, "[>         ]");
    }

    #[test]
    fn test_overview_gauge_bar_half() {
        let bar = gauge_bar(1, 2);
        // ratio=0.5 → filled=5 → 4 `=` + `>` + 5 spaces
        assert_eq!(bar, "[====>     ]");
    }

    #[test]
    fn test_overview_gauge_bar_full() {
        let bar = gauge_bar(2, 2);
        // ratio=1.0 → filled=10 → 9 `=` + `>` + 0 spaces
        assert_eq!(bar, "[=========>]");
    }

    #[test]
    fn test_overview_gauge_bar_zero_max() {
        let bar = gauge_bar(0, 0);
        assert_eq!(bar, "[          ]");
    }

    #[test]
    fn test_overview_format_duration_seconds() {
        assert_eq!(format_duration(0), "0s");
        assert_eq!(format_duration(29), "29s");
        assert_eq!(format_duration(59), "59s");
    }

    #[test]
    fn test_overview_format_duration_minutes() {
        assert_eq!(format_duration(60), "1m");
        assert_eq!(format_duration(3599), "59m");
    }

    #[test]
    fn test_overview_format_duration_hours() {
        assert_eq!(format_duration(3600), "1h");
        assert_eq!(format_duration(7200), "2h");
    }

    #[test]
    fn test_overview_status_color_known() {
        assert_eq!(status_color("Active"), Color::Yellow);
        assert_eq!(status_color("Completed"), Color::Green);
        assert_eq!(status_color("Failed"), Color::Red);
        assert_eq!(status_color("ReviewPending"), Color::Blue);
        assert_eq!(status_color("Abandoned"), Color::DarkGray);
    }

    #[test]
    fn test_overview_status_color_unknown() {
        assert_eq!(status_color("SomeOtherStatus"), Color::White);
    }
}
