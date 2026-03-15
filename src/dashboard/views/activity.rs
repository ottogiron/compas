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
    format_duration_ms, format_duration_secs, humanize_exec_status, humanize_thread_status,
};
use crate::store::ThreadStatusView;

const RECENTLY_COMPLETED_LIMIT: usize = 12;
const OPS_BATCH_SECTION_LIMIT: usize = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpsSelectable {
    Thread(usize),
    Batch(String),
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
        } else if is_active_waiting(row, now_unix, stale_after_secs) {
            out.active_threads.push(*idx);
        } else if is_recently_completed(row) {
            out.recently_completed.push(*idx);
        } else {
            out.uncategorized.push(*idx);
        }
    }

    sort_indices_by_updated(rows, &mut out.running);
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

fn sort_indices_by_updated(rows: &[ThreadStatusView], indices: &mut [usize]) {
    indices.sort_by(|a, b| {
        let a_ts = rows.get(*a).map(|r| r.thread_updated_at).unwrap_or(0);
        let b_ts = rows.get(*b).map(|r| r.thread_updated_at).unwrap_or(0);
        b_ts.cmp(&a_ts)
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
    if drill_batch.is_none() {
        out.extend(
            capped_batches(&classified.batches)
                .iter()
                .map(|b| OpsSelectable::Batch(b.batch_id.clone())),
        );
    }
    out.extend(
        capped_recently_completed(&classified.recently_completed)
            .iter()
            .copied()
            .map(OpsSelectable::Thread),
    );

    out
}

pub fn ops_selected_target(
    rows: &[ThreadStatusView],
    drill_batch: Option<&str>,
    selected: usize,
    now_unix: i64,
    stale_after_secs: i64,
) -> Option<OpsSelectable> {
    ops_selectable_targets(rows, drill_batch, now_unix, stale_after_secs)
        .into_iter()
        .nth(selected)
}

pub fn ops_selectable_count(
    rows: &[ThreadStatusView],
    drill_batch: Option<&str>,
    now_unix: i64,
    stale_after_secs: i64,
) -> usize {
    ops_selectable_targets(rows, drill_batch, now_unix, stale_after_secs).len()
}

pub fn stale_active_thread_ids(
    rows: &[ThreadStatusView],
    drill_batch: Option<&str>,
    now_unix: i64,
    stale_after_secs: i64,
) -> Vec<String> {
    rows.iter()
        .filter(|r| match drill_batch {
            Some(batch) => r.batch_id.as_deref() == Some(batch),
            None => true,
        })
        .filter(|r| r.thread_status == "Active")
        .filter(|r| !is_running_now(r))
        .filter(|r| (now_unix - r.thread_updated_at).max(0) >= stale_after_secs)
        .map(|r| r.thread_id.clone())
        .collect()
}

// Compatibility helpers retained for existing call sites/tests.
pub fn selectable_indices(rows: &[ThreadStatusView]) -> Vec<usize> {
    let ClassifiedRows {
        running,
        active_threads,
        recently_completed,
        ..
    } = classify_rows(rows, None, 0, 3600);
    running
        .into_iter()
        .chain(active_threads)
        .chain(recently_completed)
        .collect()
}

fn footer_counts(
    data: &ActivityData,
    now_unix: i64,
    stale_after_secs: i64,
) -> (i64, i64, i64, i64) {
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

    let mut failed = 0i64;
    let mut completed = 0i64;
    for (status, count) in &data.thread_counts {
        match status.as_str() {
            "Failed" | "failed" => failed += count,
            "Completed" | "completed" => completed += count,
            _ => {}
        }
    }
    (active, failed, completed, stale)
}

fn build_footer_line(data: &ActivityData, now_unix: i64, stale_after_secs: i64) -> Line<'static> {
    let (active, failed, completed, stale) = footer_counts(data, now_unix, stale_after_secs);

    let label = |s: &str, color: Color| -> Span<'static> {
        Span::styled(s.to_string(), Style::new().fg(color))
    };
    let val = |n: i64| -> Span<'static> {
        Span::styled(format!("{}  ", n), Style::new().fg(theme::TEXT_BRIGHT))
    };

    Line::from(vec![
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
    ])
}

pub fn selectable_count(rows: &[ThreadStatusView]) -> usize {
    selectable_indices(rows).len()
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
            app.drill_batch.as_deref(),
            now_unix,
            stale_after_secs,
        )
        .saturating_sub(1),
    );

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

    push_section_header(
        &mut items,
        "Running",
        classified.running.len(),
        theme::ACCENT,
    );
    if classified.running.is_empty() {
        items.push(empty_line("  none"));
    } else {
        for src_idx in &classified.running {
            let Some(row) = data.rows.get(*src_idx) else {
                continue;
            };
            let is_selected = selectable_slot == selected_slot;
            sel_to_row.push(items.len());
            let mut lines = vec![make_thread_line(row, is_selected, now_unix)];
            // Add progress summary as a second line if available and still running.
            if is_running_now(row) {
                if let Some(exec_id) = &row.execution_id {
                    if let Some(summary) = app.get_progress_summary(exec_id) {
                        let truncated = super::truncate(summary, 50);
                        let mut spans = vec![
                            Span::raw("     └ "),
                            Span::styled(truncated, Style::default().fg(Color::DarkGray)),
                        ];
                        if is_selected {
                            spans.push(Span::styled(" │ ", Style::default().fg(theme::BORDER_DIM)));
                            spans.push(Span::styled("[c]", Style::default().fg(theme::ACCENT)));
                            spans.push(Span::styled(
                                " conversation",
                                Style::default().fg(theme::TEXT_DIM),
                            ));
                        }
                        lines.push(Line::from(spans));
                    } else if is_selected {
                        lines.push(make_thread_detail_line(row));
                    }
                } else if is_selected {
                    lines.push(make_thread_detail_line(row));
                }
            } else if is_selected {
                lines.push(make_thread_detail_line(row));
            }
            items.push(ListItem::new(lines));
            selectable_slot += 1;
        }
    }

    if app.drill_batch.is_none() {
        items.push(ListItem::new(Line::from(Span::raw(""))));
        push_section_header(
            &mut items,
            "Active Batches",
            classified.active_batches.len(),
            theme::ACCENT,
        );
        if classified.active_batches.is_empty() {
            items.push(empty_line("  none"));
        } else {
            for batch in capped_batches(&classified.active_batches) {
                let is_selected = selectable_slot == selected_slot;
                sel_to_row.push(items.len());
                let mut lines = vec![make_batch_line(
                    batch,
                    is_selected,
                    now_unix,
                    "A",
                    theme::ACCENT,
                )];
                if is_selected {
                    lines.push(make_batch_detail_line(batch, app.drill_batch.as_deref()));
                }
                items.push(ListItem::new(lines));
                selectable_slot += 1;
            }
        }
    }

    items.push(ListItem::new(Line::from(Span::raw(""))));
    push_section_header(
        &mut items,
        "Active Threads",
        classified.active_threads.len(),
        theme::ACCENT,
    );
    if classified.active_threads.is_empty() {
        items.push(empty_line("  none"));
    } else {
        for src_idx in &classified.active_threads {
            let Some(row) = data.rows.get(*src_idx) else {
                continue;
            };
            let is_selected = selectable_slot == selected_slot;
            sel_to_row.push(items.len());
            let mut lines = vec![make_thread_line(row, is_selected, now_unix)];
            if is_selected {
                lines.push(make_thread_detail_line(row));
            }
            items.push(ListItem::new(lines));
            selectable_slot += 1;
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
                let mut lines = vec![make_thread_line(row, is_selected, now_unix)];
                if is_selected {
                    lines.push(make_thread_detail_line(row));
                }
                items.push(ListItem::new(lines));
                selectable_slot += 1;
            }
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
                )];
                if is_selected {
                    lines.push(make_batch_detail_line(batch, app.drill_batch.as_deref()));
                }
                items.push(ListItem::new(lines));
                selectable_slot += 1;
            }
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
            let mut lines = vec![make_thread_line(row, is_selected, now_unix)];
            if is_selected {
                lines.push(make_thread_detail_line(row));
            }
            items.push(ListItem::new(lines));
            selectable_slot += 1;
        }
    }

    let visible_height = list_area.height as usize;
    let selected_display_idx = sel_to_row.get(selected_slot).copied().unwrap_or(0);
    let scroll = compute_scroll(selected_display_idx, visible_height, items.len());

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
}

fn make_thread_detail_line(row: &ThreadStatusView) -> Line<'static> {
    let intent = row.parsed_intent.as_deref().unwrap_or("-");
    Line::from(vec![
        Span::raw("     \u{2514}\u{2500} "),
        Span::styled(intent.to_string(), Style::default().fg(theme::TEXT_DIM)),
        Span::styled(" \u{2502} ", Style::default().fg(theme::BORDER_DIM)),
        Span::styled("[c]", Style::default().fg(theme::ACCENT)),
        Span::styled(" conversation", Style::default().fg(theme::TEXT_DIM)),
    ])
}

fn make_batch_detail_line(batch: &BatchProgress, drill_batch: Option<&str>) -> Line<'static> {
    let is_drilled = drill_batch == Some(batch.batch_id.as_str());
    let (key, label) = if is_drilled {
        ("[Esc]", " back")
    } else {
        ("[Enter]", " drill")
    };
    Line::from(vec![
        Span::raw("     \u{2514}\u{2500} "),
        Span::styled(
            format!(
                "a:{} c:{} f:{}",
                batch.active, batch.completed, batch.failed
            ),
            Style::default().fg(theme::TEXT_DIM),
        ),
        Span::styled(" \u{2502} ", Style::default().fg(theme::BORDER_DIM)),
        Span::styled(key.to_string(), Style::default().fg(theme::ACCENT)),
        Span::styled(label.to_string(), Style::default().fg(theme::TEXT_DIM)),
    ])
}

fn make_thread_line(t: &ThreadStatusView, is_selected: bool, now_unix: i64) -> Line<'static> {
    let (icon, icon_color) = row_icon(t);
    let thread_id = super::truncate(&t.thread_id, 16);
    let (status_text, status_color) = row_status_display(t);
    let agent = t.agent_alias.as_deref().unwrap_or("-").to_string();
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

    Line::from(vec![
        Span::styled(format!(" {} ", icon), Style::new().fg(icon_color).bg(bg)),
        Span::styled(
            format!("{:<18}", thread_id),
            Style::new()
                .fg(theme::TEXT_BRIGHT)
                .bg(bg)
                .add_modifier(base_mod),
        ),
        Span::styled(
            format!("{:<16}", status_text),
            Style::new().fg(status_color).bg(bg).add_modifier(base_mod),
        ),
        Span::styled(
            format!("{:<12}", agent),
            Style::new()
                .fg(theme::TEXT_NORMAL)
                .bg(bg)
                .add_modifier(base_mod),
        ),
        Span::styled(
            format!("{:<14}", batch),
            Style::new()
                .fg(theme::TEXT_DIM)
                .bg(bg)
                .add_modifier(base_mod),
        ),
        Span::styled(
            duration,
            Style::new()
                .fg(theme::TEXT_DIM)
                .bg(bg)
                .add_modifier(base_mod),
        ),
    ])
}

fn make_batch_line(
    batch: &BatchProgress,
    is_selected: bool,
    now_unix: i64,
    marker: &str,
    marker_color: Color,
) -> Line<'static> {
    let fill = if batch.total == 0 {
        0
    } else {
        (batch.completed * 10 / batch.total).min(10)
    };
    let bar = format!(
        "{}{}",
        theme::BATCH_PROGRESS_FILLED.repeat(fill),
        theme::BATCH_PROGRESS_EMPTY.repeat(10 - fill)
    );
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

    Line::from(vec![
        Span::styled(
            format!(" {} ", marker),
            Style::new().fg(marker_color).bg(bg),
        ),
        Span::styled(
            format!("{:<18}", super::truncate(&batch.batch_id, 16)),
            Style::new()
                .fg(theme::TEXT_BRIGHT)
                .bg(bg)
                .add_modifier(base_mod),
        ),
        Span::styled(
            format!("{:<9}", format!("{}/{}", batch.completed, batch.total)),
            Style::new()
                .fg(theme::TEXT_NORMAL)
                .bg(bg)
                .add_modifier(base_mod),
        ),
        Span::styled(
            format!("{:<12}", bar),
            Style::new().fg(theme::ACCENT).bg(bg).add_modifier(base_mod),
        ),
        Span::styled(
            format!(
                "a:{} c:{} f:{}",
                batch.active, batch.completed, batch.failed
            ),
            Style::new()
                .fg(if batch.failed > 0 {
                    theme::FAILURE
                } else {
                    theme::TEXT_DIM
                })
                .bg(bg),
        ),
        Span::styled(
            format!("  age {}", age),
            Style::new().fg(theme::TEXT_DIM).bg(bg),
        ),
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
        }
    }

    #[test]
    fn test_ops_selectable_targets_includes_batches() {
        let rows = vec![
            make_row("t1", Some("b1"), "Active", Some("executing"), 1),
            make_row("t2", Some("b1"), "Failed", Some("failed"), 2),
            make_row("t3", Some("b2"), "Completed", Some("completed"), 3),
        ];

        let targets = ops_selectable_targets(&rows, None, 100, 3600);
        assert!(targets.iter().any(|t| matches!(t, OpsSelectable::Batch(_))));
    }

    #[test]
    fn test_ops_selectable_targets_drill_excludes_batches() {
        let rows = vec![
            make_row("t1", Some("b1"), "Active", Some("executing"), 1),
            make_row("t2", Some("b2"), "Active", Some("queued"), 2),
        ];

        let targets = ops_selectable_targets(&rows, Some("b1"), 100, 3600);
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
        let targets = ops_selectable_targets(&rows, None, 100, 3600);
        let active_thread_ids: Vec<String> = targets
            .into_iter()
            .filter_map(|t| match t {
                OpsSelectable::Thread(idx) => Some(rows[idx].thread_id.clone()),
                OpsSelectable::Batch(_) => None,
            })
            .collect();
        assert_eq!(active_thread_ids[0], "newer");
    }

    #[test]
    fn test_stale_active_thread_ids_filters_running_and_fresh() {
        let rows = vec![
            make_row("stale", Some("b1"), "Active", None, 100),
            make_row("running", Some("b1"), "Active", Some("executing"), 100),
            make_row("done", Some("b1"), "Active", Some("completed"), 100),
            make_row("fresh", Some("b1"), "Active", None, 990),
        ];
        let ids = stale_active_thread_ids(&rows, Some("b1"), 1000, 300);
        // "stale" and "done" are both Active, not running, and old enough.
        // "running" is excluded (executing). "fresh" is excluded (too recent).
        assert!(ids.contains(&"stale".to_string()));
        assert!(ids.contains(&"done".to_string()));
        assert!(!ids.contains(&"running".to_string()));
        assert!(!ids.contains(&"fresh".to_string()));
        assert_eq!(ids.len(), 2);
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

        let targets = ops_selectable_targets(&rows, None, 100, 3600);
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

        let count = ops_selectable_count(&rows, None, 100, 3600);
        let targets_len = ops_selectable_targets(&rows, None, 100, 3600).len();
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

        let targets = ops_selectable_targets(&rows, None, 100, 3600);
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

        let targets = ops_selectable_targets(&rows, None, 100, 3600);
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
        };

        let (active, failed, completed, stale) = footer_counts(&data, 1000, 300);
        assert_eq!(active, 2);
        assert_eq!(failed, 0);
        assert_eq!(completed, 0);
        assert_eq!(stale, 1);
    }
}
