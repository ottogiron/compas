//! Execution detail view — full-screen execution output with collapsible
//! Input/Output outline.
//!
//! For running executions the log file is polled for new content on each tick.
//! For completed executions the full log is loaded on open and scroll-back works.
//! If no log file exists the execution's output preview is shown instead.

use ratatui::{
    layout::Rect,
    style::{Style, Stylize},
    symbols::border,
    text::{Line, Span},
    widgets::{Block, Padding, Paragraph},
    Frame,
};
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;

use crate::dashboard::theme::{self, *};
use crate::dashboard::views::payload::{format_log_line, format_payload_lines, JsonViewMode};
use crate::dashboard::views::{format_duration_ms, humanize_exec_status};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutlineSection {
    Input,
    Output,
}

/// All state needed to render and update the full-screen execution detail view.
pub struct ExecutionDetailState {
    /// Execution ID shown in the header (may be truncated during render).
    pub exec_id: String,
    /// Agent alias shown in the header.
    pub agent_alias: String,
    /// Raw execution status string (colour-coded in header).
    pub status: String,
    /// Execution duration in milliseconds, if known.
    pub duration_ms: Option<i64>,
    /// All log lines currently loaded.
    pub lines: Vec<String>,
    /// First visible line index (0-based, may exceed max — clamped during render).
    pub scroll_offset: usize,
    /// When `true` new lines cause an automatic scroll to the bottom.
    pub follow: bool,
    /// Absolute path to the log file, if one was found.
    pub log_path: Option<PathBuf>,
    /// Byte offset into the log file after the most recent read (for tailing).
    pub file_pos: u64,
    /// Cached number of visible content rows from the last render pass.
    /// Used by the event loop to compute page-scroll distances.
    pub visible_rows: usize,
    /// JSON rendering mode for payloads/log lines.
    pub json_view_mode: JsonViewMode,
    /// Optional input payload shown under the outline.
    pub input_payload: Option<String>,
    /// True when input_payload is sourced from a strict execution-dispatch link.
    pub input_linked: bool,
    section_selected: OutlineSection,
    input_expanded: bool,
    output_expanded: bool,
}

impl ExecutionDetailState {
    /// Create a new detail view from execution metadata.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        exec_id: String,
        agent_alias: String,
        status: String,
        duration_ms: Option<i64>,
        log_path: Option<PathBuf>,
        input_payload: Option<String>,
        input_linked: bool,
        output_fallback: Option<String>,
    ) -> Self {
        let mut state = Self {
            exec_id,
            agent_alias,
            status,
            duration_ms,
            lines: Vec::new(),
            scroll_offset: 0,
            follow: true,
            log_path: None,
            file_pos: 0,
            visible_rows: 20,
            json_view_mode: JsonViewMode::Humanized,
            input_payload,
            input_linked,
            section_selected: OutlineSection::Input,
            input_expanded: false,
            output_expanded: false,
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

        if state.lines.is_empty() {
            if let Some(fallback) = output_fallback {
                if !fallback.is_empty() {
                    state.lines = split_lines(&fallback);
                }
            }
        }

        if state.follow {
            state.scroll_to_bottom();
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

        if self.follow {
            self.scroll_to_bottom();
        }
    }

    /// Scroll up by `n` lines.
    pub fn scroll_up(&mut self, n: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(n);
        if n > 0 {
            self.follow = false;
        }
    }

    /// Scroll down by `n` lines (overflow is clamped during render).
    pub fn scroll_down(&mut self, n: usize) {
        self.scroll_offset = self.scroll_offset.saturating_add(n);
    }

    /// Jump to the first line.
    pub fn scroll_to_top(&mut self) {
        self.scroll_offset = 0;
        self.follow = false;
    }

    /// Jump to the last page of content.
    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = usize::MAX;
    }

    /// Toggle follow mode. Enabling it also jumps to the bottom.
    pub fn toggle_follow(&mut self) {
        self.follow = !self.follow;
        if self.follow {
            self.scroll_to_bottom();
        }
    }

    /// Toggle JSON pretty rendering mode.
    pub fn toggle_pretty_json(&mut self) {
        self.json_view_mode = match self.json_view_mode {
            JsonViewMode::Humanized => JsonViewMode::RawPretty,
            JsonViewMode::RawPretty => JsonViewMode::Humanized,
        };
    }

    pub fn select_next_section(&mut self) {
        self.section_selected = match self.section_selected {
            OutlineSection::Input => OutlineSection::Output,
            OutlineSection::Output => OutlineSection::Input,
        };
    }

    pub fn select_prev_section(&mut self) {
        self.select_next_section();
    }

    pub fn expand_selected_section(&mut self) {
        match self.section_selected {
            OutlineSection::Input => self.input_expanded = true,
            OutlineSection::Output => self.output_expanded = true,
        }
    }

    pub fn collapse_selected_section(&mut self) {
        match self.section_selected {
            OutlineSection::Input => self.input_expanded = false,
            OutlineSection::Output => self.output_expanded = false,
        }
    }

    pub fn toggle_selected_section(&mut self) {
        match self.section_selected {
            OutlineSection::Input => self.input_expanded = !self.input_expanded,
            OutlineSection::Output => self.output_expanded = !self.output_expanded,
        }
    }

    /// Returns `true` if the execution is still in a running state.
    pub fn is_running(&self) -> bool {
        matches!(self.status.as_str(), "executing" | "picked_up" | "queued")
    }
}

/// Render the full-screen execution detail view into `area`.
pub fn render_execution_detail(f: &mut Frame, state: &mut ExecutionDetailState, area: Rect) {
    let exec_short = super::truncate(&state.exec_id, 18);

    let status_label = humanize_exec_status(&state.status);
    let status_color = theme::exec_status_color(&state.status);
    let duration_label = state
        .duration_ms
        .map(format_duration_ms)
        .unwrap_or_else(|| "-".to_string());
    let follow_indicator = if state.follow { "  ◉ follow" } else { "" };
    let json_indicator = match state.json_view_mode {
        JsonViewMode::Humanized => "  [humanized]",
        JsonViewMode::RawPretty => "  [raw-json]",
    };
    let provenance_indicator = if state.input_linked {
        "  [linked]"
    } else {
        "  [unlinked]"
    };

    let visible_rows = area.height.saturating_sub(2) as usize;
    state.visible_rows = visible_rows;

    let input_lines = state
        .input_payload
        .as_deref()
        .map(|s| format_payload_lines(s, state.json_view_mode, 24))
        .unwrap_or_else(|| {
            if state.input_linked {
                vec!["(no input)".to_string()]
            } else {
                vec!["(input unavailable: execution not linked to dispatch message)".to_string()]
            }
        });

    let output_lines: Vec<String> = if state.lines.is_empty() {
        vec!["(no output)".to_string()]
    } else {
        state
            .lines
            .iter()
            .flat_map(|line| format_log_line(line, state.json_view_mode))
            .collect()
    };

    // Build display lines as styled Lines so section headers carry per-span colours.
    let mut display_lines: Vec<Line<'static>> = Vec::new();
    display_lines.push(section_header_line(
        "Input",
        state.section_selected == OutlineSection::Input,
        state.input_expanded,
    ));
    if state.input_expanded {
        display_lines.extend(
            input_lines
                .iter()
                .map(|l| Line::from(format!("    {l}")).fg(TEXT_MUTED)),
        );
    }
    display_lines.push(section_header_line(
        "Output",
        state.section_selected == OutlineSection::Output,
        state.output_expanded,
    ));
    if state.output_expanded {
        display_lines.extend(
            output_lines
                .iter()
                .map(|l| Line::from(format!("    {l}")).fg(TEXT_NORMAL)),
        );
    }

    let max_offset = display_lines.len().saturating_sub(visible_rows).max(0);
    let scroll_offset = state.scroll_offset.min(max_offset);

    let position_label: String = if display_lines.is_empty() {
        String::new()
    } else {
        let first = scroll_offset + 1;
        let last = (scroll_offset + visible_rows).min(display_lines.len());
        format!("  {first}-{last}/{total}  ", total = display_lines.len())
    };

    let title_spans: Vec<Span> = vec![
        Span::raw(" "),
        exec_short.bold().fg(TEXT_BRIGHT),
        Span::raw("  "),
        state.agent_alias.clone().fg(TEXT_MUTED),
        Span::raw("  "),
        status_label.fg(status_color).bold(),
        Span::raw("  "),
        duration_label.fg(TEXT_DIM),
        follow_indicator.fg(ACCENT).bold(),
        json_indicator.fg(ACCENT).bold(),
        provenance_indicator
            .fg(if state.input_linked { SUCCESS } else { FAILURE })
            .bold(),
        Span::raw(" "),
    ];

    let key = |s: &'static str| -> Span<'static> { s.fg(ACCENT).bold() };

    let footer_spans: Vec<Span> = vec![
        Span::raw(" "),
        key("Esc"),
        ": back  ".fg(TEXT_MUTED),
        key("↑/↓"),
        ": section  ".fg(TEXT_MUTED),
        key("Enter"),
        ": toggle  ".fg(TEXT_MUTED),
        key("g/G"),
        ": top/bottom  ".fg(TEXT_MUTED),
        key("f"),
        ": follow  ".fg(TEXT_MUTED),
        key("J"),
        ": view mode".fg(TEXT_MUTED),
        position_label.fg(TEXT_DIM),
    ];

    let block = Block::bordered()
        .border_set(border::ONE_EIGHTH_WIDE)
        .border_style(Style::new().fg(BORDER_DIM))
        .title(Line::from(title_spans))
        .title_bottom(Line::from(footer_spans))
        .padding(Padding::new(1, 1, 0, 0))
        .style(Style::new().bg(BG_PANEL).fg(TEXT_NORMAL));

    let paragraph = Paragraph::new(display_lines)
        // Paragraph::scroll takes (u16, u16); clamp to avoid silent wrapping on large logs.
        .scroll((scroll_offset.min(u16::MAX as usize) as u16, 0))
        .style(Style::new().bg(BG_PANEL).fg(TEXT_NORMAL))
        .block(block);
    f.render_widget(paragraph, area);
}

/// Build a themed section-header Line.
///
/// - Selected cursor `▸` is rendered in `ACCENT`; unselected is a dim space.
/// - Expand/collapse chevron (`▾`/`▸`) is rendered in `TEXT_MUTED`.
/// - Section name is `TEXT_BRIGHT` when selected, `TEXT_NORMAL` otherwise.
fn section_header_line(name: &str, selected: bool, expanded: bool) -> Line<'static> {
    let cursor = if selected { "▸" } else { " " };
    let chevron = if expanded { "▾" } else { "▸" };
    Line::from(vec![
        Span::styled(cursor, Style::new().fg(ACCENT)),
        Span::raw(" "),
        Span::styled(chevron, Style::new().fg(TEXT_MUTED)),
        Span::styled(
            format!(" {name}"),
            Style::new().fg(if selected { TEXT_BRIGHT } else { TEXT_NORMAL }),
        ),
    ])
}

/// Split `s` into owned lines, preserving empty lines.
fn split_lines(s: &str) -> Vec<String> {
    s.lines().map(str::to_string).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_state(status: &str, lines: Vec<&str>, follow: bool) -> ExecutionDetailState {
        let mut s = ExecutionDetailState {
            exec_id: "exec-001".to_string(),
            agent_alias: "focused".to_string(),
            status: status.to_string(),
            duration_ms: Some(1234),
            lines: lines.into_iter().map(str::to_string).collect(),
            scroll_offset: 0,
            follow,
            log_path: None,
            file_pos: 0,
            visible_rows: 10,
            json_view_mode: JsonViewMode::RawPretty,
            input_payload: Some("{\"in\":1}".to_string()),
            input_linked: true,
            section_selected: OutlineSection::Input,
            input_expanded: false,
            output_expanded: false,
        };
        if follow {
            s.scroll_to_bottom();
        }
        s
    }

    /// Flatten a `Line`'s spans into a plain string for snapshot assertions.
    fn line_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn test_log_viewer_is_running_executing() {
        let s = make_state("executing", vec![], false);
        assert!(s.is_running());
    }

    #[test]
    fn test_log_viewer_is_not_running_completed() {
        let s = make_state("completed", vec![], false);
        assert!(!s.is_running());
    }

    #[test]
    fn test_log_viewer_scroll_up_clamps_at_zero() {
        let mut s = make_state("completed", vec!["a"; 20], false);
        s.scroll_offset = 2;
        s.scroll_up(10);
        assert_eq!(s.scroll_offset, 0);
    }

    #[test]
    fn test_log_viewer_scroll_up_disables_follow() {
        let mut s = make_state("executing", vec!["a"; 20], true);
        s.scroll_up(1);
        assert!(!s.follow);
    }

    #[test]
    fn test_log_viewer_toggle_follow_on() {
        let mut s = make_state("executing", vec!["a"; 5], false);
        s.scroll_offset = 0;
        s.toggle_follow();
        assert!(s.follow);
        assert_eq!(s.scroll_offset, usize::MAX);
    }

    #[test]
    fn test_outline_expand_collapse() {
        let mut s = make_state("completed", vec!["a"], false);
        s.section_selected = OutlineSection::Input;
        s.expand_selected_section();
        assert!(s.input_expanded);
        s.collapse_selected_section();
        assert!(!s.input_expanded);
    }

    #[test]
    fn test_section_header_line_selected_expanded() {
        // selected=true → cursor "▸" (ACCENT), expanded=true → chevron "▾" (TEXT_MUTED)
        let line = section_header_line("Input", true, true);
        assert_eq!(line_text(&line), "▸ ▾ Input");
    }

    #[test]
    fn test_section_header_line_unselected_collapsed() {
        // selected=false → cursor " ", expanded=false → chevron "▸"
        let line = section_header_line("Output", false, false);
        assert_eq!(line_text(&line), "  ▸ Output");
    }

    #[test]
    fn test_default_sections_collapsed_and_input_selected() {
        let s = make_state("completed", vec!["a"], false);
        assert_eq!(s.section_selected, OutlineSection::Input);
        assert!(!s.input_expanded);
        assert!(!s.output_expanded);
    }
}
