//! Executions tab — recent execution records grouped by batch.
//!
//! Renders section headers for each batch and selectable execution rows under
//! each section. The special "No batch" group is shown last when present.

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
use std::collections::HashMap;

use crate::dashboard::app::App;
use crate::dashboard::views::{exec_status_color, format_duration_ms, humanize_exec_status};

// ── Entry point ───────────────────────────────────────────────────────────────

/// Render the Executions tab into `area`.
pub fn render_executions(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .style(Style::default().bg(Color::Black).fg(Color::White))
        .title(" Executions ")
        .title_bottom(Line::from(vec![
            Span::raw(" "),
            Span::styled(
                "↑/↓",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(": select  "),
            Span::styled(
                "Enter",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(": view log "),
            Span::raw("  "),
            Span::styled("L/U", Style::default().fg(Color::Cyan)),
            Span::raw(": linked/unlinked"),
        ]));

    // ── No data yet ──────────────────────────────────────────────────────────
    let Some(data) = &app.executions_data else {
        let p = Paragraph::new(Line::from(Span::styled(
            "  Fetching…",
            Style::default().fg(Color::DarkGray),
        )))
        .style(Style::default().bg(Color::Black).fg(Color::White))
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
        .style(Style::default().bg(Color::Black).fg(Color::White))
        .block(block);
        f.render_widget(p, area);
        return;
    }

    // ── Build grouped list ────────────────────────────────────────────────────
    let selected = app
        .executions_selected
        .min(data.executions.len().saturating_sub(1));
    let groups = group_execution_indices_by_batch(&data.executions);
    let mut items: Vec<ListItem<'static>> = Vec::new();
    let mut exec_to_row: HashMap<usize, usize> = HashMap::new();

    items.push(ListItem::new(Line::from(vec![
        Span::styled(
            format!("{:<14}", "Agent"),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{:<19}", "Thread ID"),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{:<13}", "Status"),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{:<6}", "Prov"),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{:<10}", "Duration"),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{:<6}", "Exit"),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "Error",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
    ])));
    items.push(ListItem::new(Line::from(Span::raw(""))));

    for group in groups {
        items.push(ListItem::new(Line::from(vec![
            Span::styled(
                format!(" Batch {} ", group.label),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("({})", group.indices.len()),
                Style::default().fg(Color::Cyan),
            ),
        ])));

        for &exec_idx in &group.indices {
            let Some(e) = data.executions.get(exec_idx) else {
                continue;
            };
            exec_to_row.insert(exec_idx, items.len());
            let status_color = exec_status_color(&e.status);
            let thread_id = truncate(&e.thread_id, 16);
            let duration = e
                .duration_ms
                .map(format_duration_ms)
                .unwrap_or_else(|| "-".to_string());
            let provenance = if e.dispatch_message_id.is_some() {
                "L".to_string()
            } else {
                "U".to_string()
            };
            let exit_code = e
                .exit_code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "-".to_string());
            let error_preview = e
                .error_detail
                .as_deref()
                .map(|s| truncate(s, 40))
                .unwrap_or_else(|| "-".to_string());

            items.push(ListItem::new(Line::from(vec![
                Span::raw(" "),
                Span::styled(
                    format!("{:<14}", e.agent_alias),
                    Style::default().fg(Color::White),
                ),
                Span::styled(
                    format!("{:<19}", thread_id),
                    Style::default().fg(Color::White),
                ),
                Span::styled(
                    format!("{:<13}", humanize_exec_status(&e.status)),
                    Style::default().fg(status_color),
                ),
                Span::styled(
                    format!("{:<6}", provenance),
                    Style::default().fg(if e.dispatch_message_id.is_some() {
                        Color::Green
                    } else {
                        Color::Red
                    }),
                ),
                Span::styled(
                    format!("{:<10}", duration),
                    Style::default().fg(Color::White),
                ),
                Span::styled(
                    format!("{:<6}", exit_code),
                    Style::default().fg(Color::White),
                ),
                Span::styled(error_preview, Style::default().fg(Color::White)),
            ])));
        }

        items.push(ListItem::new(Line::from(Span::raw(""))));
    }

    let selected_row = exec_to_row.get(&selected).copied().unwrap_or(0);
    let mut state = ListState::default().with_selected(Some(selected_row));
    let list = List::new(items)
        .block(block)
        .style(Style::default().bg(Color::Black).fg(Color::White))
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        );
    f.render_stateful_widget(list, area, &mut state);

    let mut scrollbar_state = ScrollbarState::new(data.executions.len().max(1)).position(selected);
    let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
    f.render_stateful_widget(scrollbar, area, &mut scrollbar_state);
}

// ── Helpers ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct BatchGroup {
    label: String,
    indices: Vec<usize>,
}

fn group_execution_indices_by_batch(executions: &[crate::store::ExecutionRow]) -> Vec<BatchGroup> {
    let mut grouped: HashMap<String, Vec<usize>> = HashMap::new();
    let mut latest_ts: HashMap<String, i64> = HashMap::new();

    for (idx, e) in executions.iter().enumerate() {
        let key = e.batch_id.clone().unwrap_or_else(|| "No batch".to_string());
        grouped.entry(key.clone()).or_default().push(idx);
        latest_ts
            .entry(key)
            .and_modify(|ts| *ts = (*ts).max(e.queued_at))
            .or_insert(e.queued_at);
    }

    let mut labels: Vec<String> = grouped.keys().cloned().collect();
    labels.sort_by(|a, b| {
        if a == "No batch" && b != "No batch" {
            return std::cmp::Ordering::Greater;
        }
        if b == "No batch" && a != "No batch" {
            return std::cmp::Ordering::Less;
        }
        latest_ts
            .get(b)
            .copied()
            .unwrap_or(0)
            .cmp(&latest_ts.get(a).copied().unwrap_or(0))
            .then_with(|| a.cmp(b))
    });

    labels
        .into_iter()
        .map(|label| BatchGroup {
            indices: grouped.remove(&label).unwrap_or_default(),
            label,
        })
        .collect()
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

    #[test]
    fn test_group_execution_indices_by_batch_orders_by_latest_queued_at() {
        let rows = vec![
            crate::store::ExecutionRow {
                id: "1".to_string(),
                thread_id: "t1".to_string(),
                batch_id: Some("b1".to_string()),
                agent_alias: "a".to_string(),
                dispatch_message_id: None,
                status: "completed".to_string(),
                queued_at: 10,
                picked_up_at: None,
                started_at: None,
                finished_at: None,
                duration_ms: None,
                exit_code: None,
                output_preview: None,
                error_detail: None,
                parsed_intent: None,
            },
            crate::store::ExecutionRow {
                id: "2".to_string(),
                thread_id: "t2".to_string(),
                batch_id: Some("b2".to_string()),
                agent_alias: "a".to_string(),
                dispatch_message_id: None,
                status: "completed".to_string(),
                queued_at: 20,
                picked_up_at: None,
                started_at: None,
                finished_at: None,
                duration_ms: None,
                exit_code: None,
                output_preview: None,
                error_detail: None,
                parsed_intent: None,
            },
            crate::store::ExecutionRow {
                id: "3".to_string(),
                thread_id: "t3".to_string(),
                batch_id: None,
                agent_alias: "a".to_string(),
                dispatch_message_id: None,
                status: "completed".to_string(),
                queued_at: 30,
                picked_up_at: None,
                started_at: None,
                finished_at: None,
                duration_ms: None,
                exit_code: None,
                output_preview: None,
                error_detail: None,
                parsed_intent: None,
            },
        ];
        let groups = group_execution_indices_by_batch(&rows);
        assert_eq!(groups[0].label, "b2");
        assert_eq!(groups[1].label, "b1");
        assert_eq!(groups[2].label, "No batch");
    }
}
