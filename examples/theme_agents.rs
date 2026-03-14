//! Prototype: "Precision Instrument" theme — Agents tab with sparklines
//!
//! Run: cargo run --example theme_agents -p aster-orch

use std::io;

use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::{Color, Style, Stylize},
    symbols::{self, border},
    text::{Line, Span},
    widgets::{Block, LineGauge, Padding, Paragraph, Sparkline, Tabs, Widget},
};

// ── Design System ────────────────────────────────────────────────────────────

const BG_PRIMARY: Color = Color::Rgb(12, 12, 16);
const BG_PANEL: Color = Color::Rgb(20, 20, 26);
const BG_CARD: Color = Color::Rgb(24, 24, 30);
const BORDER_DIM: Color = Color::Rgb(50, 50, 58);
const TEXT_MUTED: Color = Color::Rgb(100, 100, 112);
const TEXT_NORMAL: Color = Color::Rgb(180, 180, 190);
const TEXT_BRIGHT: Color = Color::Rgb(220, 220, 230);
const ACCENT: Color = Color::Rgb(232, 135, 75);
const SUCCESS: Color = Color::Rgb(80, 200, 120);
const SUCCESS_DIM: Color = Color::Rgb(50, 130, 75);
const FAILURE: Color = Color::Rgb(200, 60, 60);

fn panel(title: &str) -> Block<'_> {
    Block::bordered()
        .border_set(border::ONE_EIGHTH_WIDE)
        .border_style(Style::new().fg(BORDER_DIM))
        .title_top(Line::from(format!(" {title} ")).left_aligned())
        .title_style(Style::new().fg(TEXT_MUTED))
        .padding(Padding::proportional(1))
        .style(Style::new().bg(BG_PANEL))
}

fn card_block(title: &str, health_color: Color) -> Block<'_> {
    Block::bordered()
        .border_set(border::ONE_EIGHTH_WIDE)
        .border_style(Style::new().fg(BORDER_DIM))
        .title_top(Line::from(vec![format!(" ● {title} ").fg(health_color).bold()]).left_aligned())
        .padding(Padding::new(2, 2, 0, 0))
        .style(Style::new().bg(BG_CARD))
}

// ── Mock Data ────────────────────────────────────────────────────────────────

struct AgentData {
    alias: &'static str,
    backend: &'static str,
    model: &'static str,
    role: &'static str,
    active: u16,
    completed: u16,
    failed: u16,
    health: Color,
    sparkline: Vec<u64>,
    capacity_ratio: f64,
}

fn mock_agents() -> Vec<AgentData> {
    vec![
        AgentData {
            alias: "focused",
            backend: "claude",
            model: "opus-4",
            role: "worker",
            active: 1,
            completed: 47,
            failed: 2,
            health: SUCCESS,
            sparkline: vec![
                0, 1, 2, 1, 3, 2, 4, 3, 2, 1, 3, 5, 4, 3, 2, 1, 2, 3, 4, 2, 1, 0, 1, 2,
            ],
            capacity_ratio: 0.33,
        },
        AgentData {
            alias: "spark",
            backend: "claude",
            model: "sonnet-4",
            role: "worker",
            active: 1,
            completed: 62,
            failed: 0,
            health: SUCCESS,
            sparkline: vec![
                2, 3, 4, 5, 3, 2, 1, 2, 4, 6, 5, 3, 2, 1, 0, 1, 3, 5, 7, 5, 3, 2, 1, 2,
            ],
            capacity_ratio: 0.33,
        },
        AgentData {
            alias: "pixel",
            backend: "claude",
            model: "sonnet-4",
            role: "worker",
            active: 0,
            completed: 31,
            failed: 1,
            health: SUCCESS,
            sparkline: vec![
                1, 0, 0, 1, 2, 1, 0, 0, 1, 1, 2, 3, 2, 1, 0, 0, 0, 1, 2, 1, 0, 0, 0, 0,
            ],
            capacity_ratio: 0.0,
        },
        AgentData {
            alias: "reviewer",
            backend: "claude",
            model: "opus-4",
            role: "reviewer",
            active: 0,
            completed: 28,
            failed: 0,
            health: SUCCESS,
            sparkline: vec![
                0, 0, 1, 0, 1, 1, 0, 0, 1, 0, 0, 1, 1, 0, 0, 0, 1, 0, 1, 0, 0, 0, 0, 0,
            ],
            capacity_ratio: 0.0,
        },
    ]
}

// ── Rendering ────────────────────────────────────────────────────────────────

fn render_tab_bar(area: Rect, buf: &mut Buffer) {
    let tabs = Tabs::new(["Ops", "Agents", "History", "Settings"])
        .select(1)
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

fn render_agent_card(area: Rect, buf: &mut Buffer, agent: &AgentData) {
    let block = card_block(agent.alias, agent.health);
    let inner = block.inner(area);
    block.render(area, buf);

    if inner.height < 4 || inner.width < 20 {
        return;
    }

    // Row 1: metadata
    let [meta_area, spark_area, gauge_area, stats_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(inner);

    let meta = Line::from(vec![
        "backend: ".fg(TEXT_MUTED),
        agent.backend.fg(TEXT_NORMAL),
        "  model: ".fg(TEXT_MUTED),
        agent.model.fg(TEXT_NORMAL),
        "  role: ".fg(TEXT_MUTED),
        agent.role.fg(TEXT_NORMAL),
    ]);
    Paragraph::new(meta)
        .style(Style::new().bg(BG_CARD))
        .render(meta_area, buf);

    // Row 2: sparkline (activity over last 24 intervals)
    let spark = Sparkline::default()
        .data(&agent.sparkline)
        .bar_set(symbols::bar::NINE_LEVELS)
        .style(Style::new().fg(ACCENT))
        .max(8);
    spark.render(spark_area, buf);

    // Row 3: capacity gauge
    let gauge = LineGauge::default()
        .filled_symbol("━")
        .unfilled_symbol("╌")
        .filled_style(Style::new().fg(if agent.active > 0 {
            ACCENT
        } else {
            Color::Rgb(60, 60, 68)
        }))
        .unfilled_style(Style::new().fg(Color::Rgb(35, 35, 42)))
        .label(Line::from(vec![
            "load ".fg(TEXT_MUTED),
            format!("{:.0}%", agent.capacity_ratio * 100.0).fg(if agent.active > 0 {
                ACCENT
            } else {
                TEXT_MUTED
            }),
        ]))
        .ratio(agent.capacity_ratio);
    gauge.render(gauge_area, buf);

    // Row 4: stats
    let stats = Line::from(vec![
        "active: ".fg(TEXT_MUTED),
        format!("{}", agent.active)
            .fg(if agent.active > 0 { ACCENT } else { TEXT_MUTED })
            .bold(),
        "  completed: ".fg(TEXT_MUTED),
        format!("{}", agent.completed).fg(SUCCESS_DIM),
        "  failed: ".fg(TEXT_MUTED),
        format!("{}", agent.failed).fg(if agent.failed > 0 {
            FAILURE
        } else {
            TEXT_MUTED
        }),
    ]);
    Paragraph::new(stats)
        .style(Style::new().bg(BG_CARD))
        .render(stats_area, buf);
}

fn render_agents_content(area: Rect, buf: &mut Buffer) {
    let outer = panel("Agents");
    let inner = outer.inner(area);
    outer.render(area, buf);

    let agents = mock_agents();
    let n = agents.len();

    // Each card gets 6 rows (4 content + 1 border top + 1 border bottom)
    let constraints: Vec<Constraint> = agents
        .iter()
        .enumerate()
        .flat_map(|(i, _)| {
            if i < n - 1 {
                vec![Constraint::Length(6), Constraint::Length(0)]
            } else {
                vec![Constraint::Length(6)]
            }
        })
        .chain(std::iter::once(Constraint::Fill(1)))
        .collect();

    let areas = Layout::vertical(constraints).split(inner);

    for (i, agent) in agents.iter().enumerate() {
        let card_area = areas[i * 2];
        render_agent_card(card_area, buf, agent);
    }
}

fn render_status_bar(area: Rect, buf: &mut Buffer) {
    let bar = Line::from(vec![
        " q".fg(ACCENT).bold(),
        ": quit  ".fg(TEXT_MUTED),
        "j/k".fg(ACCENT).bold(),
        ": select  ".fg(TEXT_MUTED),
        "Tab".fg(ACCENT).bold(),
        ": next tab".fg(TEXT_MUTED),
    ]);
    Paragraph::new(bar)
        .style(Style::new().bg(Color::Rgb(25, 25, 32)).fg(TEXT_MUTED))
        .render(area, buf);
}

// ── Main ─────────────────────────────────────────────────────────────────────

fn main() -> io::Result<()> {
    let mut terminal = ratatui::init();

    loop {
        terminal.draw(|frame| {
            let area = frame.area();

            Block::default()
                .style(Style::new().bg(BG_PRIMARY))
                .render(area, frame.buffer_mut());

            let [tab_bar, content, status_bar] = Layout::vertical([
                Constraint::Length(3),
                Constraint::Fill(1),
                Constraint::Length(1),
            ])
            .areas(area);

            render_tab_bar(tab_bar, frame.buffer_mut());
            render_agents_content(content, frame.buffer_mut());
            render_status_bar(status_bar, frame.buffer_mut());
        })?;

        if let crossterm::event::Event::Key(key) = crossterm::event::read()? {
            if key.kind != crossterm::event::KeyEventKind::Press {
                continue;
            }
            if matches!(key.code, crossterm::event::KeyCode::Char('q')) {
                break;
            }
        }
    }

    ratatui::restore();
    Ok(())
}
