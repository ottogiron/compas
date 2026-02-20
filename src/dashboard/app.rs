//! TUI App — terminal setup, event loop, and layout rendering.
//!
//! Layout (top → bottom):
//!   ┌─────────────────────────────────────┐  ← tab bar   (3 rows)
//!   │  Overview  Threads  Agents  …       │
//!   ├─────────────────────────────────────┤
//!   │                                     │  ← content   (fills remaining)
//!   │  <tab placeholder>                  │
//!   ├─────────────────────────────────────┤
//!   │  q: quit │ Tab: switch tab │ …      │  ← status bar (1 row)
//!   └─────────────────────────────────────┘

use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Tabs},
    Frame, Terminal,
};
use std::{
    io::{self, Stdout},
    time::{Duration, Instant},
};
use tokio::runtime::Handle;

use crate::config::types::OrchestratorConfig;
use crate::dashboard::views::agents;
use crate::dashboard::views::executions;
use crate::dashboard::views::overview::render_overview;
use crate::dashboard::views::threads::render_threads;
use crate::store::{ExecutionRow, Store, ThreadStatusView};
use std::path::PathBuf;

// ── Constants ─────────────────────────────────────────────────────────────────

const TABS: &[&str] = &["Overview", "Threads", "Agents", "Executions", "Settings"];
const TICK_RATE: Duration = Duration::from_millis(250);

// ── Overview data ─────────────────────────────────────────────────────────────

/// Snapshot of live metrics fetched from SQLite for the Overview tab.
pub struct OverviewData {
    /// Per-status thread counts: `[(status, count), …]`.
    pub thread_counts: Vec<(String, i64)>,
    /// Number of executions in the `queued` state.
    pub queue_depth: i64,
    /// Total message rows in the `messages` table.
    pub total_messages: i64,
    /// Active execution count per agent: `[(alias, count), …]`.
    pub active_by_agent: Vec<(String, i64)>,
    /// Most recent worker heartbeat row, if any.
    pub heartbeat: Option<(String, i64, i64, Option<String>)>,
}

// ── Agents data ───────────────────────────────────────────────────────────────

/// Compact execution record for display in the Agents tab.
pub struct ExecutionSummary {
    pub status: String,
    pub duration_ms: Option<i64>,
}

/// Snapshot of agent-level metrics fetched from SQLite for the Agents tab.
pub struct AgentsData {
    /// Recent execution summaries per agent: `[(alias, [summary, …]), …]`.
    pub executions_by_agent: Vec<(String, Vec<ExecutionSummary>)>,
    /// Active execution count per agent: `[(alias, count), …]`.
    pub active_counts: Vec<(String, i64)>,
    /// Age of the most recent worker heartbeat in seconds, if any.
    pub heartbeat_age_secs: Option<u64>,
    /// When this snapshot was fetched (used for staleness checks).
    pub fetched_at: Instant,
}

// ── Threads data ──────────────────────────────────────────────────────────────

/// Snapshot of thread rows fetched from SQLite for the Threads tab.
pub struct ThreadsData {
    /// Rows returned by `store.status_view(None, None, None, 50)`.
    pub threads: Vec<ThreadStatusView>,
    /// When this snapshot was fetched (used for staleness checks).
    pub fetched_at: Instant,
}

// ── Executions data ───────────────────────────────────────────────────────────

/// Snapshot of recent execution rows fetched from SQLite for the Executions tab.
pub struct ExecutionsData {
    /// Up to 50 most-recent execution rows, newest first.
    pub executions: Vec<ExecutionRow>,
    /// When this snapshot was fetched (used for staleness checks).
    pub fetched_at: Instant,
}

// ── App state ─────────────────────────────────────────────────────────────────

/// Central application state passed into every draw call and mutated by events.
pub struct App {
    /// Index of the currently selected tab.
    pub active_tab: usize,
    /// Set to `true` to break the event loop and exit cleanly.
    pub should_quit: bool,
    /// Handle to the SQLite store.
    pub store: Store,
    /// Orchestrator configuration (agents, orchestration settings, etc.).
    pub config: OrchestratorConfig,
    /// Path to the config file — shown on the Settings tab.
    pub config_path: PathBuf,
    /// How often to re-query SQLite for fresh metrics.
    pub poll_interval: Duration,
    /// Most recently fetched Overview metrics; `None` until the first poll.
    pub overview_data: Option<OverviewData>,
    /// Most recently fetched Threads rows; `None` until the first poll on tab 1.
    pub threads_data: Option<ThreadsData>,
    /// Most recently fetched Agents metrics; `None` until the first poll on tab 2.
    pub agents_data: Option<AgentsData>,
    /// Most recently fetched execution rows; `None` until the first poll on tab 3.
    pub executions_data: Option<ExecutionsData>,
    /// Tokio runtime handle — used to drive async store queries from the
    /// synchronous TUI thread via `Handle::block_on`.
    handle: Handle,
    /// Instant of the last successful data refresh.
    last_refresh: Instant,
}

impl App {
    pub fn new(
        store: Store,
        config: OrchestratorConfig,
        config_path: PathBuf,
        handle: Handle,
        poll_interval: Duration,
    ) -> Self {
        Self {
            active_tab: 0,
            should_quit: false,
            store,
            config,
            config_path,
            poll_interval,
            overview_data: None,
            threads_data: None,
            agents_data: None,
            executions_data: None,
            handle,
            // Subtract the full interval so the very first tick triggers a refresh.
            last_refresh: Instant::now()
                .checked_sub(poll_interval)
                .unwrap_or_else(Instant::now),
        }
    }

    /// Advance to the next tab, wrapping around.
    pub fn next_tab(&mut self) {
        self.active_tab = (self.active_tab + 1) % TABS.len();
    }

    /// Move to the previous tab, wrapping around.
    pub fn prev_tab(&mut self) {
        if self.active_tab == 0 {
            self.active_tab = TABS.len() - 1;
        } else {
            self.active_tab -= 1;
        }
    }

    /// Fetch fresh metrics from SQLite and update `overview_data`.
    ///
    /// Uses `Handle::block_on` to drive async queries from the synchronous
    /// TUI thread. Silently swallows errors — stale data is preferable to a
    /// panic inside the event loop.
    pub fn refresh_overview(&mut self) {
        // Split borrows explicitly so the borrow checker sees two distinct fields.
        let store = &self.store;
        let handle = &self.handle;

        let data = handle.block_on(async {
            let thread_counts = store.thread_counts().await.unwrap_or_default();
            let queue_depth = store.queue_depth().await.unwrap_or(0);
            let total_messages = store.message_count().await.unwrap_or(0);
            let active_by_agent = store.active_executions_by_agent().await.unwrap_or_default();
            let heartbeat = store.latest_heartbeat().await.unwrap_or(None);

            OverviewData {
                thread_counts,
                queue_depth,
                total_messages,
                active_by_agent,
                heartbeat,
            }
        });

        self.overview_data = Some(data);
        self.last_refresh = Instant::now();
    }

    /// Fetch the latest thread rows from SQLite and update `threads_data`.
    ///
    /// Queries up to 50 threads ordered by `updated_at DESC`. Silently swallows
    /// errors — stale data is preferable to a panic inside the event loop.
    pub fn refresh_threads(&mut self) {
        let store = &self.store;
        let handle = &self.handle;

        let threads = handle.block_on(async {
            store
                .status_view(None, None, None, 50)
                .await
                .unwrap_or_default()
        });

        self.threads_data = Some(ThreadsData {
            threads,
            fetched_at: Instant::now(),
        });
    }

    /// Fetch fresh per-agent metrics from SQLite and update `agents_data`.
    ///
    /// Uses `Handle::block_on` to drive async queries from the synchronous
    /// TUI thread. Silently swallows errors — stale data is preferable to a
    /// panic inside the event loop.
    pub fn refresh_agents(&mut self) {
        // Collect aliases up-front to avoid holding a borrow into self.config
        // across the async block.
        let aliases: Vec<String> = self.config.agents.iter().map(|a| a.alias.clone()).collect();

        let store = &self.store;
        let handle = &self.handle;

        let data = handle.block_on(async {
            let active_counts = store.active_executions_by_agent().await.unwrap_or_default();

            let heartbeat = store.latest_heartbeat().await.unwrap_or(None);
            let now_unix = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64;
            let heartbeat_age_secs =
                heartbeat.map(|(_, last_beat_at, _, _)| (now_unix - last_beat_at).max(0) as u64);

            let mut executions_by_agent: Vec<(String, Vec<ExecutionSummary>)> = Vec::new();
            for alias in &aliases {
                let execs = store
                    .recent_agent_executions(alias, 3)
                    .await
                    .unwrap_or_default();
                let summaries = execs
                    .into_iter()
                    .map(|e| ExecutionSummary {
                        status: e.status,
                        duration_ms: e.duration_ms,
                    })
                    .collect();
                executions_by_agent.push((alias.clone(), summaries));
            }

            AgentsData {
                executions_by_agent,
                active_counts,
                heartbeat_age_secs,
                fetched_at: Instant::now(),
            }
        });

        self.agents_data = Some(data);
    }

    /// Fetch the 50 most recent executions from SQLite and update `executions_data`.
    ///
    /// Ordered by `queued_at DESC`. Silently swallows errors — stale data is
    /// preferable to a panic inside the event loop.
    pub fn refresh_executions(&mut self) {
        let store = &self.store;
        let handle = &self.handle;

        let executions =
            handle.block_on(async { store.recent_executions(50).await.unwrap_or_default() });

        self.executions_data = Some(ExecutionsData {
            executions,
            fetched_at: Instant::now(),
        });
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

/// Set up the terminal, run the event loop, and restore the terminal on exit
/// (including on panic).
///
/// `handle` is a Tokio runtime handle obtained from the caller's async context
/// and used to drive async store queries from this synchronous blocking thread.
/// `poll_interval_secs` overrides the default 2-second refresh cadence.
pub fn run_tui(
    store: Store,
    config: OrchestratorConfig,
    config_path: PathBuf,
    handle: Handle,
    poll_interval_secs: u64,
) -> io::Result<()> {
    let poll_interval = Duration::from_secs(poll_interval_secs);

    // Enter raw mode and alternate screen.
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Install a panic hook that restores the terminal before printing the panic.
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Best-effort restore — ignore errors inside a panic handler.
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
        original_hook(info);
    }));

    let mut app = App::new(store, config, config_path, handle, poll_interval);
    let result = event_loop(&mut terminal, &mut app);

    // Restore terminal unconditionally (panic hook handles the panic path).
    let _ = disable_raw_mode();
    let _ = execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    );
    let _ = terminal.show_cursor();

    result
}

// ── Event loop ────────────────────────────────────────────────────────────────

fn event_loop(terminal: &mut Terminal<CrosstermBackend<Stdout>>, app: &mut App) -> io::Result<()> {
    loop {
        let interval = app.poll_interval;

        // Refresh Overview metrics if the polling interval has elapsed.
        if app.last_refresh.elapsed() >= interval {
            app.refresh_overview();
        }

        // Refresh Threads tab data when it is active and stale.
        if app.active_tab == 1 {
            let is_stale = app
                .threads_data
                .as_ref()
                .map(|d| d.fetched_at.elapsed() >= interval)
                .unwrap_or(true);
            if is_stale {
                app.refresh_threads();
            }
        }

        // Refresh Agents tab data when it is active and stale.
        if app.active_tab == 2 {
            let is_stale = app
                .agents_data
                .as_ref()
                .map(|d| d.fetched_at.elapsed() >= interval)
                .unwrap_or(true);
            if is_stale {
                app.refresh_agents();
            }
        }

        // Refresh Executions tab data when it is active and stale.
        if app.active_tab == 3 {
            let is_stale = app
                .executions_data
                .as_ref()
                .map(|d| d.fetched_at.elapsed() >= interval)
                .unwrap_or(true);
            if is_stale {
                app.refresh_executions();
            }
        }

        terminal.draw(|f| draw(f, app))?;

        // Poll with a tick timeout so the loop stays responsive to resizes.
        if event::poll(TICK_RATE)? {
            match event::read()? {
                Event::Key(key) => match key.code {
                    KeyCode::Char('q') => app.should_quit = true,
                    // Manual refresh — refreshes whichever tab is currently active.
                    KeyCode::Char('r') => match app.active_tab {
                        0 => app.refresh_overview(),
                        1 => app.refresh_threads(),
                        2 => app.refresh_agents(),
                        3 => app.refresh_executions(),
                        _ => {} // Settings tab has no data to refresh
                    },
                    // Number keys jump directly to a tab (1-5 → index 0-4).
                    KeyCode::Char('1') => app.active_tab = 0,
                    KeyCode::Char('2') => app.active_tab = 1,
                    KeyCode::Char('3') => app.active_tab = 2,
                    KeyCode::Char('4') => app.active_tab = 3,
                    KeyCode::Char('5') => app.active_tab = 4,
                    KeyCode::Tab => app.next_tab(),
                    // Shift+Tab — crossterm reports this as BackTab.
                    KeyCode::BackTab => app.prev_tab(),
                    _ => {}
                },
                // Resize is handled automatically by ratatui on the next draw.
                Event::Resize(_, _) => {}
                _ => {}
            }
        }

        if app.should_quit {
            break;
        }
    }
    Ok(())
}

// ── Rendering ─────────────────────────────────────────────────────────────────

fn draw(f: &mut Frame, app: &App) {
    let area = f.area();

    // Vertical split: tab bar | content | status bar.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // tab bar (border + label row + border)
            Constraint::Min(0),    // content pane
            Constraint::Length(1), // status bar (no border)
        ])
        .split(area);

    render_tab_bar(f, app, chunks[0]);
    render_content(f, app, chunks[1]);
    render_status_bar(f, chunks[2]);
}

fn render_tab_bar(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let tab_titles: Vec<Line> = TABS.iter().map(|&t| Line::from(Span::raw(t))).collect();

    let tabs = Tabs::new(tab_titles)
        .block(
            Block::default().borders(Borders::ALL).title(Span::styled(
                " aster-orch dashboard ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )),
        )
        .select(app.active_tab)
        .style(Style::default().fg(Color::White))
        .highlight_style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        );

    f.render_widget(tabs, area);
}

fn render_content(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    match app.active_tab {
        0 => render_overview(f, app, area),
        1 => render_threads(f, app, area),
        2 => agents::render_agents_tab(f, app, area),
        3 => executions::render_executions(f, app, area),
        4 => render_settings(f, app, area),
        _ => {
            let tab_name = TABS[app.active_tab];
            let body = format!("  {} — coming soon", tab_name);

            let content = Paragraph::new(Line::from(Span::styled(
                body,
                Style::default().fg(Color::DarkGray),
            )))
            .block(Block::default().borders(Borders::ALL));

            f.render_widget(content, area);
        }
    }
}

fn render_settings(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let block = Block::default().borders(Borders::ALL).title(" Settings ");

    let label = |s: &str| {
        Span::styled(
            s.to_string(),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
    };
    let value = |s: String| Span::styled(s, Style::default().fg(Color::White));

    let poll_secs = app.poll_interval.as_secs();

    let lines = vec![
        Line::from(vec![
            Span::raw("  "),
            label("Config:       "),
            value(app.config_path.display().to_string()),
        ]),
        Line::from(vec![
            Span::raw("  "),
            label("DB:           "),
            value(app.config.db_path.display().to_string()),
        ]),
        Line::from(vec![
            Span::raw("  "),
            label("Poll interval:"),
            value(format!(" {}s", poll_secs)),
        ]),
        Line::from(vec![
            Span::raw("  "),
            label("Agents:       "),
            value(format!(" {}", app.config.agents.len())),
        ]),
    ];

    let p = Paragraph::new(lines).block(block);
    f.render_widget(p, area);
}

fn render_status_bar(f: &mut Frame, area: ratatui::layout::Rect) {
    let key = |s: &'static str| {
        Span::styled(
            s,
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
    };
    let sep = || Span::raw("  ");

    let status = Paragraph::new(Line::from(vec![
        Span::raw(" "),
        key("q"),
        Span::raw(": quit"),
        sep(),
        key("Tab"),
        Span::raw(": next"),
        sep(),
        key("Shift+Tab"),
        Span::raw(": prev"),
        sep(),
        key("1-5"),
        Span::raw(": jump"),
        sep(),
        key("r"),
        Span::raw(": refresh"),
        Span::raw(" "),
    ]))
    .style(Style::default().bg(Color::DarkGray).fg(Color::White));

    f.render_widget(status, area);
}
