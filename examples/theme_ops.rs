//! Prototype: "Precision Instrument" theme — Ops tab
//!
//! Run: cargo run --example theme_ops -p aster-orch

use std::io;

use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::{Color, Style, Stylize},
    symbols::border,
    text::{Line, Span},
    widgets::{
        Block, List, ListItem, ListState, Padding, Paragraph, Scrollbar, ScrollbarOrientation,
        ScrollbarState, Tabs, Widget,
    },
    Frame,
};

// ── Design System ────────────────────────────────────────────────────────────

const BG_PRIMARY: Color = Color::Rgb(12, 12, 16);
const BG_PANEL: Color = Color::Rgb(20, 20, 26);
const BG_HIGHLIGHT: Color = Color::Rgb(35, 35, 45);
const BORDER_DIM: Color = Color::Rgb(50, 50, 58);
const BORDER_FOCUS: Color = Color::Rgb(80, 80, 90);
const TEXT_MUTED: Color = Color::Rgb(100, 100, 112);
const TEXT_NORMAL: Color = Color::Rgb(180, 180, 190);
const TEXT_BRIGHT: Color = Color::Rgb(220, 220, 230);
const ACCENT: Color = Color::Rgb(232, 135, 75);
const ACCENT_DIM: Color = Color::Rgb(140, 70, 40);
const SUCCESS: Color = Color::Rgb(80, 200, 120);
const SUCCESS_DIM: Color = Color::Rgb(50, 130, 75);
const FAILURE: Color = Color::Rgb(200, 60, 60);
const WARNING: Color = Color::Rgb(200, 170, 50);

fn panel_focused(title: &str) -> Block<'_> {
    Block::bordered()
        .border_set(border::ONE_EIGHTH_WIDE)
        .border_style(Style::new().fg(BORDER_FOCUS))
        .title_top(Line::from(format!(" {title} ")).left_aligned())
        .title_style(Style::new().fg(ACCENT))
        .padding(Padding::proportional(1))
        .style(Style::new().bg(BG_PANEL))
}

// ── App State ────────────────────────────────────────────────────────────────

struct App {
    selected: usize,
    active_tab: usize,
}

impl App {
    fn new() -> Self {
        Self {
            selected: 0,
            active_tab: 0,
        }
    }
}

// ── Mock Data ────────────────────────────────────────────────────────────────

struct ThreadRow {
    icon: &'static str,
    icon_color: Color,
    id: &'static str,
    status: &'static str,
    status_color: Color,
    agent: &'static str,
    batch: &'static str,
    duration: &'static str,
}

struct BatchRow {
    id: &'static str,
    completed: usize,
    total: usize,
    active: usize,
    failed: usize,
    age: &'static str,
}

fn mock_running() -> Vec<ThreadRow> {
    vec![
        ThreadRow {
            icon: "▸",
            icon_color: ACCENT,
            id: "01KKN1XF5BSQRHQ…",
            status: "Executing",
            status_color: ACCENT,
            agent: "focused",
            batch: "DASH-MOD…",
            duration: "2m 14s",
        },
        ThreadRow {
            icon: "▸",
            icon_color: ACCENT,
            id: "01KKN0WJ6982VVF…",
            status: "Executing",
            status_color: ACCENT,
            agent: "spark",
            batch: "DASH-MOD…",
            duration: "1m 48s",
        },
        ThreadRow {
            icon: "◌",
            icon_color: WARNING,
            id: "01KKN0QG0D7S872…",
            status: "Queued",
            status_color: WARNING,
            agent: "pixel",
            batch: "DASH-MOD…",
            duration: "12s",
        },
    ]
}

fn mock_batches() -> Vec<BatchRow> {
    vec![
        BatchRow {
            id: "DASH-MODERNIZE",
            completed: 6,
            total: 8,
            active: 2,
            failed: 0,
            age: "9m",
        },
        BatchRow {
            id: "P4B6",
            completed: 3,
            total: 3,
            active: 0,
            failed: 0,
            age: "32m",
        },
        BatchRow {
            id: "P4B5",
            completed: 7,
            total: 7,
            active: 0,
            failed: 0,
            age: "1h 19m",
        },
        BatchRow {
            id: "ORCH-BENCH-UX",
            completed: 1,
            total: 1,
            active: 0,
            failed: 0,
            age: "55m",
        },
    ]
}

fn mock_completed() -> Vec<ThreadRow> {
    vec![
        ThreadRow {
            icon: "✓",
            icon_color: SUCCESS_DIM,
            id: "01KKN1FRNPM235X…",
            status: "Completed",
            status_color: SUCCESS_DIM,
            agent: "reviewer",
            batch: "DASH-MOD…",
            duration: "3m 43s",
        },
        ThreadRow {
            icon: "✓",
            icon_color: SUCCESS_DIM,
            id: "01KKN0Q78KFVVXG…",
            status: "Completed",
            status_color: SUCCESS_DIM,
            agent: "spark",
            batch: "DASH-MOD…",
            duration: "2m 18s",
        },
        ThreadRow {
            icon: "✓",
            icon_color: SUCCESS_DIM,
            id: "01KKN0QG0D7S872…",
            status: "Completed",
            status_color: SUCCESS_DIM,
            agent: "pixel",
            batch: "DASH-MOD…",
            duration: "2m 42s",
        },
        ThreadRow {
            icon: "✗",
            icon_color: FAILURE,
            id: "01KKMZ0VNQ9YXPC…",
            status: "Failed",
            status_color: FAILURE,
            agent: "chill",
            batch: "P4B6",
            duration: "8m 12s",
        },
    ]
}

// ── Rendering ────────────────────────────────────────────────────────────────

fn render_tab_bar(area: Rect, buf: &mut Buffer, active_tab: usize) {
    let tabs = Tabs::new(["Ops", "Agents", "History", "Settings"])
        .select(active_tab)
        .style(Style::new().fg(TEXT_MUTED).bg(BG_PRIMARY))
        .highlight_style(Style::new().fg(ACCENT).bold())
        .divider(Span::styled(" │ ", Style::new().fg(BORDER_DIM)))
        .block(
            Block::bordered()
                .border_set(border::ONE_EIGHTH_WIDE)
                .border_style(Style::new().fg(BORDER_DIM))
                .title_top(Line::from(" aster-orch ".fg(TEXT_BRIGHT).bold()).left_aligned())
                .style(Style::new().bg(BG_PRIMARY)),
        );
    tabs.render(area, buf);
}

fn render_status_bar(area: Rect, buf: &mut Buffer) {
    let bar = Line::from(vec![
        " q".fg(ACCENT).bold(),
        ": quit  ".fg(TEXT_MUTED),
        "?".fg(ACCENT).bold(),
        ": help  ".fg(TEXT_MUTED),
        "Tab".fg(ACCENT).bold(),
        ": next  ".fg(TEXT_MUTED),
        "a".fg(ACCENT).bold(),
        ": actions  ".fg(TEXT_MUTED),
        "s".fg(ACCENT).bold(),
        ": stale cleanup".fg(TEXT_MUTED),
    ]);
    Paragraph::new(bar)
        .style(Style::new().bg(Color::Rgb(25, 25, 32)).fg(TEXT_MUTED))
        .render(area, buf);
}

fn make_thread_item(t: &ThreadRow) -> ListItem<'static> {
    ListItem::new(Line::from(vec![
        Span::styled(format!(" {} ", t.icon), Style::new().fg(t.icon_color)),
        Span::styled(format!("{:<18}", t.id), Style::new().fg(TEXT_NORMAL)),
        Span::styled(format!("{:<12}", t.status), Style::new().fg(t.status_color)),
        Span::styled(format!("{:<10}", t.agent), Style::new().fg(TEXT_MUTED)),
        Span::styled(
            format!("{:<12}", t.batch),
            Style::new().fg(Color::Rgb(70, 70, 80)),
        ),
        Span::styled(t.duration, Style::new().fg(TEXT_MUTED)),
    ]))
}

fn make_batch_item(b: &BatchRow) -> ListItem<'static> {
    let ratio = if b.total == 0 {
        0
    } else {
        (b.completed * 10) / b.total
    };
    let fill = ratio.min(10);
    let bar_filled: String = "━".repeat(fill);
    let bar_empty: String = "╌".repeat(10 - fill);

    let is_active = b.active > 0;
    let marker_color = if is_active { ACCENT } else { TEXT_MUTED };
    let marker = if is_active { "▸" } else { "·" };

    ListItem::new(Line::from(vec![
        Span::styled(format!(" {} ", marker), Style::new().fg(marker_color)),
        Span::styled(
            format!("{:<18}", b.id),
            Style::new().fg(if is_active { TEXT_BRIGHT } else { TEXT_NORMAL }),
        ),
        Span::styled(
            format!("{}/{}", b.completed, b.total),
            Style::new().fg(TEXT_MUTED),
        ),
        Span::raw("  "),
        Span::styled(
            bar_filled,
            Style::new().fg(if is_active { ACCENT } else { SUCCESS_DIM }),
        ),
        Span::styled(bar_empty, Style::new().fg(Color::Rgb(40, 40, 48))),
        Span::raw("  "),
        Span::styled(
            format!("a:{} f:{}", b.active, b.failed),
            Style::new().fg(if b.failed > 0 { FAILURE } else { TEXT_MUTED }),
        ),
        Span::styled(
            format!("  {}", b.age),
            Style::new().fg(Color::Rgb(70, 70, 80)),
        ),
    ]))
}

fn section_header(label: &str, count: usize, color: Color) -> ListItem<'static> {
    ListItem::new(Line::from(vec![
        Span::styled(format!(" {label}"), Style::new().fg(color).bold()),
        Span::styled(format!(" ({count})"), Style::new().fg(TEXT_MUTED)),
    ]))
}

fn render_ops_list(frame: &mut Frame, area: Rect, selected: usize) {
    let running = mock_running();
    let batches = mock_batches();
    let completed = mock_completed();

    let mut items: Vec<ListItem> = Vec::new();

    // Running
    items.push(section_header("Running", running.len(), ACCENT));
    for t in &running {
        items.push(make_thread_item(t));
    }

    // Batches
    items.push(ListItem::new(Line::raw("")));
    items.push(section_header("Batches", batches.len(), TEXT_MUTED));
    for b in &batches {
        items.push(make_batch_item(b));
    }

    // Recently Completed
    items.push(ListItem::new(Line::raw("")));
    items.push(section_header(
        "Recently Completed",
        completed.len(),
        SUCCESS_DIM,
    ));
    for t in &completed {
        items.push(make_thread_item(t));
    }

    let health_line =
        Line::from(vec!["worker beat: ".fg(TEXT_MUTED), "4s".fg(SUCCESS)]).right_aligned();

    let list = List::new(items)
        .block(
            panel_focused("Ops")
                .title_top(health_line)
                .padding(Padding::new(1, 1, 0, 0)),
        )
        .highlight_style(Style::new().bg(BG_HIGHLIGHT))
        .style(Style::new().bg(BG_PANEL).fg(TEXT_NORMAL));

    let mut state = ListState::default().with_selected(Some(selected));
    frame.render_stateful_widget(list, area, &mut state);

    // Footer
    let footer_area = Rect::new(
        area.x + 1,
        area.y + area.height.saturating_sub(1),
        area.width.saturating_sub(2),
        1,
    );
    let footer = Line::from(vec![
        " Active: ".fg(TEXT_MUTED),
        "3".fg(ACCENT).bold(),
        "  Stale: ".fg(TEXT_MUTED),
        "0".fg(TEXT_MUTED),
        "  Failed: ".fg(TEXT_MUTED),
        "1".fg(FAILURE),
        "  Completed: ".fg(TEXT_MUTED),
        "101".fg(SUCCESS_DIM),
    ]);
    frame.render_widget(
        Paragraph::new(footer).style(Style::new().bg(BG_PANEL)),
        footer_area,
    );

    // Scrollbar
    let mut scrollbar_state = ScrollbarState::new(20).position(selected);
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

    loop {
        terminal.draw(|frame| {
            let area = frame.area();

            // Clear background
            Block::default()
                .style(Style::new().bg(BG_PRIMARY))
                .render(area, frame.buffer_mut());

            let [tab_bar, content, status_bar] = Layout::vertical([
                Constraint::Length(3),
                Constraint::Fill(1),
                Constraint::Length(1),
            ])
            .areas(area);

            render_tab_bar(tab_bar, frame.buffer_mut(), app.active_tab);
            render_status_bar(status_bar, frame.buffer_mut());

            // Ops content: full-width list (context panel removed in OPS-1)
            render_ops_list(frame, content, app.selected);
        })?;

        if let crossterm::event::Event::Key(key) = crossterm::event::read()? {
            if key.kind != crossterm::event::KeyEventKind::Press {
                continue;
            }
            match key.code {
                crossterm::event::KeyCode::Char('q') => break,
                crossterm::event::KeyCode::Down | crossterm::event::KeyCode::Char('j') => {
                    app.selected = (app.selected + 1).min(19);
                }
                crossterm::event::KeyCode::Up | crossterm::event::KeyCode::Char('k') => {
                    app.selected = app.selected.saturating_sub(1);
                }
                crossterm::event::KeyCode::Tab => {
                    app.active_tab = (app.active_tab + 1) % 4;
                }
                _ => {}
            }
        }
    }

    ratatui::restore();
    Ok(())
}
