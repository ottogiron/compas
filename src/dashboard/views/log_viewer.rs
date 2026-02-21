//! Log viewer — full-screen execution log tail/scroll view.
//!
//! Layout (within the full terminal area):
//!   ┌ <exec_id>  <agent>  <STATUS>  <duration>  [follow] ──────────────────────┐
//!   │  [log line 1]                                                              │
//!   │  [log line 2]                                                              │
//!   │  …                                                                        │
//!   ├ Esc: back  g: top  G: bottom  f: toggle follow  N-M/Total ───────────────┤
//!   └────────────────────────────────────────────────────────────────────────────┘
//!
//! For running executions the log file is polled for new content on each tick.
//! For completed executions the full log is loaded on open and scroll-back works.
//! If no log file exists the execution's `output_preview` or a fallback message
//! string is shown instead.

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;

use crate::dashboard::views::payload::format_log_line;
use crate::dashboard::views::{exec_status_color, format_duration_ms, humanize_exec_status};

// ── State ─────────────────────────────────────────────────────────────────────

/// All state needed to render and update the full-screen log viewer.
pub struct LogViewerState {
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
}

impl LogViewerState {
    /// Create a new viewer from execution metadata.
    ///
    /// `log_path` is the expected log-file path.  If the file exists its full
    /// contents are loaded immediately and `file_pos` is advanced to the end.
    /// If the file does not exist (or cannot be read), `fallback_content` is
    /// split into lines and displayed instead.
    pub fn new(
        exec_id: String,
        agent_alias: String,
        status: String,
        duration_ms: Option<i64>,
        log_path: Option<PathBuf>,
        fallback_content: Option<String>,
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
        };

        // Try to load from the log file first.
        if let Some(path) = log_path {
            if path.exists() {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    state.lines = split_lines(&content);
                }
                // Advance past the full file so future tail reads only see new bytes.
                if let Ok(meta) = std::fs::metadata(&path) {
                    state.file_pos = meta.len();
                }
                state.log_path = Some(path);
            } else {
                // Record the path even though it doesn't exist yet — the file
                // may appear once the agent starts writing.
                state.log_path = Some(path);
            }
        }

        // Fall back to the provided preview text if no file content was loaded.
        if state.lines.is_empty() {
            if let Some(fallback) = fallback_content {
                if !fallback.is_empty() {
                    state.lines = split_lines(&fallback);
                }
            }
        }

        // Begin at the bottom when follow mode is on and content is available.
        if state.follow && !state.lines.is_empty() {
            state.scroll_offset = state.lines.len().saturating_sub(1);
        }

        state
    }

    // ── File polling ──────────────────────────────────────────────────────────

    /// Read any new bytes from the log file and append them as new lines.
    ///
    /// Called on each tick when the execution is still running.  Silently
    /// ignores I/O errors — stale display is preferable to a panic.
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
            return; // nothing new
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

    // ── Navigation ────────────────────────────────────────────────────────────

    /// Scroll up by `n` lines.
    pub fn scroll_up(&mut self, n: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(n);
        // Disable follow mode when the user scrolls up manually.
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
        self.scroll_offset = self.lines.len().saturating_sub(1);
    }

    /// Toggle follow mode.  Enabling it also jumps to the bottom.
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

    /// Returns `true` if the execution is still in a running state and the log
    /// file should be polled for new content.
    pub fn is_running(&self) -> bool {
        matches!(self.status.as_str(), "executing" | "picked_up" | "queued")
    }
}

// ── Render ────────────────────────────────────────────────────────────────────

/// Render the full-screen log viewer into `area`, updating
/// `state.visible_rows` for use by the event loop.
pub fn render_log_viewer(f: &mut Frame, state: &mut LogViewerState, area: Rect) {
    // ── Header title spans ────────────────────────────────────────────────────
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
        .unwrap_or_else(|| "–".to_string());
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

    // ── Viewport geometry ─────────────────────────────────────────────────────
    // Reserve 2 rows for the top/bottom block borders and 1 for the title bar
    // that ratatui draws inside the top border.
    let visible_rows = area.height.saturating_sub(2) as usize;
    state.visible_rows = visible_rows;

    let display_lines: Vec<String> = state
        .lines
        .iter()
        .flat_map(|line| format_log_line(line, state.pretty_json))
        .collect();

    // Clamp scroll offset so we never show blank lines below the content.
    let max_offset = display_lines.len().saturating_sub(visible_rows).max(0);
    let scroll_offset = state.scroll_offset.min(max_offset);

    // ── Position indicator ────────────────────────────────────────────────────
    let position_label: String = if display_lines.is_empty() {
        String::new()
    } else {
        let first = scroll_offset + 1;
        let last = (scroll_offset + visible_rows).min(display_lines.len());
        format!("  {first}-{last}/{total}  ", total = display_lines.len())
    };

    // ── Footer keybinding spans ───────────────────────────────────────────────
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
        key("g"),
        Span::raw(": top  "),
        key("G"),
        Span::raw(": bottom  "),
        key("f"),
        Span::raw(": follow  "),
        key("J"),
        Span::raw(": json"),
        Span::styled(position_label, Style::default().fg(Color::DarkGray)),
    ];

    // ── Build block ───────────────────────────────────────────────────────────
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Line::from(title_spans))
        .title_bottom(Line::from(footer_spans));

    // ── Slice lines for the current viewport ──────────────────────────────────
    let visible_lines: Vec<Line> = if display_lines.is_empty() {
        vec![Line::from(Span::styled(
            "  (no output)",
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
        display_lines
            .iter()
            .skip(scroll_offset)
            .take(visible_rows)
            .map(|l| Line::from(Span::raw(format!(" {l}"))))
            .collect()
    };

    let paragraph = Paragraph::new(visible_lines).block(block);
    f.render_widget(paragraph, area);
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Split `s` into owned lines, preserving empty lines.
fn split_lines(s: &str) -> Vec<String> {
    s.lines().map(str::to_string).collect()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_state(status: &str, lines: Vec<&str>, follow: bool) -> LogViewerState {
        let mut s = LogViewerState {
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
        };
        if follow && !s.lines.is_empty() {
            s.scroll_offset = s.lines.len().saturating_sub(1);
        }
        s
    }

    // is_running

    #[test]
    fn test_log_viewer_is_running_executing() {
        let s = make_state("executing", vec![], false);
        assert!(s.is_running());
    }

    #[test]
    fn test_log_viewer_is_running_picked_up() {
        let s = make_state("picked_up", vec![], false);
        assert!(s.is_running());
    }

    #[test]
    fn test_log_viewer_is_running_queued() {
        let s = make_state("queued", vec![], false);
        assert!(s.is_running());
    }

    #[test]
    fn test_log_viewer_is_not_running_completed() {
        let s = make_state("completed", vec![], false);
        assert!(!s.is_running());
    }

    #[test]
    fn test_log_viewer_is_not_running_failed() {
        let s = make_state("failed", vec![], false);
        assert!(!s.is_running());
    }

    // scroll_up / scroll_down

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
    fn test_log_viewer_scroll_down_accumulates() {
        let mut s = make_state("completed", vec!["a"; 20], false);
        s.scroll_offset = 0;
        s.scroll_down(5);
        assert_eq!(s.scroll_offset, 5);
    }

    // scroll_to_top / scroll_to_bottom

    #[test]
    fn test_log_viewer_scroll_to_top() {
        let mut s = make_state("completed", vec!["a"; 20], false);
        s.scroll_offset = 15;
        s.scroll_to_top();
        assert_eq!(s.scroll_offset, 0);
        assert!(!s.follow);
    }

    #[test]
    fn test_log_viewer_scroll_to_bottom() {
        let mut s = make_state("completed", vec!["a"; 20], false);
        s.scroll_to_bottom();
        assert_eq!(s.scroll_offset, 19); // len - 1
    }

    // toggle_follow

    #[test]
    fn test_log_viewer_toggle_follow_on() {
        let mut s = make_state("executing", vec!["a"; 5], false);
        s.scroll_offset = 0;
        s.toggle_follow();
        assert!(s.follow);
        // Scroll jumped to bottom.
        assert_eq!(s.scroll_offset, 4);
    }

    #[test]
    fn test_log_viewer_toggle_follow_off() {
        let mut s = make_state("executing", vec!["a"; 5], true);
        s.toggle_follow();
        assert!(!s.follow);
    }

    // new() — fallback content

    #[test]
    fn test_log_viewer_new_fallback_used_when_no_file() {
        let s = LogViewerState::new(
            "exec-1".to_string(),
            "agent".to_string(),
            "completed".to_string(),
            None,
            None, // no log path
            Some("line1\nline2\nline3".to_string()),
        );
        assert_eq!(s.lines, vec!["line1", "line2", "line3"]);
    }

    #[test]
    fn test_log_viewer_new_empty_fallback_results_in_no_lines() {
        let s = LogViewerState::new(
            "exec-1".to_string(),
            "agent".to_string(),
            "completed".to_string(),
            None,
            None,
            None,
        );
        assert!(s.lines.is_empty());
    }

    #[test]
    fn test_log_viewer_new_follow_starts_at_bottom() {
        let s = LogViewerState::new(
            "exec-1".to_string(),
            "agent".to_string(),
            "executing".to_string(),
            None,
            None,
            Some("a\nb\nc\nd\ne".to_string()),
        );
        // follow is true by default → starts at last line
        assert!(s.follow);
        assert_eq!(s.scroll_offset, 4); // 5 lines, index 4
    }

    // poll_log_file — no-op when path is None

    #[test]
    fn test_log_viewer_poll_no_path_is_noop() {
        let mut s = make_state("executing", vec![], false);
        s.log_path = None;
        s.poll_log_file(); // must not panic
        assert!(s.lines.is_empty());
    }

    // poll_log_file — reads new content from a real temp file

    #[test]
    fn test_log_viewer_poll_reads_new_lines() {
        use std::io::Write;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("test.log");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "first line").unwrap();
        f.flush().unwrap();
        drop(f);

        let mut s = LogViewerState::new(
            "e".to_string(),
            "a".to_string(),
            "executing".to_string(),
            None,
            Some(path.clone()),
            None,
        );

        // After new(), one line loaded.
        assert_eq!(s.lines.len(), 1);
        assert_eq!(s.lines[0], "first line");

        // Append a second line.
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(f, "second line").unwrap();
        f.flush().unwrap();
        drop(f);

        s.poll_log_file();
        assert_eq!(s.lines.len(), 2);
        assert_eq!(s.lines[1], "second line");
    }

    // split_lines

    #[test]
    fn test_split_lines_basic() {
        assert_eq!(split_lines("a\nb\nc"), vec!["a", "b", "c"]);
    }

    #[test]
    fn test_split_lines_empty() {
        let result = split_lines("");
        assert!(result.is_empty());
    }

    #[test]
    fn test_split_lines_single() {
        assert_eq!(split_lines("hello"), vec!["hello"]);
    }
}
