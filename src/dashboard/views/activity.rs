//! Ops tab — unified selectable control-plane list with Running, Active Batches,
//! Active Threads, Batches, and Recently Completed.
//!
//! Layout: full-width grouped list with keyboard selection, section counts,
//! and inline detail sub-lines for the selected item.

use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{
        List, ListItem, ListState, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
    },
    Frame,
};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::dashboard::app::{ActivityData, App};
use crate::dashboard::theme;
use crate::dashboard::views::{
    format_cost_usd, format_duration_ms, format_duration_secs, format_tokens, humanize_exec_status,
    humanize_thread_status,
};
use crate::store::{MergeOperation, ThreadStatusView};

const RECENTLY_COMPLETED_LIMIT: usize = 12;
const OPS_BATCH_SECTION_LIMIT: usize = 10;
const MERGE_QUEUE_RECENT_LIMIT: usize = 5;
/// Terminal merge ops (completed/cancelled) visible for this many seconds.
const MERGE_TERMINAL_WINDOW_SECS: i64 = 600; // 10 minutes
/// Failed merge ops persist longer in the list.
const MERGE_FAILED_WINDOW_SECS: i64 = 1800; // 30 minutes

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpsSelectable {
    Thread(usize),
    Batch(String),
    MergeOp(String),
}

#[derive(Debug, Clone)]
struct BatchProgress {
    batch_id: String,
    completed: usize,
    total: usize,
    active: usize,
    failed: usize,
    oldest_active_updated_at: Option<i64>,
    latest_updated_at: i64,
}

#[derive(Debug, Default)]
struct ClassifiedRows {
    running: Vec<usize>,
    scheduled: Vec<usize>,
    active_threads: Vec<usize>,
    uncategorized: Vec<usize>,
    recently_completed: Vec<usize>,
    active_batches: Vec<BatchProgress>,
    batches: Vec<BatchProgress>,
}

fn is_running_now(t: &ThreadStatusView) -> bool {
    t.execution_status
        .as_deref()
        .map(super::is_running_exec_status)
        .unwrap_or(false)
}

fn is_stale_active(t: &ThreadStatusView, now_unix: i64, stale_after_secs: i64) -> bool {
    t.thread_status == "Active"
        && !is_running_now(t)
        && (now_unix - t.thread_updated_at).max(0) >= stale_after_secs
}

fn is_active_waiting(t: &ThreadStatusView, now_unix: i64, stale_after_secs: i64) -> bool {
    t.thread_status == "Active"
        && !is_running_now(t)
        && !is_stale_active(t, now_unix, stale_after_secs)
}

fn is_recently_completed(t: &ThreadStatusView) -> bool {
    t.execution_status.as_deref() == Some("completed") || t.thread_status == "Completed"
}

fn is_scheduled(t: &ThreadStatusView, now_unix: i64) -> bool {
    t.execution_status.as_deref() == Some("queued") && t.eligible_at.is_some_and(|ea| ea > now_unix)
}

fn classify_rows(
    rows: &[ThreadStatusView],
    drill_batch: Option<&str>,
    now_unix: i64,
    stale_after_secs: i64,
) -> ClassifiedRows {
    let filtered: Vec<(usize, &ThreadStatusView)> = rows
        .iter()
        .enumerate()
        .filter(|(_, r)| match drill_batch {
            Some(batch) => r.batch_id.as_deref() == Some(batch),
            None => true,
        })
        .collect();

    let mut out = ClassifiedRows::default();

    for (idx, row) in &filtered {
        if is_running_now(row) {
            out.running.push(*idx);
        } else if is_scheduled(row, now_unix) {
            out.scheduled.push(*idx);
        } else if is_active_waiting(row, now_unix, stale_after_secs) {
            out.active_threads.push(*idx);
        } else if is_recently_completed(row) {
            out.recently_completed.push(*idx);
        } else {
            out.uncategorized.push(*idx);
        }
    }

    sort_indices_by_updated(rows, &mut out.running);
    sort_scheduled_by_eligible_at(rows, &mut out.scheduled);
    sort_indices_by_updated(rows, &mut out.active_threads);
    sort_indices_by_updated(rows, &mut out.uncategorized);
    sort_indices_by_updated(rows, &mut out.recently_completed);

    if drill_batch.is_none() {
        out.batches = batch_progress(rows, now_unix, stale_after_secs);
        out.active_batches = out
            .batches
            .iter()
            .filter(|b| b.active > 0)
            .cloned()
            .collect();
    }

    out
}

fn capped_recently_completed(indices: &[usize]) -> &[usize] {
    let end = indices.len().min(RECENTLY_COMPLETED_LIMIT);
    &indices[..end]
}

fn capped_batches(batches: &[BatchProgress]) -> &[BatchProgress] {
    let end = batches.len().min(OPS_BATCH_SECTION_LIMIT);
    &batches[..end]
}

/// Filter merge ops into active and recently-terminal lists.
///
/// Active: queued, claimed, executing.
/// Recent terminal: completed/failed/cancelled within time window.
/// Failed ops use a longer persistence window (30 min) vs other terminal (10 min).
///
/// Supersession: a failed op is excluded when a completed op with the same
/// `(thread_id, target_branch)` has a `finished_at` >= the failed op's
/// `finished_at`. This only considers the slice passed in — the caller is
/// responsible for providing a representative window of ops.
fn filter_merge_ops(ops: &[MergeOperation], now_unix: i64) -> Vec<&MergeOperation> {
    let mut out: Vec<&MergeOperation> = Vec::new();
    let mut terminal: Vec<&MergeOperation> = Vec::new();

    // Build a lookup of the latest completed finished_at per (thread_id, target_branch).
    let mut completed_times: HashMap<(&str, &str), i64> = HashMap::new();
    for op in ops {
        if op.status.as_str() == "completed" {
            let finished = op.finished_at.unwrap_or(op.queued_at);
            let key = (op.thread_id.as_str(), op.target_branch.as_str());
            let entry = completed_times.entry(key).or_insert(finished);
            if finished > *entry {
                *entry = finished;
            }
        }
    }

    for op in ops {
        match op.status.as_str() {
            "queued" | "claimed" | "executing" => out.push(op),
            "failed" => {
                let finished = op.finished_at.unwrap_or(op.queued_at);
                if (now_unix - finished) < MERGE_FAILED_WINDOW_SECS {
                    // Superseded: a completed op for the same pair finished at or after this failure.
                    let key = (op.thread_id.as_str(), op.target_branch.as_str());
                    let superseded = completed_times
                        .get(&key)
                        .is_some_and(|&completed_t| completed_t >= finished);
                    if !superseded {
                        terminal.push(op);
                    }
                }
            }
            "completed" | "cancelled" => {
                let finished = op.finished_at.unwrap_or(op.queued_at);
                if (now_unix - finished) < MERGE_TERMINAL_WINDOW_SECS {
                    terminal.push(op);
                }
            }
            _ => {}
        }
    }

    // Sort terminal by finished_at desc, cap at limit
    terminal.sort_by(|a, b| {
        let a_t = a.finished_at.unwrap_or(a.queued_at);
        let b_t = b.finished_at.unwrap_or(b.queued_at);
        b_t.cmp(&a_t)
    });
    terminal.truncate(MERGE_QUEUE_RECENT_LIMIT);
    out.extend(terminal);
    out
}

fn sort_indices_by_updated(rows: &[ThreadStatusView], indices: &mut [usize]) {
    indices.sort_by(|a, b| {
        let a_ts = rows.get(*a).map(|r| r.thread_updated_at).unwrap_or(0);
        let b_ts = rows.get(*b).map(|r| r.thread_updated_at).unwrap_or(0);
        b_ts.cmp(&a_ts)
    });
}

/// Sort scheduled items by `eligible_at` ascending (soonest first).
fn sort_scheduled_by_eligible_at(rows: &[ThreadStatusView], indices: &mut [usize]) {
    indices.sort_by(|a, b| {
        let a_ea = rows.get(*a).and_then(|r| r.eligible_at).unwrap_or(i64::MAX);
        let b_ea = rows.get(*b).and_then(|r| r.eligible_at).unwrap_or(i64::MAX);
        a_ea.cmp(&b_ea)
    });
}

fn batch_progress(
    rows: &[ThreadStatusView],
    now_unix: i64,
    stale_after_secs: i64,
) -> Vec<BatchProgress> {
    #[derive(Default)]
    struct Agg {
        completed: usize,
        total: usize,
        active: usize,
        failed: usize,
        oldest_active_updated_at: Option<i64>,
        latest_updated_at: i64,
    }

    let mut map: HashMap<String, Agg> = HashMap::new();

    for r in rows {
        let Some(batch_id) = r.batch_id.as_deref() else {
            continue;
        };
        if batch_id.is_empty() {
            continue;
        }

        let agg = map.entry(batch_id.to_string()).or_default();
        agg.total += 1;
        agg.latest_updated_at = agg.latest_updated_at.max(r.thread_updated_at);

        let es = r.execution_status.as_deref().unwrap_or("");
        if r.thread_status == "Completed" || es == "completed" {
            agg.completed += 1;
        }
        if r.thread_status == "Failed" || matches!(es, "failed" | "timed_out" | "crashed") {
            agg.failed += 1;
        }
        if is_running_now(r) || is_active_waiting(r, now_unix, stale_after_secs) {
            agg.active += 1;
            agg.oldest_active_updated_at = Some(match agg.oldest_active_updated_at {
                Some(current) => current.min(r.thread_updated_at),
                None => r.thread_updated_at,
            });
        }
    }

    let mut out: Vec<BatchProgress> = map
        .into_iter()
        .map(|(batch_id, agg)| BatchProgress {
            batch_id,
            completed: agg.completed,
            total: agg.total,
            active: agg.active,
            failed: agg.failed,
            oldest_active_updated_at: agg.oldest_active_updated_at,
            latest_updated_at: agg.latest_updated_at,
        })
        .collect();

    out.sort_by(|a, b| {
        b.latest_updated_at
            .cmp(&a.latest_updated_at)
            .then_with(|| a.batch_id.cmp(&b.batch_id))
    });

    out
}

pub fn ops_selectable_targets(
    rows: &[ThreadStatusView],
    merge_ops: &[MergeOperation],
    drill_batch: Option<&str>,
    now_unix: i64,
    stale_after_secs: i64,
) -> Vec<OpsSelectable> {
    let classified = classify_rows(rows, drill_batch, now_unix, stale_after_secs);
    let mut out = Vec::new();

    out.extend(
        classified
            .running
            .iter()
            .copied()
            .map(OpsSelectable::Thread),
    );
    out.extend(
        classified
            .scheduled
            .iter()
            .copied()
            .map(OpsSelectable::Thread),
    );
    if drill_batch.is_none() {
        out.extend(
            capped_batches(&classified.active_batches)
                .iter()
                .map(|b| OpsSelectable::Batch(b.batch_id.clone())),
        );
    }
    out.extend(
        classified
            .active_threads
            .iter()
            .copied()
            .map(OpsSelectable::Thread),
    );
    if drill_batch.is_some() {
        out.extend(
            classified
                .uncategorized
                .iter()
                .copied()
                .map(OpsSelectable::Thread),
        );
    }
    // Merge queue — between Active Threads and Recently Completed.
    // Not shown when drilling into a batch (merge ops are not batch-scoped).
    if drill_batch.is_none() {
        let visible_merge_ops = filter_merge_ops(merge_ops, now_unix);
        out.extend(
            visible_merge_ops
                .iter()
                .map(|op| OpsSelectable::MergeOp(op.id.clone())),
        );
    }
    out.extend(
        capped_recently_completed(&classified.recently_completed)
            .iter()
            .copied()
            .map(OpsSelectable::Thread),
    );
    if drill_batch.is_none() {
        out.extend(
            capped_batches(&classified.batches)
                .iter()
                .map(|b| OpsSelectable::Batch(b.batch_id.clone())),
        );
    }

    out
}

pub fn ops_selected_target(
    rows: &[ThreadStatusView],
    merge_ops: &[MergeOperation],
    drill_batch: Option<&str>,
    selected: usize,
    now_unix: i64,
    stale_after_secs: i64,
) -> Option<OpsSelectable> {
    ops_selectable_targets(rows, merge_ops, drill_batch, now_unix, stale_after_secs)
        .into_iter()
        .nth(selected)
}

pub fn ops_selectable_count(
    rows: &[ThreadStatusView],
    merge_ops: &[MergeOperation],
    drill_batch: Option<&str>,
    now_unix: i64,
    stale_after_secs: i64,
) -> usize {
    ops_selectable_targets(rows, merge_ops, drill_batch, now_unix, stale_after_secs).len()
}

fn footer_counts(
    data: &ActivityData,
    now_unix: i64,
    stale_after_secs: i64,
) -> (i64, i64, i64, i64, i64) {
    let active = data
        .rows
        .iter()
        .filter(|r| is_running_now(r) || is_active_waiting(r, now_unix, stale_after_secs))
        .count() as i64;
    let stale = data
        .rows
        .iter()
        .filter(|r| is_stale_active(r, now_unix, stale_after_secs))
        .count() as i64;
    let scheduled = data
        .rows
        .iter()
        .filter(|r| is_scheduled(r, now_unix))
        .count() as i64;

    let mut failed = 0i64;
    let mut completed = 0i64;
    for (status, count) in &data.thread_counts {
        match status.as_str() {
            "Failed" | "failed" => failed += count,
            "Completed" | "completed" => completed += count,
            _ => {}
        }
    }
    (active, failed, completed, stale, scheduled)
}

fn build_footer_line(data: &ActivityData, now_unix: i64, stale_after_secs: i64) -> Line<'static> {
    let (active, failed, completed, stale, scheduled) =
        footer_counts(data, now_unix, stale_after_secs);

    let label = |s: &str, color: Color| -> Span<'static> {
        Span::styled(s.to_string(), Style::new().fg(color))
    };
    let val = |n: i64| -> Span<'static> {
        Span::styled(format!("{}  ", n), Style::new().fg(theme::TEXT_BRIGHT))
    };

    let mut spans: Vec<Span<'static>> = vec![
        Span::raw(" "),
        label("Active: ", theme::ACCENT),
        val(active),
        label("Stale: ", theme::TEXT_DIM),
        val(stale),
        label("Failed: ", theme::FAILURE),
        val(failed),
        label("Completed: ", theme::SUCCESS_DIM),
        val(completed),
        label("Pending: ", theme::TEXT_MUTED),
        Span::styled(
            format!("{}", data.queue_depth),
            Style::new().fg(theme::TEXT_BRIGHT),
        ),
    ];

    if scheduled > 0 {
        spans.push(Span::styled(
            "  ".to_string(),
            Style::new().fg(theme::TEXT_BRIGHT),
        ));
        spans.push(label("Sched: ", theme::TEXT_MUTED));
        spans.push(Span::styled(
            format!("{}", scheduled),
            Style::new().fg(theme::TEXT_BRIGHT),
        ));
    }

    // Merge counts: show when any active merge ops exist
    let merge_executing = data
        .merge_ops
        .iter()
        .filter(|o| o.status == "executing" || o.status == "claimed")
        .count();
    let merge_queued = data
        .merge_ops
        .iter()
        .filter(|o| o.status == "queued")
        .count();
    if merge_executing + merge_queued > 0 {
        spans.push(Span::styled(
            "  │  ".to_string(),
            Style::new().fg(theme::BORDER_DIM),
        ));
        spans.push(label("Merges: ", theme::TEXT_MUTED));
        if merge_executing > 0 {
            spans.push(Span::styled(
                format!("{}▸", merge_executing),
                Style::new().fg(theme::ACCENT),
            ));
        }
        if merge_queued > 0 {
            if merge_executing > 0 {
                spans.push(Span::styled(
                    " ".to_string(),
                    Style::new().fg(theme::TEXT_BRIGHT),
                ));
            }
            spans.push(Span::styled(
                format!("{}◌", merge_queued),
                Style::new().fg(theme::WARNING),
            ));
        }
    }

    if let Some(cost) = &data.cost_summary {
        if cost.total_cost_usd > 0.0 || cost.total_tokens_in > 0 {
            spans.push(Span::styled(
                "  │  ".to_string(),
                Style::new().fg(theme::BORDER_DIM),
            ));
            spans.push(label("Cost: ", theme::TEXT_MUTED));
            spans.push(Span::styled(
                format_cost_usd(cost.total_cost_usd),
                Style::new().fg(theme::TEXT_BRIGHT),
            ));
            spans.push(Span::styled(
                "  ".to_string(),
                Style::new().fg(theme::TEXT_BRIGHT),
            ));
            spans.push(label("Tok: ", theme::TEXT_MUTED));
            spans.push(Span::styled(
                format!(
                    "{}/{}",
                    format_tokens(cost.total_tokens_in),
                    format_tokens(cost.total_tokens_out)
                ),
                Style::new().fg(theme::TEXT_BRIGHT),
            ));
        }
    }

    Line::from(spans)
}

pub fn render_activity(f: &mut Frame, app: &App, area: Rect) {
    let now_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    let (health_str, health_color) = compute_health(&app.activity_data, now_unix);

    let block = theme::panel_focused("Ops")
        .title_top(Line::from(health_str.fg(health_color)).right_aligned());

    let Some(data) = &app.activity_data else {
        f.render_widget(
            Paragraph::new(Line::from("  Fetching...".fg(theme::TEXT_DIM)))
                .style(Style::new().bg(theme::BG_PANEL).fg(theme::TEXT_NORMAL))
                .block(block),
            area,
        );
        return;
    };

    let inner = block.inner(area);
    f.render_widget(block, area);

    let stale_after_secs = app.config.load().orchestration.stale_active_secs as i64;
    render_ops_list(f, app, data, inner, now_unix, stale_after_secs);
}

fn render_ops_list(
    f: &mut Frame,
    app: &App,
    data: &ActivityData,
    area: Rect,
    now_unix: i64,
    stale_after_secs: i64,
) {
    let [list_area, footer_area] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(area);

    let classified = classify_rows(
        &data.rows,
        app.drill_batch.as_deref(),
        now_unix,
        stale_after_secs,
    );
    let recent_indices = capped_recently_completed(&classified.recently_completed);

    let mut items: Vec<ListItem<'static>> = Vec::new();
    let mut sel_to_row: Vec<usize> = Vec::new();
    let mut selectable_slot = 0usize;
    let selected_slot = app.activity_selected.min(
        ops_selectable_count(
            &data.rows,
            &data.merge_ops,
            app.drill_batch.as_deref(),
            now_unix,
            stale_after_secs,
        )
        .saturating_sub(1),
    );

    let list_width = list_area.width as usize;

    if let Some(batch) = app.drill_batch.as_deref() {
        items.push(ListItem::new(Line::from(vec![
            Span::raw(" "),
            format!("Filter: batch {}", super::truncate(batch, 22))
                .fg(theme::ACCENT)
                .bold(),
            Span::raw("  "),
            "x".fg(theme::ACCENT).bold(),
            Span::raw("/"),
            "Esc".fg(theme::ACCENT).bold(),
            Span::raw(": back"),
        ])));
        items.push(ListItem::new(Line::from(Span::raw(""))));
    }

    let all_active_empty = classified.running.is_empty()
        && classified.scheduled.is_empty()
        && (app.drill_batch.is_some() || classified.active_batches.is_empty())
        && classified.active_threads.is_empty();

    if all_active_empty {
        items.push(ListItem::new(Line::from(
            "  no active work".to_string().fg(theme::TEXT_DIM),
        )));
    } else {
        let mut rendered_active_section = false;

        if !classified.running.is_empty() {
            push_section_header(
                &mut items,
                "Running",
                classified.running.len(),
                theme::ACCENT,
            );
            for src_idx in &classified.running {
                let Some(row) = data.rows.get(*src_idx) else {
                    continue;
                };
                let is_selected = selectable_slot == selected_slot;
                sel_to_row.push(items.len());
                let mut lines = vec![make_thread_line(row, is_selected, now_unix, list_width)];
                // Add progress summary as a second line if available and still running.
                if is_running_now(row) {
                    if let Some(exec_id) = &row.execution_id {
                        if let Some(summary) = app.get_progress_summary(exec_id) {
                            let avail = list_width
                                .saturating_sub(DETAIL_PREFIX_LEN)
                                .saturating_sub(HINT_SUFFIX_LEN);
                            let truncated = super::truncate(summary, avail);
                            let padded = if is_selected {
                                format!("{:<width$}", truncated, width = avail)
                            } else {
                                truncated.to_string()
                            };
                            let mut spans = vec![
                                Span::raw("     └ "),
                                Span::styled(padded, Style::default().fg(theme::TEXT_DIM)),
                            ];
                            if is_selected {
                                spans.push(Span::styled(
                                    " │ ",
                                    Style::default().fg(theme::BORDER_DIM),
                                ));
                                spans.push(Span::styled("[c]", Style::default().fg(theme::ACCENT)));
                                spans.push(Span::styled(
                                    " conversation",
                                    Style::default().fg(theme::TEXT_DIM),
                                ));
                            }
                            lines.push(Line::from(spans));
                        } else if is_selected {
                            lines.push(make_thread_detail_line(row, list_width));
                        }
                    } else if is_selected {
                        lines.push(make_thread_detail_line(row, list_width));
                    }
                } else if is_selected {
                    lines.push(make_thread_detail_line(row, list_width));
                }
                items.push(ListItem::new(lines));
                selectable_slot += 1;
            }
            rendered_active_section = true;
        }

        if !classified.scheduled.is_empty() {
            if rendered_active_section {
                items.push(ListItem::new(Line::from(Span::raw(""))));
            }
            push_section_header(
                &mut items,
                "Scheduled",
                classified.scheduled.len(),
                theme::TEXT_MUTED,
            );
            for src_idx in &classified.scheduled {
                let Some(row) = data.rows.get(*src_idx) else {
                    continue;
                };
                let is_selected = selectable_slot == selected_slot;
                sel_to_row.push(items.len());
                let mut lines = vec![make_thread_line(row, is_selected, now_unix, list_width)];
                if is_selected {
                    lines.push(make_scheduled_detail_line(row, now_unix, list_width));
                }
                items.push(ListItem::new(lines));
                selectable_slot += 1;
            }
            rendered_active_section = true;
        }

        if app.drill_batch.is_none() && !classified.active_batches.is_empty() {
            if rendered_active_section {
                items.push(ListItem::new(Line::from(Span::raw(""))));
            }
            push_section_header(
                &mut items,
                "Active Batches",
                classified.active_batches.len(),
                theme::ACCENT,
            );
            for batch in capped_batches(&classified.active_batches) {
                let is_selected = selectable_slot == selected_slot;
                sel_to_row.push(items.len());
                let mut lines = vec![make_batch_line(
                    batch,
                    is_selected,
                    now_unix,
                    "A",
                    theme::ACCENT,
                    list_width,
                )];
                if is_selected {
                    lines.push(make_batch_detail_line(batch));
                }
                items.push(ListItem::new(lines));
                selectable_slot += 1;
            }
            rendered_active_section = true;
        }

        if !classified.active_threads.is_empty() {
            if rendered_active_section {
                items.push(ListItem::new(Line::from(Span::raw(""))));
            }
            push_section_header(
                &mut items,
                "Active Threads",
                classified.active_threads.len(),
                theme::ACCENT,
            );
            for src_idx in &classified.active_threads {
                let Some(row) = data.rows.get(*src_idx) else {
                    continue;
                };
                let is_selected = selectable_slot == selected_slot;
                sel_to_row.push(items.len());
                let mut lines = vec![make_thread_line(row, is_selected, now_unix, list_width)];
                if is_selected {
                    lines.push(make_thread_detail_line(row, list_width));
                }
                items.push(ListItem::new(lines));
                selectable_slot += 1;
            }
        }
    }

    if app.drill_batch.is_some() {
        items.push(ListItem::new(Line::from(Span::raw(""))));
        push_section_header(
            &mut items,
            "Other",
            classified.uncategorized.len(),
            theme::TEXT_DIM,
        );
        if classified.uncategorized.is_empty() {
            items.push(empty_line("  none"));
        } else {
            for src_idx in &classified.uncategorized {
                let Some(row) = data.rows.get(*src_idx) else {
                    continue;
                };
                let is_selected = selectable_slot == selected_slot;
                sel_to_row.push(items.len());
                let mut lines = vec![make_thread_line(row, is_selected, now_unix, list_width)];
                if is_selected {
                    lines.push(make_thread_detail_line(row, list_width));
                }
                items.push(ListItem::new(lines));
                selectable_slot += 1;
            }
        }
    }

    // ── Merge Queue section ─────────────────────────────────────────────
    // Skip merge queue when drilling into a batch — it's not batch-scoped.
    let visible_merge_ops = if app.drill_batch.is_none() {
        filter_merge_ops(&data.merge_ops, now_unix)
    } else {
        Vec::new()
    };
    if !visible_merge_ops.is_empty() {
        let has_failed = visible_merge_ops.iter().any(|o| o.status == "failed");
        let header_color = if has_failed {
            theme::FAILURE
        } else {
            theme::ACCENT
        };

        items.push(ListItem::new(Line::from(Span::raw(""))));
        push_section_header(
            &mut items,
            "Merge Queue",
            visible_merge_ops.len(),
            header_color,
        );
        for op in &visible_merge_ops {
            let is_selected = selectable_slot == selected_slot;
            sel_to_row.push(items.len());
            let mut lines = vec![make_merge_op_line(op, is_selected, now_unix, list_width)];
            if is_selected {
                lines.push(make_merge_op_detail_line(op, list_width));
            }
            items.push(ListItem::new(lines));
            selectable_slot += 1;
        }
    }

    items.push(ListItem::new(Line::from(Span::raw(""))));
    push_section_header(
        &mut items,
        "Recently Completed",
        recent_indices.len(),
        theme::SUCCESS_DIM,
    );
    if recent_indices.is_empty() {
        items.push(empty_line("  none"));
    } else {
        for src_idx in recent_indices {
            let Some(row) = data.rows.get(*src_idx) else {
                continue;
            };
            let is_selected = selectable_slot == selected_slot;
            sel_to_row.push(items.len());
            let mut lines = vec![make_thread_line(row, is_selected, now_unix, list_width)];
            if is_selected {
                lines.push(make_thread_detail_line(row, list_width));
            }
            items.push(ListItem::new(lines));
            selectable_slot += 1;
        }
    }

    if app.drill_batch.is_none() {
        items.push(ListItem::new(Line::from(Span::raw(""))));
        push_section_header(
            &mut items,
            "Batches",
            classified.batches.len(),
            theme::TEXT_MUTED,
        );
        if classified.batches.is_empty() {
            items.push(empty_line("  none"));
        } else {
            for batch in capped_batches(&classified.batches) {
                let is_selected = selectable_slot == selected_slot;
                sel_to_row.push(items.len());
                let mut lines = vec![make_batch_line(
                    batch,
                    is_selected,
                    now_unix,
                    "B",
                    theme::TEXT_MUTED,
                    list_width,
                )];
                if is_selected {
                    lines.push(make_batch_detail_line(batch));
                }
                items.push(ListItem::new(lines));
                selectable_slot += 1;
            }
        }
    }

    let visible_height = list_area.height as usize;
    let selected_display_idx = sel_to_row.get(selected_slot).copied().unwrap_or(0);
    // Capture per-item heights before List::new() consumes them.
    let item_heights: Vec<usize> = items.iter().map(|item| item.height()).collect();
    // Compute total display rows (sum of lines per ListItem) — items can be multi-line
    // (e.g. selected items with inline detail, running items with progress summary).
    let total_display_rows: usize = item_heights.iter().sum();
    // Compute the display-row offset of the selected item (sum of heights of preceding items).
    let selected_row_offset: usize = item_heights[..selected_display_idx].iter().sum();
    let scroll = compute_scroll(selected_row_offset, visible_height, total_display_rows);

    let mut state = ListState::default().with_selected(Some(selected_display_idx));
    *state.offset_mut() = scroll;

    let list = List::new(items)
        .highlight_style(Style::new().bg(theme::BG_HIGHLIGHT))
        .style(Style::new().bg(theme::BG_PRIMARY).fg(theme::TEXT_NORMAL));
    f.render_stateful_widget(list, list_area, &mut state);

    let mut scrollbar_state = ScrollbarState::new(selectable_slot.max(1)).position(selected_slot);
    let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
        .thumb_style(theme::scrollbar_thumb_style())
        .track_style(theme::scrollbar_track_style());
    f.render_stateful_widget(scrollbar, list_area, &mut scrollbar_state);

    f.render_widget(
        Paragraph::new(build_footer_line(data, now_unix, stale_after_secs))
            .style(Style::new().bg(theme::BG_PRIMARY).fg(theme::TEXT_DIM)),
        footer_area,
    );

    // ── Populate click cache ────────────────────────────────────────────────
    // Build display-row offsets for each selectable slot using the captured
    // `item_heights` and `sel_to_row` mapping, so mouse clicks can map
    // (click_y + scroll) → slot index.
    {
        let mut slot_geometry = Vec::with_capacity(sel_to_row.len());
        for &item_idx in &sel_to_row {
            // Display-row offset = sum of heights of all items before this one.
            let offset: usize = item_heights[..item_idx].iter().sum();
            let height = item_heights.get(item_idx).copied().unwrap_or(1);
            slot_geometry.push((offset, height));
        }
        let mut cache = app.ops_click_cache.borrow_mut();
        cache.slot_geometry = slot_geometry;
        cache.scroll = scroll;
        cache.list_rect = list_area;
    }
}

/// The `[c] conversation` hint occupies a fixed-width suffix so it stays
/// visually stable as the selection moves across rows with varying text.
const DETAIL_PREFIX_LEN: usize = 7; // "     └─ "
const HINT_SUFFIX_LEN: usize = 20; // " │ [c] conversation"

fn make_thread_detail_line(row: &ThreadStatusView, list_width: usize) -> Line<'static> {
    let detail = row
        .summary
        .as_deref()
        .unwrap_or_else(|| row.parsed_intent.as_deref().unwrap_or("-"));
    let avail = list_width
        .saturating_sub(DETAIL_PREFIX_LEN)
        .saturating_sub(HINT_SUFFIX_LEN);
    let padded = format!("{:<width$}", super::truncate(detail, avail), width = avail);
    Line::from(vec![
        Span::raw("     \u{2514}\u{2500} "),
        Span::styled(padded, Style::default().fg(theme::TEXT_DIM)),
        Span::styled(" \u{2502} ", Style::default().fg(theme::BORDER_DIM)),
        Span::styled("[c]", Style::default().fg(theme::ACCENT)),
        Span::styled(" conversation", Style::default().fg(theme::TEXT_DIM)),
    ])
}

fn make_scheduled_detail_line(
    row: &ThreadStatusView,
    now_unix: i64,
    list_width: usize,
) -> Line<'static> {
    let due = row
        .eligible_at
        .map(|ea| {
            let delta = (ea - now_unix).max(0);
            format!("due {}", format_relative_future(delta))
        })
        .unwrap_or_else(|| "scheduled".to_string());

    let detail = row
        .summary
        .as_deref()
        .map(|s| format!("{} — {}", due, s))
        .unwrap_or(due);

    let avail = list_width
        .saturating_sub(DETAIL_PREFIX_LEN)
        .saturating_sub(HINT_SUFFIX_LEN);
    let padded = format!("{:<width$}", super::truncate(&detail, avail), width = avail);
    Line::from(vec![
        Span::raw("     \u{2514}\u{2500} "),
        Span::styled(padded, Style::default().fg(theme::TEXT_DIM)),
        Span::styled(" \u{2502} ", Style::default().fg(theme::BORDER_DIM)),
        Span::styled("[c]", Style::default().fg(theme::ACCENT)),
        Span::styled(" conversation", Style::default().fg(theme::TEXT_DIM)),
    ])
}

/// Format a future duration in seconds as a human-readable relative time.
/// Examples: "in 30s", "in 5m 20s", "in 2h 15m", "in 1d 3h".
fn format_relative_future(secs: i64) -> String {
    if secs <= 0 {
        return "now".to_string();
    }
    let days = secs / 86400;
    let hours = (secs % 86400) / 3600;
    let minutes = (secs % 3600) / 60;
    let seconds = secs % 60;

    if days > 0 {
        format!("in {}d {}h", days, hours)
    } else if hours > 0 {
        format!("in {}h {}m", hours, minutes)
    } else if minutes > 0 {
        format!("in {}m {}s", minutes, seconds)
    } else {
        format!("in {}s", seconds)
    }
}

fn make_batch_detail_line(batch: &BatchProgress) -> Line<'static> {
    // Batch rows only appear when drill_batch is None (Batches/Active Batches sections
    // are guarded by `if app.drill_batch.is_none()`), so always show [Enter] drill.
    let (key, label) = ("[Enter]", " drill");
    let mut spans = vec![Span::raw("     \u{2514}\u{2500} ")];

    // "N active · N done · N fail" with semantic colors
    spans.push(Span::styled(
        batch.active.to_string(),
        Style::default().fg(theme::ACCENT),
    ));
    spans.push(Span::styled(
        " active",
        Style::default().fg(theme::TEXT_DIM),
    ));
    spans.push(Span::styled(" · ", Style::default().fg(theme::BORDER_DIM)));
    spans.push(Span::styled(
        batch.completed.to_string(),
        Style::default().fg(theme::SUCCESS_DIM),
    ));
    spans.push(Span::styled(" done", Style::default().fg(theme::TEXT_DIM)));
    spans.push(Span::styled(" · ", Style::default().fg(theme::BORDER_DIM)));
    spans.push(Span::styled(
        batch.failed.to_string(),
        Style::default().fg(theme::FAILURE),
    ));
    spans.push(Span::styled(" fail", Style::default().fg(theme::TEXT_DIM)));

    spans.push(Span::styled(
        " \u{2502} ",
        Style::default().fg(theme::BORDER_DIM),
    ));
    spans.push(Span::styled(
        key.to_string(),
        Style::default().fg(theme::ACCENT),
    ));
    spans.push(Span::styled(
        label.to_string(),
        Style::default().fg(theme::TEXT_DIM),
    ));
    Line::from(spans)
}

fn make_thread_line(
    t: &ThreadStatusView,
    is_selected: bool,
    now_unix: i64,
    width: usize,
) -> Line<'static> {
    let (icon, icon_color) = row_icon(t);
    let is_wide = width >= 100;
    let thread_id = if is_wide {
        super::truncate_left(&t.thread_id, 8)
    } else {
        super::truncate_left(&t.thread_id, 6)
    };
    let (status_text, status_color) = row_status_display(t);
    let agent = t.agent_alias.as_deref().unwrap_or("-").to_string();
    let agent_width: usize = if is_wide { 18 } else { 14 };
    let agent_display = super::truncate(&agent, agent_width.saturating_sub(2));
    let batch = t
        .batch_id
        .as_deref()
        .map(|b| super::truncate(b, 12))
        .unwrap_or_else(|| "-".to_string());
    let duration = row_duration(t, now_unix);

    let bg = if is_selected {
        theme::BG_HIGHLIGHT
    } else {
        theme::BG_PRIMARY
    };
    let base_mod = if is_selected {
        Modifier::BOLD
    } else {
        Modifier::empty()
    };

    let mut spans = vec![
        Span::styled(format!(" {} ", icon), Style::new().fg(icon_color).bg(bg)),
        Span::styled(
            format!("{:<w$}", thread_id, w = if is_wide { 10 } else { 8 }),
            Style::new()
                .fg(theme::TEXT_BRIGHT)
                .bg(bg)
                .add_modifier(base_mod),
        ),
        Span::styled(
            format!("{:<w$}", status_text, w = if is_wide { 16 } else { 12 }),
            Style::new().fg(status_color).bg(bg).add_modifier(base_mod),
        ),
        Span::styled(
            format!("{:<w$}", agent_display, w = agent_width),
            Style::new()
                .fg(theme::TEXT_NORMAL)
                .bg(bg)
                .add_modifier(base_mod),
        ),
    ];

    // Separator between Agent and Summary
    spans.push(Span::styled(
        " │ ",
        Style::new().fg(theme::BORDER_DIM).bg(bg),
    ));

    // Summary column (between agent and batch_id)
    let summary_text = t.summary.as_deref().unwrap_or("-");
    // Fixed-width columns budget (excluding summary).
    // Wide:   icon(3) + id(10) + status(16) + agent(18) + sep1(3) + sep2(3) + batch(14) + duration(8) = 75
    // Narrow: icon(3) + id(8)  + status(12) + agent(14) + sep1(3) + duration(8)                      = 48
    let fixed_cols = if is_wide { 75 } else { 48 };
    let summary_width = (width.saturating_sub(fixed_cols)).clamp(10, 60);
    let summary_trunc = summary_width.saturating_sub(2); // ensure ≥1 char of right-padding before separator
    spans.push(Span::styled(
        format!(
            "{:<w$}",
            super::truncate(summary_text, summary_trunc),
            w = summary_width
        ),
        Style::new()
            .fg(theme::TEXT_NORMAL)
            .bg(bg)
            .add_modifier(base_mod),
    ));

    if is_wide {
        // Separator between Summary and Batch
        spans.push(Span::styled(
            " │ ",
            Style::new().fg(theme::BORDER_DIM).bg(bg),
        ));
        spans.push(Span::styled(
            format!("{:<14}", batch),
            Style::new()
                .fg(theme::TEXT_DIM)
                .bg(bg)
                .add_modifier(base_mod),
        ));
    }

    spans.push(Span::styled(
        format!("{:<8}", duration),
        Style::new()
            .fg(theme::TEXT_DIM)
            .bg(bg)
            .add_modifier(base_mod),
    ));

    Line::from(spans)
}

fn make_batch_line(
    batch: &BatchProgress,
    is_selected: bool,
    now_unix: i64,
    marker: &str,
    marker_color: Color,
    width: usize,
) -> Line<'static> {
    let is_wide = width >= 100;
    let (id_trunc, id_width) = if is_wide { (16, 18) } else { (12, 14) };
    let (prog_width, bar_len, bar_width) = if is_wide { (9, 10, 12) } else { (7, 6, 8) };

    let fill = if batch.total == 0 {
        0
    } else {
        (batch.completed * bar_len / batch.total).min(bar_len)
    };
    let bar_filled = theme::BATCH_PROGRESS_FILLED.repeat(fill);
    let bar_empty = theme::BATCH_PROGRESS_EMPTY.repeat(bar_len - fill);

    // State-aware bar color: green when complete, red when failures, amber in-progress.
    let bar_color = if batch.completed == batch.total && batch.total > 0 {
        theme::SUCCESS
    } else if batch.failed > 0 {
        theme::FAILURE
    } else {
        theme::ACCENT
    };
    let age = batch
        .oldest_active_updated_at
        .map(|ts| format_duration_secs((now_unix - ts).max(0)))
        .unwrap_or_else(|| format_duration_secs((now_unix - batch.latest_updated_at).max(0)));

    let bg = if is_selected {
        theme::BG_HIGHLIGHT
    } else {
        theme::BG_PRIMARY
    };
    let base_mod = if is_selected {
        Modifier::BOLD
    } else {
        Modifier::empty()
    };

    let mut spans = vec![
        Span::styled(
            format!(" {} ", marker),
            Style::new().fg(marker_color).bg(bg),
        ),
        Span::styled(
            format!(
                "{:<w$}",
                super::truncate(&batch.batch_id, id_trunc),
                w = id_width
            ),
            Style::new()
                .fg(theme::TEXT_BRIGHT)
                .bg(bg)
                .add_modifier(base_mod),
        ),
        Span::styled(
            format!(
                "{:<w$}",
                format!("{}/{}", batch.completed, batch.total),
                w = prog_width
            ),
            Style::new()
                .fg(theme::TEXT_NORMAL)
                .bg(bg)
                .add_modifier(base_mod),
        ),
        Span::styled(
            bar_filled,
            Style::new().fg(bar_color).bg(bg).add_modifier(base_mod),
        ),
        Span::styled(
            format!("{:<w$}", bar_empty, w = bar_width - fill),
            Style::new()
                .fg(theme::TEXT_DIM)
                .bg(bg)
                .add_modifier(base_mod),
        ),
    ];

    if is_wide {
        // Build batch stats with zero-suppression and semantic colors
        let mut parts: Vec<Span<'static>> = Vec::new();
        if batch.active > 0 {
            parts.push(Span::styled(
                batch.active.to_string(),
                Style::new().fg(theme::ACCENT).bg(bg),
            ));
            parts.push(Span::styled(
                " active",
                Style::new().fg(theme::TEXT_DIM).bg(bg),
            ));
        }
        if batch.completed > 0 {
            if !parts.is_empty() {
                parts.push(Span::styled(
                    " · ",
                    Style::new().fg(theme::BORDER_DIM).bg(bg),
                ));
            }
            parts.push(Span::styled(
                batch.completed.to_string(),
                Style::new().fg(theme::SUCCESS_DIM).bg(bg),
            ));
            parts.push(Span::styled(
                " done",
                Style::new().fg(theme::TEXT_DIM).bg(bg),
            ));
        }
        if batch.failed > 0 {
            if !parts.is_empty() {
                parts.push(Span::styled(
                    " · ",
                    Style::new().fg(theme::BORDER_DIM).bg(bg),
                ));
            }
            parts.push(Span::styled(
                batch.failed.to_string(),
                Style::new().fg(theme::FAILURE).bg(bg),
            ));
            parts.push(Span::styled(
                " fail",
                Style::new().fg(theme::TEXT_DIM).bg(bg),
            ));
        }
        spans.extend(parts);
    }

    spans.push(Span::styled(
        format!("  age {}", age),
        Style::new().fg(theme::TEXT_DIM).bg(bg),
    ));

    Line::from(spans)
}

fn merge_op_icon(status: &str) -> (&'static str, Color) {
    match status {
        "executing" | "claimed" => (theme::MARKER_RUNNING, theme::ACCENT),
        "queued" => (theme::MARKER_QUEUED, theme::WARNING),
        "completed" => (theme::MARKER_COMPLETED, theme::SUCCESS_DIM),
        "failed" => (theme::MARKER_FAILED, theme::FAILURE),
        "cancelled" => (" ", theme::TEXT_DIM),
        _ => (" ", theme::TEXT_NORMAL),
    }
}

fn make_merge_op_line(
    op: &MergeOperation,
    is_selected: bool,
    now_unix: i64,
    width: usize,
) -> Line<'static> {
    let (icon, icon_color) = merge_op_icon(&op.status);

    let bg = if is_selected {
        theme::BG_HIGHLIGHT
    } else {
        theme::BG_PRIMARY
    };
    let base_mod = if is_selected {
        Modifier::BOLD
    } else {
        Modifier::empty()
    };

    // Source branch — truncate compas/ prefix to c/ if needed
    let source = if width < 100 {
        op.source_branch.replace("compas/", "c/")
    } else {
        op.source_branch.clone()
    };
    let branch_budget = if width >= 100 { 50 } else { 30 };
    let arrow = format!(
        "{} → {}",
        super::truncate(&source, branch_budget / 2),
        super::truncate(&op.target_branch, branch_budget / 2)
    );
    let arrow_width = branch_budget + 4; // " → " = 3 chars + margin

    let status_text = match op.status.as_str() {
        "queued" => "Queued",
        "claimed" => "Claimed",
        "executing" => "Executing",
        "completed" => "Completed",
        "failed" => "Failed",
        "cancelled" => "Cancelled",
        other => other,
    };
    let status_color = theme::exec_status_color(&op.status);

    let strategy = format!("[{}]", op.merge_strategy);

    let duration = if let Some(ms) = op.duration_ms {
        format_duration_ms(ms)
    } else if matches!(op.status.as_str(), "executing" | "claimed") {
        let started = op.started_at.unwrap_or(op.queued_at);
        format_duration_secs((now_unix - started).max(0))
    } else {
        "-".to_string()
    };

    let mut spans = vec![
        Span::styled(format!(" {} ", icon), Style::new().fg(icon_color).bg(bg)),
        Span::styled(
            format!("{:<w$}", arrow, w = arrow_width),
            Style::new()
                .fg(theme::TEXT_BRIGHT)
                .bg(bg)
                .add_modifier(base_mod),
        ),
        Span::styled(
            format!("{:<12}", status_text),
            Style::new().fg(status_color).bg(bg).add_modifier(base_mod),
        ),
    ];

    if width >= 100 {
        spans.push(Span::styled(
            format!("{:<10}", strategy),
            Style::new()
                .fg(theme::TEXT_DIM)
                .bg(bg)
                .add_modifier(base_mod),
        ));
    }

    spans.push(Span::styled(
        format!("{:<8}", duration),
        Style::new()
            .fg(theme::TEXT_DIM)
            .bg(bg)
            .add_modifier(base_mod),
    ));

    Line::from(spans)
}

fn make_merge_op_detail_line(op: &MergeOperation, list_width: usize) -> Line<'static> {
    let avail = list_width
        .saturating_sub(DETAIL_PREFIX_LEN)
        .saturating_sub(HINT_SUFFIX_LEN);

    let detail = if op.status == "failed" {
        // Show conflict files if present
        if let Some(ref cf_json) = op.conflict_files {
            if let Ok(files) = serde_json::from_str::<Vec<String>>(cf_json) {
                if files.is_empty() {
                    op.error_detail.as_deref().unwrap_or("failed").to_string()
                } else {
                    let file_list = files.join(", ");
                    format!("{} conflicts: {}", files.len(), file_list)
                }
            } else {
                op.error_detail.as_deref().unwrap_or("failed").to_string()
            }
        } else {
            op.error_detail.as_deref().unwrap_or("failed").to_string()
        }
    } else {
        // Normal: show op id and thread id
        let op_short = super::truncate_left(&op.id, 7);
        let thread_short = super::truncate_left(&op.thread_id, 7);
        format!(
            "op:{}  thread:{}  by:{}",
            op_short, thread_short, op.requested_by
        )
    };

    let padded = format!("{:<width$}", super::truncate(&detail, avail), width = avail);
    Line::from(vec![
        Span::raw("     └─ "),
        Span::styled(padded, Style::default().fg(theme::TEXT_DIM)),
        Span::styled(" │ ", Style::default().fg(theme::BORDER_DIM)),
        Span::styled("[Enter]", Style::default().fg(theme::ACCENT)),
        Span::styled(" thread", Style::default().fg(theme::TEXT_DIM)),
    ])
}

fn push_section_header(
    items: &mut Vec<ListItem<'static>>,
    title: &str,
    count: usize,
    color: Color,
) {
    items.push(ListItem::new(Line::from(vec![
        Span::raw(" "),
        title.to_string().fg(theme::TEXT_BRIGHT).bold(),
        Span::raw(" "),
        format!("({count})").fg(color).bold(),
    ])));
}

fn empty_line(s: &str) -> ListItem<'static> {
    ListItem::new(Line::from(s.to_string().fg(theme::TEXT_DIM)))
}

fn row_icon(t: &ThreadStatusView) -> (&'static str, Color) {
    let ts = t.thread_status.as_str();
    let es = t.execution_status.as_deref().unwrap_or("");
    if ts == "Failed" || matches!(es, "failed" | "crashed") {
        (theme::MARKER_FAILED, theme::FAILURE)
    } else if es == "timed_out" {
        (theme::MARKER_TIMEOUT, theme::FAILURE)
    } else if matches!(es, "executing" | "picked_up" | "queued") {
        (theme::MARKER_RUNNING, theme::ACCENT)
    } else if es == "completed" || ts == "Completed" {
        (theme::MARKER_COMPLETED, theme::SUCCESS_DIM)
    } else {
        (" ", theme::TEXT_NORMAL)
    }
}

fn row_status_display(t: &ThreadStatusView) -> (String, Color) {
    if t.thread_status == "Failed" {
        if let Some(es) = &t.execution_status {
            if matches!(es.as_str(), "executing" | "picked_up" | "queued") {
                return ("Failed (running)".to_string(), theme::FAILURE);
            }
        }
        return ("Failed".to_string(), theme::FAILURE);
    }

    if let Some(es) = &t.execution_status {
        (
            humanize_exec_status(es).to_string(),
            theme::exec_status_color(es),
        )
    } else {
        let ts = &t.thread_status;
        (
            humanize_thread_status(ts).to_string(),
            theme::thread_status_color(ts),
        )
    }
}

fn row_duration(t: &ThreadStatusView, now_unix: i64) -> String {
    let es = t.execution_status.as_deref().unwrap_or("");
    if matches!(es, "executing" | "picked_up" | "queued") {
        if let Some(started) = t.started_at {
            return format_duration_secs((now_unix - started).max(0));
        }
        return "-".to_string();
    }
    if let Some(ms) = t.duration_ms {
        return format_duration_ms(ms);
    }
    format_duration_secs((now_unix - t.thread_updated_at).max(0))
}

fn compute_scroll(selected: usize, visible: usize, total: usize) -> usize {
    if visible == 0 || total <= visible {
        return 0;
    }
    let half = visible / 2;
    if selected <= half {
        0
    } else if selected + visible > total {
        total - visible
    } else {
        selected - half
    }
}

fn compute_health(data: &Option<ActivityData>, now_unix: i64) -> (String, Color) {
    match data {
        Some(d) => match &d.heartbeat {
            Some((_, last_beat_at, _, _)) => {
                let age = (now_unix - last_beat_at).max(0);
                let color = if age < 30 {
                    theme::SUCCESS
                } else {
                    theme::FAILURE
                };
                (format!(" worker beat: {}s ", age), color)
            }
            None => (" worker beat: none ".to_string(), theme::TEXT_DIM),
        },
        None => (" ".to_string(), theme::TEXT_DIM),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_row(
        thread_id: &str,
        batch_id: Option<&str>,
        thread_status: &str,
        execution_status: Option<&str>,
        updated_at: i64,
    ) -> ThreadStatusView {
        ThreadStatusView {
            thread_id: thread_id.to_string(),
            batch_id: batch_id.map(|b| b.to_string()),
            summary: None,
            thread_status: thread_status.to_string(),
            thread_created_at: 0,
            thread_updated_at: updated_at,
            execution_id: None,
            agent_alias: None,
            execution_status: execution_status.map(|s| s.to_string()),
            queued_at: None,
            started_at: None,
            finished_at: None,
            duration_ms: None,
            error_detail: None,
            parsed_intent: None,
            prompt_hash: None,
            eligible_at: None,
            eligible_reason: None,
        }
    }

    #[test]
    fn test_ops_selectable_targets_includes_batches() {
        let rows = vec![
            make_row("t1", Some("b1"), "Active", Some("executing"), 1),
            make_row("t2", Some("b1"), "Failed", Some("failed"), 2),
            make_row("t3", Some("b2"), "Completed", Some("completed"), 3),
        ];

        let targets = ops_selectable_targets(&rows, &[], None, 100, 3600);
        assert!(targets.iter().any(|t| matches!(t, OpsSelectable::Batch(_))));
    }

    #[test]
    fn test_ops_selectable_targets_drill_excludes_batches() {
        let rows = vec![
            make_row("t1", Some("b1"), "Active", Some("executing"), 1),
            make_row("t2", Some("b2"), "Active", Some("queued"), 2),
        ];

        let targets = ops_selectable_targets(&rows, &[], Some("b1"), 100, 3600);
        assert!(!targets.iter().any(|t| matches!(t, OpsSelectable::Batch(_))));
        assert_eq!(targets.len(), 1);
    }

    #[test]
    fn test_batch_progress_counts() {
        let rows = vec![
            make_row("t1", Some("b1"), "Active", Some("executing"), 1),
            make_row("t2", Some("b1"), "Failed", Some("failed"), 2),
            make_row("t3", Some("b1"), "Completed", Some("completed"), 3),
        ];

        let batches = batch_progress(&rows, 100, 3600);
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].total, 3);
        assert_eq!(batches[0].completed, 1);
        assert_eq!(batches[0].failed, 1);
    }

    #[test]
    fn test_section_threads_sorted_desc_by_updated_at() {
        let rows = vec![
            make_row("older", Some("b1"), "Active", None, 10),
            make_row("newer", Some("b1"), "Active", None, 20),
        ];
        let targets = ops_selectable_targets(&rows, &[], None, 100, 3600);
        let active_thread_ids: Vec<String> = targets
            .into_iter()
            .filter_map(|t| match t {
                OpsSelectable::Thread(idx) => Some(rows[idx].thread_id.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(active_thread_ids[0], "newer");
    }

    #[test]
    fn test_active_completed_thread_shows_in_active_section() {
        // An Active thread with a completed execution (waiting for operator review)
        // should appear in the active_threads section, not recently_completed.
        let rows = vec![
            make_row(
                "waiting-review",
                Some("b1"),
                "Active",
                Some("completed"),
                500,
            ),
            make_row(
                "truly-completed",
                Some("b1"),
                "Completed",
                Some("completed"),
                500,
            ),
        ];
        let classified = classify_rows(&rows, None, 1000, 3600);
        assert_eq!(classified.active_threads.len(), 1);
        assert_eq!(
            rows[classified.active_threads[0]].thread_id,
            "waiting-review"
        );
        // The Completed thread should be in recently_completed, not active_threads
        assert_eq!(classified.recently_completed.len(), 1);
        assert_eq!(
            rows[classified.recently_completed[0]].thread_id,
            "truly-completed"
        );
    }

    #[test]
    fn test_ops_selectable_targets_caps_recently_completed() {
        let mut rows = Vec::new();
        for i in 0..(RECENTLY_COMPLETED_LIMIT + 5) {
            rows.push(make_row(
                &format!("t{i}"),
                Some("b1"),
                "Completed",
                Some("completed"),
                i as i64,
            ));
        }

        let targets = ops_selectable_targets(&rows, &[], None, 100, 3600);
        let recent_threads = targets
            .into_iter()
            .filter(|t| matches!(t, OpsSelectable::Thread(_)))
            .count();
        assert_eq!(recent_threads, RECENTLY_COMPLETED_LIMIT);
    }

    #[test]
    fn test_ops_selectable_count_matches_targets_len() {
        let rows = vec![
            make_row("running", Some("b1"), "Active", Some("executing"), 10),
            make_row("active", Some("b1"), "Active", None, 9),
            make_row("done", Some("b1"), "Completed", Some("completed"), 8),
        ];

        let count = ops_selectable_count(&rows, &[], None, 100, 3600);
        let targets_len = ops_selectable_targets(&rows, &[], None, 100, 3600).len();
        assert_eq!(count, targets_len);
    }

    #[test]
    fn test_ops_selectable_targets_caps_batches_section() {
        let mut rows = Vec::new();
        for i in 0..(OPS_BATCH_SECTION_LIMIT + 5) {
            rows.push(make_row(
                &format!("t{i}"),
                Some(&format!("b{i}")),
                "Completed",
                Some("completed"),
                i as i64,
            ));
        }

        let targets = ops_selectable_targets(&rows, &[], None, 100, 3600);
        let batch_count = targets
            .iter()
            .filter(|t| matches!(t, OpsSelectable::Batch(_)))
            .count();
        assert_eq!(batch_count, OPS_BATCH_SECTION_LIMIT);
    }

    #[test]
    fn test_ops_selectable_targets_caps_active_batches_section() {
        let mut rows = Vec::new();
        for i in 0..(OPS_BATCH_SECTION_LIMIT + 5) {
            rows.push(make_row(
                &format!("t{i}"),
                Some(&format!("b{i}")),
                "Active",
                None,
                i as i64,
            ));
        }

        let targets = ops_selectable_targets(&rows, &[], None, 100, 3600);
        let batch_count = targets
            .iter()
            .filter(|t| matches!(t, OpsSelectable::Batch(_)))
            .count();
        assert_eq!(batch_count, OPS_BATCH_SECTION_LIMIT * 2);
    }

    #[test]
    fn test_footer_counts_excludes_stale_from_active() {
        let data = ActivityData {
            rows: vec![
                make_row("running", Some("b1"), "Active", Some("executing"), 950),
                make_row("fresh", Some("b1"), "Active", None, 980),
                make_row("stale", Some("b1"), "Active", None, 100),
            ],
            thread_counts: vec![
                ("Active".to_string(), 3),
                ("Failed".to_string(), 0),
                ("Completed".to_string(), 0),
            ],
            queue_depth: 0,
            heartbeat: None,
            fetched_at: std::time::Instant::now(),
            cost_summary: None,
            merge_ops: Vec::new(),
        };

        let (active, failed, completed, stale, scheduled) = footer_counts(&data, 1000, 300);
        assert_eq!(active, 2);
        assert_eq!(failed, 0);
        assert_eq!(completed, 0);
        assert_eq!(stale, 1);
        assert_eq!(scheduled, 0);
    }

    #[test]
    fn test_format_relative_future_seconds() {
        assert_eq!(format_relative_future(0), "now");
        assert_eq!(format_relative_future(45), "in 45s");
        assert_eq!(format_relative_future(-5), "now");
    }

    #[test]
    fn test_format_relative_future_minutes() {
        assert_eq!(format_relative_future(90), "in 1m 30s");
        assert_eq!(format_relative_future(600), "in 10m 0s");
    }

    #[test]
    fn test_format_relative_future_hours() {
        assert_eq!(format_relative_future(3661), "in 1h 1m");
        assert_eq!(format_relative_future(7200), "in 2h 0m");
    }

    #[test]
    fn test_format_relative_future_days() {
        assert_eq!(format_relative_future(90000), "in 1d 1h");
        assert_eq!(format_relative_future(172800), "in 2d 0h");
    }

    fn make_merge_op(id: &str, status: &str, finished_at: Option<i64>) -> MergeOperation {
        make_merge_op_for(id, status, finished_at, &format!("thread-{}", id), "main")
    }

    fn make_merge_op_for(
        id: &str,
        status: &str,
        finished_at: Option<i64>,
        thread_id: &str,
        target_branch: &str,
    ) -> MergeOperation {
        MergeOperation {
            id: id.to_string(),
            thread_id: thread_id.to_string(),
            source_branch: format!("compas/{}", thread_id),
            target_branch: target_branch.to_string(),
            merge_strategy: "merge".to_string(),
            requested_by: "operator".to_string(),
            status: status.to_string(),
            push_requested: false,
            queued_at: 100,
            claimed_at: None,
            started_at: None,
            finished_at,
            duration_ms: None,
            result_summary: None,
            error_detail: None,
            conflict_files: None,
        }
    }

    #[test]
    fn test_merge_queue_section_hidden_when_empty() {
        let rows = vec![make_row("t1", Some("b1"), "Active", Some("executing"), 10)];
        let merge_ops: Vec<MergeOperation> = Vec::new();
        let targets = ops_selectable_targets(&rows, &merge_ops, None, 100, 3600);
        assert!(!targets
            .iter()
            .any(|t| matches!(t, OpsSelectable::MergeOp(_))));
    }

    #[test]
    fn test_merge_queue_section_shows_active_ops() {
        let rows = vec![make_row("t1", Some("b1"), "Active", Some("executing"), 10)];
        let merge_ops = vec![
            make_merge_op("op1", "queued", None),
            make_merge_op("op2", "executing", None),
        ];
        let targets = ops_selectable_targets(&rows, &merge_ops, None, 100, 3600);
        let merge_count = targets
            .iter()
            .filter(|t| matches!(t, OpsSelectable::MergeOp(_)))
            .count();
        assert_eq!(merge_count, 2);
    }

    #[test]
    fn test_ops_selectable_includes_merge_ops() {
        let rows = vec![
            make_row("t1", None, "Active", Some("executing"), 10),
            make_row("t2", None, "Completed", Some("completed"), 5),
        ];
        let merge_ops = vec![make_merge_op("op1", "executing", None)];
        let targets = ops_selectable_targets(&rows, &merge_ops, None, 100, 3600);

        // Merge ops should appear between active threads and recently completed
        let merge_positions: Vec<usize> = targets
            .iter()
            .enumerate()
            .filter(|(_, t)| matches!(t, OpsSelectable::MergeOp(_)))
            .map(|(i, _)| i)
            .collect();
        let completed_positions: Vec<usize> = targets
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                if let OpsSelectable::Thread(idx) = t {
                    rows[*idx].thread_status == "Completed"
                } else {
                    false
                }
            })
            .map(|(i, _)| i)
            .collect();

        assert!(!merge_positions.is_empty());
        assert!(!completed_positions.is_empty());
        // Merge ops should come before recently completed
        assert!(merge_positions[0] < completed_positions[0]);
    }

    #[test]
    fn test_merge_ops_terminal_filtered_by_time_window() {
        let now_unix = 1000;
        // Completed op within window (10 min = 600s)
        let recent = make_merge_op("recent", "completed", Some(500));
        // Completed op outside window
        let old = make_merge_op("old", "completed", Some(100));
        // Failed op within extended window (30 min = 1800s)
        let mut failed_recent = make_merge_op("failed-recent", "failed", Some(500));
        failed_recent.error_detail = Some("merge conflict".to_string());

        let ops = vec![recent, old, failed_recent];
        let visible = filter_merge_ops(&ops, now_unix);

        let ids: Vec<&str> = visible.iter().map(|o| o.id.as_str()).collect();
        assert!(ids.contains(&"recent"));
        assert!(!ids.contains(&"old")); // too old
        assert!(ids.contains(&"failed-recent"));
    }

    #[test]
    fn test_filter_merge_ops_superseded_by_completed() {
        let now_unix = 1000;
        // Failed at t=400, then a successful retry completed at t=500 — same pair.
        let failed = make_merge_op_for("f1", "failed", Some(400), "thr-A", "main");
        let completed = make_merge_op_for("c1", "completed", Some(500), "thr-A", "main");

        let ops = vec![failed, completed];
        let visible = filter_merge_ops(&ops, now_unix);
        let ids: Vec<&str> = visible.iter().map(|o| o.id.as_str()).collect();

        assert!(!ids.contains(&"f1"), "failed op should be superseded");
        assert!(ids.contains(&"c1"), "completed op should still show");
    }

    #[test]
    fn test_filter_merge_ops_multiple_failures_superseded() {
        let now_unix = 1000;
        // Two failures before a success — all for the same (thread_id, target_branch).
        let fail1 = make_merge_op_for("f1", "failed", Some(300), "thr-A", "main");
        let fail2 = make_merge_op_for("f2", "failed", Some(400), "thr-A", "main");
        let success = make_merge_op_for("c1", "completed", Some(500), "thr-A", "main");

        let ops = vec![fail1, fail2, success];
        let visible = filter_merge_ops(&ops, now_unix);
        let ids: Vec<&str> = visible.iter().map(|o| o.id.as_str()).collect();

        assert!(!ids.contains(&"f1"), "first failure should be superseded");
        assert!(!ids.contains(&"f2"), "second failure should be superseded");
        assert!(ids.contains(&"c1"), "completed op should still show");
    }

    #[test]
    fn test_filter_merge_ops_different_target_branch_independent() {
        let now_unix = 1000;
        // Success on main does NOT suppress failure on release/v2.
        let fail_release = make_merge_op_for("f1", "failed", Some(400), "thr-A", "release/v2");
        let success_main = make_merge_op_for("c1", "completed", Some(500), "thr-A", "main");

        let ops = vec![fail_release, success_main];
        let visible = filter_merge_ops(&ops, now_unix);
        let ids: Vec<&str> = visible.iter().map(|o| o.id.as_str()).collect();

        assert!(
            ids.contains(&"f1"),
            "failure on release/v2 should NOT be superseded by success on main"
        );
        assert!(ids.contains(&"c1"));
    }

    #[test]
    fn test_filter_merge_ops_not_superseded_without_retry() {
        let now_unix = 1000;
        // Failed op within 30-min window, no completed pair exists — must still show.
        let failed = make_merge_op_for("f1", "failed", Some(500), "thr-A", "main");

        let ops = vec![failed];
        let visible = filter_merge_ops(&ops, now_unix);
        let ids: Vec<&str> = visible.iter().map(|o| o.id.as_str()).collect();

        assert!(
            ids.contains(&"f1"),
            "failed op with no retry should still be visible"
        );
    }

    #[test]
    fn test_ops_selectable_excludes_merge_ops_in_drill_mode() {
        let rows = vec![make_row("t1", Some("b1"), "Active", Some("executing"), 10)];
        let merge_ops = vec![
            make_merge_op("op1", "executing", None),
            make_merge_op("op2", "queued", None),
        ];
        let targets = ops_selectable_targets(&rows, &merge_ops, Some("b1"), 100, 3600);
        assert!(
            !targets
                .iter()
                .any(|t| matches!(t, OpsSelectable::MergeOp(_))),
            "merge ops should not appear when drilling into a batch"
        );
    }
}
