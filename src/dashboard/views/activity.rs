//! Activity tab — unified selectable list with Needs Attention, Running,
//! Batches, and Recently Completed sections.
//!
//! Layout (within the content pane):
//!   ┌ Activity ──────────────────────── ● 2s ┐
//!   │  ── Needs Attention ──                  │
//!   │  ⚠  abc123def456…  Review Pending  …   │
//!   │                                         │
//!   │  ── Running ──                          │
//!   │  ▶  def456abc123…  Executing        …  │
//!   │                                         │
//!   │  ── Batches ──                          │
//!   │     TUI-UX         3/4 ████████░░      │
//!   │                                         │
//!   │  ── Recently Completed ──               │
//!   │  ✓  ghi789jkl012…  Completed       …   │
//!   ├─────────────────────────────────────────┤
//!   │  Active: 2  Review: 1  Failed: 0  …    │
//!   └─────────────────────────────────────────┘
//!
//! Row selection is unified across selectable sections (Needs Attention,
//! Running, Recently Completed) — section headers and batch rows are
//! non-selectable and skipped by navigation. Enter opens the log viewer
//! for the selected row's execution.

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
use crate::dashboard::views::{
    exec_status_color, format_duration_ms, format_duration_secs, humanize_exec_status,
    humanize_thread_status, thread_status_color,
};
use crate::store::ThreadStatusView;

// ── Section classification ─────────────────────────────────────────────────────

fn is_needs_attention(t: &ThreadStatusView) -> bool {
    let ts = t.thread_status.as_str();
    // Completed/abandoned threads are terminal and not actionable.
    if matches!(ts, "Completed" | "Abandoned") {
        return false;
    }
    let es = t.execution_status.as_deref().unwrap_or("");
    ts == "ReviewPending" || matches!(es, "failed" | "timed_out" | "crashed")
}

fn is_running(t: &ThreadStatusView) -> bool {
    let es = t.execution_status.as_deref().unwrap_or("");
    matches!(es, "executing" | "picked_up" | "queued")
}

fn is_completed(t: &ThreadStatusView) -> bool {
    t.execution_status.as_deref() == Some("completed")
}

// ── Batch progress aggregation ─────────────────────────────────────────────────

/// Aggregated progress for a single batch.
struct BatchProgress {
    batch_id: String,
    completed: usize,
    total: usize,
}

/// Derive per-batch progress from `rows`.
///
/// Groups rows by `batch_id` (rows with `None` or empty `batch_id` are
/// ignored).  Only batches that have at least one non-completed thread are
/// returned — fully-done batches are hidden.  Results are sorted by
/// `batch_id` for deterministic ordering.
fn batch_progress(rows: &[ThreadStatusView]) -> Vec<BatchProgress> {
    // batch_id → (completed_count, total_count)
    let mut map: HashMap<String, (usize, usize)> = HashMap::new();

    for t in rows {
        let bid = match t.batch_id.as_deref() {
            Some(b) if !b.is_empty() => b.to_string(),
            _ => continue,
        };
        let entry = map.entry(bid).or_insert((0, 0));
        entry.1 += 1;
        if t.thread_status == "Completed" {
            entry.0 += 1;
        }
    }

    let mut result: Vec<BatchProgress> = map
        .into_iter()
        .filter(|(_, (completed, total))| completed < total)
        .map(|(batch_id, (completed, total))| BatchProgress {
            batch_id,
            completed,
            total,
        })
        .collect();

    result.sort_by(|a, b| a.batch_id.cmp(&b.batch_id));
    result
}

// ── Public helpers used by app.rs for selection clamping ──────────────────────

/// Return the ordered flat list of source-row indices that are selectable.
///
/// Order: Needs Attention, then Running, then Recently Completed (≤5).
/// Classification is mutually exclusive (first match wins): a row that
/// matches `is_needs_attention` is never also counted as running or completed.
pub fn selectable_indices(rows: &[ThreadStatusView]) -> Vec<usize> {
    let mut attention = Vec::new();
    let mut running = Vec::new();
    let mut completed = Vec::new();
    for (i, t) in rows.iter().enumerate() {
        if is_needs_attention(t) {
            attention.push(i);
        } else if is_running(t) {
            running.push(i);
        } else if is_completed(t) && completed.len() < 5 {
            completed.push(i);
        }
    }
    let mut out = Vec::with_capacity(attention.len() + running.len() + completed.len());
    out.extend(attention);
    out.extend(running);
    out.extend(completed);
    out
}

/// Total count of selectable rows for a given snapshot (used for clamping).
pub fn selectable_count(rows: &[ThreadStatusView]) -> usize {
    selectable_indices(rows).len()
}

// ── Entry point ────────────────────────────────────────────────────────────────

/// Render the Activity tab into `area`.
pub fn render_activity(f: &mut Frame, app: &App, area: Rect) {
    let now_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    // ── Worker health dot ──────────────────────────────────────────────────────
    let (health_str, health_color) = compute_health(&app.activity_data, now_unix);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            " Activity ",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ))
        .title_top(
            Line::from(Span::styled(health_str, Style::default().fg(health_color))).right_aligned(),
        );

    // ── Loading state ──────────────────────────────────────────────────────────
    let Some(data) = &app.activity_data else {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "  Fetching…",
                Style::default().fg(Color::DarkGray),
            )))
            .block(block),
            area,
        );
        return;
    };

    // ── Layout: bordered block → inner → list + footer ────────────────────────
    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(inner);
    let list_area = chunks[0];
    let footer_area = chunks[1];

    // ── Categorize rows with source indices ────────────────────────────────────
    let rows = &data.rows;
    let sel_idxs = selectable_indices(rows);
    let sel_src: Option<usize> = sel_idxs.get(app.activity_selected).copied();

    // Classify each row into exactly one bucket (first match wins).
    let mut na_indexed: Vec<(usize, &ThreadStatusView)> = Vec::new();
    let mut run_indexed: Vec<(usize, &ThreadStatusView)> = Vec::new();
    let mut cmp_indexed: Vec<(usize, &ThreadStatusView)> = Vec::new();
    for (i, t) in rows.iter().enumerate() {
        if is_needs_attention(t) {
            na_indexed.push((i, t));
        } else if is_running(t) {
            run_indexed.push((i, t));
        } else if is_completed(t) && cmp_indexed.len() < 5 {
            cmp_indexed.push((i, t));
        }
    }

    // ── Build display lines ────────────────────────────────────────────────────
    // Each entry is (line, is_selectable).  We track which display-line index
    // maps to which selectable slot so we can scroll to keep it visible.
    let mut display_lines: Vec<Line<'static>> = Vec::new();
    // Maps selectable-slot index → display_lines index.
    let mut sel_to_display: Vec<usize> = Vec::new();

    // Needs Attention ─────────────────────────────────────────────────────────
    if !na_indexed.is_empty() {
        display_lines.push(section_header_line("── Needs Attention ──"));
        for (src_i, t) in &na_indexed {
            sel_to_display.push(display_lines.len());
            display_lines.push(make_row_line(t, sel_src == Some(*src_i), now_unix));
        }
    }

    // Running ─────────────────────────────────────────────────────────────────
    if !run_indexed.is_empty() {
        if !na_indexed.is_empty() {
            display_lines.push(Line::from(Span::raw("")));
        }
        display_lines.push(section_header_line("── Running ──"));
        for (src_i, t) in &run_indexed {
            sel_to_display.push(display_lines.len());
            display_lines.push(make_row_line(t, sel_src == Some(*src_i), now_unix));
        }
    }

    // Batches ─────────────────────────────────────────────────────────────────
    // Informational only — batch rows are NOT added to sel_to_display.
    let batches = batch_progress(rows);
    if !batches.is_empty() {
        if !na_indexed.is_empty() || !run_indexed.is_empty() {
            display_lines.push(Line::from(Span::raw("")));
        }
        display_lines.push(section_header_line("── Batches ──"));
        for bp in &batches {
            display_lines.push(make_batch_line(bp));
        }
    }

    // Recently Completed ──────────────────────────────────────────────────────
    if !cmp_indexed.is_empty() {
        if !na_indexed.is_empty() || !run_indexed.is_empty() || !batches.is_empty() {
            display_lines.push(Line::from(Span::raw("")));
        }
        display_lines.push(section_header_line("── Recently Completed ──"));
        for (src_i, t) in &cmp_indexed {
            sel_to_display.push(display_lines.len());
            display_lines.push(make_row_line(t, sel_src == Some(*src_i), now_unix));
        }
    }

    // Empty state ──────────────────────────────────────────────────────────────
    if na_indexed.is_empty()
        && run_indexed.is_empty()
        && cmp_indexed.is_empty()
        && batches.is_empty()
    {
        display_lines.push(Line::from(Span::styled(
            "  All clear — no activity",
            Style::default().fg(Color::DarkGray),
        )));
    }

    // ── Scroll to keep selected row visible ────────────────────────────────────
    let visible_height = list_area.height as usize;
    let selected_display_idx = sel_to_display
        .get(app.activity_selected)
        .copied()
        .unwrap_or(0);
    let scroll = compute_scroll(selected_display_idx, visible_height, display_lines.len());

    // ── Render list ───────────────────────────────────────────────────────────
    let visible: Vec<Line<'static>> = display_lines
        .into_iter()
        .skip(scroll)
        .take(visible_height.max(1))
        .collect();

    f.render_widget(Paragraph::new(visible), list_area);

    // ── Render footer ─────────────────────────────────────────────────────────
    f.render_widget(
        Paragraph::new(build_footer_line(data)).style(Style::default().fg(Color::DarkGray)),
        footer_area,
    );
}

// ── Row builder ───────────────────────────────────────────────────────────────

fn make_row_line(t: &ThreadStatusView, is_selected: bool, now_unix: i64) -> Line<'static> {
    let (icon, icon_color) = row_icon(t);
    let thread_id = truncate_id(&t.thread_id, 12);
    let (status_text, status_color) = row_status_display(t);
    let agent = t.agent_alias.as_deref().unwrap_or("-").to_string();
    let batch = match t.batch_id.as_deref() {
        Some(b) if !b.is_empty() => truncate_id(b, 8),
        _ => "-".to_string(),
    };
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
            format!("{:<18}", status_text),
            Style::default()
                .fg(status_color)
                .bg(bg)
                .add_modifier(base_mod),
        ),
        Span::styled(
            format!("{:<14}", agent),
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

// ── Batch progress line ────────────────────────────────────────────────────────

/// Build a display-only (non-selectable) line for a single batch.
///
/// Format: `   BATCH-ID       3/4     ████████░░`
/// - Batch label in Cyan, truncated to 12 chars (+ "…" if longer)
/// - `completed/total` fraction in White
/// - 10-character ASCII progress bar (█ / ░) in Yellow (or Green at 100%)
fn make_batch_line(bp: &BatchProgress) -> Line<'static> {
    let label = truncate_id(&bp.batch_id, 12);
    let fraction = format!("{}/{}", bp.completed, bp.total);

    let filled = if bp.total > 0 {
        (bp.completed * 10 / bp.total).min(10)
    } else {
        0
    };
    let bar: String = "█".repeat(filled) + &"░".repeat(10 - filled);

    let bar_color = if bp.completed == bp.total {
        Color::Green // only reachable if filter logic changes
    } else {
        Color::Yellow
    };

    Line::from(vec![
        Span::raw("   "), // indent aligns with icon column
        Span::styled(format!("{:<14}", label), Style::default().fg(Color::Cyan)),
        Span::styled(
            format!("{:<8}", fraction),
            Style::default().fg(Color::White),
        ),
        Span::styled(bar, Style::default().fg(bar_color)),
    ])
}

// ── Section header ─────────────────────────────────────────────────────────────

fn section_header_line(text: &str) -> Line<'static> {
    Line::from(Span::styled(
        format!(" {}", text),
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM),
    ))
}

// ── Column helpers ─────────────────────────────────────────────────────────────

fn row_icon(t: &ThreadStatusView) -> (&'static str, Color) {
    let ts = t.thread_status.as_str();
    let es = t.execution_status.as_deref().unwrap_or("");
    if ts == "ReviewPending" {
        ("⚠", Color::Blue)
    } else if matches!(es, "failed" | "crashed") {
        ("✗", Color::Red)
    } else if es == "timed_out" {
        ("⏱", Color::Red)
    } else if matches!(es, "executing" | "picked_up" | "queued") {
        ("▶", Color::Yellow)
    } else if es == "completed" {
        ("✓", Color::Green)
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
        // Live duration — recomputed every render
        if let Some(started) = t.started_at {
            return format_duration_secs((now_unix - started).max(0));
        }
        "-".to_string()
    } else if let Some(ms) = t.duration_ms {
        format_duration_ms(ms)
    } else {
        // Fall back to thread age
        format_duration_secs((now_unix - t.thread_updated_at).max(0))
    }
}

fn truncate_id(id: &str, max: usize) -> String {
    if id.len() > max {
        // Safety: max must be within ASCII range of the id; use char_indices for safety.
        let cut = id
            .char_indices()
            .nth(max)
            .map(|(i, _)| i)
            .unwrap_or(id.len());
        format!("{}…", &id[..cut])
    } else {
        id.to_string()
    }
}

// ── Scroll helper ──────────────────────────────────────────────────────────────

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

// ── Worker health ──────────────────────────────────────────────────────────────

fn compute_health(data: &Option<ActivityData>, now_unix: i64) -> (String, Color) {
    match data {
        Some(d) => match &d.heartbeat {
            Some((_, last_beat_at, _, _)) => {
                let age = (now_unix - last_beat_at).max(0);
                let color = if age < 30 { Color::Green } else { Color::Red };
                (format!(" ● {}s ", age), color)
            }
            None => (" ○ no beat ".to_string(), Color::DarkGray),
        },
        None => (" ".to_string(), Color::DarkGray),
    }
}

// ── Summary footer ─────────────────────────────────────────────────────────────

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
    let pending = data.queue_depth;

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
        Span::styled(format!("{}", pending), Style::default().fg(Color::White)),
    ])
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_row(
        thread_id: &str,
        thread_status: &str,
        exec_status: Option<&str>,
        started_at: Option<i64>,
        duration_ms: Option<i64>,
        updated_at: i64,
    ) -> ThreadStatusView {
        ThreadStatusView {
            thread_id: thread_id.to_string(),
            batch_id: None,
            thread_status: thread_status.to_string(),
            thread_created_at: 0,
            thread_updated_at: updated_at,
            execution_id: None,
            agent_alias: None,
            execution_status: exec_status.map(|s| s.to_string()),
            queued_at: None,
            started_at,
            finished_at: None,
            duration_ms,
            error_detail: None,
            parsed_intent: None,
        }
    }

    // ── selectable_indices ────────────────────────────────────────────────────

    #[test]
    fn test_selectable_indices_needs_attention_review_pending() {
        let rows = vec![make_row("t1", "ReviewPending", None, None, None, 0)];
        let idxs = selectable_indices(&rows);
        assert_eq!(idxs, vec![0]);
    }

    #[test]
    fn test_selectable_indices_needs_attention_failed_exec() {
        let rows = vec![make_row("t1", "Active", Some("failed"), None, None, 0)];
        let idxs = selectable_indices(&rows);
        assert_eq!(idxs, vec![0]);
    }

    #[test]
    fn test_selectable_indices_needs_attention_failed_thread_with_failed_exec() {
        let rows = vec![make_row("t1", "Failed", Some("failed"), None, None, 0)];
        let idxs = selectable_indices(&rows);
        assert_eq!(idxs, vec![0]);
    }

    #[test]
    fn test_selectable_indices_needs_attention_timed_out() {
        let rows = vec![make_row("t1", "Active", Some("timed_out"), None, None, 0)];
        let idxs = selectable_indices(&rows);
        assert_eq!(idxs, vec![0]);
    }

    #[test]
    fn test_selectable_indices_running() {
        let rows = vec![make_row(
            "t1",
            "Active",
            Some("executing"),
            Some(100),
            None,
            0,
        )];
        let idxs = selectable_indices(&rows);
        assert_eq!(idxs, vec![0]);
    }

    #[test]
    fn test_selectable_indices_completed() {
        let rows = vec![make_row(
            "t1",
            "Completed",
            Some("completed"),
            None,
            Some(500),
            0,
        )];
        let idxs = selectable_indices(&rows);
        assert_eq!(idxs, vec![0]);
    }

    #[test]
    fn test_selectable_indices_completed_capped_at_5() {
        let rows: Vec<ThreadStatusView> = (0..8)
            .map(|i| {
                make_row(
                    &format!("t{}", i),
                    "Completed",
                    Some("completed"),
                    None,
                    Some(100),
                    i as i64,
                )
            })
            .collect();
        let idxs = selectable_indices(&rows);
        assert_eq!(idxs.len(), 5, "should cap completed section at 5");
    }

    #[test]
    fn test_selectable_indices_order() {
        // Needs Attention first, then Running, then Completed
        let rows = vec![
            make_row("t0", "Completed", Some("completed"), None, Some(100), 0),
            make_row("t1", "Active", Some("executing"), Some(1000), None, 1),
            make_row("t2", "ReviewPending", None, None, None, 2),
        ];
        let idxs = selectable_indices(&rows);
        // t2 (idx 2) is NeedsAttention, t1 (idx 1) is Running, t0 (idx 0) is Completed
        assert_eq!(idxs, vec![2, 1, 0]);
    }

    #[test]
    fn test_selectable_indices_completed_not_needs_attention() {
        let rows = vec![
            make_row("t0", "Completed", Some("completed"), None, Some(100), 0),
            make_row("t1", "Active", Some("executing"), Some(1000), None, 1),
        ];
        let idxs = selectable_indices(&rows);
        assert_eq!(idxs, vec![1, 0], "completed row must not be in Needs Attention");
    }

    #[test]
    fn test_selectable_indices_review_pending_with_completed_exec_no_duplicate() {
        // A ReviewPending thread with a completed execution should appear
        // exactly once — in Needs Attention, NOT also in Recently Completed.
        let rows = vec![
            make_row("t1", "ReviewPending", Some("completed"), None, Some(300), 0),
            make_row("t2", "Completed", Some("completed"), None, Some(100), 1),
        ];
        let idxs = selectable_indices(&rows);
        // t1 (idx 0) → Needs Attention only; t2 (idx 1) → Completed
        assert_eq!(idxs, vec![0, 1]);
        // Verify t1 is NOT duplicated
        assert_eq!(
            idxs.iter().filter(|&&i| i == 0).count(),
            1,
            "ReviewPending+completed row must appear exactly once"
        );
    }

    #[test]
    fn test_selectable_indices_skips_non_categorized() {
        // Active thread with no execution — not in any section
        let rows = vec![make_row("t1", "Active", None, None, None, 0)];
        let idxs = selectable_indices(&rows);
        assert!(idxs.is_empty());
    }

    #[test]
    fn test_selectable_indices_abandoned_not_selectable_without_failing_exec() {
        let rows = vec![make_row("t1", "Abandoned", None, None, None, 0)];
        let idxs = selectable_indices(&rows);
        assert!(idxs.is_empty());
    }

    #[test]
    fn test_selectable_count() {
        let rows = vec![
            make_row("t1", "ReviewPending", None, None, None, 0),
            make_row("t2", "Active", Some("executing"), Some(100), None, 1),
            make_row("t3", "Completed", Some("completed"), None, Some(500), 2),
        ];
        assert_eq!(selectable_count(&rows), 3);
    }

    // ── truncate_id ───────────────────────────────────────────────────────────

    #[test]
    fn test_truncate_id_short() {
        assert_eq!(truncate_id("abc", 12), "abc");
    }

    #[test]
    fn test_truncate_id_exact() {
        assert_eq!(truncate_id("abcdefghijkl", 12), "abcdefghijkl");
    }

    #[test]
    fn test_truncate_id_long() {
        let id = "abcdefghijklmnopqrstuvwxyz";
        let result = truncate_id(id, 12);
        // Should be 12 chars + ellipsis character
        assert!(result.starts_with("abcdefghijkl"));
        assert!(result.ends_with('…'));
    }

    // ── compute_scroll ────────────────────────────────────────────────────────

    #[test]
    fn test_compute_scroll_fits_in_view() {
        assert_eq!(compute_scroll(2, 20, 10), 0);
    }

    #[test]
    fn test_compute_scroll_at_top() {
        assert_eq!(compute_scroll(0, 5, 20), 0);
    }

    #[test]
    fn test_compute_scroll_near_bottom() {
        // selected=18, visible=5, total=20 → can't go past total-visible=15
        assert_eq!(compute_scroll(18, 5, 20), 15);
    }

    #[test]
    fn test_compute_scroll_midpoint() {
        // selected=10, visible=6, half=3 → offset=7
        assert_eq!(compute_scroll(10, 6, 30), 7);
    }

    #[test]
    fn test_compute_scroll_zero_visible() {
        assert_eq!(compute_scroll(5, 0, 20), 0);
    }

    // ── row_duration ──────────────────────────────────────────────────────────

    #[test]
    fn test_row_duration_running_with_started_at() {
        let t = make_row("t1", "Active", Some("executing"), Some(1000), None, 0);
        let now = 1030;
        let d = row_duration(&t, now);
        assert_eq!(d, "30s");
    }

    #[test]
    fn test_row_duration_completed_uses_duration_ms() {
        let t = make_row("t1", "Completed", Some("completed"), None, Some(1500), 0);
        let d = row_duration(&t, 9999);
        assert_eq!(d, "1s");
    }

    #[test]
    fn test_row_duration_fallback_to_thread_age() {
        // No exec status, no duration_ms
        let t = make_row("t1", "Active", None, None, None, 900);
        let d = row_duration(&t, 960);
        assert_eq!(d, "1m 0s"); // 960-900 = 60s → format_duration_secs(60) = "1m 0s"
    }

    // ── row_icon ──────────────────────────────────────────────────────────────

    #[test]
    fn test_row_icon_review_pending() {
        let t = make_row("t1", "ReviewPending", None, None, None, 0);
        let (icon, color) = row_icon(&t);
        assert_eq!(icon, "⚠");
        assert_eq!(color, Color::Blue);
    }

    #[test]
    fn test_row_icon_failed() {
        let t = make_row("t1", "Active", Some("failed"), None, None, 0);
        let (icon, color) = row_icon(&t);
        assert_eq!(icon, "✗");
        assert_eq!(color, Color::Red);
    }

    #[test]
    fn test_row_icon_timed_out() {
        let t = make_row("t1", "Active", Some("timed_out"), None, None, 0);
        let (icon, _) = row_icon(&t);
        assert_eq!(icon, "⏱");
    }

    #[test]
    fn test_row_icon_executing() {
        let t = make_row("t1", "Active", Some("executing"), Some(1000), None, 0);
        let (icon, color) = row_icon(&t);
        assert_eq!(icon, "▶");
        assert_eq!(color, Color::Yellow);
    }

    #[test]
    fn test_row_icon_completed() {
        let t = make_row("t1", "Completed", Some("completed"), None, Some(500), 0);
        let (icon, color) = row_icon(&t);
        assert_eq!(icon, "✓");
        assert_eq!(color, Color::Green);
    }

    // ── compute_health ────────────────────────────────────────────────────────

    #[test]
    fn test_compute_health_green_when_fresh() {
        let data = Some(ActivityData {
            rows: vec![],
            thread_counts: vec![],
            queue_depth: 0,
            heartbeat: Some(("w1".to_string(), 1000, 900, None)),
            fetched_at: std::time::Instant::now(),
        });
        let now = 1020; // 20s ago → green
        let (_, color) = compute_health(&data, now);
        assert_eq!(color, Color::Green);
    }

    #[test]
    fn test_compute_health_red_when_stale() {
        let data = Some(ActivityData {
            rows: vec![],
            thread_counts: vec![],
            queue_depth: 0,
            heartbeat: Some(("w1".to_string(), 1000, 900, None)),
            fetched_at: std::time::Instant::now(),
        });
        let now = 1060; // 60s ago → red
        let (_, color) = compute_health(&data, now);
        assert_eq!(color, Color::Red);
    }

    #[test]
    fn test_compute_health_no_heartbeat() {
        let data = Some(ActivityData {
            rows: vec![],
            thread_counts: vec![],
            queue_depth: 0,
            heartbeat: None,
            fetched_at: std::time::Instant::now(),
        });
        let (s, color) = compute_health(&data, 0);
        assert!(s.contains("no beat"));
        assert_eq!(color, Color::DarkGray);
    }

    #[test]
    fn test_compute_health_no_data() {
        let (_, color) = compute_health(&None, 0);
        assert_eq!(color, Color::DarkGray);
    }

    // ── build_footer_line ─────────────────────────────────────────────────────

    #[test]
    fn test_build_footer_line_counts() {
        let data = ActivityData {
            rows: vec![],
            thread_counts: vec![
                ("Active".to_string(), 3),
                ("ReviewPending".to_string(), 1),
                ("Failed".to_string(), 2),
                ("Completed".to_string(), 10),
            ],
            queue_depth: 5,
            heartbeat: None,
            fetched_at: std::time::Instant::now(),
        };
        let line = build_footer_line(&data);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("Active: 3"), "active count");
        assert!(text.contains("Review: 1"), "review count");
        assert!(text.contains("Failed: 2"), "failed count");
        assert!(text.contains("Completed: 10"), "completed count");
        assert!(text.contains("Pending: 5"), "pending count");
    }

    #[test]
    fn test_build_footer_line_snake_case_statuses() {
        // Ensure snake_case variants from DB are also counted
        let data = ActivityData {
            rows: vec![],
            thread_counts: vec![("active".to_string(), 2), ("review_pending".to_string(), 1)],
            queue_depth: 0,
            heartbeat: None,
            fetched_at: std::time::Instant::now(),
        };
        let line = build_footer_line(&data);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("Active: 2"), "active count from snake_case");
        assert!(text.contains("Review: 1"), "review count from snake_case");
    }

    // ── batch_progress ────────────────────────────────────────────────────────

    /// Minimal row factory for batch tests — only batch_id and thread_status matter.
    fn make_batch_row(
        thread_id: &str,
        thread_status: &str,
        batch_id: Option<&str>,
    ) -> ThreadStatusView {
        ThreadStatusView {
            thread_id: thread_id.to_string(),
            batch_id: batch_id.map(|s| s.to_string()),
            thread_status: thread_status.to_string(),
            thread_created_at: 0,
            thread_updated_at: 0,
            execution_id: None,
            agent_alias: None,
            execution_status: None,
            queued_at: None,
            started_at: None,
            finished_at: None,
            duration_ms: None,
            error_detail: None,
            parsed_intent: None,
        }
    }

    #[test]
    fn test_batch_progress_groups_and_counts_correctly() {
        let rows = vec![
            make_batch_row("t1", "Active", Some("BATCH-1")),
            make_batch_row("t2", "Completed", Some("BATCH-1")),
            make_batch_row("t3", "Active", Some("BATCH-1")),
            make_batch_row("t4", "Active", Some("BATCH-2")),
        ];
        let batches = batch_progress(&rows);
        assert_eq!(batches.len(), 2, "two distinct active batches");

        let b1 = batches.iter().find(|b| b.batch_id == "BATCH-1").unwrap();
        assert_eq!(b1.total, 3);
        assert_eq!(b1.completed, 1);

        let b2 = batches.iter().find(|b| b.batch_id == "BATCH-2").unwrap();
        assert_eq!(b2.total, 1);
        assert_eq!(b2.completed, 0);
    }

    #[test]
    fn test_batch_progress_hides_fully_completed_batch() {
        let rows = vec![
            make_batch_row("t1", "Completed", Some("DONE-BATCH")),
            make_batch_row("t2", "Completed", Some("DONE-BATCH")),
            make_batch_row("t3", "Active", Some("PARTIAL-BATCH")),
        ];
        let batches = batch_progress(&rows);
        assert_eq!(batches.len(), 1, "fully-completed batch must be hidden");
        assert_eq!(batches[0].batch_id, "PARTIAL-BATCH");
    }

    #[test]
    fn test_batch_progress_skips_rows_with_no_batch_id() {
        let rows = vec![
            make_batch_row("t1", "Active", None),
            make_batch_row("t2", "Active", Some("")),
            make_batch_row("t3", "Active", Some("REAL-BATCH")),
        ];
        let batches = batch_progress(&rows);
        assert_eq!(batches.len(), 1, "None and empty batch_id must be ignored");
        assert_eq!(batches[0].batch_id, "REAL-BATCH");
    }

    #[test]
    fn test_batch_progress_empty_when_no_batches() {
        let rows = vec![make_row("t1", "Active", Some("executing"), None, None, 0)];
        let batches = batch_progress(&rows);
        assert!(batches.is_empty(), "no batch_ids → empty result");
    }

    #[test]
    fn test_batch_progress_sorted_by_batch_id() {
        let rows = vec![
            make_batch_row("t1", "Active", Some("ZZZ-BATCH")),
            make_batch_row("t2", "Active", Some("AAA-BATCH")),
            make_batch_row("t3", "Active", Some("MMM-BATCH")),
        ];
        let batches = batch_progress(&rows);
        assert_eq!(batches.len(), 3);
        assert_eq!(batches[0].batch_id, "AAA-BATCH");
        assert_eq!(batches[1].batch_id, "MMM-BATCH");
        assert_eq!(batches[2].batch_id, "ZZZ-BATCH");
    }

    #[test]
    fn test_batch_progress_bar_chars() {
        // Verify the progress bar string has correct fill/empty chars
        let bp = BatchProgress {
            batch_id: "TEST".to_string(),
            completed: 3,
            total: 4,
        };
        let line = make_batch_line(&bp);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        // 3/4 = 7 filled, 3 empty (integer division: 3*10/4 = 7)
        assert!(text.contains("███████░░░"), "progress bar fill ratio");
        assert!(text.contains("3/4"), "fraction");
    }

    #[test]
    fn test_batch_progress_bar_zero_completed() {
        let bp = BatchProgress {
            batch_id: "EMPTY".to_string(),
            completed: 0,
            total: 5,
        };
        let line = make_batch_line(&bp);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("░░░░░░░░░░"), "fully empty bar");
    }
}
