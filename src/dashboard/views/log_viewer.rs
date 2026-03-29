//! Execution detail view — full-screen overlay with error-first card + tabs.
//!
//! Layout (top to bottom):
//! 1. Title bar — exec ID, agent, status marker+label, duration, attempt
//! 2. Error card — only for failed/crashed/timed_out, `FAILURE` border
//! 3. Context line — `parsed_intent`
//! 4. Timing line — phase breakdown from timestamps
//! 5. Tab bar — Input | Output | Timeline (N)
//! 6. Content pane — active tab, scrollable, `Wrap { trim: false }`
//! 7. Footer — keybinding hints, follow indicator, scroll position
//!
//! For running executions the log file is polled for new content on each tick.
//! For completed executions the full log is loaded on open.

use ratatui::{
    layout::Rect,
    style::{Style, Stylize},
    symbols::border,
    text::{Line, Span},
    widgets::{Block, Padding, Paragraph, Wrap},
    Frame,
};
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;

use crate::dashboard::theme::{self, *};
use crate::dashboard::views::conversation::markdown_to_lines_standalone;
use crate::dashboard::views::payload::{
    extract_content_from_log_line, format_log_line, format_payload_lines, JsonViewMode,
};
use crate::dashboard::views::{
    format_duration_ms, format_duration_secs, format_timestamp_ms, humanize_exec_status, truncate,
};
use crate::store::{ExecutionEventRow, ExecutionRow};

/// Maximum visual lines for error detail text inside the error card.
const ERROR_DETAIL_MAX_LINES: usize = 3;

/// Maximum characters for timeline event summaries.
const TIMELINE_SUMMARY_MAX: usize = 60;

// ── Tab types ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Input = 0,
    Output = 1,
    Timeline = 2,
}

impl Tab {
    pub fn from_index(i: usize) -> Self {
        match i {
            0 => Tab::Input,
            1 => Tab::Output,
            2 => Tab::Timeline,
            _ => Tab::Input,
        }
    }

    fn index(self) -> usize {
        self as usize
    }
}

#[derive(Debug, Clone, Default)]
struct TabState {
    scroll_offset: usize,
    follow: bool,
}

// ── State ────────────────────────────────────────────────────────────────────

/// All state needed to render and update the full-screen execution detail view.
pub struct ExecutionDetailState {
    /// Full execution row from the store.
    pub execution: ExecutionRow,
    /// All log lines currently loaded (Output tab content).
    pub lines: Vec<String>,
    /// Absolute path to the log file, if one was found.
    pub log_path: Option<PathBuf>,
    /// Byte offset into the log file after the most recent read (for tailing).
    pub file_pos: u64,
    /// Cached number of visible content rows from the last render pass.
    /// Used by the event loop to compute page-scroll distances.
    pub visible_rows: usize,
    /// JSON rendering mode for payloads/log lines.
    pub json_view_mode: JsonViewMode,
    /// Optional input payload shown under the Input tab.
    pub input_payload: Option<String>,
    /// True when input_payload is sourced from a strict execution-dispatch link.
    pub input_linked: bool,
    /// Timeline events loaded from execution_events table.
    pub timeline_events: Vec<ExecutionEventRow>,
    /// True when the initial load hit the event limit (indicates truncation).
    pub timeline_truncated: bool,
    /// Cached tab bar rect from the last render pass (used for mouse click detection).
    pub tab_bar_rect: Option<Rect>,
    active_tab: Tab,
    tab_states: [TabState; 3],
}

impl ExecutionDetailState {
    /// Create a new detail view from an execution row and associated data.
    ///
    /// Sets default tab and follow mode based on execution status:
    /// - Failed/Crashed/Timed out -> Output tab, follow off, pre-scrolled to bottom
    /// - Executing/Picked up -> Output tab, follow on
    /// - Completed -> Output tab
    /// - Queued -> Input tab
    pub fn new(
        execution: ExecutionRow,
        log_path: Option<PathBuf>,
        input_payload: Option<String>,
        input_linked: bool,
        timeline_events: Vec<ExecutionEventRow>,
        timeline_truncated: bool,
    ) -> Self {
        let (default_tab, output_follow, output_scroll) = match execution.status.as_str() {
            "failed" | "crashed" | "timed_out" => (Tab::Output, false, usize::MAX),
            "executing" | "picked_up" => (Tab::Output, true, 0),
            "completed" => (Tab::Output, false, 0),
            _ => (Tab::Input, false, 0), // queued and others
        };

        let tab_states = [
            TabState::default(), // Input (0)
            TabState {
                // Output (1)
                scroll_offset: output_scroll,
                follow: output_follow,
            },
            TabState::default(), // Timeline (2)
        ];

        let mut state = Self {
            execution,
            lines: Vec::new(),
            log_path: None,
            file_pos: 0,
            visible_rows: 20,
            json_view_mode: JsonViewMode::Humanized,
            input_payload,
            input_linked,
            timeline_events,
            timeline_truncated,
            tab_bar_rect: None,
            active_tab: default_tab,
            tab_states,
        };

        if let Some(path) = log_path {
            if path.exists() {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    state.lines = split_lines(&content);
                }
                if let Ok(meta) = std::fs::metadata(&path) {
                    state.file_pos = meta.len();
                }
                state.log_path = Some(path);
            } else {
                state.log_path = Some(path);
            }
        }

        // Fallback to output_preview when no log file content.
        if state.lines.is_empty() {
            if let Some(ref fallback) = state.execution.output_preview {
                if !fallback.is_empty() {
                    state.lines = split_lines(fallback);
                }
            }
        }

        state
    }

    /// Read any new bytes from the log file and append them as new lines.
    pub fn poll_log_file(&mut self) {
        let Some(ref path) = self.log_path else {
            return;
        };
        let Ok(mut file) = std::fs::File::open(path) else {
            return;
        };
        let Ok(meta) = file.metadata() else {
            return;
        };

        let file_len = meta.len();
        if file_len <= self.file_pos {
            return;
        }

        if file.seek(SeekFrom::Start(self.file_pos)).is_err() {
            return;
        }

        let mut new_bytes = Vec::new();
        if file.read_to_end(&mut new_bytes).is_err() {
            return;
        }

        self.file_pos = file_len;
        let new_text = String::from_utf8_lossy(&new_bytes);
        for line in new_text.lines() {
            self.lines.push(line.to_string());
        }

        if self.tab_states[Tab::Output.index()].follow {
            self.tab_states[Tab::Output.index()].scroll_offset = usize::MAX;
        }
    }

    // ── Tab navigation ───────────────────────────────────────────────────────

    /// Switch to the next tab (wrapping).
    pub fn next_tab(&mut self) {
        self.active_tab = Tab::from_index((self.active_tab.index() + 1) % 3);
    }

    /// Switch to the previous tab (wrapping).
    pub fn prev_tab(&mut self) {
        self.active_tab = Tab::from_index((self.active_tab.index() + 2) % 3);
    }

    /// Jump to a specific tab.
    pub fn set_tab(&mut self, tab: Tab) {
        self.active_tab = tab;
    }

    // ── Scroll (operates on active tab) ──────────────────────────────────────

    /// Scroll the active tab up by `n` lines.
    pub fn scroll_up(&mut self, n: usize) {
        let ts = &mut self.tab_states[self.active_tab.index()];
        ts.scroll_offset = ts.scroll_offset.saturating_sub(n);
        if n > 0 {
            ts.follow = false;
        }
    }

    /// Scroll the active tab down by `n` lines (overflow clamped during render).
    pub fn scroll_down(&mut self, n: usize) {
        let ts = &mut self.tab_states[self.active_tab.index()];
        ts.scroll_offset = ts.scroll_offset.saturating_add(n);
    }

    /// Jump to the first line of the active tab.
    pub fn scroll_to_top(&mut self) {
        let ts = &mut self.tab_states[self.active_tab.index()];
        ts.scroll_offset = 0;
        ts.follow = false;
    }

    /// Jump to the last page of the active tab.
    pub fn scroll_to_bottom(&mut self) {
        let ts = &mut self.tab_states[self.active_tab.index()];
        ts.scroll_offset = usize::MAX;
    }

    /// Toggle follow mode for the active tab. Enabling it jumps to the bottom.
    pub fn toggle_follow(&mut self) {
        let ts = &mut self.tab_states[self.active_tab.index()];
        ts.follow = !ts.follow;
        if ts.follow {
            ts.scroll_offset = usize::MAX;
        }
    }

    /// Toggle JSON pretty rendering mode.
    pub fn toggle_pretty_json(&mut self) {
        self.json_view_mode = match self.json_view_mode {
            JsonViewMode::Humanized => JsonViewMode::RawPretty,
            JsonViewMode::RawPretty => JsonViewMode::Humanized,
        };
    }

    /// Returns `true` if the execution is still in a running state.
    pub fn is_running(&self) -> bool {
        matches!(
            self.execution.status.as_str(),
            "executing" | "picked_up" | "queued"
        )
    }
}

// ── Rendering ────────────────────────────────────────────────────────────────

/// Render the full-screen execution detail view into `area`.
pub fn render_execution_detail(f: &mut Frame, state: &mut ExecutionDetailState, area: Rect) {
    // Inner width after outer block borders + padding (2 borders + 2 padding).
    let outer_inner_width = area.width.saturating_sub(4);

    // Pre-compute content lines for the active tab.
    let content_lines = build_content_lines(state);

    // Build content paragraph (not yet scrolled) to measure visual line count.
    let content_para = Paragraph::new(content_lines)
        .wrap(Wrap { trim: false })
        .style(Style::new().bg(BG_PANEL).fg(TEXT_NORMAL));
    let visual_line_count = content_para.line_count(outer_inner_width);

    // Compute header height.
    let show_error = is_failure_status(&state.execution.status);
    let error_card_h = if show_error {
        compute_error_card_height(&state.execution, outer_inner_width)
    } else {
        0
    };
    // Header = error_card + context(1) + timing(1) + blank(1) + tab_bar(1) + blank(1).
    let header_h = error_card_h + 5;
    let inner_height = area.height.saturating_sub(2) as usize;
    let content_visible = inner_height.saturating_sub(header_h as usize);
    state.visible_rows = content_visible;

    // Compute scroll for the active tab — writeback clamped offset.
    let ts = &mut state.tab_states[state.active_tab.index()];
    let max_offset = visual_line_count.saturating_sub(content_visible);
    let scroll_offset = if ts.follow {
        max_offset
    } else {
        ts.scroll_offset.min(max_offset)
    };
    ts.scroll_offset = scroll_offset;
    let follow_on = ts.follow;

    // Build title and footer.
    let title_spans = build_title_spans(state);
    let footer_spans = build_footer_spans(state, follow_on, scroll_offset, visual_line_count);

    // Render outer block.
    let outer_block = Block::bordered()
        .border_set(border::ONE_EIGHTH_WIDE)
        .border_style(Style::new().fg(BORDER_DIM))
        .title(Line::from(title_spans))
        .title_bottom(Line::from(footer_spans))
        .padding(Padding::new(1, 1, 0, 0))
        .style(Style::new().bg(BG_PANEL).fg(TEXT_NORMAL));

    let inner = outer_block.inner(area);
    f.render_widget(outer_block, area);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let bottom = inner.y + inner.height;
    let mut y = inner.y;

    // Error card (conditional).
    if show_error && error_card_h > 0 {
        let card_area = Rect::new(inner.x, y, inner.width, error_card_h);
        render_error_card(f, &state.execution, card_area);
        y += error_card_h;
    }

    // Context line (intent).
    if y < bottom {
        let ctx_area = Rect::new(inner.x, y, inner.width, 1);
        render_context_line(f, &state.execution, ctx_area);
        y += 1;
    }

    // Timing line.
    if y < bottom {
        let timing_area = Rect::new(inner.x, y, inner.width, 1);
        render_timing_line(f, &state.execution, timing_area);
        y += 1;
    }

    // Blank line.
    if y < bottom {
        y += 1;
    }

    // Tab bar.
    if y < bottom {
        let tab_area = Rect::new(inner.x, y, inner.width, 1);
        state.tab_bar_rect = Some(tab_area);
        render_tab_bar(f, state.active_tab, &state.timeline_events, tab_area);
        y += 1;
    }

    // Blank line.
    if y < bottom {
        y += 1;
    }

    // Content pane (remaining height).
    let content_h = bottom.saturating_sub(y);
    if content_h > 0 {
        let content_area = Rect::new(inner.x, y, inner.width, content_h);
        let scrolled = content_para.scroll((scroll_offset.min(u16::MAX as usize) as u16, 0));
        f.render_widget(scrolled, content_area);
    }
}

// ── Title & footer builders ──────────────────────────────────────────────────

fn build_title_spans(state: &ExecutionDetailState) -> Vec<Span<'static>> {
    let exec = &state.execution;
    let exec_short = truncate(&exec.id, 18);
    let status_label = humanize_exec_status(&exec.status);
    let status_color = theme::exec_status_color(&exec.status);
    let marker = exec_status_marker(&exec.status);
    let duration_label = exec
        .duration_ms
        .map(format_duration_ms)
        .unwrap_or_else(|| "-".to_string());

    let mut spans = vec![
        Span::raw(" "),
        exec_short.bold().fg(TEXT_BRIGHT),
        " ── ".fg(TEXT_DIM),
        exec.agent_alias.clone().fg(TEXT_MUTED),
        " ── ".fg(TEXT_DIM),
        format!("{} {}", marker, status_label)
            .fg(status_color)
            .bold(),
        " ── ".fg(TEXT_DIM),
        duration_label.fg(TEXT_DIM),
    ];

    if exec.attempt_number > 1 {
        spans.push(" ── ".fg(TEXT_DIM));
        spans.push(format!("#{}", exec.attempt_number).fg(WARNING).bold());
    }

    spans.push(Span::raw(" "));
    spans
}

fn build_footer_spans(
    state: &ExecutionDetailState,
    follow_on: bool,
    scroll_offset: usize,
    total_lines: usize,
) -> Vec<Span<'static>> {
    let key = |s: &'static str| -> Span<'static> { s.fg(ACCENT).bold() };

    let mut spans = vec![
        Span::raw(" "),
        key("Esc"),
        " back  ".fg(TEXT_MUTED),
        key("Tab"),
        " pane  ".fg(TEXT_MUTED),
        key("↑↓"),
        " scroll  ".fg(TEXT_MUTED),
        key("g/G"),
        " top/bottom  ".fg(TEXT_MUTED),
        key("f"),
        " follow  ".fg(TEXT_MUTED),
        key("J"),
        " json".fg(TEXT_MUTED),
    ];

    if follow_on {
        spans.push("  ◉".fg(ACCENT).bold());
    }

    let json_indicator = match state.json_view_mode {
        JsonViewMode::Humanized => "",
        JsonViewMode::RawPretty => "  [json]",
    };
    if !json_indicator.is_empty() {
        spans.push(json_indicator.fg(ACCENT).bold());
    }

    let position = if total_lines == 0 {
        String::new()
    } else if follow_on {
        format!("  {} ", total_lines)
    } else {
        format!("  {}/{} ", scroll_offset + 1, total_lines)
    };
    spans.push(position.fg(TEXT_DIM));

    spans
}

// ── Header section renderers ─────────────────────────────────────────────────

fn render_error_card(f: &mut Frame, execution: &ExecutionRow, area: Rect) {
    let category = execution.error_category.as_deref().unwrap_or("unknown");
    let exit_code = execution.exit_code;
    let detail = execution.error_detail.as_deref().unwrap_or("");

    let mut lines: Vec<Line<'static>> = Vec::new();

    // Line 1: category left, exit_code right.
    let cat_text = format!("category: {}", category);
    let exit_text = exit_code
        .map(|c| format!("exit code: {}", c))
        .unwrap_or_default();
    let inner_w = area.width.saturating_sub(4) as usize;
    let pad_len = inner_w.saturating_sub(cat_text.len() + exit_text.len());
    lines.push(Line::from(vec![
        Span::styled(cat_text, Style::new().fg(TEXT_MUTED)),
        Span::raw(" ".repeat(pad_len)),
        Span::styled(exit_text, Style::new().fg(TEXT_MUTED)),
    ]));

    // Lines 2+: error_detail wrapped (Paragraph handles wrapping).
    if !detail.is_empty() {
        lines.push(Line::from(Span::styled(
            detail.to_string(),
            Style::new().fg(TEXT_NORMAL),
        )));
    }

    let block = Block::bordered()
        .border_set(border::ONE_EIGHTH_WIDE)
        .border_style(Style::new().fg(FAILURE))
        .title(Line::from(" Error ".fg(FAILURE).bold()))
        .padding(Padding::new(1, 1, 0, 0))
        .style(Style::new().bg(BG_CARD));

    let paragraph = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(block);

    f.render_widget(paragraph, area);
}

/// Compute the exact height of the error card using ratatui's own wrapping engine.
fn compute_error_card_height(execution: &ExecutionRow, area_width: u16) -> u16 {
    let category = execution.error_category.as_deref().unwrap_or("unknown");
    let exit_code = execution.exit_code;
    let detail = execution.error_detail.as_deref().unwrap_or("");

    let mut lines: Vec<Line<'static>> = Vec::new();
    let cat_text = format!("category: {}", category);
    let exit_text = exit_code
        .map(|c| format!("exit code: {}", c))
        .unwrap_or_default();
    lines.push(Line::from(vec![
        Span::from(cat_text),
        Span::from(exit_text),
    ]));
    if !detail.is_empty() {
        lines.push(Line::from(detail.to_string()));
    }

    // Card inner width: area - 2 card borders - 2 card padding.
    let card_inner_w = area_width.saturating_sub(4).max(1);
    let visual_lines = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .line_count(card_inner_w)
        .min(ERROR_DETAIL_MAX_LINES + 1); // 1 category + up to N detail
                                          // 2 borders + visual content lines.
    (2 + visual_lines) as u16
}

fn render_context_line(f: &mut Frame, execution: &ExecutionRow, area: Rect) {
    let intent = execution
        .parsed_intent
        .as_deref()
        .unwrap_or("(unavailable)");
    let intent_color = if execution.parsed_intent.is_some() {
        TEXT_MUTED
    } else {
        TEXT_DIM
    };
    let line = Line::from(vec![
        Span::styled("intent: ", Style::new().fg(TEXT_DIM)),
        Span::styled(intent.to_string(), Style::new().fg(intent_color)),
    ]);
    f.render_widget(Paragraph::new(vec![line]), area);
}

fn render_timing_line(f: &mut Frame, execution: &ExecutionRow, area: Rect) {
    let mut spans: Vec<Span<'static>> = vec![Span::styled("timing: ", Style::new().fg(TEXT_DIM))];

    let now_secs = now_unix_secs();

    // Queued phase: queued_at -> picked_up_at (or started_at, or now).
    let queued_end = execution
        .picked_up_at
        .or(execution.started_at)
        .unwrap_or(now_secs);
    let queued_dur = (queued_end - execution.queued_at).max(0);
    spans.push(Span::styled(
        format!("queued {}", format_duration_secs(queued_dur)),
        Style::new().fg(WARNING),
    ));

    // Picked-up phase: picked_up_at -> started_at.
    if let (Some(picked_up), Some(started)) = (execution.picked_up_at, execution.started_at) {
        let dur = (started - picked_up).max(0);
        spans.push(Span::styled(" → ", Style::new().fg(TEXT_DIM)));
        spans.push(Span::styled(
            format!("picked up {}", format_duration_secs(dur)),
            Style::new().fg(TEXT_MUTED),
        ));
    }

    // Executing phase: started_at -> finished_at (or now).
    if let Some(started) = execution.started_at {
        let end = execution.finished_at.unwrap_or(now_secs);
        let dur = (end - started).max(0);
        spans.push(Span::styled(" → ", Style::new().fg(TEXT_DIM)));
        spans.push(Span::styled(
            format!("executing {}", format_duration_secs(dur)),
            Style::new().fg(ACCENT),
        ));
    }

    let line = Line::from(spans);
    f.render_widget(Paragraph::new(vec![line]), area);
}

fn render_tab_bar(f: &mut Frame, active: Tab, events: &[ExecutionEventRow], area: Rect) {
    let tab_labels = [
        "Input".to_string(),
        "Output".to_string(),
        format!("Timeline ({})", events.len()),
    ];

    let mut spans: Vec<Span<'static>> = vec![Span::styled("╶ ", Style::new().fg(BORDER_DIM))];

    for (i, label) in tab_labels.into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("    "));
        }
        let tab = Tab::from_index(i);
        if active == tab {
            spans.push(Span::styled(label, Style::new().fg(ACCENT).bold()));
        } else {
            spans.push(Span::styled(label, Style::new().fg(TEXT_MUTED)));
        }
    }

    // Fill remaining width with separator.
    let used_width: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    let remaining = (area.width as usize).saturating_sub(used_width + 1);
    if remaining > 0 {
        let fill = format!(" {}", "╶".repeat(remaining));
        spans.push(Span::styled(fill, Style::new().fg(BORDER_DIM)));
    }

    let line = Line::from(spans);
    f.render_widget(Paragraph::new(vec![line]), area);
}

// ── Content builders ─────────────────────────────────────────────────────────

fn build_content_lines(state: &ExecutionDetailState) -> Vec<Line<'static>> {
    match state.active_tab {
        Tab::Input => build_input_lines(state),
        Tab::Output => build_output_lines(state),
        Tab::Timeline => build_timeline_lines(&state.timeline_events, state.timeline_truncated),
    }
}

fn build_output_lines(state: &ExecutionDetailState) -> Vec<Line<'static>> {
    if state.lines.is_empty() {
        return vec![Line::from("(no output)").fg(TEXT_MUTED)];
    }
    match state.json_view_mode {
        JsonViewMode::Humanized => {
            // Content extraction — shows agent's narrative text with markdown rendering.
            let text_lines: Vec<String> = state
                .lines
                .iter()
                .filter_map(|line| extract_content_from_log_line(line))
                .flat_map(|texts| texts.into_iter())
                .collect();
            if text_lines.is_empty() {
                vec![Line::from("(no text content)").fg(TEXT_MUTED)]
            } else {
                let combined = text_lines.join("\n");
                markdown_to_lines_standalone(&combined)
            }
        }
        JsonViewMode::RawPretty => {
            // Raw protocol — for debugging, pretty-printed JSON.
            state
                .lines
                .iter()
                .flat_map(|line| {
                    format_log_line(line, JsonViewMode::RawPretty)
                        .into_iter()
                        .map(|l| Line::from(l).fg(TEXT_NORMAL))
                })
                .collect()
        }
    }
}

fn build_timeline_lines(events: &[ExecutionEventRow], truncated: bool) -> Vec<Line<'static>> {
    if events.is_empty() {
        return vec![Line::from("  (no events)").fg(TEXT_MUTED)];
    }

    let mut lines: Vec<Line<'static>> = Vec::new();

    for ev in events {
        let ts = format_timestamp_ms(ev.timestamp_ms);
        let tool_or_type = ev.tool_name.as_deref().unwrap_or(&ev.event_type);
        let tool_display = format!("{:<16}", truncate(tool_or_type, 16));
        let is_error = ev.event_type.contains("error") || ev.event_type.contains("fail");
        let error_marker = if is_error { "✗   " } else { "    " };
        let summary_color = if is_error { FAILURE } else { TEXT_NORMAL };
        let marker_color = if is_error { FAILURE } else { TEXT_DIM };
        let summary = truncate(&ev.summary, TIMELINE_SUMMARY_MAX);

        lines.push(Line::from(vec![
            Span::styled(format!("  {}  ", ts), Style::new().fg(TEXT_DIM)),
            Span::styled(tool_display, Style::new().fg(TEXT_MUTED)),
            Span::styled(error_marker.to_string(), Style::new().fg(marker_color)),
            Span::styled(summary, Style::new().fg(summary_color)),
        ]));
    }

    if truncated {
        lines.push(Line::from(format!("  (showing first {} events)", events.len())).fg(TEXT_DIM));
    }

    lines
}

fn build_input_lines(state: &ExecutionDetailState) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();

    // Provenance label.
    if state.input_linked {
        lines.push(Line::from(Span::styled(
            "[linked]",
            Style::new().fg(SUCCESS),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            "[unlinked]",
            Style::new().fg(TEXT_DIM),
        )));
    }
    lines.push(Line::from(""));

    match state.input_payload.as_deref() {
        Some(payload) => {
            let trimmed = payload.trim_start();
            if trimmed.starts_with('{') {
                // JSON payload — use structured formatting.
                for line in format_payload_lines(payload, state.json_view_mode, 200) {
                    lines.push(Line::from(line).fg(TEXT_MUTED));
                }
            } else {
                // Markdown payload — render with pulldown_cmark (standalone, no prefix).
                lines.extend(markdown_to_lines_standalone(payload));
            }
        }
        None => {
            if state.input_linked {
                lines.push(Line::from("(no input)").fg(TEXT_MUTED));
            } else {
                lines.push(
                    Line::from("(input unavailable: execution not linked to dispatch message)")
                        .fg(TEXT_MUTED),
                );
            }
        }
    }

    lines
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn is_failure_status(status: &str) -> bool {
    matches!(status, "failed" | "crashed" | "timed_out")
}

fn exec_status_marker(status: &str) -> &'static str {
    match status {
        "executing" | "picked_up" => MARKER_RUNNING,
        "queued" => MARKER_QUEUED,
        "completed" => MARKER_COMPLETED,
        "failed" | "crashed" => MARKER_FAILED,
        "timed_out" => MARKER_TIMEOUT,
        _ => " ",
    }
}

fn now_unix_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Split `s` into owned lines, preserving empty lines.
fn split_lines(s: &str) -> Vec<String> {
    s.lines().map(str::to_string).collect()
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_execution(status: &str) -> ExecutionRow {
        ExecutionRow {
            id: "exec-001".to_string(),
            thread_id: "t-1".to_string(),
            batch_id: None,
            agent_alias: "focused".to_string(),
            dispatch_message_id: None,
            status: status.to_string(),
            queued_at: 1_700_000_000,
            picked_up_at: Some(1_700_000_001),
            started_at: Some(1_700_000_002),
            finished_at: Some(1_700_000_100),
            duration_ms: Some(98_000),
            exit_code: Some(0),
            output_preview: None,
            error_detail: None,
            parsed_intent: Some("Test intent".to_string()),
            prompt_hash: None,
            attempt_number: 1,
            retry_after: None,
            error_category: None,
            original_dispatch_message_id: None,
            pid: None,
            eligible_at: None,
            eligible_reason: None,
        }
    }

    fn make_state(status: &str, lines: Vec<&str>) -> ExecutionDetailState {
        let execution = make_execution(status);
        let mut state = ExecutionDetailState::new(
            execution,
            None,
            Some("{\"in\":1}".to_string()),
            true,
            Vec::new(),
            false,
        );
        state.lines = lines.into_iter().map(str::to_string).collect();
        state
    }

    // ── is_running ───────────────────────────────────────────────────────────

    #[test]
    fn test_log_viewer_is_running_executing() {
        let s = make_state("executing", vec![]);
        assert!(s.is_running());
    }

    #[test]
    fn test_log_viewer_is_not_running_completed() {
        let s = make_state("completed", vec![]);
        assert!(!s.is_running());
    }

    // ── Scroll ───────────────────────────────────────────────────────────────

    #[test]
    fn test_log_viewer_scroll_up_clamps_at_zero() {
        let mut s = make_state("completed", vec!["a"; 20]);
        s.set_tab(Tab::Output);
        s.tab_states[Tab::Output.index()].scroll_offset = 2;
        s.scroll_up(10);
        assert_eq!(s.tab_states[Tab::Output.index()].scroll_offset, 0);
    }

    #[test]
    fn test_log_viewer_scroll_up_disables_follow() {
        let mut s = make_state("executing", vec!["a"; 20]);
        s.set_tab(Tab::Output);
        s.tab_states[Tab::Output.index()].follow = true;
        s.scroll_up(1);
        assert!(!s.tab_states[Tab::Output.index()].follow);
    }

    #[test]
    fn test_log_viewer_toggle_follow_on() {
        let mut s = make_state("executing", vec!["a"; 5]);
        s.set_tab(Tab::Output);
        s.tab_states[Tab::Output.index()].scroll_offset = 0;
        s.tab_states[Tab::Output.index()].follow = false;
        s.toggle_follow();
        assert!(s.tab_states[Tab::Output.index()].follow);
        assert_eq!(s.tab_states[Tab::Output.index()].scroll_offset, usize::MAX);
    }

    // ── Tab navigation ───────────────────────────────────────────────────────

    #[test]
    fn test_tab_navigation_next() {
        let mut s = make_state("completed", vec!["a"]);
        s.active_tab = Tab::Input;
        s.next_tab();
        assert_eq!(s.active_tab, Tab::Output);
        s.next_tab();
        assert_eq!(s.active_tab, Tab::Timeline);
        s.next_tab();
        assert_eq!(s.active_tab, Tab::Input);
    }

    #[test]
    fn test_tab_navigation_prev() {
        let mut s = make_state("completed", vec!["a"]);
        s.active_tab = Tab::Input;
        s.prev_tab();
        assert_eq!(s.active_tab, Tab::Timeline);
        s.prev_tab();
        assert_eq!(s.active_tab, Tab::Output);
        s.prev_tab();
        assert_eq!(s.active_tab, Tab::Input);
    }

    #[test]
    fn test_set_tab() {
        let mut s = make_state("completed", vec!["a"]);
        s.set_tab(Tab::Timeline);
        assert_eq!(s.active_tab, Tab::Timeline);
        s.set_tab(Tab::Input);
        assert_eq!(s.active_tab, Tab::Input);
    }

    // ── Status-adaptive defaults ─────────────────────────────────────────────

    #[test]
    fn test_status_adaptive_defaults_failed() {
        let execution = make_execution("failed");
        let s = ExecutionDetailState::new(execution, None, None, false, Vec::new(), false);
        assert_eq!(s.active_tab, Tab::Output);
        assert!(!s.tab_states[Tab::Output.index()].follow);
        assert_eq!(s.tab_states[Tab::Output.index()].scroll_offset, usize::MAX);
    }

    #[test]
    fn test_status_adaptive_defaults_executing() {
        let execution = make_execution("executing");
        let s = ExecutionDetailState::new(execution, None, None, false, Vec::new(), false);
        assert_eq!(s.active_tab, Tab::Output);
        assert!(s.tab_states[Tab::Output.index()].follow);
    }

    #[test]
    fn test_status_adaptive_defaults_queued() {
        let execution = make_execution("queued");
        let s = ExecutionDetailState::new(execution, None, None, false, Vec::new(), false);
        assert_eq!(s.active_tab, Tab::Input);
        assert!(!s.tab_states[Tab::Input.index()].follow);
    }

    #[test]
    fn test_status_adaptive_defaults_completed() {
        let execution = make_execution("completed");
        let s = ExecutionDetailState::new(execution, None, None, false, Vec::new(), false);
        assert_eq!(s.active_tab, Tab::Output);
        assert!(!s.tab_states[Tab::Output.index()].follow);
        assert_eq!(s.tab_states[Tab::Output.index()].scroll_offset, 0);
    }

    // ── Independent tab scroll ───────────────────────────────────────────────

    #[test]
    fn test_independent_tab_scroll() {
        let mut s = make_state("completed", vec!["a"; 20]);
        s.set_tab(Tab::Output);
        s.tab_states[Tab::Output.index()].scroll_offset = 5;
        s.set_tab(Tab::Timeline);
        assert_eq!(s.tab_states[Tab::Timeline.index()].scroll_offset, 0);
        assert_eq!(s.tab_states[Tab::Output.index()].scroll_offset, 5);
    }

    // ── Helpers ──────────────────────────────────────────────────────────────

    #[test]
    fn test_is_failure_status() {
        assert!(is_failure_status("failed"));
        assert!(is_failure_status("crashed"));
        assert!(is_failure_status("timed_out"));
        assert!(!is_failure_status("completed"));
        assert!(!is_failure_status("executing"));
    }

    #[test]
    fn test_exec_status_marker_values() {
        assert_eq!(exec_status_marker("executing"), MARKER_RUNNING);
        assert_eq!(exec_status_marker("completed"), MARKER_COMPLETED);
        assert_eq!(exec_status_marker("failed"), MARKER_FAILED);
        assert_eq!(exec_status_marker("timed_out"), MARKER_TIMEOUT);
        assert_eq!(exec_status_marker("queued"), MARKER_QUEUED);
    }
}
