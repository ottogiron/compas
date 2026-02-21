//! Executions tab — recent execution records grouped by batch.
//!
//! Renders section headers for each batch and selectable execution rows under
//! each section. The special "Unbatched" group is pinned first when present.

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
use crate::store::ExecutionRow;

pub const HISTORY_UNBATCHED_KEY: &str = "__UNBATCHED__";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistorySelectable {
    Batch(String),
    Execution(usize),
}

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
            Span::raw(": drill/open "),
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
    let selectables = history_selectable_targets(
        &data.executions,
        app.history_drill_batch.as_deref(),
        app.history_group_visible_limit(),
    );
    let selected = app
        .executions_selected
        .min(selectables.len().saturating_sub(1));
    let groups = group_execution_indices_by_batch(
        &data.executions,
        app.history_drill_batch.as_deref(),
        app.history_group_visible_limit(),
    );
    let mut items: Vec<ListItem<'static>> = Vec::new();
    let mut selectable_to_row: HashMap<usize, usize> = HashMap::new();
    let mut selectable_slot = 0usize;

    if let Some(batch) = app.history_drill_batch.as_deref() {
        items.push(ListItem::new(Line::from(vec![
            Span::raw(" "),
            Span::styled(
                format!("Filter: batch {}", history_batch_display(batch)),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(
                "x",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("/"),
            Span::styled(
                "Esc",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(": back"),
        ])));
        items.push(ListItem::new(Line::from(Span::raw(""))));
    }

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
        if app.history_drill_batch.is_none() {
            selectable_to_row.insert(selectable_slot, items.len());
            selectable_slot += 1;
        }
        let mut header_spans = vec![Span::styled(
            format!(" Batch {} ", group.label),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )];
        if group.hidden_count > 0 {
            header_spans.push(Span::styled(
                format!(" … +{} more", group.hidden_count),
                Style::default().fg(Color::DarkGray),
            ));
        }
        items.push(ListItem::new(Line::from(header_spans)));

        for &exec_idx in &group.indices {
            let Some(e) = data.executions.get(exec_idx) else {
                continue;
            };
            selectable_to_row.insert(selectable_slot, items.len());
            selectable_slot += 1;
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

    let selected_row = selectable_to_row.get(&selected).copied().unwrap_or(0);
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

    let mut scrollbar_state = ScrollbarState::new(selectables.len().max(1)).position(selected);
    let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
    f.render_stateful_widget(scrollbar, area, &mut scrollbar_state);
}

// ── Helpers ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct BatchGroup {
    key: String,
    label: String,
    indices: Vec<usize>,
    hidden_count: usize,
}

fn sort_execution_indices_desc(executions: &[ExecutionRow], indices: &mut [usize]) {
    indices.sort_by(|a, b| {
        let a_ref = &executions[*a];
        let b_ref = &executions[*b];
        b_ref
            .queued_at
            .cmp(&a_ref.queued_at)
            .then_with(|| b_ref.id.cmp(&a_ref.id))
    });
}

fn history_batch_key(batch_id: Option<&str>) -> String {
    batch_id
        .map(|b| b.to_string())
        .unwrap_or_else(|| HISTORY_UNBATCHED_KEY.to_string())
}

fn history_batch_display(batch_key: &str) -> String {
    if batch_key == HISTORY_UNBATCHED_KEY {
        "Unbatched".to_string()
    } else {
        batch_key.to_string()
    }
}

fn group_execution_indices_by_batch(
    executions: &[ExecutionRow],
    drill_batch: Option<&str>,
    per_group_limit: usize,
) -> Vec<BatchGroup> {
    let mut grouped: HashMap<String, Vec<usize>> = HashMap::new();
    let mut latest_ts: HashMap<String, i64> = HashMap::new();

    for (idx, e) in executions.iter().enumerate() {
        let key = history_batch_key(e.batch_id.as_deref());
        if let Some(drill) = drill_batch {
            if drill != key {
                continue;
            }
        }
        grouped.entry(key.clone()).or_default().push(idx);
        latest_ts
            .entry(key)
            .and_modify(|ts| *ts = (*ts).max(e.queued_at))
            .or_insert(e.queued_at);
    }

    let mut labels: Vec<String> = grouped.keys().cloned().collect();
    labels.sort_by(|a, b| {
        if a == HISTORY_UNBATCHED_KEY && b != HISTORY_UNBATCHED_KEY {
            return std::cmp::Ordering::Less;
        }
        if b == HISTORY_UNBATCHED_KEY && a != HISTORY_UNBATCHED_KEY {
            return std::cmp::Ordering::Greater;
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
        .map(|key| {
            let mut indices = grouped.remove(&key).unwrap_or_default();
            sort_execution_indices_desc(executions, &mut indices);
            let visible_limit = per_group_limit.max(1);
            let hidden_count = indices.len().saturating_sub(visible_limit);
            indices.truncate(visible_limit);
            BatchGroup {
                key: key.clone(),
                label: history_batch_display(&key),
                indices,
                hidden_count,
            }
        })
        .collect()
}

pub fn history_selectable_targets(
    executions: &[ExecutionRow],
    drill_batch: Option<&str>,
    per_group_limit: usize,
) -> Vec<HistorySelectable> {
    let groups = group_execution_indices_by_batch(executions, drill_batch, per_group_limit);
    let mut out = Vec::new();
    for group in groups {
        if drill_batch.is_none() {
            out.push(HistorySelectable::Batch(group.key));
        }
        out.extend(group.indices.into_iter().map(HistorySelectable::Execution));
    }
    out
}

pub fn history_selectable_count(
    executions: &[ExecutionRow],
    drill_batch: Option<&str>,
    per_group_limit: usize,
) -> usize {
    history_selectable_targets(executions, drill_batch, per_group_limit).len()
}

pub fn history_selected_target(
    executions: &[ExecutionRow],
    drill_batch: Option<&str>,
    selected: usize,
    per_group_limit: usize,
) -> Option<HistorySelectable> {
    history_selectable_targets(executions, drill_batch, per_group_limit)
        .into_iter()
        .nth(selected)
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
        let groups = group_execution_indices_by_batch(&rows, None, 50);
        assert_eq!(groups[0].label, "Unbatched");
        assert_eq!(groups[1].label, "b2");
        assert_eq!(groups[2].label, "b1");
    }

    #[test]
    fn test_group_execution_indices_in_batch_sorted_desc() {
        let mut rows = Vec::new();
        for (id, ts) in [("a", 1), ("b", 3), ("c", 2)] {
            rows.push(crate::store::ExecutionRow {
                id: id.to_string(),
                thread_id: format!("t-{id}"),
                batch_id: Some("b1".to_string()),
                agent_alias: "x".to_string(),
                dispatch_message_id: None,
                status: "completed".to_string(),
                queued_at: ts,
                picked_up_at: None,
                started_at: None,
                finished_at: None,
                duration_ms: None,
                exit_code: None,
                output_preview: None,
                error_detail: None,
                parsed_intent: None,
            });
        }
        let groups = group_execution_indices_by_batch(&rows, Some("b1"), 50);
        assert_eq!(groups.len(), 1);
        let ids: Vec<String> = groups[0]
            .indices
            .iter()
            .map(|i| rows[*i].id.clone())
            .collect();
        assert_eq!(ids, vec!["b".to_string(), "c".to_string(), "a".to_string()]);
    }

    #[test]
    fn test_selectable_targets_include_batch_headers_when_not_drilled() {
        let rows = vec![crate::store::ExecutionRow {
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
        }];
        let targets = history_selectable_targets(&rows, None, 50);
        assert!(matches!(targets.first(), Some(HistorySelectable::Batch(_))));
    }

    #[test]
    fn test_selectable_targets_hide_batch_headers_when_drilled() {
        let rows = vec![crate::store::ExecutionRow {
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
        }];
        let targets = history_selectable_targets(&rows, Some("b1"), 50);
        assert!(targets
            .iter()
            .all(|t| matches!(t, HistorySelectable::Execution(_))));
    }
}
