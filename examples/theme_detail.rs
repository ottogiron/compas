//! Prototype: "Precision Instrument" theme — Execution detail view
//!
//! Run: cargo run --example theme_detail -p aster-orch

use std::io;

use ratatui::{
    style::{Color, Style, Stylize},
    symbols::border,
    text::{Line, Span},
    widgets::{Block, Padding, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Widget},
    Frame,
};

// ── Design System ────────────────────────────────────────────────────────────

const BG_PRIMARY: Color = Color::Rgb(12, 12, 16);
const BG_PANEL: Color = Color::Rgb(20, 20, 26);

const BORDER_DIM: Color = Color::Rgb(50, 50, 58);
const TEXT_MUTED: Color = Color::Rgb(100, 100, 112);
const TEXT_NORMAL: Color = Color::Rgb(180, 180, 190);
const TEXT_BRIGHT: Color = Color::Rgb(220, 220, 230);
const ACCENT: Color = Color::Rgb(232, 135, 75);
const ACCENT_DIM: Color = Color::Rgb(140, 70, 40);
const SUCCESS: Color = Color::Rgb(80, 200, 120);

// ── App State ────────────────────────────────────────────────────────────────

struct App {
    scroll_offset: usize,
    input_expanded: bool,
    output_expanded: bool,
    selected_section: usize, // 0 = input, 1 = output
    follow: bool,
}

impl App {
    fn new() -> Self {
        Self {
            scroll_offset: 0,
            input_expanded: false,
            output_expanded: true,
            selected_section: 1,
            follow: true,
        }
    }
}

// ── Mock Data ────────────────────────────────────────────────────────────────

fn mock_input() -> Vec<String> {
    vec![
        "## Task: Modernize `app.rs` with ratatui 0.30 patterns".into(),
        "".into(),
        "**Scope:** ONLY modify this file:".into(),
        "- `crates/aster-orch/src/dashboard/app.rs`".into(),
        "".into(),
        "**Changes required:**".into(),
        "1. ratatui::init() / ratatui::restore() + DefaultTerminal".into(),
        "2. Layout::vertical() / Layout::horizontal() + destructuring".into(),
        "3. Constraint::Fill(1) instead of Constraint::Min(0)".into(),
        "4. Flex::Center for modals".into(),
        "5. event.as_key_press_event()".into(),
        "6. Widget trait on App".into(),
    ]
}

#[allow(clippy::vec_init_then_push)]
fn mock_output() -> Vec<String> {
    let mut lines = Vec::new();

    lines.push("╭─ Agent: focused ──────────────────────────────────────────╮".into());
    lines.push("│ Starting task: modernize app.rs                          │".into());
    lines.push("╰──────────────────────────────────────────────────────────╯".into());
    lines.push("".into());
    lines.push("[14:32:07] Reading crates/aster-orch/src/dashboard/app.rs".into());
    lines.push("[14:32:07]   1703 lines, 48.2 KiB".into());
    lines.push("[14:32:08] Analyzing import structure...".into());
    lines.push("[14:32:08]   Found 14 ratatui imports to modernize".into());
    lines.push("[14:32:09] Applying ratatui::init()/restore()...".into());
    lines.push("[14:32:09]   Removed manual enable_raw_mode()".into());
    lines.push("[14:32:09]   Removed manual EnterAlternateScreen".into());
    lines.push("[14:32:09]   Removed manual panic hook".into());
    lines.push("[14:32:10]   Added ratatui::init() at entry".into());
    lines.push("[14:32:10]   Added ratatui::restore() at exit".into());
    lines.push("[14:32:10]   Replaced Terminal<CrosstermBackend<Stdout>> → DefaultTerminal".into());
    lines.push("".into());
    lines.push("[14:32:11] Applying Layout::vertical()/horizontal()...".into());
    lines
        .push("[14:32:11]   main layout: chunks[0..3] → let [tab_bar, content, status_bar]".into());
    lines.push(
        "[14:32:12]   centered_rect: Layout::default().direction() → Layout::vertical()".into(),
    );
    lines.push("[14:32:12]   Removed Direction import".into());
    lines.push("".into());
    lines.push("[14:32:13] Applying Constraint::Fill(1)...".into());
    lines.push("[14:32:13]   Replaced Constraint::Min(0) in main layout".into());
    lines.push("".into());
    lines.push("[14:32:14] Implementing Widget for &App...".into());
    lines.push("[14:32:14]   Created render_tab_bar_widget(area, buf)".into());
    lines.push("[14:32:15]   Created render_content_widget(area, buf)".into());
    lines.push("[14:32:15]   Created render_status_bar_widget(area, buf)".into());
    lines.push("[14:32:16]   Created render_help_overlay_widget(area, buf)".into());
    lines.push("[14:32:16]   Created render_ops_list(frame, area)".into());
    lines.push("[14:32:17]   Hybrid approach: Widget for static, Frame for stateful".into());
    lines.push("".into());
    lines.push("[14:32:18] Applying Stylize shorthand...".into());
    lines.push("[14:32:18]   38 Span::styled() calls → .cyan().bold() etc.".into());
    lines.push("[14:32:19]   12 Style::default() → Style::new()".into());
    lines.push("[14:32:19]   8 Block::default().borders(ALL) → Block::bordered()".into());
    lines.push("".into());
    lines.push("[14:32:20] Running verification...".into());
    lines.push("[14:32:20]   cargo check -p aster-orch ✓".into());
    lines.push("[14:32:25]   cargo test -p aster-orch".into());
    lines.push("[14:32:28]     317 tests passed, 0 failed".into());
    lines.push("".into());
    lines.push("╭─ Result ─────────────────────────────────────────────────╮".into());
    lines.push("│  Status:    Completed                                    │".into());
    lines.push("│  Duration:  21s                                          │".into());
    lines.push("│  Net diff:  -87 lines                                   │".into());
    lines.push("╰──────────────────────────────────────────────────────────╯".into());

    // Pad to make scrollable
    for i in 0..40 {
        lines.push(format!("[14:32:{}] ... (log continues)", 30 + i));
    }

    lines
}

// ── Rendering ────────────────────────────────────────────────────────────────

fn render_detail(frame: &mut Frame, app: &App) {
    let area = frame.area();

    Block::default()
        .style(Style::new().bg(BG_PRIMARY))
        .render(area, frame.buffer_mut());

    // Title bar spans
    let mut title_spans = vec![
        " 01KKN1XF5BSQ… ".fg(TEXT_NORMAL),
        " ".into(),
        "focused".fg(ACCENT),
        " ".into(),
        "Completed".fg(SUCCESS).bold(),
        " ".into(),
        "21s".fg(TEXT_MUTED),
    ];

    if app.follow {
        title_spans.push(" ".into());
        title_spans.push("◉ follow".fg(ACCENT).bold());
    }

    let title_line = Line::from(title_spans).left_aligned();

    let pos_line = Line::from(vec![
        format!("{}", app.scroll_offset + 1).fg(TEXT_MUTED),
        "/".fg(Color::Rgb(60, 60, 68)),
        format!("{} ", mock_output().len()).fg(TEXT_MUTED),
    ])
    .right_aligned();

    let footer = Line::from(vec![
        " Esc".fg(ACCENT).bold(),
        ": back  ".fg(TEXT_MUTED),
        "j/k".fg(ACCENT).bold(),
        ": section  ".fg(TEXT_MUTED),
        "Enter".fg(ACCENT).bold(),
        ": toggle  ".fg(TEXT_MUTED),
        "f".fg(ACCENT).bold(),
        ": follow  ".fg(TEXT_MUTED),
        "g/G".fg(ACCENT).bold(),
        ": top/bottom  ".fg(TEXT_MUTED),
        "J".fg(ACCENT).bold(),
        ": view mode".fg(TEXT_MUTED),
    ]);

    let block = Block::bordered()
        .border_set(border::ONE_EIGHTH_WIDE)
        .border_style(Style::new().fg(BORDER_DIM))
        .title_top(title_line)
        .title_top(pos_line)
        .title_bottom(footer)
        .padding(Padding::new(1, 1, 0, 0))
        .style(Style::new().bg(BG_PANEL));

    let inner = block.inner(area);
    block.render(area, frame.buffer_mut());

    // Build display lines
    let input = mock_input();
    let output = mock_output();

    let mut display_lines: Vec<Line> = Vec::new();

    // Input section header
    let input_marker = if app.selected_section == 0 {
        "▸"
    } else {
        " "
    };
    let input_chevron = if app.input_expanded { "▾" } else { "▸" };
    display_lines.push(Line::from(vec![
        input_marker.fg(ACCENT),
        " ".into(),
        input_chevron.fg(TEXT_MUTED),
        " Input"
            .fg(if app.selected_section == 0 {
                TEXT_BRIGHT
            } else {
                TEXT_NORMAL
            })
            .bold(),
        format!("  ({} lines)", input.len()).fg(Color::Rgb(60, 60, 68)),
    ]));

    if app.input_expanded {
        for line in &input {
            display_lines.push(Line::from(vec![
                "    ".into(),
                Span::styled(line.as_str(), Style::new().fg(TEXT_MUTED)),
            ]));
        }
        display_lines.push(Line::raw(""));
    }

    // Output section header
    let output_marker = if app.selected_section == 1 {
        "▸"
    } else {
        " "
    };
    let output_chevron = if app.output_expanded { "▾" } else { "▸" };
    display_lines.push(Line::from(vec![
        output_marker.fg(ACCENT),
        " ".into(),
        output_chevron.fg(TEXT_MUTED),
        " Output"
            .fg(if app.selected_section == 1 {
                TEXT_BRIGHT
            } else {
                TEXT_NORMAL
            })
            .bold(),
        format!("  ({} lines)", output.len()).fg(Color::Rgb(60, 60, 68)),
    ]));

    if app.output_expanded {
        for line in &output {
            let color = if line.starts_with('[') {
                // Timestamp lines
                TEXT_NORMAL
            } else if line.starts_with('╭') || line.starts_with('│') || line.starts_with('╰')
            {
                // Box drawing
                ACCENT_DIM
            } else {
                TEXT_MUTED
            };
            display_lines.push(Line::from(vec![
                "    ".into(),
                Span::styled(line.as_str(), Style::new().fg(color)),
            ]));
        }
    }

    let paragraph = Paragraph::new(display_lines.clone())
        .scroll((app.scroll_offset.min(u16::MAX as usize) as u16, 0))
        .style(Style::new().bg(BG_PANEL).fg(TEXT_NORMAL));
    paragraph.render(inner, frame.buffer_mut());

    // Scrollbar
    let total = display_lines.len();
    let mut scrollbar_state = ScrollbarState::new(total).position(app.scroll_offset);
    let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
        .thumb_symbol("┃")
        .track_symbol(Some("│"))
        .thumb_style(Style::new().fg(ACCENT_DIM))
        .track_style(Style::new().fg(Color::Rgb(35, 35, 42)));
    frame.render_stateful_widget(scrollbar, area, &mut scrollbar_state);
}

// ── Main ─────────────────────────────────────────────────────────────────────

fn main() -> io::Result<()> {
    let mut terminal = ratatui::init();
    let mut app = App::new();

    let total_lines = mock_input().len() + mock_output().len() + 10;

    loop {
        terminal.draw(|frame| {
            render_detail(frame, &app);
        })?;

        if let crossterm::event::Event::Key(key) = crossterm::event::read()? {
            if key.kind != crossterm::event::KeyEventKind::Press {
                continue;
            }
            match key.code {
                crossterm::event::KeyCode::Char('q') | crossterm::event::KeyCode::Esc => break,
                crossterm::event::KeyCode::Down | crossterm::event::KeyCode::Char('j') => {
                    app.selected_section = (app.selected_section + 1).min(1);
                }
                crossterm::event::KeyCode::Up | crossterm::event::KeyCode::Char('k') => {
                    app.selected_section = app.selected_section.saturating_sub(1);
                }
                crossterm::event::KeyCode::Enter => {
                    if app.selected_section == 0 {
                        app.input_expanded = !app.input_expanded;
                    } else {
                        app.output_expanded = !app.output_expanded;
                    }
                }
                crossterm::event::KeyCode::Char('f') => {
                    app.follow = !app.follow;
                }
                crossterm::event::KeyCode::Char('G') => {
                    app.scroll_offset = total_lines.saturating_sub(10);
                }
                crossterm::event::KeyCode::Char('g') => {
                    app.scroll_offset = 0;
                }
                crossterm::event::KeyCode::PageDown => {
                    app.scroll_offset = (app.scroll_offset + 20).min(total_lines);
                }
                crossterm::event::KeyCode::PageUp => {
                    app.scroll_offset = app.scroll_offset.saturating_sub(20);
                }
                _ => {}
            }
        }
    }

    ratatui::restore();
    Ok(())
}
