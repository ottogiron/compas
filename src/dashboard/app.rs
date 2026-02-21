//! TUI App — terminal setup, event loop, and layout rendering.
//!
//! Layout (top → bottom):
//!   ┌─────────────────────────────────────┐  ← tab bar   (3 rows)
//!   │  Activity  Agents  History  Settings │
//!   ├─────────────────────────────────────┤
//!   │                                     │  ← content   (fills remaining)
//!   │  <tab placeholder>                  │
//!   ├─────────────────────────────────────┤
//!   │  q: quit │ Tab: switch tab │ …      │  ← status bar (1 row)
//!   └─────────────────────────────────────┘
//!
//! When `viewing_log` is `Some`, the log viewer occupies the full terminal area
//! (tab bar and status bar are hidden for maximum vertical space).

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
    path::PathBuf,
    time::{Duration, Instant},
};
use tokio::runtime::Handle;

use crate::config::types::OrchestratorConfig;
use crate::dashboard::views::activity::{self, render_activity};
use crate::dashboard::views::agents;
use crate::dashboard::views::executions;
use crate::dashboard::views::log_viewer::{render_log_viewer, LogViewerState};
use crate::store::{ExecutionRow, Store, ThreadStatusView};

// ── Constants ─────────────────────────────────────────────────────────────────

const TABS: &[&str] = &["Activity", "Agents", "History", "Settings"];
const TICK_RATE: Duration = Duration::from_millis(250);

// ── Activity data ─────────────────────────────────────────────────────────────

/// Snapshot of live metrics fetched from SQLite for the Activity tab.
pub struct ActivityData {
    /// All thread rows from `status_view(None, None, None, 50)`.
    pub rows: Vec<ThreadStatusView>,
    /// Per-status thread counts: `[(status, count), …]`.
    pub thread_counts: Vec<(String, i64)>,
    /// Number of executions in the `queued` state (Pending in footer).
    pub queue_depth: i64,
    /// Most recent worker heartbeat row, if any.
    pub heartbeat: Option<(String, i64, i64, Option<String>)>,
    /// When this snapshot was fetched (used for staleness checks).
    pub fetched_at: Instant,
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

// ── Executions data ───────────────────────────────────────────────────────────

/// Snapshot of recent execution rows fetched from SQLite for the History tab.
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
    /// Most recently fetched Activity metrics; `None` until the first poll on tab 0.
    pub activity_data: Option<ActivityData>,
    /// Index of the highlighted row in the Activity tab (across all sections).
    pub activity_selected: usize,
    /// Most recently fetched Agents metrics; `None` until the first poll on tab 1.
    pub agents_data: Option<AgentsData>,
    /// Most recently fetched execution rows; `None` until the first poll on tab 2.
    pub executions_data: Option<ExecutionsData>,
    /// Index of the highlighted row in the History tab.
    pub executions_selected: usize,
    /// Active log viewer state.  `Some` when the log viewer overlay is open.
    pub viewing_log: Option<LogViewerState>,
    /// Directory where execution log files are stored (`{state_dir}/logs/`).
    pub log_dir: PathBuf,
    /// Tokio runtime handle — used to drive async store queries from the
    /// synchronous TUI thread via `Handle::block_on`.
    handle: Handle,
}

impl App {
    pub fn new(
        store: Store,
        config: OrchestratorConfig,
        config_path: PathBuf,
        handle: Handle,
        poll_interval: Duration,
    ) -> Self {
        let log_dir = config.log_dir();
        Self {
            active_tab: 0,
            should_quit: false,
            store,
            config,
            config_path,
            poll_interval,
            activity_data: None,
            activity_selected: 0,
            agents_data: None,
            executions_data: None,
            executions_selected: 0,
            viewing_log: None,
            log_dir,
            handle,
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

    /// Fetch fresh metrics from SQLite and update `activity_data`.
    ///
    /// A single `status_view` call feeds all three sections; supplementary
    /// queries populate the summary footer and worker-health dot.
    /// Silently swallows errors — stale data is preferable to a panic inside
    /// the event loop.
    pub fn refresh_activity(&mut self) {
        let store = &self.store;
        let handle = &self.handle;

        let data = handle.block_on(async {
            let rows = store
                .status_view(None, None, None, 50)
                .await
                .unwrap_or_default();
            let thread_counts = store.thread_counts().await.unwrap_or_default();
            let queue_depth = store.queue_depth().await.unwrap_or(0);
            let heartbeat = store.latest_heartbeat().await.unwrap_or(None);

            ActivityData {
                rows,
                thread_counts,
                queue_depth,
                heartbeat,
                fetched_at: Instant::now(),
            }
        });

        // Clamp selection to new selectable count.
        let count = activity::selectable_count(&data.rows);
        if count > 0 {
            self.activity_selected = self.activity_selected.min(count - 1);
        } else {
            self.activity_selected = 0;
        }

        self.activity_data = Some(data);
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

        // Clamp the selection to the new row count.
        if !executions.is_empty() {
            self.executions_selected = self.executions_selected.min(executions.len() - 1);
        } else {
            self.executions_selected = 0;
        }

        self.executions_data = Some(ExecutionsData {
            executions,
            fetched_at: Instant::now(),
        });
    }

    // ── Row selection ─────────────────────────────────────────────────────────

    /// Move the selection up by one row on the active list tab (0 or 2).
    pub fn select_prev_row(&mut self) {
        match self.active_tab {
            0 => {
                self.activity_selected = self.activity_selected.saturating_sub(1);
            }
            2 => {
                self.executions_selected = self.executions_selected.saturating_sub(1);
            }
            _ => {}
        }
    }

    /// Move the selection down by one row on the active list tab (0 or 2).
    pub fn select_next_row(&mut self) {
        match self.active_tab {
            0 => {
                let max = self
                    .activity_data
                    .as_ref()
                    .map(|d| activity::selectable_count(&d.rows).saturating_sub(1))
                    .unwrap_or(0);
                self.activity_selected = (self.activity_selected + 1).min(max);
            }
            2 => {
                let max = self
                    .executions_data
                    .as_ref()
                    .map(|d| d.executions.len().saturating_sub(1))
                    .unwrap_or(0);
                self.executions_selected = (self.executions_selected + 1).min(max);
            }
            _ => {}
        }
    }

    // ── Log viewer ────────────────────────────────────────────────────────────

    /// Open the log viewer for the currently selected row on the active tab.
    ///
    /// For the Activity tab (tab 0): uses the execution attached to the selected
    /// row.
    /// For the History tab (tab 2): uses the selected `ExecutionRow` directly.
    /// Silently does nothing if there is no data or the selected row has no
    /// execution information.
    pub fn open_log_viewer(&mut self) {
        match self.active_tab {
            0 => self.open_log_viewer_from_activity(),
            2 => self.open_log_viewer_from_execution(),
            _ => {}
        }
    }

    fn open_log_viewer_from_activity(&mut self) {
        let Some(data) = &self.activity_data else {
            return;
        };
        let sel_idxs = activity::selectable_indices(&data.rows);
        let Some(&src_idx) = sel_idxs.get(self.activity_selected) else {
            return;
        };
        let Some(row) = data.rows.get(src_idx) else {
            return;
        };

        let exec_id = match &row.execution_id {
            Some(id) if !id.is_empty() => id.clone(),
            _ => return,
        };
        let agent_alias = row.agent_alias.clone().unwrap_or_else(|| "-".to_string());
        let status = row
            .execution_status
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        let duration_ms = row.duration_ms;
        let thread_id = row.thread_id.clone();
        let fallback = self.fetch_message_fallback(&thread_id);
        let log_path = self.log_dir.join(format!("{}.log", exec_id));
        self.viewing_log = Some(LogViewerState::new(
            exec_id,
            agent_alias,
            status,
            duration_ms,
            Some(log_path),
            fallback,
        ));
    }

    fn open_log_viewer_from_execution(&mut self) {
        let Some(data) = &self.executions_data else {
            return;
        };
        let Some(row) = data.executions.get(self.executions_selected) else {
            return;
        };

        let exec_id = row.id.clone();
        let agent_alias = row.agent_alias.clone();
        let status = row.status.clone();
        let duration_ms = row.duration_ms;
        let output_preview = row.output_preview.clone();
        let thread_id = row.thread_id.clone();

        // Use output_preview as primary fallback; fall through to message body
        // if it is empty.
        let fallback = if output_preview.as_deref().unwrap_or("").is_empty() {
            self.fetch_message_fallback(&thread_id)
        } else {
            output_preview
        };

        let log_path = self.log_dir.join(format!("{}.log", exec_id));
        self.viewing_log = Some(LogViewerState::new(
            exec_id,
            agent_alias,
            status,
            duration_ms,
            Some(log_path),
            fallback,
        ));
    }

    /// Fetch the body of the last message on `thread_id` as a fallback string.
    ///
    /// Uses `Handle::block_on`.  Returns `None` on any error.
    fn fetch_message_fallback(&self, thread_id: &str) -> Option<String> {
        let store = &self.store;
        let handle = &self.handle;
        let tid = thread_id.to_string();
        handle.block_on(async {
            let messages = store.get_thread_messages(&tid).await.ok()?;
            messages.into_iter().last().map(|m| m.body)
        })
    }

    /// Close the log viewer and return to the list view.
    pub fn close_log_viewer(&mut self) {
        self.viewing_log = None;
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

        // ── Log viewer polling ────────────────────────────────────────────────
        // If a running execution's log viewer is open, poll on every tick for
        // new file content (TICK_RATE ≈ 250 ms, close to the 200 ms target).
        if let Some(ref mut viewer) = app.viewing_log {
            if viewer.is_running() {
                viewer.poll_log_file();
            }
        }

        // ── Background data refreshes (only when log viewer is closed) ────────
        if app.viewing_log.is_none() {
            // Refresh Activity tab data when it is active and stale.
            if app.active_tab == 0 {
                let is_stale = app
                    .activity_data
                    .as_ref()
                    .map(|d| d.fetched_at.elapsed() >= interval)
                    .unwrap_or(true);
                if is_stale {
                    app.refresh_activity();
                }
            }

            // Refresh Agents tab data when it is active and stale.
            if app.active_tab == 1 {
                let is_stale = app
                    .agents_data
                    .as_ref()
                    .map(|d| d.fetched_at.elapsed() >= interval)
                    .unwrap_or(true);
                if is_stale {
                    app.refresh_agents();
                }
            }

            // Refresh History tab data when it is active and stale.
            if app.active_tab == 2 {
                let is_stale = app
                    .executions_data
                    .as_ref()
                    .map(|d| d.fetched_at.elapsed() >= interval)
                    .unwrap_or(true);
                if is_stale {
                    app.refresh_executions();
                }
            }
        }

        terminal.draw(|f| draw(f, app))?;

        // Poll with a tick timeout so the loop stays responsive to resizes.
        if event::poll(TICK_RATE)? {
            match event::read()? {
                Event::Key(key) => {
                    if app.viewing_log.is_some() {
                        handle_log_viewer_key(app, key.code);
                    } else {
                        handle_list_key(app, key.code);
                    }
                }
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

/// Handle a key event when the log viewer is open.
fn handle_log_viewer_key(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Esc => {
            app.close_log_viewer();
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if let Some(ref mut viewer) = app.viewing_log {
                viewer.scroll_up(1);
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if let Some(ref mut viewer) = app.viewing_log {
                viewer.scroll_down(1);
            }
        }
        KeyCode::PageUp => {
            if let Some(ref mut viewer) = app.viewing_log {
                let page = viewer.visible_rows.max(1);
                viewer.scroll_up(page);
            }
        }
        KeyCode::PageDown => {
            if let Some(ref mut viewer) = app.viewing_log {
                let page = viewer.visible_rows.max(1);
                viewer.scroll_down(page);
            }
        }
        KeyCode::Char('g') => {
            if let Some(ref mut viewer) = app.viewing_log {
                viewer.scroll_to_top();
            }
        }
        KeyCode::Char('G') => {
            if let Some(ref mut viewer) = app.viewing_log {
                viewer.scroll_to_bottom();
            }
        }
        KeyCode::Char('f') => {
            if let Some(ref mut viewer) = app.viewing_log {
                viewer.toggle_follow();
            }
        }
        _ => {}
    }
}

/// Handle a key event when the normal list/tab view is active.
fn handle_list_key(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Char('q') => app.should_quit = true,
        // Manual refresh.
        KeyCode::Char('r') => match app.active_tab {
            0 => app.refresh_activity(),
            1 => app.refresh_agents(),
            2 => app.refresh_executions(),
            _ => {}
        },
        // Number keys jump directly to a tab (1-4 → index 0-3).
        KeyCode::Char('1') => app.active_tab = 0,
        KeyCode::Char('2') => app.active_tab = 1,
        KeyCode::Char('3') => app.active_tab = 2,
        KeyCode::Char('4') => app.active_tab = 3,
        KeyCode::Tab => app.next_tab(),
        // Shift+Tab — crossterm reports this as BackTab.
        KeyCode::BackTab => app.prev_tab(),
        // Row navigation (Activity and History tabs).
        KeyCode::Up | KeyCode::Char('k') => app.select_prev_row(),
        KeyCode::Down | KeyCode::Char('j') => app.select_next_row(),
        // Open log viewer for the selected row.
        KeyCode::Enter => app.open_log_viewer(),
        _ => {}
    }
}

// ── Rendering ─────────────────────────────────────────────────────────────────

fn draw(f: &mut Frame, app: &mut App) {
    // When the log viewer is open, render it full-screen for maximum space.
    if let Some(ref mut viewer) = app.viewing_log {
        render_log_viewer(f, viewer, f.area());
        return;
    }

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
        0 => render_activity(f, app, area),
        1 => agents::render_agents_tab(f, app, area),
        2 => executions::render_executions(f, app, area),
        3 => render_settings(f, app, area),
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
        Line::from(vec![
            Span::raw("  "),
            label("Log dir:      "),
            value(format!(" {}", app.log_dir.display())),
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
        key("1-4"),
        Span::raw(": jump"),
        sep(),
        key("r"),
        Span::raw(": refresh"),
        sep(),
        key("↑/↓"),
        Span::raw(": select"),
        sep(),
        key("Enter"),
        Span::raw(": log"),
        Span::raw(" "),
    ]))
    .style(Style::default().bg(Color::DarkGray).fg(Color::White));

    f.render_widget(status, area);
}
