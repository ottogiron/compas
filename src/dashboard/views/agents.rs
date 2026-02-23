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
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, List, ListItem, ListState, Paragraph, Scrollbar, ScrollbarOrientation,
        ScrollbarState,
    },
    Frame,
};

use crate::config::types::{AgentConfig, AgentRole};
use crate::dashboard::app::App;
use crate::dashboard::views::{exec_status_color, format_duration_ms, humanize_exec_status};

// ── Entry point ───────────────────────────────────────────────────────────────

/// Render the Agents tab into `area`.
///
/// Builds one bordered card per configured agent, stacked vertically.
/// Each card is `CARD_HEIGHT` rows tall; any leftover space at the bottom
/// is left blank.
pub fn render_agents_tab(f: &mut Frame, app: &App, area: Rect) {
    let cfg = app.config.load();
    if cfg.agents.is_empty() {
        let p = Paragraph::new(Line::from(Span::styled(
            "  No agents configured.",
            Style::default().fg(Color::DarkGray),
        )))
        .style(Style::default().bg(Color::Black).fg(Color::White))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .style(Style::default().bg(Color::Black).fg(Color::White))
                .title(" Agents "),
        );
        f.render_widget(p, area);
        return;
    }

    let n = cfg.agents.len();
    let selected = app.agents_selected.min(n.saturating_sub(1));
    let mut items: Vec<ListItem> = Vec::new();
    for (idx, agent) in cfg.agents.iter().enumerate() {
        if idx > 0 {
            items.push(ListItem::new(Line::from(Span::styled(
                "────────────────────────────────────────────────────────────────────────────",
                Style::default().fg(Color::DarkGray),
            ))));
        }
        items.push(build_agent_item(app, agent));
    }

    let mut state = ListState::default();
    state.select(Some(selected.saturating_mul(2)));
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .style(Style::default().bg(Color::Black).fg(Color::White))
                .title(" Agents "),
        )
        .highlight_style(Style::default().bg(Color::DarkGray))
        .style(Style::default().bg(Color::Black).fg(Color::White));
    f.render_stateful_widget(list, area, &mut state);

    let mut scrollbar_state = ScrollbarState::new(n.max(1)).position(selected);
    let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
    f.render_stateful_widget(scrollbar, area, &mut scrollbar_state);
}

// ── Agent card ────────────────────────────────────────────────────────────────

fn build_agent_item(app: &App, agent: &AgentConfig) -> ListItem<'static> {
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
                    .map(format_duration_ms)
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
        // Row 1: config summary
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

    ListItem::new(lines)
}
