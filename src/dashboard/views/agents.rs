//! Agents tab — one card per configured agent showing health, activity, and
//! recent execution results.
//!
//! Layout (vertical list of agent cards):
//!   ┌ focused ───────────────────────────────────────────────────────────────┐
//!   │  ● focused       │ backend: claude  │ model: sonnet  │ role: worker    │
//!   │  Active: 1                                                              │
//!   │  completed (1234ms)                                                     │
//!   │  failed (-)                                                             │
//!   │  completed (890ms)                                                      │
//!   └────────────────────────────────────────────────────────────────────────┘

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::config::types::{AgentConfig, AgentRole};
use crate::dashboard::app::App;
use crate::dashboard::views::humanize_exec_status;

// ── Entry point ───────────────────────────────────────────────────────────────

/// Render the Agents tab into `area`.
///
/// Builds one bordered card per configured agent, stacked vertically.
/// Each card is `CARD_HEIGHT` rows tall; any leftover space at the bottom
/// is left blank.
pub fn render_agents_tab(f: &mut Frame, app: &App, area: Rect) {
    if app.config.agents.is_empty() {
        let p = Paragraph::new(Line::from(Span::styled(
            "  No agents configured.",
            Style::default().fg(Color::DarkGray),
        )))
        .block(Block::default().borders(Borders::ALL).title(" Agents "));
        f.render_widget(p, area);
        return;
    }

    // Each card: border-top + 4 content rows + border-bottom = 6 rows.
    const CARD_HEIGHT: u16 = 6;

    let n = app.config.agents.len();
    let mut constraints: Vec<Constraint> =
        (0..n).map(|_| Constraint::Length(CARD_HEIGHT)).collect();
    // Absorb any remaining vertical space so ratatui doesn't complain.
    constraints.push(Constraint::Min(0));

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    for (i, agent) in app.config.agents.iter().enumerate() {
        render_agent_card(f, app, agent, chunks[i]);
    }
}

// ── Agent card ────────────────────────────────────────────────────────────────

fn render_agent_card(f: &mut Frame, app: &App, agent: &AgentConfig, area: Rect) {
    // ── Health dot colour based on heartbeat age ──────────────────────────────
    let health_color = app
        .agents_data
        .as_ref()
        .and_then(|d| d.heartbeat_age_secs)
        .map(|age| if age < 30 { Color::Green } else { Color::Red })
        .unwrap_or(Color::DarkGray); // no data yet

    // ── Active execution count for this agent ─────────────────────────────────
    let active_count: i64 = app
        .agents_data
        .as_ref()
        .and_then(|d| {
            d.active_counts
                .iter()
                .find(|(a, _)| a == &agent.alias)
                .map(|(_, c)| *c)
        })
        .unwrap_or(0);

    // ── Recent execution summary lines for this agent ─────────────────────────
    let recent_lines: Vec<Line> = match app
        .agents_data
        .as_ref()
        .and_then(|d| {
            d.executions_by_agent
                .iter()
                .find(|(a, _)| a == &agent.alias)
        })
        .map(|(_, execs)| execs)
    {
        None => vec![Line::from(Span::styled(
            "  Fetching…",
            Style::default().fg(Color::DarkGray),
        ))],
        Some(execs) if execs.is_empty() => vec![Line::from(Span::styled(
            "  No recent executions.",
            Style::default().fg(Color::DarkGray),
        ))],
        Some(execs) => execs
            .iter()
            .map(|e| {
                let dur_label = e
                    .duration_ms
                    .map(|ms| format!("{}ms", ms))
                    .unwrap_or_else(|| "-".to_string());
                let color = exec_status_color(&e.status);
                Line::from(vec![
                    Span::raw("  "),
                    Span::styled(
                        format!("{} ({})", humanize_exec_status(&e.status), dur_label),
                        Style::default().fg(color),
                    ),
                ])
            })
            .collect(),
    };

    // ── Role / model labels ───────────────────────────────────────────────────
    let role_label = match agent.role {
        AgentRole::Worker => "worker",
        AgentRole::Operator => "operator",
    };
    let model_label = agent.model.as_deref().unwrap_or("-");

    // ── Assemble card lines ───────────────────────────────────────────────────
    let mut lines: Vec<Line> = vec![
        // Row 1: identity / config summary
        Line::from(vec![
            Span::raw("  "),
            Span::styled("● ", Style::default().fg(health_color)),
            Span::styled(
                format!("{:<12}", agent.alias),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("│ ", Style::default().fg(Color::DarkGray)),
            Span::raw(format!("backend: {}  ", agent.backend)),
            Span::styled("│ ", Style::default().fg(Color::DarkGray)),
            Span::raw(format!("model: {}  ", model_label)),
            Span::styled("│ ", Style::default().fg(Color::DarkGray)),
            Span::raw(format!("role: {}", role_label)),
        ]),
        // Row 2: active execution count
        Line::from(vec![
            Span::raw("  Active: "),
            Span::styled(
                format!("{}", active_count),
                Style::default()
                    .fg(if active_count > 0 {
                        Color::Yellow
                    } else {
                        Color::DarkGray
                    })
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
    ];

    // Rows 3+: recent execution summaries (up to 3)
    lines.extend(recent_lines);

    let title = format!(" {} ", agent.alias);
    let p = Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(p, area);
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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agents_exec_status_color_completed() {
        assert_eq!(exec_status_color("completed"), Color::Green);
    }

    #[test]
    fn test_agents_exec_status_color_failed() {
        assert_eq!(exec_status_color("failed"), Color::Red);
    }

    #[test]
    fn test_agents_exec_status_color_crashed() {
        assert_eq!(exec_status_color("crashed"), Color::Red);
    }

    #[test]
    fn test_agents_exec_status_color_timed_out() {
        assert_eq!(exec_status_color("timed_out"), Color::Red);
    }

    #[test]
    fn test_agents_exec_status_color_executing() {
        assert_eq!(exec_status_color("executing"), Color::Yellow);
    }

    #[test]
    fn test_agents_exec_status_color_picked_up() {
        assert_eq!(exec_status_color("picked_up"), Color::Yellow);
    }

    #[test]
    fn test_agents_exec_status_color_queued() {
        assert_eq!(exec_status_color("queued"), Color::Cyan);
    }

    #[test]
    fn test_agents_exec_status_color_cancelled() {
        assert_eq!(exec_status_color("cancelled"), Color::DarkGray);
    }

    #[test]
    fn test_agents_exec_status_color_unknown() {
        assert_eq!(exec_status_color("some_other_status"), Color::White);
    }
}
