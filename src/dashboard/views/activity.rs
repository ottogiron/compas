//! Ops tab — unified selectable control-plane list with Needs Attention,
//! Running, Batches, and Recently Completed sections.
//!
//! Layout (within content pane):
//! - Left: grouped list with keyboard selection and section counts.
//! - Right: context panel for the selected thread/batch with available actions.

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::dashboard::app::{ActivityData, App};
use crate::dashboard::views::payload::format_payload_lines;
use crate::dashboard::views::{
    exec_status_color, format_duration_ms, format_duration_secs, humanize_exec_status,
    humanize_thread_status, thread_status_color,
};
use crate::store::ThreadStatusView;

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
    review: usize,
    failed: usize,
    oldest_active_updated_at: Option<i64>,
}

#[derive(Debug, Default)]
struct ClassifiedRows {
    needs_attention: Vec<usize>,
    running: Vec<usize>,
    recently_completed: Vec<usize>,
    batches: Vec<BatchProgress>,
}

fn is_needs_attention(t: &ThreadStatusView) -> bool {
    let ts = t.thread_status.as_str();
    let es = t.execution_status.as_deref().unwrap_or("");

    if matches!(ts, "Completed" | "Abandoned") {
        return false;
    }

    ts == "ReviewPending" || ts == "Failed" || matches!(es, "failed" | "timed_out" | "crashed")
}

fn is_running(t: &ThreadStatusView) -> bool {
    matches!(
        t.execution_status.as_deref().unwrap_or(""),
        "executing" | "picked_up" | "queued"
    )
}

fn is_recently_completed(t: &ThreadStatusView) -> bool {
    t.execution_status.as_deref() == Some("completed") || t.thread_status == "Completed"
}

fn classify_rows(rows: &[ThreadStatusView], drill_batch: Option<&str>) -> ClassifiedRows {
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
        if is_needs_attention(row) {
            out.needs_attention.push(*idx);
        } else if is_running(row) {
            out.running.push(*idx);
        } else if is_recently_completed(row) && out.recently_completed.len() < 8 {
            out.recently_completed.push(*idx);
        }
    }

    if drill_batch.is_none() {
        out.batches = batch_progress(rows);
    }

    out
}

fn batch_progress(rows: &[ThreadStatusView]) -> Vec<BatchProgress> {
    #[derive(Default)]
    struct Agg {
        completed: usize,
        total: usize,
        active: usize,
        review: usize,
        failed: usize,
        oldest_active_updated_at: Option<i64>,
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

        let es = r.execution_status.as_deref().unwrap_or("");
        if r.thread_status == "Completed" || es == "completed" {
            agg.completed += 1;
        }
        if r.thread_status == "ReviewPending" {
            agg.review += 1;
        }
        if r.thread_status == "Failed" || matches!(es, "failed" | "timed_out" | "crashed") {
            agg.failed += 1;
        }
        if is_running(r) || r.thread_status == "Active" {
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
            review: agg.review,
            failed: agg.failed,
            oldest_active_updated_at: agg.oldest_active_updated_at,
        })
        .collect();

    out.sort_by(|a, b| {
        let a_score = (a.failed > 0, a.review > 0, a.active > 0);
        let b_score = (b.failed > 0, b.review > 0, b.active > 0);
        b_score
            .cmp(&a_score)
            .then_with(|| a.batch_id.cmp(&b.batch_id))
    });

    out
}

pub fn ops_selectable_targets(
    rows: &[ThreadStatusView],
    drill_batch: Option<&str>,
) -> Vec<OpsSelectable> {
    let classified = classify_rows(rows, drill_batch);
    let mut out = Vec::new();

    out.extend(
        classified
            .needs_attention
            .iter()
            .copied()
            .map(OpsSelectable::Thread),
    );
    out.extend(
        classified
            .running
            .iter()
            .copied()
            .map(OpsSelectable::Thread),
    );
    out.extend(
        classified
            .batches
            .iter()
            .map(|b| OpsSelectable::Batch(b.batch_id.clone())),
    );
    out.extend(
        classified
            .recently_completed
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
) -> Option<OpsSelectable> {
    ops_selectable_targets(rows, drill_batch)
        .into_iter()
        .nth(selected)
}

pub fn ops_selectable_count(rows: &[ThreadStatusView], drill_batch: Option<&str>) -> usize {
    ops_selectable_targets(rows, drill_batch).len()
}

// Compatibility helpers retained for existing call sites/tests.
pub fn selectable_indices(rows: &[ThreadStatusView]) -> Vec<usize> {
    let ClassifiedRows {
        needs_attention,
        running,
        recently_completed,
        ..
    } = classify_rows(rows, None);
    needs_attention
        .into_iter()
        .chain(running)
        .chain(recently_completed)
        .collect()
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

    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            " Ops ",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ))
        .title_top(
            Line::from(Span::styled(health_str, Style::default().fg(health_color))).right_aligned(),
        );

    let Some(data) = &app.activity_data else {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "  Fetching...",
                Style::default().fg(Color::DarkGray),
            )))
            .block(block),
            area,
        );
        return;
    };

    let inner = block.inner(area);
    f.render_widget(block, area);

    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(64), Constraint::Percentage(36)])
        .split(inner);

    let left = panes[0];
    let right = panes[1];

    render_ops_list(f, app, data, left, now_unix);
    render_context_panel(f, app, data, right, now_unix);
}

fn render_ops_list(f: &mut Frame, app: &App, data: &ActivityData, area: Rect, now_unix: i64) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(area);
    let list_area = layout[0];
    let footer_area = layout[1];

    let classified = classify_rows(&data.rows, app.drill_batch.as_deref());

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut sel_to_line: Vec<usize> = Vec::new();
    let mut selectable_slot = 0usize;

    if let Some(batch) = app.drill_batch.as_deref() {
        lines.push(Line::from(vec![
            Span::raw(" "),
            Span::styled(
                format!("Filter: batch {}", truncate_id(batch, 22)),
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
        ]));
        lines.push(Line::from(Span::raw("")));
    }

    push_section_header(
        &mut lines,
        "Needs Attention",
        classified.needs_attention.len(),
        Color::Red,
    );
    if classified.needs_attention.is_empty() {
        lines.push(empty_line("  none"));
    } else {
        for src_idx in &classified.needs_attention {
            let Some(row) = data.rows.get(*src_idx) else {
                continue;
            };
            let is_selected = selectable_slot == app.activity_selected;
            sel_to_line.push(lines.len());
            lines.push(make_thread_line(row, is_selected, now_unix));
            selectable_slot += 1;
        }
    }

    lines.push(Line::from(Span::raw("")));

    push_section_header(
        &mut lines,
        "Running",
        classified.running.len(),
        Color::Yellow,
    );
    if classified.running.is_empty() {
        lines.push(empty_line("  none"));
    } else {
        for src_idx in &classified.running {
            let Some(row) = data.rows.get(*src_idx) else {
                continue;
            };
            let is_selected = selectable_slot == app.activity_selected;
            sel_to_line.push(lines.len());
            lines.push(make_thread_line(row, is_selected, now_unix));
            selectable_slot += 1;
        }
    }

    if app.drill_batch.is_none() {
        lines.push(Line::from(Span::raw("")));
        push_section_header(&mut lines, "Batches", classified.batches.len(), Color::Cyan);
        if classified.batches.is_empty() {
            lines.push(empty_line("  none"));
        } else {
            for batch in &classified.batches {
                let is_selected = selectable_slot == app.activity_selected;
                sel_to_line.push(lines.len());
                lines.push(make_batch_line(batch, is_selected, now_unix));
                selectable_slot += 1;
            }
        }
    }

    lines.push(Line::from(Span::raw("")));
    push_section_header(
        &mut lines,
        "Recently Completed",
        classified.recently_completed.len(),
        Color::Green,
    );
    if classified.recently_completed.is_empty() {
        lines.push(empty_line("  none"));
    } else {
        for src_idx in &classified.recently_completed {
            let Some(row) = data.rows.get(*src_idx) else {
                continue;
            };
            let is_selected = selectable_slot == app.activity_selected;
            sel_to_line.push(lines.len());
            lines.push(make_thread_line(row, is_selected, now_unix));
            selectable_slot += 1;
        }
    }

    let visible_height = list_area.height as usize;
    let selected_display_idx = sel_to_line.get(app.activity_selected).copied().unwrap_or(0);
    let scroll = compute_scroll(selected_display_idx, visible_height, lines.len());

    let visible: Vec<Line<'static>> = lines
        .into_iter()
        .skip(scroll)
        .take(visible_height.max(1))
        .collect();

    f.render_widget(Paragraph::new(visible), list_area);
    f.render_widget(
        Paragraph::new(build_footer_line(data)).style(Style::default().fg(Color::DarkGray)),
        footer_area,
    );
}

fn render_context_panel(f: &mut Frame, app: &App, data: &ActivityData, area: Rect, now_unix: i64) {
    let block = Block::default().borders(Borders::ALL).title(" Context ");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let selected = ops_selected_target(
        &data.rows,
        app.drill_batch.as_deref(),
        app.activity_selected,
    );

    let mut lines: Vec<Line> = Vec::new();

    match selected {
        Some(OpsSelectable::Thread(src_idx)) => {
            if let Some(row) = data.rows.get(src_idx) {
                let thread_status = humanize_thread_status(&row.thread_status);
                let exec_status = row
                    .execution_status
                    .as_deref()
                    .map(humanize_exec_status)
                    .unwrap_or("-");
                let duration = row_duration(row, now_unix);
                let batch = row.batch_id.as_deref().unwrap_or("-");

                lines.push(kv_line("Thread", &row.thread_id));
                lines.push(kv_line("Batch", batch));
                lines.push(kv_line("Agent", row.agent_alias.as_deref().unwrap_or("-")));
                lines.push(kv_line("Thread Status", thread_status));
                lines.push(kv_line("Execution", exec_status));
                lines.push(kv_line("Duration", &duration));
                lines.push(kv_line(
                    "Intent",
                    row.parsed_intent.as_deref().unwrap_or("-"),
                ));
                lines.push(Line::from(Span::raw("")));

                let can_abandon = row.thread_status != "Abandoned";
                let can_reopen = matches!(
                    row.thread_status.as_str(),
                    "Completed" | "Failed" | "Abandoned"
                );

                lines.push(Line::from(vec![
                    Span::styled(
                        "Available Actions",
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(":"),
                ]));
                lines.push(action_line(
                    "abandon",
                    "b",
                    can_abandon,
                    "already abandoned",
                ));
                lines.push(action_line(
                    "reopen",
                    "o",
                    can_reopen,
                    "requires terminal status",
                ));
                lines.push(action_line("action menu", "a", true, ""));
                lines.push(Line::from(Span::raw("")));

                lines.push(Line::from(vec![
                    Span::styled(
                        format!(
                            "Payload [{}]",
                            if app.pretty_payload { "pretty" } else { "raw" }
                        ),
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw("  "),
                    Span::styled(
                        "J",
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(": toggle"),
                ]));

                let payload = row
                    .error_detail
                    .as_deref()
                    .or(row.parsed_intent.as_deref())
                    .unwrap_or("-");
                for formatted in format_payload_lines(payload, app.pretty_payload, 12) {
                    lines.push(Line::from(vec![
                        Span::raw("  "),
                        Span::styled(formatted, Style::default().fg(Color::White)),
                    ]));
                }
            }
        }
        Some(OpsSelectable::Batch(batch_id)) => {
            let mut active = 0usize;
            let mut review = 0usize;
            let mut failed = 0usize;
            let mut completed = 0usize;
            let mut total = 0usize;
            for row in &data.rows {
                if row.batch_id.as_deref() != Some(batch_id.as_str()) {
                    continue;
                }
                total += 1;
                if row.thread_status == "ReviewPending" {
                    review += 1;
                }
                if row.thread_status == "Completed" {
                    completed += 1;
                }
                if row.thread_status == "Failed" {
                    failed += 1;
                }
                if row.thread_status == "Active" {
                    active += 1;
                }
            }

            lines.push(kv_line("Batch", &batch_id));
            lines.push(kv_line("Total Threads", &total.to_string()));
            lines.push(kv_line("Active", &active.to_string()));
            lines.push(kv_line("Review", &review.to_string()));
            lines.push(kv_line("Failed", &failed.to_string()));
            lines.push(kv_line("Completed", &completed.to_string()));
            lines.push(Line::from(Span::raw("")));
            lines.push(Line::from(vec![
                Span::styled(
                    "Batch Actions",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(":"),
            ]));
            if app.drill_batch.as_deref() == Some(batch_id.as_str()) {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(
                        "Esc",
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(": back to all batches"),
                ]));
            } else {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(
                        "Enter",
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(": drill into this batch"),
                ]));
            }
        }
        None => {
            lines.push(Line::from(Span::styled(
                " No selection",
                Style::default().fg(Color::DarkGray),
            )));
        }
    }

    f.render_widget(Paragraph::new(lines), inner);
}

fn make_thread_line(t: &ThreadStatusView, is_selected: bool, now_unix: i64) -> Line<'static> {
    let (icon, icon_color) = row_icon(t);
    let thread_id = truncate_id(&t.thread_id, 12);
    let (status_text, status_color) = row_status_display(t);
    let agent = t.agent_alias.as_deref().unwrap_or("-").to_string();
    let batch = t
        .batch_id
        .as_deref()
        .map(|b| truncate_id(b, 8))
        .unwrap_or_else(|| "-".to_string());
    let duration = row_duration(t, now_unix);

    let bg = if is_selected {
        Color::DarkGray
    } else {
        Color::Reset
    };
    let base_mod = if is_selected {
        Modifier::BOLD
    } else {
        Modifier::empty()
    };

    Line::from(vec![
        Span::styled(
            format!(" {} ", icon),
            Style::default().fg(icon_color).bg(bg),
        ),
        Span::styled(
            format!("{:<14}", thread_id),
            Style::default()
                .fg(Color::White)
                .bg(bg)
                .add_modifier(base_mod),
        ),
        Span::styled(
            format!("{:<16}", status_text),
            Style::default()
                .fg(status_color)
                .bg(bg)
                .add_modifier(base_mod),
        ),
        Span::styled(
            format!("{:<12}", agent),
            Style::default()
                .fg(Color::White)
                .bg(bg)
                .add_modifier(base_mod),
        ),
        Span::styled(
            format!("{:<10}", batch),
            Style::default()
                .fg(Color::DarkGray)
                .bg(bg)
                .add_modifier(base_mod),
        ),
        Span::styled(
            duration,
            Style::default()
                .fg(Color::DarkGray)
                .bg(bg)
                .add_modifier(base_mod),
        ),
    ])
}

fn make_batch_line(batch: &BatchProgress, is_selected: bool, now_unix: i64) -> Line<'static> {
    let fill = if batch.total == 0 {
        0
    } else {
        (batch.completed * 10 / batch.total).min(10)
    };
    let bar = format!("{}{}", "#".repeat(fill), "-".repeat(10 - fill));
    let age = batch
        .oldest_active_updated_at
        .map(|ts| format_duration_secs((now_unix - ts).max(0)))
        .unwrap_or_else(|| "-".to_string());

    let bg = if is_selected {
        Color::DarkGray
    } else {
        Color::Reset
    };
    let base_mod = if is_selected {
        Modifier::BOLD
    } else {
        Modifier::empty()
    };

    Line::from(vec![
        Span::styled(" B ", Style::default().fg(Color::Cyan).bg(bg)),
        Span::styled(
            format!("{:<14}", truncate_id(&batch.batch_id, 12)),
            Style::default()
                .fg(Color::White)
                .bg(bg)
                .add_modifier(base_mod),
        ),
        Span::styled(
            format!("{:<9}", format!("{}/{}", batch.completed, batch.total)),
            Style::default()
                .fg(Color::White)
                .bg(bg)
                .add_modifier(base_mod),
        ),
        Span::styled(
            format!("{:<12}", bar),
            Style::default()
                .fg(Color::Yellow)
                .bg(bg)
                .add_modifier(base_mod),
        ),
        Span::styled(
            format!("a:{} r:{} f:{}", batch.active, batch.review, batch.failed),
            Style::default().fg(Color::DarkGray).bg(bg),
        ),
        Span::styled(
            format!("  age {}", age),
            Style::default().fg(Color::DarkGray).bg(bg),
        ),
    ])
}

fn push_section_header(lines: &mut Vec<Line<'static>>, title: &str, count: usize, color: Color) {
    lines.push(Line::from(vec![
        Span::raw(" "),
        Span::styled(
            title.to_string(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            format!("({count})"),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
    ]));
}

fn empty_line(s: &str) -> Line<'static> {
    Line::from(Span::styled(
        s.to_string(),
        Style::default().fg(Color::DarkGray),
    ))
}

fn kv_line(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{}: ", label),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(value.to_string(), Style::default().fg(Color::White)),
    ])
}

fn action_line(name: &str, key: &str, enabled: bool, disabled_reason: &str) -> Line<'static> {
    let status = if enabled {
        "ready".to_string()
    } else {
        format!("blocked ({})", disabled_reason)
    };
    let status_color = if enabled {
        Color::Green
    } else {
        Color::DarkGray
    };

    Line::from(vec![
        Span::raw("  "),
        Span::styled(
            key.to_string(),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(": {}  ", name)),
        Span::styled(status, Style::default().fg(status_color)),
    ])
}

fn row_icon(t: &ThreadStatusView) -> (&'static str, Color) {
    let ts = t.thread_status.as_str();
    let es = t.execution_status.as_deref().unwrap_or("");
    if ts == "ReviewPending" {
        ("!", Color::Blue)
    } else if matches!(es, "failed" | "crashed") {
        ("X", Color::Red)
    } else if es == "timed_out" {
        ("T", Color::Red)
    } else if matches!(es, "executing" | "picked_up" | "queued") {
        (">", Color::Yellow)
    } else if es == "completed" || ts == "Completed" {
        ("*", Color::Green)
    } else {
        (" ", Color::White)
    }
}

fn row_status_display(t: &ThreadStatusView) -> (String, Color) {
    if let Some(es) = &t.execution_status {
        (humanize_exec_status(es).to_string(), exec_status_color(es))
    } else {
        let ts = &t.thread_status;
        (
            humanize_thread_status(ts).to_string(),
            thread_status_color(ts),
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

fn truncate_id(id: &str, max_chars: usize) -> String {
    if id.len() <= max_chars {
        return id.to_string();
    }

    let cut = id
        .char_indices()
        .nth(max_chars)
        .map(|(i, _)| i)
        .unwrap_or(id.len());
    format!("{}...", &id[..cut])
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
                let color = if age < 30 { Color::Green } else { Color::Red };
                (format!(" * {}s ", age), color)
            }
            None => (" o no beat ".to_string(), Color::DarkGray),
        },
        None => (" ".to_string(), Color::DarkGray),
    }
}

fn build_footer_line(data: &ActivityData) -> Line<'static> {
    let mut active = 0i64;
    let mut review = 0i64;
    let mut failed = 0i64;
    let mut completed = 0i64;

    for (status, count) in &data.thread_counts {
        match status.as_str() {
            "Active" | "active" => active += count,
            "ReviewPending" | "review_pending" => review += count,
            "Failed" | "failed" => failed += count,
            "Completed" | "completed" => completed += count,
            _ => {}
        }
    }

    let label = |s: &str, color: Color| -> Span<'static> {
        Span::styled(s.to_string(), Style::default().fg(color))
    };
    let val = |n: i64| -> Span<'static> {
        Span::styled(format!("{}  ", n), Style::default().fg(Color::White))
    };

    Line::from(vec![
        Span::raw(" "),
        label("Active: ", Color::Yellow),
        val(active),
        label("Review: ", Color::Blue),
        val(review),
        label("Failed: ", Color::Red),
        val(failed),
        label("Completed: ", Color::Green),
        val(completed),
        label("Pending: ", Color::Cyan),
        Span::styled(
            format!("{}", data.queue_depth),
            Style::default().fg(Color::White),
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_row(
        thread_id: &str,
        batch_id: Option<&str>,
        thread_status: &str,
        execution_status: Option<&str>,
    ) -> ThreadStatusView {
        ThreadStatusView {
            thread_id: thread_id.to_string(),
            batch_id: batch_id.map(|b| b.to_string()),
            thread_status: thread_status.to_string(),
            thread_created_at: 0,
            thread_updated_at: 0,
            execution_id: None,
            agent_alias: None,
            execution_status: execution_status.map(|s| s.to_string()),
            queued_at: None,
            started_at: None,
            finished_at: None,
            duration_ms: None,
            error_detail: None,
            parsed_intent: None,
        }
    }

    #[test]
    fn test_ops_selectable_targets_includes_batches() {
        let rows = vec![
            make_row("t1", Some("b1"), "Active", Some("executing")),
            make_row("t2", Some("b1"), "ReviewPending", Some("queued")),
            make_row("t3", Some("b2"), "Completed", Some("completed")),
        ];

        let targets = ops_selectable_targets(&rows, None);
        assert!(targets.iter().any(|t| matches!(t, OpsSelectable::Batch(_))));
    }

    #[test]
    fn test_ops_selectable_targets_drill_excludes_batches() {
        let rows = vec![
            make_row("t1", Some("b1"), "Active", Some("executing")),
            make_row("t2", Some("b2"), "Active", Some("queued")),
        ];

        let targets = ops_selectable_targets(&rows, Some("b1"));
        assert!(!targets.iter().any(|t| matches!(t, OpsSelectable::Batch(_))));
        assert_eq!(targets.len(), 1);
    }

    #[test]
    fn test_batch_progress_counts() {
        let rows = vec![
            make_row("t1", Some("b1"), "Active", Some("executing")),
            make_row("t2", Some("b1"), "ReviewPending", Some("queued")),
            make_row("t3", Some("b1"), "Completed", Some("completed")),
        ];

        let batches = batch_progress(&rows);
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].total, 3);
        assert_eq!(batches[0].completed, 1);
        assert_eq!(batches[0].review, 1);
    }
}
