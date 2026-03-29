//! Agents tab — one card per configured agent showing health, activity, and
//! recent execution results.
//!
//! Layout (vertical list of agent cards):
//!   ┌ focused ──────────────────────────────────────────────────────────────┐
//!   │  ● focused       claude / sonnet-4 / worker                           │
//!   │  Active: 1                                                            │
//!   │  Completed (1.2s)  3m ago                                             │
//!   │  Failed (-)  5m ago                                                   │
//!   │  Completed (890ms)  12m ago                                           │
//!   └──────────────────────────────────────────────────────────────────────┘

use ratatui::{
    layout::Rect,
    style::{Style, Stylize},
    text::{Line, Span},
    widgets::{
        List, ListItem, ListState, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
    },
    Frame,
};

use crate::config::types::{AgentConfig, AgentRole};
use crate::dashboard::app::App;
use crate::dashboard::theme::{self, *};
use crate::dashboard::views::{
    format_cost_usd, format_duration_ms, format_tokens, humanize_exec_status,
};

// ── Entry point ───────────────────────────────────────────────────────────────

/// Render the Agents tab into `area`.
///
/// Builds one card per configured agent, stacked vertically with separators.
/// Card height varies based on recent execution count.
pub fn render_agents_tab(f: &mut Frame, app: &App, area: Rect) {
    let cfg = app.config.load();
    if cfg.agents.is_empty() {
        let p = Paragraph::new(Line::from("  No agents configured.".fg(TEXT_DIM)))
            .style(Style::new().bg(BG_PANEL).fg(TEXT_NORMAL))
            .block(theme::panel("Agents"));
        f.render_widget(p, area);
        return;
    }

    let n = cfg.agents.len();
    let selected = app.agents_selected.min(n.saturating_sub(1));
    let mut items: Vec<ListItem> = Vec::new();
    // Track per-agent card item indices so we can compute click geometry.
    let mut card_item_indices: Vec<usize> = Vec::with_capacity(n);
    for (idx, agent) in cfg.agents.iter().enumerate() {
        if idx > 0 {
            // Dynamic width: long enough string that ratatui clips to terminal width.
            items.push(ListItem::new(Line::from("─".repeat(200).fg(BORDER_DIM))));
        }
        card_item_indices.push(items.len());
        items.push(build_agent_item(app, agent));
    }

    // Capture per-item heights before List::new() consumes them.
    let item_heights: Vec<usize> = items.iter().map(|item| item.height()).collect();

    let mut state = ListState::default();
    state.select(Some(selected.saturating_mul(2)));
    let list = List::new(items)
        .block(theme::panel("Agents"))
        .highlight_style(Style::new().bg(BG_HIGHLIGHT))
        .style(Style::new().bg(BG_PANEL).fg(TEXT_NORMAL));
    f.render_stateful_widget(list, area, &mut state);

    // Account for separator rows between cards: n cards + (n-1) separators
    let mut scrollbar_state =
        ScrollbarState::new((2 * n).saturating_sub(1).max(1)).position(selected.saturating_mul(2));
    let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
        .thumb_style(theme::scrollbar_thumb_style())
        .track_style(theme::scrollbar_track_style());
    f.render_stateful_widget(scrollbar, area, &mut scrollbar_state);

    // ── Populate click cache ────────────────────────────────────────────────
    // The list area inside the bordered block is where clicks land.
    let inner = theme::panel("Agents").inner(area);
    {
        let mut card_geometry = Vec::with_capacity(n);
        for &item_idx in &card_item_indices {
            let offset: usize = item_heights[..item_idx].iter().sum();
            let height = item_heights.get(item_idx).copied().unwrap_or(1);
            card_geometry.push((offset, height));
        }
        let mut cache = app.agents_click_cache.borrow_mut();
        cache.card_geometry = card_geometry;
        cache.list_rect = inner;
    }
}

// ── Agent card ────────────────────────────────────────────────────────────────

fn build_agent_item(app: &App, agent: &AgentConfig) -> ListItem<'static> {
    // ── Health dot colour based on circuit breaker state ────────────────────────
    let health_color = app
        .agents_data
        .as_ref()
        .and_then(|d| {
            d.circuit_states
                .iter()
                .find(|(b, _, _)| *b == agent.backend)
                .map(|(_, state, _)| match state.as_str() {
                    "open" => FAILURE,
                    "half_open" => ACCENT,
                    _ => SUCCESS, // "closed" or unknown → healthy
                })
        })
        // Fall back to worker heartbeat when no CB data is available.
        .or_else(|| {
            app.agents_data
                .as_ref()
                .and_then(|d| d.heartbeat_age_secs)
                .map(|age| if age < 30 { SUCCESS } else { FAILURE })
        })
        .unwrap_or(TEXT_DIM); // no data yet

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
        None => vec![Line::from("  Fetching…".fg(TEXT_DIM))],
        Some(execs) if execs.is_empty() => {
            vec![Line::from("  No recent executions.".fg(TEXT_DIM))]
        }
        Some(execs) => {
            let now_unix = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64;
            execs
                .iter()
                .map(|e| {
                    let dur_label = e
                        .duration_ms
                        .map(format_duration_ms)
                        .unwrap_or_else(|| "-".to_string());
                    let color = theme::exec_status_color(&e.status);
                    let mut spans: Vec<Span<'static>> = vec![
                        Span::from("  "),
                        format!("{} ({})", humanize_exec_status(&e.status), dur_label).fg(color),
                    ];
                    // Append relative timestamp when available.
                    if let Some(finished) = e.finished_at {
                        let ago = format_relative_time(now_unix - finished);
                        spans.push(Span::from("  "));
                        spans.push(ago.fg(TEXT_DIM));
                    }
                    Line::from(spans)
                })
                .collect()
        }
    };

    // ── Role / model labels ───────────────────────────────────────────────────
    let role_label = match agent.role {
        AgentRole::Worker => "worker",
        AgentRole::Operator => "operator",
    };
    let model_label = agent.model.as_deref().unwrap_or("-");

    // ── Cost/token lookup for this agent ─────────────────────────────────────
    let agent_cost = app.agents_data.as_ref().and_then(|d| {
        d.cost_by_agent
            .iter()
            .find(|c| c.agent_alias == agent.alias)
    });

    // ── Circuit breaker state for this agent's backend ──────────────────────
    let circuit_info = app.agents_data.as_ref().and_then(|d| {
        d.circuit_states
            .iter()
            .find(|(b, _, _)| *b == agent.backend)
            .map(|(_, state, _failures)| state.clone())
    });

    // ── Assemble card lines ───────────────────────────────────────────────────
    // Compact format: ● alias       backend / model / role     CB:STATE
    let mut row1_spans: Vec<Span<'static>> = vec![
        Span::from("  "),
        "● ".fg(health_color),
        format!("{:<12}", agent.alias).fg(TEXT_BRIGHT).bold(),
        Span::from(agent.backend.to_string()),
        " / ".fg(BORDER_DIM),
        Span::from(model_label.to_string()),
        " / ".fg(BORDER_DIM),
        Span::from(role_label.to_string()),
    ];

    // Append circuit breaker indicator when not closed.
    if let Some(ref cb_state) = circuit_info {
        match cb_state.as_str() {
            "open" => {
                row1_spans.push("     ".into());
                row1_spans.push("CB:OPEN".fg(FAILURE).bold());
            }
            "half_open" => {
                row1_spans.push("     ".into());
                row1_spans.push("CB:PROBE".fg(ACCENT).bold());
            }
            _ => {} // "closed" — no indicator
        }
    }

    let mut lines: Vec<Line> = vec![
        // Row 1: config summary (with optional circuit breaker indicator)
        Line::from(row1_spans),
        // Row 2: active execution count
        Line::from(vec![
            Span::from("  Active: "),
            format!("{active_count}")
                .fg(if active_count > 0 { ACCENT } else { TEXT_DIM })
                .bold(),
        ]),
    ];

    // Row 3 (optional): cost / token summary
    if let Some(cost) = agent_cost {
        let has_cost = cost.total_cost_usd > 0.0;
        let has_tokens = cost.total_tokens_in > 0 || cost.total_tokens_out > 0;
        if has_cost || has_tokens {
            let mut cost_spans: Vec<Span<'static>> = vec![Span::from("  ")];
            if has_cost {
                cost_spans.push(format_cost_usd(cost.total_cost_usd).fg(TEXT_BRIGHT).bold());
                cost_spans.push("  │  ".fg(BORDER_DIM));
            }
            if has_tokens {
                cost_spans.push(format_tokens(cost.total_tokens_in).fg(TEXT_BRIGHT));
                cost_spans.push(" in  │  ".fg(BORDER_DIM));
                cost_spans.push(format_tokens(cost.total_tokens_out).fg(TEXT_BRIGHT));
                cost_spans.push(" out".fg(TEXT_DIM));
            }
            lines.push(Line::from(cost_spans));
        }
    }

    // Rows 4+: recent execution summaries (up to 3)
    lines.extend(recent_lines);

    ListItem::new(lines)
}

// ── Helpers ───────��──────────────────────────────────────────────────────────

/// Format an age in seconds to a compact relative label: `"3m ago"`, `"2h ago"`, etc.
fn format_relative_time(age_secs: i64) -> String {
    let age = age_secs.max(0);
    if age < 60 {
        format!("{}s ago", age)
    } else if age < 3_600 {
        format!("{}m ago", age / 60)
    } else if age < 86_400 {
        format!("{}h ago", age / 3_600)
    } else {
        format!("{}d ago", age / 86_400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_relative_time_seconds() {
        assert_eq!(format_relative_time(0), "0s ago");
        assert_eq!(format_relative_time(30), "30s ago");
        assert_eq!(format_relative_time(59), "59s ago");
    }

    #[test]
    fn test_format_relative_time_minutes() {
        assert_eq!(format_relative_time(60), "1m ago");
        assert_eq!(format_relative_time(180), "3m ago");
        assert_eq!(format_relative_time(3599), "59m ago");
    }

    #[test]
    fn test_format_relative_time_hours() {
        assert_eq!(format_relative_time(3600), "1h ago");
        assert_eq!(format_relative_time(7200), "2h ago");
        assert_eq!(format_relative_time(86399), "23h ago");
    }

    #[test]
    fn test_format_relative_time_days() {
        assert_eq!(format_relative_time(86400), "1d ago");
        assert_eq!(format_relative_time(172800), "2d ago");
    }

    #[test]
    fn test_format_relative_time_negative_clamps_to_zero() {
        assert_eq!(format_relative_time(-5), "0s ago");
        assert_eq!(format_relative_time(-100), "0s ago");
    }
}
