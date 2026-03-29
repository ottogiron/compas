//! Dispatch prompt overlay for sending work to agents from the TUI.
//!
//! Three-step flow: SelectAgent → EnterInstruction → Confirm.
//! Rendered as a centered modal overlay on top of the Ops tab.

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Widget};

use crate::dashboard::theme;
use crate::dashboard::views;

// ── State ────────────────────────────────────────────────────────────────────

/// Current step in the dispatch prompt flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchStep {
    SelectAgent,
    EnterInstruction,
    Confirm,
}

/// State for the dispatch prompt overlay.
#[derive(Debug, Clone)]
pub struct DispatchPromptState {
    /// Current step in the multi-step flow.
    pub step: DispatchStep,
    /// Available worker agent aliases.
    pub agents: Vec<String>,
    /// Index of the currently selected agent.
    pub selected_agent: usize,
    /// The instruction text being composed.
    pub instruction: String,
    /// Optional batch ID for grouped dispatches.
    pub batch_id: Option<String>,
    /// Pre-filled thread ID when continuing an existing thread.
    pub thread_id: Option<String>,
    /// Display label for the thread being continued.
    pub thread_summary: Option<String>,
    /// Error feedback from a failed dispatch attempt.
    pub error: Option<String>,
}

impl DispatchPromptState {
    /// Create a new dispatch prompt state with the given worker agents and optional
    /// thread context (for continuing an existing thread).
    pub fn new(
        agents: Vec<String>,
        thread_id: Option<String>,
        thread_summary: Option<String>,
    ) -> Self {
        Self {
            step: DispatchStep::SelectAgent,
            agents,
            selected_agent: 0,
            instruction: String::new(),
            batch_id: None,
            thread_id,
            thread_summary,
            error: None,
        }
    }

    /// The alias of the currently selected agent.
    pub fn selected_agent_alias(&self) -> Option<&str> {
        self.agents.get(self.selected_agent).map(|s| s.as_str())
    }
}

// ── Rendering ────────────────────────────────────────────────────────────────

/// Render the dispatch prompt overlay.
pub fn render_dispatch_prompt(state: &DispatchPromptState, area: Rect, buf: &mut Buffer) {
    match state.step {
        DispatchStep::SelectAgent => render_select_agent(state, area, buf),
        DispatchStep::EnterInstruction => render_enter_instruction(state, area, buf),
        DispatchStep::Confirm => render_confirm(state, area, buf),
    }
}

fn render_select_agent(state: &DispatchPromptState, area: Rect, buf: &mut Buffer) {
    let height = (state.agents.len() as u16 + 6).min(area.height);
    let modal = centered_rect_fixed(50, height, area);

    Clear.render(modal, buf);

    let title = if state.thread_id.is_some() {
        " Dispatch — Continue Thread "
    } else {
        " Dispatch — New Thread "
    };

    let block = Block::bordered()
        .title(title)
        .title_style(Style::new().fg(theme::ACCENT))
        .border_style(Style::new().fg(theme::BORDER_FOCUS))
        .style(Style::new().bg(theme::BG_PANEL));
    let inner = block.inner(modal);
    block.render(modal, buf);

    let mut lines: Vec<Line<'_>> = Vec::new();

    // Thread context header
    if let Some(ref summary) = state.thread_summary {
        lines.push(Line::from(vec![
            Span::styled("  Thread: ", Style::new().fg(theme::TEXT_MUTED)),
            Span::styled(summary.as_str(), Style::new().fg(theme::TEXT_NORMAL)),
        ]));
        lines.push(Line::raw(""));
    }

    lines.push(Line::styled(
        "  Select agent:",
        Style::new().fg(theme::TEXT_MUTED),
    ));

    for (i, agent) in state.agents.iter().enumerate() {
        let marker = if i == state.selected_agent {
            " ▸ "
        } else {
            "   "
        };
        let style = if i == state.selected_agent {
            Style::new().fg(theme::ACCENT).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(theme::TEXT_NORMAL)
        };
        lines.push(Line::from(vec![Span::styled(
            format!("{}{}", marker, agent),
            style,
        )]));
    }

    lines.push(Line::raw(""));
    lines.push(Line::styled(
        "  Enter select   Esc cancel",
        Style::new().fg(theme::TEXT_DIM),
    ));

    if let Some(ref err) = state.error {
        lines.push(Line::styled(
            format!("  {}", err),
            Style::new().fg(theme::FAILURE),
        ));
    }

    Paragraph::new(lines)
        .style(Style::new().bg(theme::BG_PANEL))
        .render(inner, buf);
}

fn render_enter_instruction(state: &DispatchPromptState, area: Rect, buf: &mut Buffer) {
    let modal = centered_rect_fixed(60, 12, area);

    Clear.render(modal, buf);

    let block = Block::bordered()
        .title(" Dispatch — Instruction ")
        .title_style(Style::new().fg(theme::ACCENT))
        .border_style(Style::new().fg(theme::BORDER_FOCUS))
        .style(Style::new().bg(theme::BG_PANEL));
    let inner = block.inner(modal);
    block.render(modal, buf);

    let agent_label = state.selected_agent_alias().unwrap_or("?").to_string();

    let mut lines: Vec<Line<'_>> = vec![Line::from(vec![
        Span::styled("  Agent: ", Style::new().fg(theme::TEXT_MUTED)),
        Span::styled(agent_label, Style::new().fg(theme::ACCENT)),
    ])];

    if let Some(ref summary) = state.thread_summary {
        lines.push(Line::from(vec![
            Span::styled("  Thread: ", Style::new().fg(theme::TEXT_MUTED)),
            Span::styled(summary.as_str(), Style::new().fg(theme::TEXT_NORMAL)),
        ]));
    }

    lines.push(Line::raw(""));
    lines.push(Line::styled(
        "  Instruction:",
        Style::new().fg(theme::TEXT_MUTED),
    ));

    // Show instruction with cursor indicator. Truncate from the left so the
    // user always sees the end of what they're typing.
    let visible_instruction = views::truncate_left(&state.instruction, 50);
    let display_text = if state.instruction.is_empty() {
        "  │".to_string()
    } else {
        format!("  {}│", visible_instruction)
    };
    lines.push(Line::styled(
        display_text,
        Style::new().fg(theme::TEXT_BRIGHT),
    ));

    lines.push(Line::raw(""));
    lines.push(Line::styled(
        "  Enter submit   Esc back",
        Style::new().fg(theme::TEXT_DIM),
    ));

    if let Some(ref err) = state.error {
        lines.push(Line::styled(
            format!("  {}", err),
            Style::new().fg(theme::FAILURE),
        ));
    }

    Paragraph::new(lines)
        .style(Style::new().bg(theme::BG_PANEL))
        .render(inner, buf);
}

fn render_confirm(state: &DispatchPromptState, area: Rect, buf: &mut Buffer) {
    let modal = centered_rect_fixed(60, 14, area);

    Clear.render(modal, buf);

    let block = Block::bordered()
        .title(" Dispatch — Confirm ")
        .title_style(Style::new().fg(theme::ACCENT))
        .border_style(Style::new().fg(theme::BORDER_FOCUS))
        .style(Style::new().bg(theme::BG_PANEL));
    let inner = block.inner(modal);
    block.render(modal, buf);

    let agent_label = state.selected_agent_alias().unwrap_or("?").to_string();

    let instruction_preview = views::truncate(&state.instruction, 40);

    let thread_label = state.thread_id.as_deref().unwrap_or("(new)");

    let mut lines: Vec<Line<'_>> = vec![
        Line::styled(
            "  Confirm dispatch:",
            Style::new()
                .fg(theme::TEXT_BRIGHT)
                .add_modifier(Modifier::BOLD),
        ),
        Line::raw(""),
        Line::from(vec![
            Span::styled("  Agent:       ", Style::new().fg(theme::TEXT_MUTED)),
            Span::styled(agent_label, Style::new().fg(theme::ACCENT)),
        ]),
        Line::from(vec![
            Span::styled("  Thread:      ", Style::new().fg(theme::TEXT_MUTED)),
            Span::styled(thread_label, Style::new().fg(theme::TEXT_NORMAL)),
        ]),
        Line::from(vec![
            Span::styled("  Instruction: ", Style::new().fg(theme::TEXT_MUTED)),
            Span::styled(instruction_preview, Style::new().fg(theme::TEXT_NORMAL)),
        ]),
        Line::raw(""),
        Line::styled(
            "  [y]es dispatch   [b]ack   [n]/Esc cancel",
            Style::new().fg(theme::TEXT_DIM),
        ),
    ];

    if let Some(ref err) = state.error {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            format!("  {}", err),
            Style::new().fg(theme::FAILURE),
        ));
    }

    Paragraph::new(lines)
        .style(Style::new().bg(theme::BG_PANEL))
        .render(inner, buf);
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Create a centered rectangle with fixed pixel dimensions.
fn centered_rect_fixed(width: u16, height: u16, r: Rect) -> Rect {
    let h = height.min(r.height);
    let w = width.min(r.width);
    let [area] = Layout::vertical([Constraint::Length(h)])
        .flex(Flex::Center)
        .areas(r);
    let [area] = Layout::horizontal([Constraint::Length(w)])
        .flex(Flex::Center)
        .areas(area);
    area
}
