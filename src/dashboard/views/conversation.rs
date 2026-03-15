//! Thread conversation view — full-screen overlay showing message history
//! interleaved with execution lifecycle markers.
//!
//! Opened with `c` from the Ops tab, closed with Esc. Follow the log viewer
//! pattern: full-screen overlay that replaces the tab bar.

use chrono::{TimeZone, Utc};
use pulldown_cmark::{Event, HeadingLevel, Tag, TagEnd};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style, Stylize},
    symbols::border,
    text::{Line, Span},
    widgets::{Block, Padding, Paragraph, Wrap},
    Frame,
};

use crate::dashboard::theme::{self, *};
use crate::dashboard::views::format_duration_ms;
use crate::store::{ExecutionRow, MessageRow};

// ── State ─────────────────────────────────────────────────────────────────────

/// All state needed to render and update the full-screen conversation view.
pub struct ConversationViewState {
    /// Thread being displayed.
    pub thread_id: String,
    /// Optional batch this thread belongs to.
    pub batch_id: Option<String>,
    /// Current thread status (e.g. "Active", "Completed").
    pub thread_status: String,
    /// All messages loaded for this thread.
    pub messages: Vec<MessageRow>,
    /// All executions loaded for this thread.
    pub executions: Vec<ExecutionRow>,
    /// Scroll position (in display lines).
    pub scroll_offset: usize,
    /// When true, new messages auto-scroll to the bottom.
    pub follow_mode: bool,
    /// Highest message id seen so far — used for incremental polling.
    pub last_message_id: Option<i64>,
    /// Cached visible rows from last render (for page-scroll sizing).
    pub visible_rows: usize,
}

impl ConversationViewState {
    /// Create a new conversation view from loaded data.
    pub fn new(
        thread_id: String,
        batch_id: Option<String>,
        thread_status: String,
        messages: Vec<MessageRow>,
        executions: Vec<ExecutionRow>,
    ) -> Self {
        let last_message_id = messages.iter().map(|m| m.id).max();
        Self {
            thread_id,
            batch_id,
            thread_status,
            messages,
            executions,
            scroll_offset: usize::MAX,
            follow_mode: true,
            last_message_id,
            visible_rows: 20,
        }
    }

    /// Scroll up by `n` lines (disables follow mode).
    pub fn scroll_up(&mut self, n: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(n);
        if n > 0 {
            self.follow_mode = false;
        }
    }

    /// Scroll down by `n` lines.
    pub fn scroll_down(&mut self, n: usize) {
        self.scroll_offset = self.scroll_offset.saturating_add(n);
    }

    /// Jump to the first line.
    pub fn scroll_to_top(&mut self) {
        self.scroll_offset = 0;
        self.follow_mode = false;
    }

    /// Jump to the bottom (follow position).
    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = usize::MAX;
    }

    /// Toggle follow mode — enabling it also scrolls to the bottom.
    pub fn toggle_follow(&mut self) {
        self.follow_mode = !self.follow_mode;
        if self.follow_mode {
            self.scroll_to_bottom();
        }
    }

    /// Returns `true` if the thread is still active.
    pub fn is_active(&self) -> bool {
        matches!(self.thread_status.as_str(), "Active" | "active")
    }
}

// ── Conversation item ─────────────────────────────────────────────────────────

/// A merged timeline entry — either a message or an execution lifecycle event.
enum ConversationItem<'a> {
    Message(&'a MessageRow),
    ExecutionStarted {
        agent: &'a str,
        started_at: i64,
    },
    ExecutionCompleted {
        agent: &'a str,
        finished_at: i64,
        duration_ms: Option<i64>,
        success: bool,
    },
}

fn item_timestamp(item: &ConversationItem<'_>) -> i64 {
    match item {
        ConversationItem::Message(m) => m.created_at,
        ConversationItem::ExecutionStarted { started_at, .. } => *started_at,
        ConversationItem::ExecutionCompleted { finished_at, .. } => *finished_at,
    }
}

/// Merge messages and executions into a sorted timeline.
fn build_items<'a>(
    messages: &'a [MessageRow],
    executions: &'a [ExecutionRow],
) -> Vec<ConversationItem<'a>> {
    let mut items: Vec<ConversationItem<'a>> = Vec::new();

    for msg in messages {
        items.push(ConversationItem::Message(msg));
    }

    for exec in executions {
        let started_at = exec.started_at.unwrap_or(exec.queued_at);
        items.push(ConversationItem::ExecutionStarted {
            agent: &exec.agent_alias,
            started_at,
        });
        if let Some(finished_at) = exec.finished_at {
            items.push(ConversationItem::ExecutionCompleted {
                agent: &exec.agent_alias,
                finished_at,
                duration_ms: exec.duration_ms,
                success: exec.status == "completed",
            });
        }
    }

    items.sort_by_key(|i| item_timestamp(i));
    items
}

// ── Formatting helpers ────────────────────────────────────────────────────────

/// Format a Unix timestamp (seconds) as "HH:MM UTC" for today or "Mon DD HH:MM" for older.
fn format_timestamp(ts_secs: i64) -> String {
    let dt = match Utc.timestamp_opt(ts_secs, 0) {
        chrono::LocalResult::Single(dt) => dt,
        _ => return "-".to_string(),
    };
    let today = Utc::now().date_naive();
    if dt.date_naive() == today {
        dt.format("%H:%M UTC").to_string()
    } else {
        dt.format("%b %d %H:%M").to_string()
    }
}

/// Color for intent badge label.
fn intent_color(intent: &str) -> Color {
    match intent {
        "dispatch" => Color::Cyan,
        "review-request" => Color::Yellow,
        "changes-requested" => Color::Red,
        "error" => Color::Red,
        _ => Color::DarkGray,
    }
}

/// Parse markdown body text and return styled ratatui lines, each prefixed with `│ `.
fn markdown_to_lines(body: &str, border_color: Color) -> Vec<Line<'static>> {
    let prefix = Span::styled("│ ", Style::default().fg(border_color));

    let parser = pulldown_cmark::Parser::new(body);

    // Style stack: accumulates modifier/color state from nested tags.
    let mut style_stack: Vec<Style> = vec![Style::default().fg(TEXT_NORMAL)];
    let mut current_spans: Vec<Span<'static>> = Vec::new();
    let mut result: Vec<Line<'static>> = Vec::new();
    let mut in_code_block = false;
    let mut in_heading = false;
    let mut list_depth: usize = 0;

    let current_style = |stack: &[Style]| -> Style {
        stack
            .last()
            .copied()
            .unwrap_or(Style::default().fg(TEXT_NORMAL))
    };

    let flush_line = |spans: &mut Vec<Span<'static>>,
                      result: &mut Vec<Line<'static>>,
                      prefix: &Span<'static>| {
        let mut line_spans = vec![prefix.clone()];
        line_spans.append(spans);
        result.push(Line::from(line_spans));
    };

    for event in parser {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                in_heading = true;
                let heading_style = match level {
                    HeadingLevel::H1 => Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                    _ => Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                };
                style_stack.push(heading_style);
            }
            Event::End(TagEnd::Heading(_)) => {
                flush_line(&mut current_spans, &mut result, &prefix);
                style_stack.pop();
                in_heading = false;
            }
            Event::Start(Tag::Paragraph) => {
                // No extra action needed — text events will accumulate.
            }
            Event::End(TagEnd::Paragraph) => {
                if !current_spans.is_empty() {
                    flush_line(&mut current_spans, &mut result, &prefix);
                }
                // Add blank line after paragraph (unless in heading — already handled).
                if !in_heading {
                    result.push(Line::from(vec![prefix.clone()]));
                }
            }
            Event::Start(Tag::Emphasis) => {
                style_stack.push(current_style(&style_stack).add_modifier(Modifier::ITALIC));
            }
            Event::End(TagEnd::Emphasis) => {
                style_stack.pop();
            }
            Event::Start(Tag::Strong) => {
                style_stack.push(current_style(&style_stack).add_modifier(Modifier::BOLD));
            }
            Event::End(TagEnd::Strong) => {
                style_stack.pop();
            }
            Event::Start(Tag::CodeBlock(_)) => {
                in_code_block = true;
                if !current_spans.is_empty() {
                    flush_line(&mut current_spans, &mut result, &prefix);
                }
            }
            Event::End(TagEnd::CodeBlock) => {
                in_code_block = false;
            }
            Event::Start(Tag::List(_)) => {
                list_depth += 1;
            }
            Event::End(TagEnd::List(_)) => {
                list_depth = list_depth.saturating_sub(1);
            }
            Event::Start(Tag::Item) => {
                let indent = "  ".repeat(list_depth.saturating_sub(1));
                current_spans.push(Span::styled(
                    format!("{}• ", indent),
                    Style::default().fg(TEXT_MUTED),
                ));
            }
            Event::End(TagEnd::Item) => {
                if !current_spans.is_empty() {
                    flush_line(&mut current_spans, &mut result, &prefix);
                }
            }
            Event::Code(text) => {
                current_spans.push(Span::styled(
                    text.to_string(),
                    Style::default().fg(Color::Cyan),
                ));
            }
            Event::Text(text) => {
                if in_code_block {
                    // Code blocks: render each line with dim style.
                    let code_style = Style::default().fg(TEXT_MUTED);
                    for (i, code_line) in text.lines().enumerate() {
                        if i > 0 {
                            flush_line(&mut current_spans, &mut result, &prefix);
                        }
                        current_spans.push(Span::styled(code_line.to_string(), code_style));
                    }
                } else {
                    // Normal text: split on newlines to preserve source line breaks.
                    for (i, segment) in text.lines().enumerate() {
                        if i > 0 {
                            flush_line(&mut current_spans, &mut result, &prefix);
                        }
                        current_spans.push(Span::styled(
                            segment.to_string(),
                            current_style(&style_stack),
                        ));
                    }
                }
            }
            Event::SoftBreak => {
                // Treat soft break as a space (CommonMark default).
                current_spans.push(Span::styled(" ".to_string(), current_style(&style_stack)));
            }
            Event::HardBreak => {
                flush_line(&mut current_spans, &mut result, &prefix);
            }
            Event::Rule => {
                if !current_spans.is_empty() {
                    flush_line(&mut current_spans, &mut result, &prefix);
                }
                result.push(Line::from(vec![
                    prefix.clone(),
                    Span::styled(
                        "────────────────────────────────────────────",
                        Style::default().fg(TEXT_DIM),
                    ),
                ]));
            }
            // Tables, footnotes, etc. — pass through as raw text (monospace).
            _ => {}
        }
    }

    // Flush any remaining spans.
    if !current_spans.is_empty() {
        flush_line(&mut current_spans, &mut result, &prefix);
    }

    // Trim trailing empty prefix-only lines.
    while result
        .last()
        .is_some_and(|l| l.spans.len() == 1 && l.spans[0].content.as_ref() == "│ ")
    {
        result.pop();
    }

    result
}

/// Append styled lines for a single message into `lines`.
fn push_message_lines(msg: &MessageRow, lines: &mut Vec<Line<'static>>) {
    let ts = format_timestamp(msg.created_at);
    let header_color = if msg.from_alias == "operator" {
        Color::Blue
    } else {
        Color::Green
    };
    let badge_color = intent_color(&msg.intent);

    // Header line: "from → to  [intent]  timestamp"
    lines.push(Line::from(vec![
        msg.from_alias.clone().fg(header_color).bold(),
        " → ".fg(TEXT_DIM),
        msg.to_alias.clone().fg(TEXT_MUTED),
        "  ".into(),
        format!("[{}]", msg.intent).fg(badge_color),
        "  ".into(),
        ts.fg(TEXT_DIM),
    ]));

    // Body — markdown-rendered with "│ " prefix per line
    let body = &msg.body;
    if body.is_empty() {
        lines.push(Line::from(vec!["│ ".fg(TEXT_DIM), "(empty)".fg(TEXT_DIM)]));
    } else {
        lines.extend(markdown_to_lines(body, TEXT_DIM));
    }
}

/// Append a styled execution-started marker line into `lines`.
fn push_execution_started_line(agent: &str, started_at: i64, lines: &mut Vec<Line<'static>>) {
    let ts = format_timestamp(started_at);
    lines.push(Line::from(vec![
        format!("  ▶ Execution started ({})", agent).fg(TEXT_DIM),
        "   ".into(),
        ts.fg(TEXT_DIM),
    ]));
}

/// Append a styled execution-completed or failed marker line into `lines`.
fn push_execution_completed_line(
    agent: &str,
    finished_at: i64,
    duration_ms: Option<i64>,
    success: bool,
    lines: &mut Vec<Line<'static>>,
) {
    let ts = format_timestamp(finished_at);
    let dur = duration_ms
        .map(|ms| format!(" in {}", format_duration_ms(ms)))
        .unwrap_or_default();
    if success {
        lines.push(Line::from(vec![
            format!("  ✓ Completed{} ({})", dur, agent).fg(SUCCESS_DIM),
            "   ".into(),
            ts.fg(TEXT_DIM),
        ]));
    } else {
        lines.push(Line::from(vec![
            format!("  ✗ Failed{} ({})", dur, agent).fg(FAILURE),
            "   ".into(),
            ts.fg(TEXT_DIM),
        ]));
    }
}

// ── Render ────────────────────────────────────────────────────────────────────

/// Render the full-screen conversation overlay into `area`.
pub fn render_conversation(frame: &mut Frame, state: &mut ConversationViewState, area: Rect) {
    let visible_rows = area.height.saturating_sub(2) as usize;
    state.visible_rows = visible_rows;

    let items = build_items(&state.messages, &state.executions);

    // Inner content width: area minus 2 borders and 2 padding columns (left+right each).
    let area_width = area.width.saturating_sub(4) as usize;

    // Build flat display line list
    let mut display_lines: Vec<Line<'static>> = Vec::new();
    if items.is_empty() {
        display_lines.push(Line::from("  No messages.").fg(TEXT_MUTED));
    } else {
        for item in &items {
            match item {
                ConversationItem::Message(msg) => {
                    push_message_lines(msg, &mut display_lines);
                    display_lines.push(Line::from(Span::styled(
                        "─".repeat(area_width),
                        Style::default().fg(BORDER_DIM),
                    )));
                }
                ConversationItem::ExecutionStarted { agent, started_at } => {
                    push_execution_started_line(agent, *started_at, &mut display_lines);
                    display_lines.push(Line::from(""));
                }
                ConversationItem::ExecutionCompleted {
                    agent,
                    finished_at,
                    duration_ms,
                    success,
                } => {
                    push_execution_completed_line(
                        agent,
                        *finished_at,
                        *duration_ms,
                        *success,
                        &mut display_lines,
                    );
                    display_lines.push(Line::from(""));
                }
            }
        }
    }

    // Build title and footer spans
    let thread_short = super::truncate(&state.thread_id, 20);
    let status_color = theme::thread_status_color(&state.thread_status);
    let follow_indicator = if state.follow_mode {
        "  ◉ follow"
    } else {
        ""
    };

    let mut title_spans: Vec<Span> = vec![
        Span::raw(" Conversation: "),
        thread_short.fg(TEXT_BRIGHT).bold(),
        "  ".fg(TEXT_DIM),
        state.thread_status.clone().fg(status_color).bold(),
    ];
    if let Some(ref batch_id) = state.batch_id {
        title_spans.push("  batch: ".fg(TEXT_DIM));
        title_spans.push(batch_id.clone().fg(TEXT_DIM));
    }
    title_spans.push(follow_indicator.fg(ACCENT).bold());
    title_spans.push(Span::raw(" "));

    let key = |s: &'static str| -> Span<'static> { s.fg(ACCENT).bold() };
    let msg_count = state.messages.len();
    let footer_spans: Vec<Span> = vec![
        Span::raw(" "),
        key("Esc"),
        ": back  ".fg(TEXT_MUTED),
        key("j/k"),
        ": scroll  ".fg(TEXT_MUTED),
        key("g/G"),
        ": top/bottom  ".fg(TEXT_MUTED),
        key("f"),
        ": follow  ".fg(TEXT_MUTED),
        format!("  {} msgs ", msg_count).fg(TEXT_DIM),
    ];

    let block = Block::bordered()
        .border_set(border::ONE_EIGHTH_WIDE)
        .border_style(Style::new().fg(BORDER_DIM))
        .title(Line::from(title_spans))
        .title_bottom(Line::from(footer_spans))
        .padding(Padding::new(1, 1, 0, 0))
        .style(Style::new().bg(BG_PANEL).fg(TEXT_NORMAL));

    // Build paragraph first to compute visual row count (accounts for line wrapping).
    let paragraph = Paragraph::new(display_lines)
        .wrap(Wrap { trim: false })
        .style(Style::new().bg(BG_PANEL).fg(TEXT_NORMAL))
        .block(block);

    // Inner content width: area minus 2 borders and 2 padding columns.
    let inner_width = area.width.saturating_sub(4);

    // Use visual row count (after wrapping) instead of logical line count.
    let visual_line_count = paragraph.line_count(inner_width);
    let max_offset = visual_line_count.saturating_sub(visible_rows);

    // Compute scroll — when follow_mode clamp to max; otherwise use stored offset.
    let scroll_offset = if state.follow_mode {
        max_offset
    } else {
        state.scroll_offset.min(max_offset)
    };
    // Write back clamped offset so subsequent scroll_up works from the actual position.
    state.scroll_offset = scroll_offset;

    let paragraph = paragraph.scroll((scroll_offset.min(u16::MAX as usize) as u16, 0));

    frame.render_widget(paragraph, area);
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_message(id: i64, from_alias: &str, intent: &str, body: &str) -> MessageRow {
        MessageRow {
            id,
            thread_id: "t-1".to_string(),
            from_alias: from_alias.to_string(),
            to_alias: "orch-dev-2".to_string(),
            intent: intent.to_string(),
            body: body.to_string(),
            batch_id: None,
            created_at: 1_700_000_000 + id * 60,
        }
    }

    fn make_execution(id: &str, agent: &str, status: &str, queued_at: i64) -> ExecutionRow {
        ExecutionRow {
            id: id.to_string(),
            thread_id: "t-1".to_string(),
            batch_id: None,
            agent_alias: agent.to_string(),
            dispatch_message_id: None,
            status: status.to_string(),
            queued_at,
            picked_up_at: None,
            started_at: Some(queued_at + 1),
            finished_at: if status == "completed" {
                Some(queued_at + 120)
            } else {
                None
            },
            duration_ms: if status == "completed" {
                Some(120_000)
            } else {
                None
            },
            exit_code: None,
            output_preview: None,
            error_detail: None,
            parsed_intent: None,
            prompt_hash: None,
            attempt_number: 1,
            retry_after: None,
            error_category: None,
            original_dispatch_message_id: None,
        }
    }

    #[test]
    fn test_conversation_state_new_sets_last_message_id() {
        let msgs = vec![make_message(1, "operator", "dispatch", "hello")];
        let state =
            ConversationViewState::new("t-1".to_string(), None, "Active".to_string(), msgs, vec![]);
        assert_eq!(state.last_message_id, Some(1));
    }

    #[test]
    fn test_conversation_state_new_empty_last_message_id_none() {
        let state = ConversationViewState::new(
            "t-1".to_string(),
            None,
            "Active".to_string(),
            vec![],
            vec![],
        );
        assert_eq!(state.last_message_id, None);
    }

    #[test]
    fn test_conversation_state_follow_mode_default_true() {
        let state = ConversationViewState::new(
            "t-1".to_string(),
            None,
            "Active".to_string(),
            vec![],
            vec![],
        );
        assert!(state.follow_mode);
        assert_eq!(state.scroll_offset, usize::MAX);
    }

    #[test]
    fn test_conversation_state_scroll_up_disables_follow() {
        let mut state = ConversationViewState::new(
            "t-1".to_string(),
            None,
            "Active".to_string(),
            vec![],
            vec![],
        );
        state.scroll_up(5);
        assert!(!state.follow_mode);
    }

    #[test]
    fn test_conversation_state_scroll_up_clamps_at_zero() {
        let mut state = ConversationViewState::new(
            "t-1".to_string(),
            None,
            "Active".to_string(),
            vec![],
            vec![],
        );
        state.follow_mode = false;
        state.scroll_offset = 3;
        state.scroll_up(10);
        assert_eq!(state.scroll_offset, 0);
    }

    #[test]
    fn test_conversation_state_toggle_follow_enables_and_scrolls_bottom() {
        let mut state = ConversationViewState::new(
            "t-1".to_string(),
            None,
            "Active".to_string(),
            vec![],
            vec![],
        );
        state.follow_mode = false;
        state.scroll_offset = 0;
        state.toggle_follow();
        assert!(state.follow_mode);
        assert_eq!(state.scroll_offset, usize::MAX);
    }

    #[test]
    fn test_conversation_state_toggle_follow_disables() {
        let mut state = ConversationViewState::new(
            "t-1".to_string(),
            None,
            "Active".to_string(),
            vec![],
            vec![],
        );
        assert!(state.follow_mode);
        state.toggle_follow();
        assert!(!state.follow_mode);
    }

    #[test]
    fn test_conversation_state_is_active() {
        let mut state = ConversationViewState::new(
            "t-1".to_string(),
            None,
            "Active".to_string(),
            vec![],
            vec![],
        );
        assert!(state.is_active());
        state.thread_status = "Completed".to_string();
        assert!(!state.is_active());
    }

    #[test]
    fn test_build_items_empty() {
        let items = build_items(&[], &[]);
        assert!(items.is_empty());
    }

    #[test]
    fn test_build_items_sorted_by_timestamp() {
        let msgs = vec![
            make_message(2, "operator", "dispatch", "second"),
            make_message(1, "operator", "dispatch", "first"),
        ];
        let items = build_items(&msgs, &[]);
        // Should be sorted: first (created_at = 1_700_000_060) before second (1_700_000_120)
        assert_eq!(items.len(), 2);
        assert!(item_timestamp(&items[0]) <= item_timestamp(&items[1]));
    }

    #[test]
    fn test_build_items_interleaves_executions() {
        let msgs = vec![make_message(1, "operator", "dispatch", "go")];
        let execs = vec![make_execution(
            "exec-1",
            "agent",
            "completed",
            1_700_000_000 + 90,
        )];
        let items = build_items(&msgs, &execs);
        // msg at t=60, exec_started at t=91, exec_completed at t=210
        assert_eq!(items.len(), 3);
    }

    #[test]
    fn test_format_timestamp_returns_string() {
        // Just verify it produces a non-empty, non-error string for a valid ts
        let ts = 1_700_000_000i64;
        let result = format_timestamp(ts);
        assert!(!result.is_empty());
        assert_ne!(result, "-");
    }

    #[test]
    fn test_intent_color_dispatch() {
        assert_eq!(intent_color("dispatch"), Color::Cyan);
    }

    #[test]
    fn test_intent_color_review_request() {
        assert_eq!(intent_color("review-request"), Color::Yellow);
    }

    #[test]
    fn test_intent_color_changes_requested() {
        assert_eq!(intent_color("changes-requested"), Color::Red);
    }

    #[test]
    fn test_intent_color_error() {
        assert_eq!(intent_color("error"), Color::Red);
    }

    #[test]
    fn test_intent_color_status_update() {
        assert_eq!(intent_color("status-update"), Color::DarkGray);
    }

    #[test]
    fn test_push_message_lines_operator_header() {
        let msg = make_message(1, "operator", "dispatch", "hello");
        let mut lines: Vec<Line<'static>> = Vec::new();
        push_message_lines(&msg, &mut lines);
        // First line is header: "operator → orch-dev-2  [dispatch]  <ts>"
        let header_text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(header_text.contains("operator"));
        assert!(header_text.contains("orch-dev-2"));
        assert!(header_text.contains("[dispatch]"));
    }

    #[test]
    fn test_push_message_lines_body_with_prefix() {
        let msg = make_message(1, "agent", "review-request", "line one\nline two");
        let mut lines: Vec<Line<'static>> = Vec::new();
        push_message_lines(&msg, &mut lines);
        // Markdown treats "line one\nline two" as a single paragraph with soft break.
        // Lines: header, "│ line one line two" (no borders — CONV-3 removed them)
        assert_eq!(lines.len(), 2);
        let body_text: String = lines[1].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(body_text.contains("line one"));
        assert!(body_text.contains("line two"));
    }

    #[test]
    fn test_push_execution_started_line_format() {
        let mut lines: Vec<Line<'static>> = Vec::new();
        push_execution_started_line("orch-dev-2", 1_700_000_000, &mut lines);
        assert_eq!(lines.len(), 1);
        let text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("▶"));
        assert!(text.contains("orch-dev-2"));
    }

    #[test]
    fn test_push_execution_completed_line_success() {
        let mut lines: Vec<Line<'static>> = Vec::new();
        push_execution_completed_line("agent", 1_700_000_000, Some(5_000), true, &mut lines);
        let text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("✓"));
        assert!(text.contains("Completed"));
    }

    #[test]
    fn test_push_execution_completed_line_failure() {
        let mut lines: Vec<Line<'static>> = Vec::new();
        push_execution_completed_line("agent", 1_700_000_000, None, false, &mut lines);
        let text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("✗"));
        assert!(text.contains("Failed"));
    }

    // ── Markdown rendering tests ──────────────────────────────────────────

    /// Helper: collect all span text from markdown_to_lines output.
    fn md_text(body: &str) -> Vec<String> {
        markdown_to_lines(body, TEXT_DIM)
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn test_markdown_plain_text() {
        let lines = md_text("hello world");
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("hello world"));
    }

    #[test]
    fn test_markdown_empty_body() {
        let lines = markdown_to_lines("", TEXT_DIM);
        assert!(lines.is_empty());
    }

    #[test]
    fn test_markdown_heading() {
        let lines = md_text("## Summary");
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("Summary"));
    }

    #[test]
    fn test_markdown_heading_has_bold_accent_style() {
        let lines = markdown_to_lines("## Summary", TEXT_DIM);
        // Skip prefix span (index 0), check the text span style.
        let text_span = &lines[0].spans[1];
        assert_eq!(text_span.style.fg, Some(ACCENT));
        assert!(text_span.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn test_markdown_bold_text() {
        let lines = markdown_to_lines("**bold**", TEXT_DIM);
        // Spans: prefix, bold text
        let text_span = &lines[0].spans[1];
        assert!(text_span.style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(text_span.content.as_ref(), "bold");
    }

    #[test]
    fn test_markdown_italic_text() {
        let lines = markdown_to_lines("*italic*", TEXT_DIM);
        let text_span = &lines[0].spans[1];
        assert!(text_span.style.add_modifier.contains(Modifier::ITALIC));
        assert_eq!(text_span.content.as_ref(), "italic");
    }

    #[test]
    fn test_markdown_inline_code() {
        let lines = markdown_to_lines("`code`", TEXT_DIM);
        let code_span = &lines[0].spans[1];
        assert_eq!(code_span.style.fg, Some(Color::Cyan));
        assert_eq!(code_span.content.as_ref(), "code");
    }

    #[test]
    fn test_markdown_code_block() {
        let lines = md_text("```\nfn main() {}\n```");
        // Should have at least one line with code content.
        let has_code = lines.iter().any(|l| l.contains("fn main()"));
        assert!(has_code);
    }

    #[test]
    fn test_markdown_bullet_list() {
        let body = "- item one\n- item two";
        let lines = md_text(body);
        let has_bullet = lines
            .iter()
            .any(|l| l.contains("•") && l.contains("item one"));
        assert!(has_bullet, "Expected bullet prefix: {:?}", lines);
    }

    #[test]
    fn test_markdown_thematic_break() {
        let lines = md_text("above\n\n---\n\nbelow");
        let has_rule = lines.iter().any(|l| l.contains("────"));
        assert!(has_rule, "Expected thematic break line: {:?}", lines);
    }

    #[test]
    fn test_markdown_all_lines_have_prefix() {
        let body = "## Header\n\nSome **bold** text.\n\n- item\n\n```\ncode\n```\n\n---";
        let lines = markdown_to_lines(body, TEXT_DIM);
        for (i, line) in lines.iter().enumerate() {
            assert!(!line.spans.is_empty(), "Line {} should not be empty", i);
            assert_eq!(
                line.spans[0].content.as_ref(),
                "│ ",
                "Line {} should start with │ prefix",
                i
            );
        }
    }

    #[test]
    fn test_markdown_no_panic_on_malformed() {
        // Various edge cases that should not panic.
        let _ = markdown_to_lines("```", TEXT_DIM);
        let _ = markdown_to_lines("**unclosed bold", TEXT_DIM);
        let _ = markdown_to_lines("# ", TEXT_DIM);
        let _ = markdown_to_lines("---\n---\n---", TEXT_DIM);
        let _ = markdown_to_lines("```\n```\n```", TEXT_DIM);
    }
}
