//! Execution detail view — full-screen execution output with collapsible
//! Input/Output outline.
//!
//! For running executions the log file is polled for new content on each tick.
//! For completed executions the full log is loaded on open and scroll-back works.
//! If no log file exists the execution's output preview is shown instead.

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;

use crate::dashboard::views::payload::{format_log_line, format_payload_lines};
use crate::dashboard::views::{exec_status_color, format_duration_ms, humanize_exec_status};

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
    /// If true, JSON log lines are pretty-printed while rendering.
    pub pretty_json: bool,
    /// Optional input payload shown under the outline.
    pub input_payload: Option<String>,
    section_selected: OutlineSection,
    input_expanded: bool,
    output_expanded: bool,
}

impl ExecutionDetailState {
    /// Create a new detail view from execution metadata.
    pub fn new(
        exec_id: String,
        agent_alias: String,
        status: String,
        duration_ms: Option<i64>,
        log_path: Option<PathBuf>,
        input_payload: Option<String>,
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
            pretty_json: true,
            input_payload,
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
        self.pretty_json = !self.pretty_json;
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
    let exec_short: String = if state.exec_id.len() > 18 {
        format!("{}…", &state.exec_id[..18])
    } else {
        state.exec_id.clone()
    };

    let status_label = humanize_exec_status(&state.status);
    let status_color = exec_status_color(&state.status);
    let duration_label = state
        .duration_ms
        .map(format_duration_ms)
        .unwrap_or_else(|| "-".to_string());
    let follow_indicator = if state.follow { "  [follow]" } else { "" };
    let json_indicator = if state.pretty_json { "  [json]" } else { "" };

    let title_spans: Vec<Span> = vec![
        Span::raw(" "),
        Span::styled(
            exec_short,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(state.agent_alias.clone(), Style::default().fg(Color::Cyan)),
        Span::raw("  "),
        Span::styled(
            status_label,
            Style::default()
                .fg(status_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(duration_label, Style::default().fg(Color::DarkGray)),
        Span::styled(
            follow_indicator,
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            json_indicator,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
    ];

    let visible_rows = area.height.saturating_sub(2) as usize;
    state.visible_rows = visible_rows;

    let input_lines = state
        .input_payload
        .as_deref()
        .map(|s| format_payload_lines(s, state.pretty_json, 24))
        .unwrap_or_else(|| vec!["(no input)".to_string()]);

    let output_lines: Vec<String> = if state.lines.is_empty() {
        vec!["(no output)".to_string()]
    } else {
        state
            .lines
            .iter()
            .flat_map(|line| format_log_line(line, state.pretty_json))
            .collect()
    };

    let mut display_lines: Vec<String> = Vec::new();
    display_lines.push(section_header_line(
        "Input",
        state.section_selected == OutlineSection::Input,
        state.input_expanded,
    ));
    if state.input_expanded {
        display_lines.extend(input_lines.iter().map(|l| format!("    {}", l)));
    }
    display_lines.push(section_header_line(
        "Output",
        state.section_selected == OutlineSection::Output,
        state.output_expanded,
    ));
    if state.output_expanded {
        display_lines.extend(output_lines.iter().map(|l| format!("    {}", l)));
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

    let key = |s: &'static str| {
        Span::styled(
            s,
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
    };

    let footer_spans: Vec<Span> = vec![
        Span::raw(" "),
        key("Esc"),
        Span::raw(": back  "),
        key("↑/↓"),
        Span::raw(": section  "),
        key("Enter"),
        Span::raw(": toggle  "),
        key("g/G"),
        Span::raw(": top/bottom  "),
        key("f"),
        Span::raw(": follow  "),
        key("J"),
        Span::raw(": json"),
        Span::styled(position_label, Style::default().fg(Color::DarkGray)),
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .style(Style::default().bg(Color::Black).fg(Color::White))
        .title(Line::from(title_spans))
        .title_bottom(Line::from(footer_spans));

    let visible_lines: Vec<Line> = display_lines
        .iter()
        .skip(scroll_offset)
        .take(visible_rows.max(1))
        .map(|l| Line::from(Span::raw(format!(" {}", l))))
        .collect();

    let paragraph = Paragraph::new(visible_lines)
        .style(Style::default().bg(Color::Black).fg(Color::White))
        .block(block);
    f.render_widget(paragraph, area);
}

fn section_header_line(name: &str, selected: bool, expanded: bool) -> String {
    let marker = if expanded { "v" } else { ">" };
    let cursor = if selected { "*" } else { " " };
    format!("{} {} {}", cursor, marker, name)
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
            pretty_json: false,
            input_payload: Some("{\"in\":1}".to_string()),
            section_selected: OutlineSection::Input,
            input_expanded: false,
            output_expanded: false,
        };
        if follow {
            s.scroll_to_bottom();
        }
        s
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
        assert_eq!(section_header_line("Input", true, true), "* v Input");
    }

    #[test]
    fn test_default_sections_collapsed_and_input_selected() {
        let s = make_state("completed", vec!["a"], false);
        assert_eq!(s.section_selected, OutlineSection::Input);
        assert!(!s.input_expanded);
        assert!(!s.output_expanded);
    }
}
