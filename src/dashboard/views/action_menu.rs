//! Rendering components for the dashboard action menu, confirmation bar,
//! and feedback flash.

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Widget};

use crate::dashboard::theme;

/// Renders the action menu overlay as a centered popup.
///
/// `target_label` is a short description like "thread 01KMV…R2K7" or "batch GAP-11".
/// `actions` is the list of available actions (from `available_actions()`).
pub fn render_action_menu(
    target_label: &str,
    actions: &[(char, &str)],
    area: Rect,
    buf: &mut Buffer,
) {
    // Calculate popup dimensions.
    let longest_label = actions
        .iter()
        .map(|(_, label)| label.len())
        .max()
        .unwrap_or(0);
    let title = format!(" Actions: {target_label} ");
    let content_width = longest_label + 6; // "  k  label"
    let popup_width = content_width.max(40).max(title.len() + 2) as u16;
    // bordered block eats 2 rows; content = top_pad + actions + bottom_pad + footer
    let popup_height = (actions.len() + 5).max(6) as u16;

    let modal = centered_rect(popup_width, popup_height, area);

    Clear.render(modal, buf);

    let block = Block::bordered()
        .title(title)
        .title_style(Style::new().fg(theme::ACCENT))
        .border_style(Style::new().fg(theme::BORDER_FOCUS))
        .style(Style::new().bg(theme::BG_PANEL));
    let inner = block.inner(modal);
    block.render(modal, buf);

    let mut lines: Vec<Line<'_>> = Vec::new();
    lines.push(Line::raw("")); // top padding

    if actions.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            "  No actions available",
            Style::new().fg(theme::TEXT_MUTED),
        )]));
    } else {
        for &(key, label) in actions {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    String::from(key),
                    Style::new().fg(theme::ACCENT).add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(label, Style::new().fg(theme::TEXT_NORMAL)),
            ]));
        }
    }

    lines.push(Line::raw("")); // bottom padding
    lines.push(Line::from(vec![Span::styled(
        "  Esc cancel",
        Style::new().fg(theme::TEXT_MUTED),
    )]));

    Paragraph::new(lines)
        .style(Style::new().bg(theme::BG_PANEL))
        .render(inner, buf);
}

/// Produces a [`Line`] for the status bar during confirmation prompts.
///
/// Example output: `"Abandon thread 01KMV…? [y]es / [n]o"`
/// The prompt text uses [`theme::WARNING`] color. Bracketed key letters are bold.
pub fn confirmation_line(prompt: &str) -> Line<'static> {
    let owned = prompt.to_string();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut rest = owned.as_str();

    // Walk the string, pulling out `[x]` sequences as bold spans.
    loop {
        if let Some(open) = rest.find('[') {
            if let Some(close) = rest[open..].find(']') {
                let close = open + close;
                // Text before the bracket.
                if open > 0 {
                    spans.push(Span::styled(
                        rest[..open].to_string(),
                        Style::new().fg(theme::WARNING),
                    ));
                }
                // The bracketed key, including brackets.
                spans.push(Span::styled(
                    rest[open..=close].to_string(),
                    Style::new().fg(theme::WARNING).add_modifier(Modifier::BOLD),
                ));
                rest = &rest[close + 1..];
                continue;
            }
        }
        // No more brackets — emit remaining text.
        if !rest.is_empty() {
            spans.push(Span::styled(
                rest.to_string(),
                Style::new().fg(theme::WARNING),
            ));
        }
        break;
    }

    Line::from(spans)
}

/// Produces a [`Line`] for the status bar showing action result feedback.
///
/// Success: `"✓ {message}"` in [`theme::SUCCESS`] color.
/// Error: `"✗ {message}"` in [`theme::FAILURE`] color.
pub fn feedback_line(message: &str, is_error: bool) -> Line<'static> {
    let (marker, color) = if is_error {
        (theme::MARKER_FAILED, theme::FAILURE)
    } else {
        (theme::MARKER_COMPLETED, theme::SUCCESS)
    };
    Line::from(vec![Span::styled(
        format!("{marker} {message}"),
        Style::new().fg(color),
    )])
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Center a fixed-size rectangle within `r`.
fn centered_rect(width: u16, height: u16, r: Rect) -> Rect {
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

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_buf() -> (Buffer, Rect) {
        let area = Rect::new(0, 0, 80, 24);
        let buf = Buffer::empty(area);
        (buf, area)
    }

    fn buf_text(buf: &Buffer) -> String {
        buf.content.iter().map(|c| c.symbol()).collect()
    }

    #[test]
    fn test_render_action_menu_no_panic_empty() {
        let (mut buf, area) = make_buf();
        render_action_menu("thread abc", &[], area, &mut buf);
        let s = buf_text(&buf);
        assert!(s.contains("No actions available"));
        assert!(s.contains("Esc"));
    }

    #[test]
    fn test_render_action_menu_no_panic_with_actions() {
        let (mut buf, area) = make_buf();
        let actions: Vec<(char, &str)> = vec![
            ('a', "Abandon"),
            ('r', "Retry"),
            ('c', "Cancel"),
            ('d', "Delete"),
        ];
        render_action_menu("thread abc", &actions, area, &mut buf);
        let s = buf_text(&buf);
        assert!(s.contains('a'));
        assert!(s.contains("Abandon"));
        assert!(s.contains('d'));
        assert!(s.contains("Delete"));
        assert!(s.contains("Esc"));
    }

    #[test]
    fn test_render_action_menu_no_panic_six_actions() {
        let (mut buf, area) = make_buf();
        let actions: Vec<(char, &str)> = vec![
            ('a', "Abandon"),
            ('r', "Retry"),
            ('c', "Cancel"),
            ('d', "Delete"),
            ('m', "Merge"),
            ('v', "View"),
        ];
        render_action_menu("batch GAP-11", &actions, area, &mut buf);
        let s = buf_text(&buf);
        assert!(s.contains("Abandon"));
        assert!(s.contains("View"));
        assert!(s.contains("Esc cancel"));
    }

    #[test]
    fn test_confirmation_line_has_spans() {
        let line = confirmation_line("Abandon thread abc? [y]es / [n]o");
        assert!(!line.spans.is_empty());
    }

    #[test]
    fn test_confirmation_line_preserves_text() {
        let line = confirmation_line("Abandon thread abc? [y]es / [n]o");
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "Abandon thread abc? [y]es / [n]o");
    }

    #[test]
    fn test_feedback_line_success() {
        let line = feedback_line("Thread abandoned", false);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains('✓'));
        assert!(text.contains("Thread abandoned"));
    }

    #[test]
    fn test_feedback_line_error() {
        let line = feedback_line("Action timed out", true);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains('✗'));
        assert!(text.contains("Action timed out"));
    }
}
