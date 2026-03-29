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
//! When `viewing_log` is `Some`, the execution detail view occupies the full
//! terminal area (tab bar and status bar are hidden for maximum vertical space).

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    MouseButton, MouseEventKind,
};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Flex, Layout, Rect},
    style::{Style, Stylize},
    symbols::border,
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph, Tabs, Widget},
    DefaultTerminal, Frame,
};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    io,
    path::PathBuf,
    time::{Duration, Instant},
};
use tokio::runtime::Handle;
use tokio::sync::broadcast;

use crate::config::ConfigHandle;
use crate::dashboard::theme;
use crate::dashboard::views::activity::{self, render_activity, OpsSelectable};
use crate::dashboard::views::agents;
use crate::dashboard::views::conversation::{render_conversation, ConversationViewState};
use crate::dashboard::views::executions::{self, HistorySelectable};
use crate::dashboard::views::log_viewer::{
    render_execution_detail, ExecutionDetailState, Tab as LogTab,
};
use crate::events::{EventBus, OrchestratorEvent};
use crate::store::{
    AgentCostSummary, CostSummary, ExecutionRow, MergeOperation, Store, ThreadStatusView,
};

// ── Constants ─────────────────────────────────────────────────────────────────

const TABS: &[&str] = &["Ops", "Agents", "History", "Settings"];
const TICK_RATE: Duration = Duration::from_millis(250);
const ACTIVITY_ROW_LIMIT: i64 = 250;
const HISTORY_ROW_LIMIT: i64 = 200;
const HISTORY_GROUP_VISIBLE_LIMIT: usize = 10;
/// Total number of content lines in the help overlay (used for scroll bounds).
/// Must stay in sync with the `lines` vec in `render_help_overlay_widget`.
const HELP_LINE_COUNT: u16 = 21;
/// Maximum number of timeline events loaded when opening the log viewer.
const TIMELINE_EVENT_LIMIT: i64 = 500;
/// Minimum time between displayed progress summary changes per execution.
/// Prevents flickering when tool calls arrive in rapid succession.
const SUMMARY_DEBOUNCE: Duration = Duration::from_millis(1500);
/// Maximum time a single refresh query set can block the TUI thread.
/// If exceeded, the refresh is skipped and `last_refresh_error` is set.
const REFRESH_TIMEOUT: Duration = Duration::from_millis(500);

// ── Activity data ─────────────────────────────────────────────────────────────

/// Snapshot of live metrics fetched from SQLite for the Activity tab.
pub struct ActivityData {
    /// Thread rows from `status_view(None, None, None, ACTIVITY_ROW_LIMIT)`.
    pub rows: Vec<ThreadStatusView>,
    /// Per-status thread counts: `[(status, count), …]`.
    pub thread_counts: Vec<(String, i64)>,
    /// Number of executions in the `queued` state (Pending in footer).
    pub queue_depth: i64,
    /// Most recent worker heartbeat row, if any.
    pub heartbeat: Option<(String, i64, i64, Option<String>)>,
    /// When this snapshot was fetched (used for staleness checks).
    pub fetched_at: Instant,
    /// Aggregated cost and token totals from all executions.
    pub cost_summary: Option<CostSummary>,
    /// Active and recently-completed merge operations.
    pub merge_ops: Vec<MergeOperation>,
}

// ── Agents data ───────────────────────────────────────────────────────────────

/// Compact execution record for display in the Agents tab.
pub struct ExecutionSummary {
    pub status: String,
    pub duration_ms: Option<i64>,
    /// Unix timestamp (seconds) when the execution finished, if available.
    pub finished_at: Option<i64>,
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
    /// Per-agent cost and token breakdown.
    pub cost_by_agent: Vec<AgentCostSummary>,
    /// Circuit breaker states per backend: `[(backend, state, failures), …]`.
    pub circuit_states: Vec<(String, String, u32)>,
}

// ── Executions data ───────────────────────────────────────────────────────────

/// Snapshot of recent execution rows fetched from SQLite for the History tab.
pub struct ExecutionsData {
    /// Up to 200 most-recent execution rows, newest first.
    pub executions: Vec<ExecutionRow>,
    /// When this snapshot was fetched (used for staleness checks).
    pub fetched_at: Instant,
}

// ── Click caches ─────────────────────────────────────────────────────────────
//
// Render functions take `app: &App` (immutable) but mouse handlers need layout
// geometry from the most recent render pass. `RefCell`-wrapped caches are
// populated during render and read during click handling.

/// Click geometry for the Ops tab list.
#[derive(Debug, Default)]
pub(crate) struct OpsClickCache {
    /// `(display_row_offset, height)` per selectable slot.
    pub slot_geometry: Vec<(usize, usize)>,
    /// Current scroll offset of the list.
    pub scroll: usize,
    /// The rect used for the list area (excluding footer).
    pub list_rect: Rect,
}

/// Click geometry for the History tab table.
#[derive(Debug, Default)]
pub(crate) struct HistoryClickCache {
    /// Maps selectable slot index → table row index.
    pub selectable_to_row: Vec<usize>,
    /// The rect used for the table area.
    pub table_rect: Rect,
    /// Current scroll offset from `TableState::offset()`.
    pub scroll_offset: usize,
    /// Number of header rows (1 for the column header).
    pub header_rows: usize,
}

/// Click geometry for the Agents tab.
#[derive(Debug, Default)]
pub(crate) struct AgentsClickCache {
    /// `(display_row_offset, height)` per agent card (includes separator rows).
    pub card_geometry: Vec<(usize, usize)>,
    /// The rect used for the agent list area.
    pub list_rect: Rect,
}

// ── App state ─────────────────────────────────────────────────────────────────

/// Central application state passed into every draw call and mutated by events.
pub struct App {
    /// Index of the currently selected tab.
    pub active_tab: usize,
    /// Set to `true` to break the event loop and exit cleanly.
    pub should_quit: bool,
    /// When `true`, show the quit confirmation dialog.
    pub confirm_quit: bool,
    /// Handle to the SQLite store.
    pub store: Store,
    /// Orchestrator configuration — live-reloaded via `ConfigHandle`.
    pub config: ConfigHandle,
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
    /// Index of the highlighted card in the Agents tab.
    pub agents_selected: usize,
    /// Most recently fetched execution rows; `None` until the first poll on tab 2.
    pub executions_data: Option<ExecutionsData>,
    /// Index of the highlighted row in the History tab.
    pub executions_selected: usize,
    /// Active execution detail state. `Some` when the detail overlay is open.
    pub viewing_log: Option<ExecutionDetailState>,
    /// Active conversation view state. `Some` when the conversation overlay is open.
    pub viewing_conversation: Option<ConversationViewState>,
    /// Whether the help overlay is visible.
    pub show_help: bool,
    /// Vertical scroll offset for the help overlay content.
    help_scroll: u16,
    /// Viewport height of the help overlay inner area (set during render).
    help_viewport_height: Cell<u16>,
    /// Optional batch drill filter for Ops tab.
    pub drill_batch: Option<String>,
    /// Optional batch drill filter for History tab.
    pub history_drill_batch: Option<String>,
    /// Optional agent filter for History tab — set when Enter is pressed on an agent card.
    pub history_agent_filter: Option<String>,
    /// One-time onboarding hint banner.
    show_hint_banner: bool,
    /// Directory where execution log files are stored (`{state_dir}/logs/`).
    pub log_dir: PathBuf,
    /// Tokio runtime handle — used to drive async store queries from the
    /// synchronous TUI thread via `Handle::block_on`.
    handle: Handle,
    /// Per-component refresh errors. Each refresh method only clears its
    /// own field on success, so a failing activity query is not masked by
    /// a successful agents refresh.
    pub activity_refresh_error: Option<String>,
    pub agents_refresh_error: Option<String>,
    pub executions_refresh_error: Option<String>,
    /// Timestamp of the last refresh attempt for each component. Used for
    /// backoff when `*_data` is still `None` (first fetch failed) so the
    /// staleness check doesn't tight-loop on every TUI tick.
    last_activity_attempt: Option<Instant>,
    last_agents_attempt: Option<Instant>,
    last_executions_attempt: Option<Instant>,
    /// Optional broadcast receiver for worker events. When `Some`, incoming
    /// events trigger an immediate data refresh instead of waiting for the next
    /// poll interval.
    event_rx: Option<broadcast::Receiver<OrchestratorEvent>>,
    /// Maps execution_id → most recent progress summary (from ExecutionProgress events).
    progress_summaries: HashMap<String, String>,
    /// Tracks when each execution's displayed summary last changed (for debounce).
    progress_last_changed: HashMap<String, Instant>,
    /// Cached schedule run counts: `schedule_name → (last_fired_at, run_count)`.
    schedule_run_counts: Option<HashMap<String, (i64, u64)>>,
    /// Timestamp of the last schedule data refresh attempt.
    last_schedule_attempt: Option<Instant>,
    /// Vertical scroll offset for the Settings tab content.
    settings_scroll: u16,
    /// Total number of content lines in the Settings tab (set during render).
    settings_line_count: Cell<u16>,
    /// Viewport height of the Settings tab inner area (set during render).
    settings_viewport_height: Cell<u16>,
    /// Click geometry cache for the Ops tab (populated during render).
    pub(crate) ops_click_cache: RefCell<OpsClickCache>,
    /// Click geometry cache for the History tab (populated during render).
    pub(crate) history_click_cache: RefCell<HistoryClickCache>,
    /// Click geometry cache for the Agents tab (populated during render).
    pub(crate) agents_click_cache: RefCell<AgentsClickCache>,
}

impl App {
    fn now_unix() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64
    }

    fn stale_active_secs(&self) -> i64 {
        self.config.load().orchestration.stale_active_secs as i64
    }

    pub fn history_group_visible_limit(&self) -> usize {
        HISTORY_GROUP_VISIBLE_LIMIT
    }

    /// Get the most recent progress summary for a running execution, if available.
    pub fn get_progress_summary(&self, execution_id: &str) -> Option<&str> {
        self.progress_summaries
            .get(execution_id)
            .map(|s| s.as_str())
    }

    pub fn new(
        store: Store,
        config: ConfigHandle,
        config_path: PathBuf,
        handle: Handle,
        poll_interval: Duration,
        event_bus: Option<EventBus>,
    ) -> Self {
        let log_dir = config.load().log_dir();
        let event_rx = event_bus.map(|bus| bus.subscribe());
        Self {
            active_tab: 0,
            should_quit: false,
            confirm_quit: false,
            store,
            config,
            config_path,
            poll_interval,
            activity_data: None,
            activity_selected: 0,
            agents_data: None,
            agents_selected: 0,
            executions_data: None,
            executions_selected: 0,
            viewing_log: None,
            viewing_conversation: None,
            show_help: false,
            help_scroll: 0,
            help_viewport_height: Cell::new(0),
            drill_batch: None,
            history_drill_batch: None,
            history_agent_filter: None,
            show_hint_banner: true,
            log_dir,
            handle,
            activity_refresh_error: None,
            agents_refresh_error: None,
            executions_refresh_error: None,
            last_activity_attempt: None,
            last_agents_attempt: None,
            last_executions_attempt: None,
            event_rx,
            progress_summaries: HashMap::new(),
            progress_last_changed: HashMap::new(),
            schedule_run_counts: None,
            last_schedule_attempt: None,
            settings_scroll: 0,
            settings_line_count: Cell::new(0),
            settings_viewport_height: Cell::new(0),
            ops_click_cache: RefCell::new(OpsClickCache::default()),
            history_click_cache: RefCell::new(HistoryClickCache::default()),
            agents_click_cache: RefCell::new(AgentsClickCache::default()),
        }
    }

    /// Force all data to be considered stale, triggering refresh on next tick.
    pub(crate) fn invalidate_data(&mut self) {
        // Reset fetched_at to a past instant so staleness check passes immediately.
        let old = Instant::now()
            .checked_sub(self.poll_interval + Duration::from_secs(1))
            .unwrap_or_else(Instant::now);
        if let Some(ref mut d) = self.activity_data {
            d.fetched_at = old;
        } else {
            self.last_activity_attempt = None;
        }
        if let Some(ref mut d) = self.agents_data {
            d.fetched_at = old;
        } else {
            self.last_agents_attempt = None;
        }
        if let Some(ref mut d) = self.executions_data {
            d.fetched_at = old;
        } else {
            self.last_executions_attempt = None;
        }
        self.last_schedule_attempt = None;
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
    /// On DB error, retains previous data and sets `last_refresh_error`.
    /// Queries are bounded by a timeout to prevent TUI freezes.
    pub fn refresh_activity(&mut self) {
        let store = &self.store;
        let handle = &self.handle;

        let result = handle.block_on(async {
            tokio::time::timeout(REFRESH_TIMEOUT, async {
                let rows = store
                    .status_view(None, None, None, ACTIVITY_ROW_LIMIT)
                    .await?;
                let thread_counts = store.thread_counts().await?;
                let queue_depth = store.queue_depth().await.unwrap_or(0);
                let heartbeat = store.latest_heartbeat().await.unwrap_or(None);
                let cost = store.cost_summary(None).await.ok();
                const MERGE_QUEUE_FETCH_LIMIT: i64 = 20;
                let merge_ops = store
                    .list_merge_ops(None, None, None, MERGE_QUEUE_FETCH_LIMIT)
                    .await
                    .unwrap_or_default();

                Ok::<_, sqlx::Error>(ActivityData {
                    rows,
                    thread_counts,
                    queue_depth,
                    heartbeat,
                    fetched_at: Instant::now(),
                    cost_summary: cost,
                    merge_ops,
                })
            })
            .await
        });

        match result {
            Ok(Ok(data)) => {
                // Clamp selection to new selectable count.
                let count = activity::ops_selectable_count(
                    &data.rows,
                    &data.merge_ops,
                    self.drill_batch.as_deref(),
                    Self::now_unix(),
                    self.stale_active_secs(),
                );
                if count > 0 {
                    self.activity_selected = self.activity_selected.min(count - 1);
                } else {
                    self.activity_selected = 0;
                }
                self.activity_data = Some(data);
                self.activity_refresh_error = None;
            }
            Ok(Err(e)) => {
                self.activity_refresh_error = Some(format!("activity: {}", e));
                // Update fetched_at to prevent tight-loop retries on persistent error.
                if let Some(ref mut d) = self.activity_data {
                    d.fetched_at = Instant::now();
                }
            }
            Err(_) => {
                self.activity_refresh_error = Some("activity: timeout".to_string());
                if let Some(ref mut d) = self.activity_data {
                    d.fetched_at = Instant::now();
                }
            }
        }

        // Update progress_summaries for running executions from DB.
        // This runs on every refresh cycle so the summary stays current
        // even when the dashboard runs as a separate process (no EventBus).
        // All per-execution DB queries are wrapped in a single REFRESH_TIMEOUT
        // to prevent unbounded TUI blocking.
        if let Some(ref data) = self.activity_data {
            let running_execs: Vec<(String, String)> = data
                .rows
                .iter()
                .filter(|r| {
                    r.execution_status
                        .as_deref()
                        .map(crate::dashboard::views::is_running_exec_status)
                        .unwrap_or(false)
                })
                .filter_map(|r| r.execution_id.as_ref().map(|id| (id.clone(), id.clone())))
                .collect();

            if !running_execs.is_empty() {
                let store = self.store.clone();
                let exec_ids: Vec<String> =
                    running_execs.iter().map(|(id, _)| id.clone()).collect();
                // Batch all DB queries under a single timeout
                let results: Vec<(String, Option<String>)> = self.handle.block_on(async {
                    match tokio::time::timeout(REFRESH_TIMEOUT, async {
                        let mut out = Vec::new();
                        for exec_id in &exec_ids {
                            let summary = store
                                .get_latest_progress_event(exec_id)
                                .await
                                .ok()
                                .flatten()
                                .map(|ev| ev.summary);
                            out.push((exec_id.clone(), summary));
                        }
                        out
                    })
                    .await
                    {
                        Ok(results) => results,
                        Err(_) => {
                            tracing::debug!("progress summary refresh timed out");
                            Vec::new()
                        }
                    }
                });

                for (exec_id, summary) in results {
                    if let Some(summary) = summary {
                        let current = self.progress_summaries.get(&exec_id);
                        if current.map(|s| s.as_str()) != Some(&summary) {
                            // Summary changed — check debounce
                            let debounce_ok = self
                                .progress_last_changed
                                .get(&exec_id)
                                .map(|t| t.elapsed() >= SUMMARY_DEBOUNCE)
                                .unwrap_or(true);
                            if debounce_ok {
                                self.progress_summaries.insert(exec_id.clone(), summary);
                                self.progress_last_changed
                                    .insert(exec_id.clone(), Instant::now());
                            }
                        }
                    }
                }
            }

            // Clean up summaries for executions that are no longer running
            let running_ids: std::collections::HashSet<String> = data
                .rows
                .iter()
                .filter(|r| {
                    r.execution_status
                        .as_deref()
                        .map(crate::dashboard::views::is_running_exec_status)
                        .unwrap_or(false)
                })
                .filter_map(|r| r.execution_id.clone())
                .collect();
            self.progress_summaries
                .retain(|id, _| running_ids.contains(id));
            self.progress_last_changed
                .retain(|id, _| running_ids.contains(id));
        }
    }

    /// Fetch fresh per-agent metrics from SQLite and update `agents_data`.
    ///
    /// On DB error, retains previous data and sets `agents_refresh_error`.
    /// Queries are bounded by a timeout to prevent TUI freezes.
    pub fn refresh_agents(&mut self) {
        // Snapshot live config for this refresh cycle.
        let cfg = self.config.load();
        // Collect aliases up-front to avoid holding a borrow into config
        // across the async block.
        let aliases: Vec<String> = cfg.agents.iter().map(|a| a.alias.clone()).collect();

        let store = &self.store;
        let handle = &self.handle;

        let result = handle.block_on(async {
            tokio::time::timeout(REFRESH_TIMEOUT, async {
                let active_counts = store.active_executions_by_agent().await?;

                let heartbeat = store.latest_heartbeat().await.unwrap_or(None);
                let now_unix = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64;
                let heartbeat_age_secs = heartbeat
                    .map(|(_, last_beat_at, _, _)| (now_unix - last_beat_at).max(0) as u64);

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
                            finished_at: e.finished_at,
                        })
                        .collect();
                    executions_by_agent.push((alias.clone(), summaries));
                }

                let cost_by_agent = store.cost_by_agent().await.unwrap_or_default();

                let circuit_states = store.get_circuit_breaker_states().await.unwrap_or_default();

                Ok::<_, sqlx::Error>(AgentsData {
                    executions_by_agent,
                    active_counts,
                    heartbeat_age_secs,
                    fetched_at: Instant::now(),
                    cost_by_agent,
                    circuit_states,
                })
            })
            .await
        });

        match result {
            Ok(Ok(data)) => {
                self.agents_data = Some(data);
                let agent_count = cfg.agents.len();
                if agent_count > 0 {
                    self.agents_selected = self.agents_selected.min(agent_count - 1);
                } else {
                    self.agents_selected = 0;
                }
                self.agents_refresh_error = None;
            }
            Ok(Err(e)) => {
                self.agents_refresh_error = Some(format!("agents: {}", e));
                if let Some(ref mut d) = self.agents_data {
                    d.fetched_at = Instant::now();
                }
            }
            Err(_) => {
                self.agents_refresh_error = Some("agents: timeout".to_string());
                if let Some(ref mut d) = self.agents_data {
                    d.fetched_at = Instant::now();
                }
            }
        }
    }

    /// Fetch the most recent executions from SQLite and update `executions_data`.
    ///
    /// Ordered by `queued_at DESC`. On DB error, retains previous data and
    /// sets `executions_refresh_error`. Bounded by timeout.
    pub fn refresh_executions(&mut self) {
        let store = &self.store;
        let handle = &self.handle;

        let result = handle.block_on(async {
            tokio::time::timeout(REFRESH_TIMEOUT, async {
                let executions = store.recent_executions(HISTORY_ROW_LIMIT).await?;
                Ok::<_, sqlx::Error>(executions)
            })
            .await
        });

        match result {
            Ok(Ok(executions)) => {
                // Clamp the selection to the new row count (respecting agent filter).
                let effective: Vec<_> = if let Some(ref agent) = self.history_agent_filter {
                    executions
                        .iter()
                        .filter(|e| e.agent_alias == *agent)
                        .cloned()
                        .collect()
                } else {
                    executions.clone()
                };
                let count = executions::history_selectable_count(
                    &effective,
                    self.history_drill_batch.as_deref(),
                    self.history_group_visible_limit(),
                );
                if count > 0 {
                    self.executions_selected = self.executions_selected.min(count - 1);
                } else {
                    self.executions_selected = 0;
                }
                self.executions_data = Some(ExecutionsData {
                    executions,
                    fetched_at: Instant::now(),
                });
                self.executions_refresh_error = None;
            }
            Ok(Err(e)) => {
                self.executions_refresh_error = Some(format!("history: {}", e));
                if let Some(ref mut d) = self.executions_data {
                    d.fetched_at = Instant::now();
                }
            }
            Err(_) => {
                self.executions_refresh_error = Some("history: timeout".to_string());
                if let Some(ref mut d) = self.executions_data {
                    d.fetched_at = Instant::now();
                }
            }
        }
    }

    /// Fetch schedule run counts from SQLite.
    ///
    /// Lightweight query cached on the same staleness cadence as other tabs.
    pub fn refresh_schedules(&mut self) {
        let store = &self.store;
        let handle = &self.handle;

        let result = handle.block_on(async {
            tokio::time::timeout(REFRESH_TIMEOUT, store.get_all_schedule_runs()).await
        });

        match result {
            Ok(Ok(counts)) => {
                self.schedule_run_counts = Some(counts);
            }
            _ => {
                // On error/timeout, retain previous data.
            }
        }
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
            1 => {
                self.agents_selected = self.agents_selected.saturating_sub(1);
            }
            3 => {
                self.settings_scroll = self.settings_scroll.saturating_sub(1);
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
                    .map(|d| {
                        activity::ops_selectable_count(
                            &d.rows,
                            &d.merge_ops,
                            self.drill_batch.as_deref(),
                            Self::now_unix(),
                            self.stale_active_secs(),
                        )
                        .saturating_sub(1)
                    })
                    .unwrap_or(0);
                self.activity_selected = (self.activity_selected + 1).min(max);
            }
            2 => {
                let max = self
                    .effective_executions()
                    .map(|execs| {
                        executions::history_selectable_count(
                            &execs,
                            self.history_drill_batch.as_deref(),
                            self.history_group_visible_limit(),
                        )
                        .saturating_sub(1)
                    })
                    .unwrap_or(0);
                self.executions_selected = (self.executions_selected + 1).min(max);
            }
            1 => {
                let max = self.config.load().agents.len().saturating_sub(1);
                self.agents_selected = (self.agents_selected + 1).min(max);
            }
            3 => {
                let max = self
                    .settings_line_count
                    .get()
                    .saturating_sub(self.settings_viewport_height.get());
                self.settings_scroll = (self.settings_scroll + 1).min(max);
            }
            _ => {}
        }
    }

    // ── Execution detail viewer ──────────────────────────────────────────────

    /// Open the execution detail view for the currently selected row on the active tab.
    ///
    /// For the Activity tab (tab 0): uses the execution attached to the selected
    /// row.
    /// For the History tab (tab 2): uses the selected `ExecutionRow` directly.
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
        let Some(OpsSelectable::Thread(src_idx)) = activity::ops_selected_target(
            &data.rows,
            &data.merge_ops,
            self.drill_batch.as_deref(),
            self.activity_selected,
            Self::now_unix(),
            self.stale_active_secs(),
        ) else {
            return;
        };
        let Some(row) = data.rows.get(src_idx) else {
            return;
        };
        let Some(exec_id) = row.execution_id.clone() else {
            return;
        };

        let execution = self
            .handle
            .block_on(async { self.store.get_execution(&exec_id).await.ok().flatten() });
        let Some(execution) = execution else {
            return;
        };

        let (input_payload, input_linked) = self.resolve_input_payload_for_execution(&execution);
        let log_path = Some(self.log_dir.join(format!("{}.log", execution.id)));
        let timeline_events = self.handle.block_on(async {
            self.store
                .get_execution_events(&exec_id, None, None, Some(TIMELINE_EVENT_LIMIT))
                .await
                .unwrap_or_default()
        });
        let timeline_truncated = timeline_events.len() as i64 == TIMELINE_EVENT_LIMIT;
        let detail = ExecutionDetailState::new(
            execution,
            log_path,
            input_payload,
            input_linked,
            timeline_events,
            timeline_truncated,
        );
        self.viewing_log = Some(detail);
    }

    fn open_log_viewer_from_execution(&mut self) {
        let Some(execs) = self.effective_executions() else {
            return;
        };
        let Some(HistorySelectable::Execution(exec_idx)) = self.selected_history_target() else {
            return;
        };
        let Some(row) = execs.get(exec_idx) else {
            return;
        };

        let execution = row.clone();
        let (input_payload, input_linked) = self.resolve_input_payload_for_execution(row);

        let log_path = self.log_dir.join(format!("{}.log", execution.id));
        let timeline_events = self.handle.block_on(async {
            self.store
                .get_execution_events(&execution.id, None, None, Some(TIMELINE_EVENT_LIMIT))
                .await
                .unwrap_or_default()
        });
        let timeline_truncated = timeline_events.len() as i64 == TIMELINE_EVENT_LIMIT;
        let detail = ExecutionDetailState::new(
            execution,
            Some(log_path),
            input_payload,
            input_linked,
            timeline_events,
            timeline_truncated,
        );
        self.viewing_log = Some(detail);
    }

    /// Resolve strict input payload from execution provenance.
    ///
    /// Input is available only when the execution is linked to a dispatch
    /// message that belongs to the same thread and target agent.
    fn resolve_input_payload_for_execution(
        &self,
        execution: &ExecutionRow,
    ) -> (Option<String>, bool) {
        let Some(dispatch_id) = execution.dispatch_message_id else {
            return (None, false);
        };

        let store = &self.store;
        let handle = &self.handle;
        let execution_thread_id = execution.thread_id.clone();
        let execution_agent_alias = execution.agent_alias.clone();
        handle.block_on(async {
            let Some(msg) = store.get_message(dispatch_id).await.unwrap_or(None) else {
                return (None, false);
            };
            let linked =
                msg.thread_id == execution_thread_id && msg.to_alias == execution_agent_alias;
            if linked {
                (Some(msg.body), true)
            } else {
                (None, false)
            }
        })
    }

    // ── Conversation view ────────────────────────────────────────────────────

    /// Open the conversation view for the currently selected thread.
    ///
    /// Dispatches to the appropriate handler based on the active tab:
    /// - Ops tab (0): uses the selected thread from activity data
    /// - History tab (2): uses the selected execution row
    pub fn open_conversation(&mut self) {
        match self.active_tab {
            0 => self.open_conversation_from_activity(),
            2 => self.open_conversation_from_execution(),
            _ => {}
        }
    }

    fn open_conversation_from_activity(&mut self) {
        let Some(data) = &self.activity_data else {
            return;
        };
        let Some(OpsSelectable::Thread(src_idx)) = activity::ops_selected_target(
            &data.rows,
            &data.merge_ops,
            self.drill_batch.as_deref(),
            self.activity_selected,
            Self::now_unix(),
            self.stale_active_secs(),
        ) else {
            return;
        };
        let Some(row) = data.rows.get(src_idx) else {
            return;
        };
        self.load_conversation(row.thread_id.clone());
    }

    /// Open conversation view for a merge operation's source thread.
    fn open_merge_op_thread(&mut self, op_id: &str) {
        let Some(data) = &self.activity_data else {
            return;
        };
        let Some(op) = data.merge_ops.iter().find(|o| o.id == op_id) else {
            return;
        };
        let thread_id = op.thread_id.clone();
        self.load_conversation(thread_id);
    }

    fn open_conversation_from_execution(&mut self) {
        let Some(data) = &self.executions_data else {
            return;
        };
        let Some(HistorySelectable::Execution(exec_idx)) = self.selected_history_target() else {
            return;
        };
        let Some(row) = data.executions.get(exec_idx) else {
            return;
        };
        self.load_conversation(row.thread_id.clone());
    }

    fn load_conversation(&mut self, thread_id: String) {
        let store = self.store.clone();
        let tid = thread_id.clone();
        let result = self.handle.block_on(async {
            tokio::time::timeout(REFRESH_TIMEOUT, async {
                let thread = store.get_thread(&tid).await?;
                let messages = store.get_thread_messages(&tid).await?;
                let executions = store.get_thread_executions(&tid).await?;
                Ok::<_, sqlx::Error>((thread, messages, executions))
            })
            .await
        });
        if let Ok(Ok((thread, messages, executions))) = result {
            let (batch_id, thread_status) =
                thread.map(|t| (t.batch_id, t.status)).unwrap_or_default();
            self.viewing_conversation = Some(ConversationViewState::new(
                thread_id,
                batch_id,
                thread_status,
                messages,
                executions,
            ));
        }
    }

    /// Close the conversation view and return to the list view.
    pub fn close_conversation(&mut self) {
        self.viewing_conversation = None;
        match self.active_tab {
            2 => self.refresh_executions(),
            _ => self.refresh_activity(),
        }
    }

    /// Close the detail view and return to the list view.
    ///
    /// Forces an immediate activity refresh so data is current when the
    /// operator returns to the Ops tab (background refreshes are paused
    /// while the log viewer is open to avoid unnecessary DB queries).
    pub fn close_log_viewer(&mut self) {
        self.viewing_log = None;
        self.refresh_activity();
    }

    // ── Selection helpers ─────────────────────────────────────────────────────

    fn selected_activity_target(&self) -> Option<OpsSelectable> {
        let data = self.activity_data.as_ref()?;
        activity::ops_selected_target(
            &data.rows,
            &data.merge_ops,
            self.drill_batch.as_deref(),
            self.activity_selected,
            Self::now_unix(),
            self.stale_active_secs(),
        )
    }

    /// Return the effective executions list, filtered by agent if the filter is active.
    fn effective_executions(&self) -> Option<Vec<ExecutionRow>> {
        let data = self.executions_data.as_ref()?;
        if let Some(ref agent) = self.history_agent_filter {
            Some(
                data.executions
                    .iter()
                    .filter(|e| e.agent_alias == *agent)
                    .cloned()
                    .collect(),
            )
        } else {
            Some(data.executions.clone())
        }
    }

    fn selected_history_target(&self) -> Option<HistorySelectable> {
        let execs = self.effective_executions()?;
        executions::history_selected_target(
            &execs,
            self.history_drill_batch.as_deref(),
            self.executions_selected,
            self.history_group_visible_limit(),
        )
    }

    fn toggle_help(&mut self) {
        self.show_help = !self.show_help;
        if self.show_help {
            self.help_scroll = 0;
        }
    }

    fn clear_drill_filters(&mut self) {
        if self.active_tab == 0 && self.drill_batch.is_some() {
            self.drill_batch = None;
            self.activity_selected = 0;
        }
        if self.active_tab == 2 {
            if self.history_drill_batch.is_some() {
                self.history_drill_batch = None;
                self.executions_selected = 0;
            } else if self.history_agent_filter.is_some() {
                self.history_agent_filter = None;
                self.executions_selected = 0;
            }
        }
    }

    /// Switch to the History tab filtered by the currently selected agent.
    fn enter_agent_drill(&mut self) {
        let cfg = self.config.load();
        let alias = cfg
            .agents
            .get(self.agents_selected)
            .map(|a| a.alias.clone());
        let Some(alias) = alias else { return };
        self.history_agent_filter = Some(alias);
        self.history_drill_batch = None;
        self.executions_selected = 0;
        self.active_tab = 2; // Switch to History tab
        self.refresh_executions();
    }

    fn enter_batch_drill(&mut self) {
        let Some(data) = &self.activity_data else {
            return;
        };
        let Some(OpsSelectable::Batch(batch_id)) = activity::ops_selected_target(
            &data.rows,
            &data.merge_ops,
            self.drill_batch.as_deref(),
            self.activity_selected,
            Self::now_unix(),
            self.stale_active_secs(),
        ) else {
            return;
        };
        self.drill_batch = Some(batch_id);
        self.activity_selected = 0;
        self.refresh_activity();
    }

    fn select_first_row(&mut self) {
        match self.active_tab {
            0 => {
                self.activity_selected = 0;
            }
            2 => {
                self.executions_selected = 0;
            }
            1 => {
                self.agents_selected = 0;
            }
            3 => {
                self.settings_scroll = 0;
            }
            _ => {}
        }
    }

    fn select_last_row(&mut self) {
        match self.active_tab {
            0 => {
                let max = self
                    .activity_data
                    .as_ref()
                    .map(|d| {
                        activity::ops_selectable_count(
                            &d.rows,
                            &d.merge_ops,
                            self.drill_batch.as_deref(),
                            Self::now_unix(),
                            self.stale_active_secs(),
                        )
                        .saturating_sub(1)
                    })
                    .unwrap_or(0);
                self.activity_selected = max;
            }
            2 => {
                let max = self
                    .effective_executions()
                    .map(|execs| {
                        executions::history_selectable_count(
                            &execs,
                            self.history_drill_batch.as_deref(),
                            self.history_group_visible_limit(),
                        )
                        .saturating_sub(1)
                    })
                    .unwrap_or(0);
                self.executions_selected = max;
            }
            1 => {
                self.agents_selected = self.config.load().agents.len().saturating_sub(1);
            }
            3 => {
                self.settings_scroll = self
                    .settings_line_count
                    .get()
                    .saturating_sub(self.settings_viewport_height.get());
            }
            _ => {}
        }
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

/// Set up the terminal, run the event loop, and restore the terminal on exit.
///
/// `handle` is a Tokio runtime handle obtained from the caller's async context
/// and used to drive async store queries from this synchronous blocking thread.
/// `poll_interval_secs` overrides the default 2-second refresh cadence.
pub fn run_tui(
    store: Store,
    config: ConfigHandle,
    config_path: PathBuf,
    handle: Handle,
    poll_interval_secs: u64,
    event_bus: Option<EventBus>,
) -> io::Result<()> {
    let poll_interval = Duration::from_secs(poll_interval_secs);
    let mut terminal = ratatui::init();
    let _ = crossterm::execute!(std::io::stdout(), EnableMouseCapture);
    let mut app = App::new(store, config, config_path, handle, poll_interval, event_bus);
    let result = event_loop(&mut terminal, &mut app);
    let _ = crossterm::execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}

// ── Staleness helper ──────────────────────────────────────────────────────────

/// Check whether a data source needs refreshing.
///
/// Returns `true` if either the data has never been fetched or the most
/// recent fetch/attempt is older than `interval`. When `data` is `None`
/// (first fetch hasn't succeeded), `last_attempt` provides backoff so the
/// TUI doesn't tight-loop on every tick under a degraded DB.
fn is_data_stale(
    data_fetched_at: Option<Instant>,
    last_attempt: Option<Instant>,
    interval: Duration,
) -> bool {
    // If we have data, use its timestamp.
    if let Some(fetched) = data_fetched_at {
        return fetched.elapsed() >= interval;
    }
    // No data yet — respect backoff from last attempt.
    match last_attempt {
        Some(t) => t.elapsed() >= interval,
        None => true, // never attempted — fetch now
    }
}

// ── Event loop ────────────────────────────────────────────────────────────────

fn event_loop(terminal: &mut DefaultTerminal, app: &mut App) -> io::Result<()> {
    loop {
        let interval = app.poll_interval;

        // ── Log viewer polling ────────────────────────────────────────────────
        // If a running execution's log viewer is open, poll on every tick for
        // new file content (TICK_RATE ≈ 250 ms, close to the 200 ms target).
        if let Some(ref mut state) = app.viewing_log {
            if state.is_running() {
                state.poll_log_file();
            }
        }
        // Refresh timeline events for running executions alongside log polling.
        // Extract needed values first to avoid conflicting borrows on `app`.
        let timeline_refresh = app.viewing_log.as_ref().and_then(|v| {
            if v.is_running() {
                Some((
                    v.execution.id.clone(),
                    v.timeline_events.last().map(|e| e.event_index),
                ))
            } else {
                None
            }
        });
        if let Some((exec_id, last_idx)) = timeline_refresh {
            if let Ok(Ok(events)) = app.handle.block_on(async {
                tokio::time::timeout(REFRESH_TIMEOUT, async {
                    app.store
                        .get_execution_events(&exec_id, None, last_idx, None)
                        .await
                })
                .await
            }) {
                if let Some(ref mut state) = app.viewing_log {
                    state.timeline_events.extend(events);
                }
            }
        }

        // ── Conversation view live polling ─────────────────────────────────────
        // When a conversation view is open for an active thread, poll for new
        // messages and executions on each refresh tick.
        let conversation_refresh = app.viewing_conversation.as_ref().and_then(|cv| {
            if cv.is_active() {
                Some((cv.thread_id.clone(), cv.last_message_id))
            } else {
                None
            }
        });
        if let Some((tid, last_msg_id)) = conversation_refresh {
            let store = app.store.clone();
            let tid2 = tid.clone();
            if let Ok(Ok((new_msgs, execs, thread_status))) = app.handle.block_on(async {
                tokio::time::timeout(REFRESH_TIMEOUT, async {
                    let after_id = last_msg_id.unwrap_or(-1);
                    let new_msgs = store.get_messages_since(&tid2, after_id).await?;
                    let execs = store.get_thread_executions(&tid2).await?;
                    let thread_status = store
                        .get_thread(&tid2)
                        .await
                        .ok()
                        .flatten()
                        .map(|t| t.status);
                    Ok::<_, sqlx::Error>((new_msgs, execs, thread_status))
                })
                .await
            }) {
                if let Some(ref mut cv) = app.viewing_conversation {
                    if !new_msgs.is_empty() {
                        cv.last_message_id = new_msgs.iter().map(|m| m.id).max();
                        cv.messages.extend(new_msgs);
                    }
                    cv.executions = execs;
                    if let Some(status) = thread_status {
                        cv.thread_status = status;
                    }
                    if cv.follow_mode {
                        cv.scroll_to_bottom();
                    }
                }
            }
        }

        // ── Background data refreshes ──────────────────────────────────────────
        // Activity data refreshes regardless of active tab so changes are
        // visible immediately when switching views. Paused while the log
        // viewer is open to avoid unnecessary DB queries — an immediate
        // refresh fires when the viewer is closed (see close_log_viewer).
        if app.viewing_log.is_none() && app.viewing_conversation.is_none() {
            let is_stale = is_data_stale(
                app.activity_data.as_ref().map(|d| d.fetched_at),
                app.last_activity_attempt,
                interval,
            );
            if is_stale {
                app.last_activity_attempt = Some(Instant::now());
                app.refresh_activity();
            }
        }

        // Agents and History refresh when their tab is active and stale.
        if app.active_tab == 1 {
            let is_stale = is_data_stale(
                app.agents_data.as_ref().map(|d| d.fetched_at),
                app.last_agents_attempt,
                interval,
            );
            if is_stale {
                app.last_agents_attempt = Some(Instant::now());
                app.refresh_agents();
            }
        }

        if app.active_tab == 2 {
            let is_stale = is_data_stale(
                app.executions_data.as_ref().map(|d| d.fetched_at),
                app.last_executions_attempt,
                interval,
            );
            if is_stale {
                app.last_executions_attempt = Some(Instant::now());
                app.refresh_executions();
            }
        }

        // Schedule run counts refresh when Settings tab is active.
        if app.active_tab == 3 {
            let is_stale = is_data_stale(None, app.last_schedule_attempt, interval);
            if is_stale {
                app.last_schedule_attempt = Some(Instant::now());
                app.refresh_schedules();
            }
        }

        terminal.draw(|frame| {
            if let Some(ref mut conversation) = app.viewing_conversation {
                render_conversation(frame, conversation, frame.area());
            } else if let Some(ref mut viewer) = app.viewing_log {
                render_execution_detail(frame, viewer, frame.area());
            } else {
                frame.render_widget(&*app, frame.area());
                app.render_content_with_frame(frame);
            }
        })?;

        // Poll with a tick timeout so the loop stays responsive to resizes.
        if event::poll(TICK_RATE)? {
            let event = event::read()?;
            if let Event::Key(key) = event {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    app.should_quit = true;
                    continue;
                }
                if app.viewing_conversation.is_some() {
                    handle_conversation_key(app, key.code);
                } else if app.viewing_log.is_some() {
                    handle_log_viewer_key(app, key.code);
                } else if app.confirm_quit {
                    handle_quit_confirm_key(app, key.code);
                } else if app.show_help {
                    handle_help_key(app, key.code);
                } else {
                    handle_list_key(app, key.code);
                }
            } else if let Event::Mouse(mouse) = event {
                if app.viewing_conversation.is_some() {
                    handle_conversation_mouse(app, mouse.kind);
                } else if app.viewing_log.is_some() {
                    handle_log_viewer_mouse(app, mouse.kind, mouse.column, mouse.row);
                } else if app.confirm_quit {
                    // No-op: no mouse interaction in quit confirmation dialog.
                } else if app.show_help {
                    // No-op: no mouse interaction in help overlay.
                } else {
                    handle_list_mouse(app, mouse.kind, mouse.column, mouse.row);
                }
            }
        }

        // Drain pending orchestrator events and force an immediate refresh
        // if any state-changing event was received. This gives push-based
        // updates when the worker runs in the same process (or in tests).
        //
        // NOTE: All event types collapse to a single full SQLite refresh.
        // When high-frequency variants (e.g., ExecutionProgress from ORCH-EVO-1)
        // are added, per-variant filtering will be needed to avoid excessive
        // SQLite pressure.
        if let Some(ref mut rx) = app.event_rx {
            let mut got_event = false;
            loop {
                match rx.try_recv() {
                    Ok(event) => {
                        got_event = true;
                        match &event {
                            OrchestratorEvent::ExecutionProgress {
                                execution_id,
                                summary,
                                ..
                            } => {
                                // Only update if the summary actually changed
                                let current = app.progress_summaries.get(execution_id);
                                if current.map(|s| s.as_str()) != Some(summary.as_str()) {
                                    // Debounce: only update display if enough time passed
                                    let debounce_ok = app
                                        .progress_last_changed
                                        .get(execution_id)
                                        .map(|t| t.elapsed() >= SUMMARY_DEBOUNCE)
                                        .unwrap_or(true);
                                    if debounce_ok {
                                        app.progress_summaries
                                            .insert(execution_id.clone(), summary.clone());
                                        app.progress_last_changed
                                            .insert(execution_id.clone(), Instant::now());
                                    }
                                }
                            }
                            OrchestratorEvent::ExecutionCompleted { execution_id, .. } => {
                                app.progress_summaries.remove(execution_id);
                                app.progress_last_changed.remove(execution_id);
                            }
                            OrchestratorEvent::MergeStarted { .. }
                            | OrchestratorEvent::MergeCompleted { .. } => {
                                // Merge events handled by invalidate_data() below.
                            }
                            _ => {}
                        }
                    }
                    Err(broadcast::error::TryRecvError::Empty) => break,
                    Err(broadcast::error::TryRecvError::Lagged(n)) => {
                        tracing::debug!(
                            skipped = n,
                            "dashboard event receiver lagged; forcing refresh"
                        );
                        got_event = true;
                        continue;
                    }
                    Err(broadcast::error::TryRecvError::Closed) => {
                        app.event_rx = None;
                        break;
                    }
                }
            }
            if got_event {
                app.invalidate_data();
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
        KeyCode::Tab => {
            if let Some(ref mut viewer) = app.viewing_log {
                viewer.next_tab();
            }
        }
        KeyCode::BackTab => {
            if let Some(ref mut viewer) = app.viewing_log {
                viewer.prev_tab();
            }
        }
        KeyCode::Char('1') => {
            if let Some(ref mut viewer) = app.viewing_log {
                viewer.set_tab(LogTab::Input);
            }
        }
        KeyCode::Char('2') => {
            if let Some(ref mut viewer) = app.viewing_log {
                viewer.set_tab(LogTab::Output);
            }
        }
        KeyCode::Char('3') => {
            if let Some(ref mut viewer) = app.viewing_log {
                viewer.set_tab(LogTab::Timeline);
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
        KeyCode::Char('J') => {
            if let Some(ref mut viewer) = app.viewing_log {
                viewer.toggle_pretty_json();
            }
        }
        _ => {}
    }
}

/// Handle a key event when the conversation view is open.
fn handle_conversation_key(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Esc => {
            app.close_conversation();
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if let Some(ref mut cv) = app.viewing_conversation {
                cv.scroll_down(1);
            }
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if let Some(ref mut cv) = app.viewing_conversation {
                cv.scroll_up(1);
            }
        }
        KeyCode::Char('g') => {
            if let Some(ref mut cv) = app.viewing_conversation {
                cv.scroll_to_top();
            }
        }
        KeyCode::Char('G') => {
            if let Some(ref mut cv) = app.viewing_conversation {
                cv.scroll_to_bottom();
            }
        }
        KeyCode::Char('f') => {
            if let Some(ref mut cv) = app.viewing_conversation {
                cv.toggle_follow();
            }
        }
        KeyCode::PageUp => {
            if let Some(ref mut cv) = app.viewing_conversation {
                let page = cv.visible_rows.max(1);
                cv.scroll_up(page);
            }
        }
        KeyCode::PageDown => {
            if let Some(ref mut cv) = app.viewing_conversation {
                let page = cv.visible_rows.max(1);
                cv.scroll_down(page);
            }
        }
        _ => {}
    }
}

/// Handle key events while quit confirmation dialog is open.
fn handle_quit_confirm_key(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Char('y') | KeyCode::Char('Y') => app.should_quit = true,
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => app.confirm_quit = false,
        _ => {}
    }
}

/// Handle key events while help overlay is open.
fn handle_help_key(app: &mut App, code: KeyCode) {
    let max_scroll = HELP_LINE_COUNT.saturating_sub(app.help_viewport_height.get());
    match code {
        KeyCode::Esc | KeyCode::Enter | KeyCode::Char('?') => app.toggle_help(),
        KeyCode::Up | KeyCode::Char('k') => {
            app.help_scroll = app.help_scroll.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.help_scroll = app.help_scroll.saturating_add(1).min(max_scroll);
        }
        _ => {}
    }
}

/// Handle a key event when the normal list/tab view is active.
fn handle_list_key(app: &mut App, code: KeyCode) {
    app.show_hint_banner = false;
    match code {
        KeyCode::Char('q') => app.confirm_quit = true,
        KeyCode::Char('?') => app.toggle_help(),
        // Manual refresh.
        KeyCode::Char('r') => match app.active_tab {
            0 => app.refresh_activity(),
            1 => app.refresh_agents(),
            2 => app.refresh_executions(),
            3 => app.refresh_schedules(),
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
        KeyCode::Char('g') => app.select_first_row(),
        KeyCode::Char('G') => app.select_last_row(),
        KeyCode::Esc => app.clear_drill_filters(),
        KeyCode::Char('x') => app.clear_drill_filters(),
        // Open conversation view for selected thread.
        KeyCode::Char('c') => {
            app.open_conversation();
        }
        // Enter: drill batch row in Ops/History, agent drill in Agents, otherwise open execution detail.
        KeyCode::Enter => {
            if app.active_tab == 0
                && matches!(
                    app.selected_activity_target(),
                    Some(OpsSelectable::Batch(_))
                )
            {
                app.enter_batch_drill();
            } else if app.active_tab == 0 {
                if let Some(OpsSelectable::MergeOp(op_id)) = app.selected_activity_target() {
                    app.open_merge_op_thread(&op_id);
                } else {
                    app.open_log_viewer();
                }
            } else if app.active_tab == 1 {
                // Agents tab: drill into History filtered by agent alias.
                app.enter_agent_drill();
            } else if app.active_tab == 2
                && matches!(
                    app.selected_history_target(),
                    Some(HistorySelectable::Batch(_))
                )
            {
                if let Some(HistorySelectable::Batch(batch_id)) = app.selected_history_target() {
                    app.history_drill_batch = Some(batch_id);
                    app.executions_selected = 0;
                    app.refresh_executions();
                }
            } else {
                app.open_log_viewer();
            }
        }
        _ => {}
    }
}

// ── Mouse handlers ───────────────────────────────────────────────────────────

/// Scroll step for mouse wheel events in overlay views (log viewer, conversation).
const MOUSE_SCROLL_STEP: usize = 3;

/// Handle mouse events when the conversation view is open.
fn handle_conversation_mouse(app: &mut App, kind: MouseEventKind) {
    match kind {
        MouseEventKind::ScrollUp => {
            if let Some(ref mut cv) = app.viewing_conversation {
                cv.scroll_up(MOUSE_SCROLL_STEP);
            }
        }
        MouseEventKind::ScrollDown => {
            if let Some(ref mut cv) = app.viewing_conversation {
                cv.scroll_down(MOUSE_SCROLL_STEP);
            }
        }
        _ => {}
    }
}

/// Handle mouse events when the log viewer is open.
fn handle_log_viewer_mouse(app: &mut App, kind: MouseEventKind, col: u16, row: u16) {
    match kind {
        MouseEventKind::Down(MouseButton::Left) => {
            if let Some(ref mut viewer) = app.viewing_log {
                if let Some(tab_rect) = viewer.tab_bar_rect {
                    let event_count = viewer.timeline_events.len();
                    if let Some(idx) = detect_log_tab_click(col, row, tab_rect, event_count) {
                        viewer.set_tab(LogTab::from_index(idx));
                    }
                }
            }
        }
        MouseEventKind::ScrollUp => {
            if let Some(ref mut viewer) = app.viewing_log {
                viewer.scroll_up(MOUSE_SCROLL_STEP);
            }
        }
        MouseEventKind::ScrollDown => {
            if let Some(ref mut viewer) = app.viewing_log {
                viewer.scroll_down(MOUSE_SCROLL_STEP);
            }
        }
        _ => {}
    }
}

/// Detect which log viewer tab label was clicked based on cumulative x-offsets.
///
/// The tab bar layout is: `"╶ "` prefix (2 chars), then tab labels separated
/// by 4-space gaps. The "Timeline" label is dynamic: `"Timeline (N)"`.
fn detect_log_tab_click(col: u16, row: u16, tab_rect: Rect, event_count: usize) -> Option<usize> {
    if row != tab_rect.y {
        return None;
    }
    if col < tab_rect.x || col >= tab_rect.x + tab_rect.width {
        return None;
    }
    let rel_x = (col - tab_rect.x) as usize;

    let prefix_len = 2; // "╶ "
    let labels: [String; 3] = [
        "Input".to_string(),
        format!("Timeline ({})", event_count),
        "Output".to_string(),
    ];
    let gap = 4; // 4 spaces between labels

    let mut offset = prefix_len;
    for (i, label) in labels.iter().enumerate() {
        let label_width = label.len();
        if rel_x >= offset && rel_x < offset + label_width {
            return Some(i);
        }
        offset += label_width;
        if i < labels.len() - 1 {
            offset += gap;
        }
    }
    None
}

/// Handle mouse events when the normal list/tab view is active.
fn handle_list_mouse(app: &mut App, kind: MouseEventKind, col: u16, row: u16) {
    match kind {
        MouseEventKind::Down(MouseButton::Left) => {
            app.show_hint_banner = false;

            // ── Tab bar click detection ──────────────────────────────────
            // Recompute the main layout to find the tab bar rect.
            let (w, h) = crossterm::terminal::size().unwrap_or((80, 24));
            let terminal_area = Rect::new(0, 0, w, h);
            let [tab_bar, _content, _status] = Layout::vertical(MAIN_LAYOUT).areas(terminal_area);

            if row >= tab_bar.y && row < tab_bar.y + tab_bar.height {
                if let Some(tab_idx) = detect_tab_click(col, tab_bar) {
                    app.active_tab = tab_idx;
                    return;
                }
            }

            // ── List area clicks ─────────────────────────────────────────
            match app.active_tab {
                0 => handle_ops_click(app, col, row),
                1 => handle_agents_click(app, row),
                2 => handle_history_click(app, col, row),
                _ => {} // Settings tab: no clickable items
            }
        }
        MouseEventKind::ScrollUp => app.select_prev_row(),
        MouseEventKind::ScrollDown => app.select_next_row(),
        _ => {}
    }
}

/// Detect which tab label was clicked based on cumulative x-offsets.
///
/// Tab bar is a bordered block; inner area starts at `x+1`.
/// Each tab label occupies `label.len() + 2` chars (1 space padding each side),
/// dividers are 3 chars (` │ `).
fn detect_tab_click(col: u16, tab_bar: Rect) -> Option<usize> {
    let inner = tab_bar.inner(ratatui::layout::Margin::new(1, 1));
    if col < inner.x || col >= inner.x + inner.width {
        return None;
    }
    let rel_x = (col - inner.x) as usize;
    let mut offset = 0;
    for (i, label) in TABS.iter().enumerate() {
        let tab_width = label.len() + 2; // 1 space padding on each side
        if rel_x < offset + tab_width {
            return Some(i);
        }
        offset += tab_width;
        // Divider ` │ ` is 3 chars (except after the last tab).
        if i < TABS.len() - 1 {
            offset += 3;
        }
    }
    None
}

/// Handle a left-click on the Ops tab list area.
fn handle_ops_click(app: &mut App, _col: u16, row: u16) {
    let cache = app.ops_click_cache.borrow();
    if cache.slot_geometry.is_empty() {
        return;
    }
    let rect = cache.list_rect;
    if row < rect.y || row >= rect.y + rect.height {
        return;
    }
    let display_row = (row - rect.y) as usize + cache.scroll;
    // Linear search through slot geometry to find which selectable was clicked.
    let mut clicked_slot = None;
    for (slot, &(offset, height)) in cache.slot_geometry.iter().enumerate() {
        if display_row >= offset && display_row < offset + height {
            clicked_slot = Some(slot);
            break;
        }
    }
    drop(cache);

    if let Some(slot) = clicked_slot {
        if app.activity_selected == slot {
            // Click on already-selected item → Enter behavior.
            simulate_enter(app);
        } else {
            app.activity_selected = slot;
        }
    }
}

/// Handle a left-click on the Agents tab.
fn handle_agents_click(app: &mut App, row: u16) {
    let cache = app.agents_click_cache.borrow();
    if cache.card_geometry.is_empty() {
        return;
    }
    let rect = cache.list_rect;
    if row < rect.y || row >= rect.y + rect.height {
        return;
    }
    let display_row = (row - rect.y) as usize;
    let mut clicked_idx = None;
    for (idx, &(offset, height)) in cache.card_geometry.iter().enumerate() {
        if display_row >= offset && display_row < offset + height {
            clicked_idx = Some(idx);
            break;
        }
    }
    drop(cache);

    if let Some(idx) = clicked_idx {
        app.agents_selected = idx;
    }
}

/// Handle a left-click on the History tab table.
fn handle_history_click(app: &mut App, _col: u16, row: u16) {
    let cache = app.history_click_cache.borrow();
    if cache.selectable_to_row.is_empty() {
        return;
    }
    let rect = cache.table_rect;
    if row < rect.y || row >= rect.y + rect.height {
        return;
    }
    // Compute which table row was clicked: account for header rows and scroll.
    let rel_y = (row - rect.y) as usize;
    if rel_y < cache.header_rows {
        return; // Clicked on the header.
    }
    let table_row = rel_y - cache.header_rows + cache.scroll_offset;

    // Find the selectable slot that maps to this table row.
    let mut clicked_slot = None;
    for (slot, &mapped_row) in cache.selectable_to_row.iter().enumerate() {
        if mapped_row == table_row {
            clicked_slot = Some(slot);
            break;
        }
    }
    drop(cache);

    if let Some(slot) = clicked_slot {
        if app.executions_selected == slot {
            // Click on already-selected item → Enter behavior.
            simulate_enter(app);
        } else {
            app.executions_selected = slot;
        }
    }
}

/// Simulate the Enter key action for click-on-selected behavior.
fn simulate_enter(app: &mut App) {
    if app.active_tab == 0
        && matches!(
            app.selected_activity_target(),
            Some(OpsSelectable::Batch(_))
        )
    {
        app.enter_batch_drill();
    } else if app.active_tab == 0 {
        if let Some(OpsSelectable::MergeOp(op_id)) = app.selected_activity_target() {
            app.open_merge_op_thread(&op_id);
        } else {
            app.open_log_viewer();
        }
    } else if app.active_tab == 2 {
        if let Some(HistorySelectable::Batch(batch_id)) = app.selected_history_target() {
            app.history_drill_batch = Some(batch_id);
            app.executions_selected = 0;
            app.refresh_executions();
        } else {
            app.open_log_viewer();
        }
    } else {
        app.open_log_viewer();
    }
}

// ── Widget impl ───────────────────────────────────────────────────────────────

impl Widget for &App {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // Invariant: the draw closure only calls Widget::render when viewing_log is None.
        // The log viewer is rendered directly via Frame in the draw closure.
        debug_assert!(
            self.viewing_log.is_none() && self.viewing_conversation.is_none(),
            "Widget::render called while an overlay is active; draw closure contract broken"
        );

        let [tab_bar, content, status_bar] = Layout::vertical(MAIN_LAYOUT).areas(area);

        self.render_tab_bar_widget(tab_bar, buf);
        self.render_content_widget(content, buf);
        self.render_status_bar_widget(status_bar, buf);

        if self.show_help {
            self.render_help_overlay_widget(area, buf);
        }
        if self.confirm_quit {
            self.render_quit_confirm_widget(area, buf);
        }
    }
}

/// Main layout constraints shared between `Widget for &App` and `render_content_with_frame`.
const MAIN_LAYOUT: [Constraint; 3] = [
    Constraint::Length(3),
    Constraint::Fill(1),
    Constraint::Length(1),
];

// ── Rendering methods on App ──────────────────────────────────────────────────

impl App {
    /// Render stateful content that requires `Frame` (external module views).
    fn render_content_with_frame(&self, frame: &mut Frame) {
        // Layout must match MAIN_LAYOUT used in impl Widget for &App.
        let [_, content, _] = Layout::vertical(MAIN_LAYOUT).areas(frame.area());

        match self.active_tab {
            0 => render_activity(frame, self, content),
            1 => agents::render_agents_tab(frame, self, content),
            2 => executions::render_executions(frame, self, content),
            3 => self.render_settings_with_frame(frame, content),
            _ => {} // Fallback handled by Widget impl
        }
    }

    fn render_tab_bar_widget(&self, area: Rect, buf: &mut Buffer) {
        Tabs::new(TABS.iter().copied())
            .block(
                Block::bordered()
                    .border_set(border::ONE_EIGHTH_WIDE)
                    .border_style(Style::new().fg(theme::BORDER_DIM))
                    .title_top(Line::from(" compas ".fg(theme::TEXT_BRIGHT).bold()).left_aligned())
                    .style(Style::new().bg(theme::BG_PRIMARY)),
            )
            .select(self.active_tab)
            .style(Style::new().fg(theme::TEXT_MUTED).bg(theme::BG_PRIMARY))
            .highlight_style(Style::new().fg(theme::ACCENT).bold())
            .divider(Span::styled(" │ ", Style::new().fg(theme::BORDER_DIM)))
            .render(area, buf);
    }

    fn render_content_widget(&self, area: Rect, buf: &mut Buffer) {
        match self.active_tab {
            0..=3 => {} // Stateful tabs rendered by render_content_with_frame
            _ => {
                let tab_name = TABS[self.active_tab];
                let body = format!("  {} — coming soon", tab_name);
                Paragraph::new(Line::from(body.fg(theme::TEXT_DIM)))
                    .block(theme::panel(tab_name))
                    .render(area, buf);
            }
        }
    }

    fn render_settings_with_frame(&self, frame: &mut Frame, area: Rect) {
        let block = theme::panel("Settings");
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let label = |text: &str| -> Span { format!("{:<14} ", text).fg(theme::TEXT_MUTED).bold() };
        let value = |s: String| -> Span { s.fg(theme::TEXT_NORMAL) };

        let poll_secs = self.poll_interval.as_secs();
        let cfg = self.config.load();

        let mut lines = vec![
            Line::from(vec![
                Span::raw("  "),
                label("Config:"),
                value(self.config_path.display().to_string()),
            ]),
            Line::from(vec![
                Span::raw("  "),
                label("DB:"),
                value(cfg.db_path().display().to_string()),
            ]),
            Line::from(vec![
                Span::raw("  "),
                label("Poll interval:"),
                value(format!("{}s", poll_secs)),
            ]),
            Line::from(vec![
                Span::raw("  "),
                label("Agents:"),
                value(format!("{}", cfg.agents.len())),
            ]),
            Line::from(vec![
                Span::raw("  "),
                label("Log dir:"),
                value(format!("{}", self.log_dir.display())),
            ]),
        ];

        // ── Schedules section ────────────────────────────────────────────────
        let schedules = cfg.schedules.as_deref().unwrap_or(&[]);
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::raw("  "),
            label("Schedules:"),
            value(format!("{}", schedules.len())),
        ]));

        if schedules.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(
                "  No schedules configured.".fg(theme::TEXT_MUTED),
            ));
        } else {
            lines.push(Line::from(""));
            // Header row — fits within 80-col inner width.
            lines.push(Line::from(vec![
                Span::raw("  "),
                format!(
                    "{:<14} {:<10} {:<14} {:<10} {:<8} {}",
                    "Name", "Agent", "Cron", "Next", "Runs", "Status"
                )
                .fg(theme::TEXT_MUTED)
                .bold(),
            ]));

            let now = chrono::Utc::now();
            let empty_counts = std::collections::HashMap::new();
            let run_counts = self.schedule_run_counts.as_ref().unwrap_or(&empty_counts);

            for sched in schedules {
                let (_, run_count) = run_counts.get(&sched.name).copied().unwrap_or((0, 0));

                // Compute next fire time from cron expression.
                let next_fire = if sched.enabled {
                    sched
                        .cron
                        .parse::<croner::Cron>()
                        .ok()
                        .and_then(|cron| cron.find_next_occurrence(&now, false).ok())
                        .map(|next| {
                            let diff = next.signed_duration_since(now);
                            let secs = diff.num_seconds().max(0);
                            crate::dashboard::views::format_duration_secs(secs)
                        })
                        .unwrap_or_else(|| "—".to_string())
                } else {
                    "—".to_string()
                };

                let runs_label = crate::dashboard::views::truncate(
                    &format!("{}/{}", run_count, sched.max_runs),
                    7,
                );
                let status_label = if sched.enabled { "enabled" } else { "disabled" };
                let status_color = if sched.enabled {
                    theme::SUCCESS
                } else {
                    theme::TEXT_DIM
                };
                let name_color = if sched.enabled {
                    theme::TEXT_BRIGHT
                } else {
                    theme::TEXT_DIM
                };

                lines.push(Line::from(vec![
                    Span::raw("  "),
                    format!("{:<14}", crate::dashboard::views::truncate(&sched.name, 13))
                        .fg(name_color),
                    Span::raw(" "),
                    format!("{:<10}", crate::dashboard::views::truncate(&sched.agent, 9))
                        .fg(theme::TEXT_NORMAL),
                    Span::raw(" "),
                    format!("{:<14}", crate::dashboard::views::truncate(&sched.cron, 13))
                        .fg(theme::TEXT_MUTED),
                    Span::raw(" "),
                    format!("{:<10}", crate::dashboard::views::truncate(&next_fire, 9))
                        .fg(theme::ACCENT),
                    Span::raw(" "),
                    format!("{:<8}", runs_label).fg(theme::TEXT_NORMAL),
                    Span::raw(" "),
                    status_label.to_string().fg(status_color),
                ]));
            }
        }

        // Track total line count for scroll bounds (interior mutability to
        // allow updating from an &self method — same pattern as click caches).
        self.settings_line_count.set(lines.len() as u16);
        self.settings_viewport_height.set(inner.height);

        let scroll = self.settings_scroll;
        let paragraph = Paragraph::new(lines)
            .style(Style::new().bg(theme::BG_PANEL).fg(theme::TEXT_NORMAL))
            .scroll((scroll, 0));
        frame.render_widget(paragraph, inner);
    }

    fn render_status_bar_widget(&self, area: Rect, buf: &mut Buffer) {
        let key = |s: &'static str| s.fg(theme::ACCENT).bold();
        let sep = || Span::raw("  ");
        let mut spans: Vec<Span> = vec![
            Span::raw(" "),
            key("q"),
            Span::raw(": quit"),
            sep(),
            key("?"),
            Span::raw(": help"),
            sep(),
            key("Tab"),
            Span::raw(": next"),
            sep(),
            key("↑/↓"),
            Span::raw(": select"),
            Span::raw(" "),
        ];

        match self.active_tab {
            0 => {
                spans.push(sep());
                spans.push(key("Enter"));
                spans.push(Span::raw(": open/drill"));
                spans.push(sep());
                spans.push(key("c"));
                spans.push(Span::raw(": conversation"));
                spans.push(sep());
                spans.push(key("Esc"));
                spans.push(Span::raw(": back"));
            }
            2 => {
                spans.push(sep());
                spans.push(key("Enter"));
                spans.push(Span::raw(": drill/open"));
                spans.push(sep());
                spans.push(key("Esc"));
                spans.push(Span::raw(": back batch"));
            }
            1 => {
                spans.push(sep());
                spans.push(key("j/k"));
                spans.push(Span::raw(": select agent"));
            }
            3 => {
                spans.push(sep());
                spans.push(key("j/k"));
                spans.push(Span::raw(": scroll"));
                spans.push(sep());
                spans.push(key("r"));
                spans.push(Span::raw(": refresh"));
            }
            _ => {}
        }

        {
            let errors: Vec<&str> = [
                self.activity_refresh_error.as_deref(),
                self.agents_refresh_error.as_deref(),
                self.executions_refresh_error.as_deref(),
            ]
            .into_iter()
            .flatten()
            .collect();
            if !errors.is_empty() {
                let mut msg = errors.join("; ");
                if msg.chars().count() > 48 {
                    msg = format!("{}…", msg.chars().take(47).collect::<String>());
                }
                spans.push(sep());
                spans.push("⚠ ".fg(theme::WARNING));
                spans.push(msg.fg(theme::WARNING));
            }
        }

        if self.show_hint_banner {
            spans.push(sep());
            spans.push("Tip:".fg(theme::ACCENT));
            spans.push(" press ? for keymap".fg(theme::TEXT_MUTED));
        }

        Paragraph::new(Line::from(spans))
            .style(Style::new().bg(theme::BG_STATUS_BAR).fg(theme::TEXT_MUTED))
            .render(area, buf);
    }

    fn render_quit_confirm_widget(&self, area: Rect, buf: &mut Buffer) {
        let modal = centered_rect(50, 5, area);
        let block = Block::bordered()
            .border_style(Style::new().fg(theme::BORDER_FOCUS))
            .style(Style::new().bg(theme::BG_PANEL).fg(theme::TEXT_NORMAL))
            .title(" Quit ");
        let inner = block.inner(modal);
        Clear.render(modal, buf);
        block.render(modal, buf);

        let line = Line::from(vec![
            Span::raw("Are you sure you want to quit? "),
            Span::styled("y", Style::new().fg(theme::ACCENT).bold()),
            Span::raw("/"),
            Span::styled("n", Style::new().fg(theme::ACCENT).bold()),
        ]);

        Paragraph::new(line)
            .style(Style::new().bg(theme::BG_PANEL).fg(theme::TEXT_NORMAL))
            .render(inner, buf);
    }

    fn render_help_overlay_widget(&self, area: Rect, buf: &mut Buffer) {
        let modal = centered_rect(72, 23, area);
        let mut block = Block::bordered()
            .border_style(Style::new().fg(theme::BORDER_FOCUS))
            .style(Style::new().bg(theme::BG_PANEL).fg(theme::TEXT_NORMAL))
            .title(" Help ");
        let inner = block.inner(modal);
        self.help_viewport_height.set(inner.height);

        if inner.height < HELP_LINE_COUNT {
            block = block.title_bottom(Line::from(" ↑/↓ scroll ").centered());
        }

        Clear.render(modal, buf);
        block.render(modal, buf);

        let lines = vec![
            Line::from(" Global"),
            Line::from(
                "   q confirm quit / Ctrl+C quit   ? toggle help   Tab/Shift+Tab switch tabs",
            ),
            Line::from("   1-4 jump tabs   r refresh"),
            Line::from(" Navigation"),
            Line::from("   ↑/↓ or j/k move   g/G first/last"),
            Line::from(" Ops"),
            Line::from("   Enter open log or drill batch   c conversation view"),
            Line::from("   Esc/x back from batch drill"),
            Line::from(" Agents"),
            Line::from("   ↑/↓ select agent   Enter view executions"),
            Line::from(" History"),
            Line::from("   Enter drill batch/open execution   Esc back from filter"),
            Line::from(" Settings"),
            Line::from("   read-only view   r refresh"),
            Line::from(" Execution Detail"),
            Line::from("   ↑/↓ or j/k section   Enter collapse/expand   g/G top/bottom"),
            Line::from("   Esc back   f follow   J view mode"),
            Line::from(" Conversation View"),
            Line::from("   j/k or ↑/↓ scroll   g/G top/bottom   f follow   Esc back"),
            Line::from(""),
            Line::from(" Press Esc, Enter, or ? to close this panel."),
        ];

        debug_assert_eq!(
            lines.len() as u16,
            HELP_LINE_COUNT,
            "HELP_LINE_COUNT out of sync with help lines vec"
        );

        Paragraph::new(lines)
            .style(Style::new().bg(theme::BG_PANEL).fg(theme::TEXT_NORMAL))
            .scroll((self.help_scroll, 0))
            .render(inner, buf);
    }
}

fn centered_rect(percent_x: u16, height_rows: u16, r: Rect) -> Rect {
    let height = height_rows.min(r.height);
    let [area] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(r);
    let [area] = Layout::horizontal([Constraint::Percentage(percent_x)])
        .flex(Flex::Center)
        .areas(area);
    area
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod mouse_tests {
    use super::*;

    // ── detect_tab_click ─────────────────────────────────────────────────

    fn make_tab_bar(x: u16, y: u16, w: u16, h: u16) -> Rect {
        Rect::new(x, y, w, h)
    }

    #[test]
    fn test_detect_tab_click_first_tab() {
        // Tab bar bordered block: inner starts at x=1.
        // Tab labels: "Ops" (5 chars padded), "Agents" (8), "History" (9), "Settings" (10)
        // With padding: " Ops " = 5, " Agents " = 8, " History " = 9, " Settings " = 10
        // Dividers: " │ " = 3 chars
        let bar = make_tab_bar(0, 0, 80, 3);
        // Inner area: x=1, y=1, w=78, h=1
        // Tab " Ops " occupies cols 1..6 (inner-relative 0..5)
        assert_eq!(detect_tab_click(1, bar), Some(0)); // first char of " Ops "
        assert_eq!(detect_tab_click(5, bar), Some(0)); // last char of " Ops "
    }

    #[test]
    fn test_detect_tab_click_second_tab() {
        let bar = make_tab_bar(0, 0, 80, 3);
        // After "Ops" (5) + divider (3) = offset 8 → "Agents" tab starts at inner-rel 8
        // Inner starts at x=1, so absolute col = 1 + 8 = 9
        assert_eq!(detect_tab_click(9, bar), Some(1));
    }

    #[test]
    fn test_detect_tab_click_third_tab() {
        let bar = make_tab_bar(0, 0, 80, 3);
        // After "Ops" (5) + div (3) + "Agents" (8) + div (3) = 19
        // Absolute col = 1 + 19 = 20
        assert_eq!(detect_tab_click(20, bar), Some(2));
    }

    #[test]
    fn test_detect_tab_click_fourth_tab() {
        let bar = make_tab_bar(0, 0, 80, 3);
        // After "Ops" (5) + div (3) + "Agents" (8) + div (3) + "History" (9) + div (3) = 31
        // Absolute col = 1 + 31 = 32
        assert_eq!(detect_tab_click(32, bar), Some(3));
    }

    #[test]
    fn test_detect_tab_click_on_divider() {
        let bar = make_tab_bar(0, 0, 80, 3);
        // Divider between Ops and Agents: inner-relative 5, 6, 7 → absolute 6, 7, 8.
        // Clicks in the divider region resolve to the next tab (acceptable UX —
        // the gap is small and clicking between tabs selects the nearest one).
        assert_eq!(detect_tab_click(6, bar), Some(1));
        assert_eq!(detect_tab_click(7, bar), Some(1));
        assert_eq!(detect_tab_click(8, bar), Some(1));
    }

    #[test]
    fn test_detect_tab_click_outside_right() {
        let bar = make_tab_bar(0, 0, 80, 3);
        // After all tabs: 5 + 3 + 8 + 3 + 9 + 3 + 10 = 41 → inner-relative 41+
        // Absolute col = 1 + 41 = 42
        assert_eq!(detect_tab_click(42, bar), None);
    }

    #[test]
    fn test_detect_tab_click_on_border() {
        let bar = make_tab_bar(0, 0, 80, 3);
        // Col 0 is the left border — outside inner area.
        assert_eq!(detect_tab_click(0, bar), None);
    }

    #[test]
    fn test_detect_tab_click_offset_origin() {
        // Tab bar not at (0,0) — e.g., nested layout.
        let bar = make_tab_bar(10, 5, 60, 3);
        // Inner starts at x=11. First tab at absolute col 11.
        assert_eq!(detect_tab_click(11, bar), Some(0));
        assert_eq!(detect_tab_click(10, bar), None); // border
    }

    // ── detect_log_tab_click ──────────────────────────────────────────────
    //
    // Log tab bar layout (no border, 1 row):
    //   "╶ " (2 chars) + "Input" (5) + "    " (4) + "Timeline (N)" (dynamic) + "    " (4) + "Output" (6)
    //
    // With event_count=3: "Timeline (3)" = 12 chars
    // Offsets: Input starts at 2, ends at 6 (exclusive 7)
    //          gap 7..11, Timeline starts at 11, ends at 22 (exclusive 23)
    //          gap 23..27, Output starts at 27, ends at 32 (exclusive 33)

    fn make_log_tab_rect(x: u16, y: u16, w: u16) -> Rect {
        Rect::new(x, y, w, 1)
    }

    #[test]
    fn test_detect_log_tab_click_input_first_char() {
        let r = make_log_tab_rect(0, 0, 80);
        // "Input" starts at offset 2 (after "╶ ")
        assert_eq!(detect_log_tab_click(2, 0, r, 3), Some(0));
    }

    #[test]
    fn test_detect_log_tab_click_input_last_char() {
        let r = make_log_tab_rect(0, 0, 80);
        // "Input" is 5 chars: offsets 2..6 inclusive
        assert_eq!(detect_log_tab_click(6, 0, r, 3), Some(0));
    }

    #[test]
    fn test_detect_log_tab_click_timeline() {
        let r = make_log_tab_rect(0, 0, 80);
        // event_count=3 → "Timeline (3)" = 12 chars, starts at 2+5+4=11
        assert_eq!(detect_log_tab_click(11, 0, r, 3), Some(1));
        assert_eq!(detect_log_tab_click(22, 0, r, 3), Some(1)); // last char
    }

    #[test]
    fn test_detect_log_tab_click_output() {
        let r = make_log_tab_rect(0, 0, 80);
        // "Output" starts at 2+5+4+12+4=27 (with event_count=3)
        assert_eq!(detect_log_tab_click(27, 0, r, 3), Some(2));
        assert_eq!(detect_log_tab_click(32, 0, r, 3), Some(2)); // last char
    }

    #[test]
    fn test_detect_log_tab_click_gap_between_tabs() {
        let r = make_log_tab_rect(0, 0, 80);
        // Gap between Input (ends at 7) and Timeline (starts at 11): offsets 7..10
        assert_eq!(detect_log_tab_click(7, 0, r, 3), None);
        assert_eq!(detect_log_tab_click(10, 0, r, 3), None);
    }

    #[test]
    fn test_detect_log_tab_click_wrong_row() {
        let r = make_log_tab_rect(0, 5, 80);
        // Click on correct col but wrong row
        assert_eq!(detect_log_tab_click(2, 4, r, 3), None); // above
        assert_eq!(detect_log_tab_click(2, 6, r, 3), None); // below
    }

    #[test]
    fn test_detect_log_tab_click_col_before_rect() {
        let r = make_log_tab_rect(5, 0, 80);
        // col < tab_rect.x
        assert_eq!(detect_log_tab_click(4, 0, r, 3), None);
    }

    #[test]
    fn test_detect_log_tab_click_col_past_rect() {
        let r = make_log_tab_rect(0, 0, 30);
        // col >= tab_rect.x + tab_rect.width
        assert_eq!(detect_log_tab_click(30, 0, r, 3), None);
    }

    #[test]
    fn test_detect_log_tab_click_nonzero_origin() {
        let r = make_log_tab_rect(5, 10, 80);
        // "Input" starts at tab_rect.x + 2 = 7
        assert_eq!(detect_log_tab_click(7, 10, r, 3), Some(0));
        assert_eq!(detect_log_tab_click(11, 10, r, 3), Some(0)); // last char of Input
                                                                 // "Timeline (3)" starts at 5 + 11 = 16
        assert_eq!(detect_log_tab_click(16, 10, r, 3), Some(1));
        // "Output" starts at 5 + 27 = 32
        assert_eq!(detect_log_tab_click(32, 10, r, 3), Some(2));
    }

    // ── Slot geometry lookup (Ops click mapping) ─────────────────────────

    #[test]
    fn test_ops_slot_geometry_basic_lookup() {
        // Simulate 3 selectable slots at display rows 2, 3, 5 with height 1 each.
        let geometry = [(2, 1), (3, 1), (5, 1)];
        let list_rect = Rect::new(0, 10, 80, 20);
        let scroll = 0;

        // Click on row 12 → display_row = (12 - 10) + 0 = 2 → slot 0
        let display_row = (12u16 - list_rect.y) as usize + scroll;
        let slot = geometry
            .iter()
            .position(|(offset, height)| display_row >= *offset && display_row < offset + height);
        assert_eq!(slot, Some(0));

        // Click on row 13 → display_row = 3 → slot 1
        let display_row = (13u16 - list_rect.y) as usize + scroll;
        let slot = geometry
            .iter()
            .position(|(offset, height)| display_row >= *offset && display_row < offset + height);
        assert_eq!(slot, Some(1));

        // Click on row 14 → display_row = 4 → no slot (gap between 1 and 2)
        let display_row = (14u16 - list_rect.y) as usize + scroll;
        let slot = geometry
            .iter()
            .position(|(offset, height)| display_row >= *offset && display_row < offset + height);
        assert_eq!(slot, None);
    }

    #[test]
    fn test_ops_slot_geometry_with_scroll() {
        // Slots at rows 0, 2, 4 (height 2 each — selected items expand).
        let geometry = [(0, 2), (2, 2), (4, 2)];
        let list_rect = Rect::new(0, 5, 80, 10);
        let scroll = 3;

        // Click on row 5 → display_row = (5 - 5) + 3 = 3 → inside slot 1 (offset=2, height=2)
        let display_row = (5u16 - list_rect.y) as usize + scroll;
        let slot = geometry
            .iter()
            .position(|(offset, height)| display_row >= *offset && display_row < offset + height);
        assert_eq!(slot, Some(1));
    }

    #[test]
    fn test_ops_slot_geometry_multi_line_item() {
        // Slot 0 at row 1 with height 3 (e.g., running item with progress + detail).
        // Slot 1 at row 5 with height 1.
        let geometry = [(1, 3), (5, 1)];
        let list_rect = Rect::new(0, 0, 80, 20);
        let scroll = 0;

        // Clicking rows 1, 2, 3 should all resolve to slot 0.
        for click_row in 1u16..=3 {
            let display_row = (click_row - list_rect.y) as usize + scroll;
            let slot = geometry.iter().position(|(offset, height)| {
                display_row >= *offset && display_row < offset + height
            });
            assert_eq!(slot, Some(0), "click_row={click_row}");
        }

        // Row 5 → slot 1
        let display_row = (5u16 - list_rect.y) as usize + scroll;
        let slot = geometry
            .iter()
            .position(|(offset, height)| display_row >= *offset && display_row < offset + height);
        assert_eq!(slot, Some(1));
    }

    // ── History click mapping ────────────────────────────────────────────

    #[test]
    fn test_history_click_header_rejected() {
        let table_rect = Rect::new(0, 5, 80, 20);
        let header_rows = 1;

        // Click on row 5 → rel_y = 0, which is < header_rows → rejected
        let rel_y = (5u16 - table_rect.y) as usize;
        assert!(rel_y < header_rows);
    }

    #[test]
    fn test_history_click_maps_to_slot() {
        let selectable_to_row = [0, 1, 3, 4]; // slot → table row
        let table_rect = Rect::new(0, 5, 80, 20);
        let header_rows = 1;
        let scroll_offset = 0;

        // Click on row 6 → rel_y = 1, table_row = 1 - 1 + 0 = 0 → slot 0
        let rel_y = (6u16 - table_rect.y) as usize;
        let table_row = rel_y - header_rows + scroll_offset;
        let slot = selectable_to_row.iter().position(|&r| r == table_row);
        assert_eq!(slot, Some(0));

        // Click on row 9 → rel_y = 4, table_row = 3 → slot 2
        let rel_y = (9u16 - table_rect.y) as usize;
        let table_row = rel_y - header_rows + scroll_offset;
        let slot = selectable_to_row.iter().position(|&r| r == table_row);
        assert_eq!(slot, Some(2));
    }

    #[test]
    fn test_history_click_with_scroll() {
        let selectable_to_row = [0, 1, 5, 6];
        let header_rows = 1;
        let scroll_offset = 3;

        let table_rect = Rect::new(0, 0, 80, 20);
        // Click on row 2 → rel_y = 2, table_row = 2 - 1 + 3 = 4 → no match
        let rel_y = (2u16 - table_rect.y) as usize;
        let table_row = rel_y - header_rows + scroll_offset;
        let slot = selectable_to_row.iter().position(|&r| r == table_row);
        assert_eq!(slot, None);

        // Click on row 3 → rel_y = 3, table_row = 3 - 1 + 3 = 5 → slot 2
        let rel_y = (3u16 - table_rect.y) as usize;
        let table_row = rel_y - header_rows + scroll_offset;
        let slot = selectable_to_row.iter().position(|&r| r == table_row);
        assert_eq!(slot, Some(2));
    }

    // ── help_scroll ──────────────────────────────────────────────────────

    use crate::config::types::{AgentConfig, OrchestratorConfig};
    use sqlx::SqlitePool;

    async fn make_test_app() -> App {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        let store = Store::new(pool);
        store.setup().await.unwrap();
        let config = OrchestratorConfig {
            default_workdir: PathBuf::from("/tmp"),
            state_dir: PathBuf::from("/tmp/compas-test"),
            poll_interval_secs: 1,
            models: None,
            agents: vec![AgentConfig {
                alias: "test".to_string(),
                backend: "stub".to_string(),
                role: Default::default(),
                safety_mode: None,
                model: None,
                prompt: None,
                prompt_file: None,
                timeout_secs: None,
                backend_args: None,
                env: None,
                workdir: None,
                workspace: None,
                max_retries: 0,
                retry_backoff_secs: 30,
                handoff: None,
            }],
            worktree_dir: None,
            orchestration: Default::default(),
            database: Default::default(),
            notifications: Default::default(),
            backend_definitions: None,
            hooks: None,
            schedules: None,
        };
        let handle = ConfigHandle::new(config);
        App::new(
            store,
            handle,
            PathBuf::from("/tmp/config.yaml"),
            Handle::current(),
            Duration::from_secs(2),
            None,
        )
    }

    #[tokio::test]
    async fn test_help_scroll_up_clamps_at_zero() {
        let mut app = make_test_app().await;
        app.show_help = true;
        app.help_scroll = 0;
        handle_help_key(&mut app, KeyCode::Up);
        assert_eq!(app.help_scroll, 0);
        handle_help_key(&mut app, KeyCode::Char('k'));
        assert_eq!(app.help_scroll, 0);
    }

    #[tokio::test]
    async fn test_help_scroll_down_clamps_at_max() {
        let mut app = make_test_app().await;
        app.show_help = true;
        // Simulate a viewport smaller than content.
        app.help_viewport_height.set(10);
        let max_scroll = HELP_LINE_COUNT.saturating_sub(10); // 11
        app.help_scroll = max_scroll;
        handle_help_key(&mut app, KeyCode::Down);
        assert_eq!(app.help_scroll, max_scroll);
        handle_help_key(&mut app, KeyCode::Char('j'));
        assert_eq!(app.help_scroll, max_scroll);
    }

    #[tokio::test]
    async fn test_help_toggle_resets_scroll() {
        let mut app = make_test_app().await;
        // Open help and scroll down.
        app.toggle_help();
        assert!(app.show_help);
        app.help_scroll = 5;
        // Close and re-open — scroll should reset.
        app.toggle_help();
        assert!(!app.show_help);
        app.toggle_help();
        assert!(app.show_help);
        assert_eq!(app.help_scroll, 0);
    }

    // ── quit confirmation ────────────────────────────────────────────────

    #[tokio::test]
    async fn test_handle_list_key_q_sets_confirm_not_quit() {
        let mut app = make_test_app().await;
        assert!(!app.confirm_quit);
        assert!(!app.should_quit);

        handle_list_key(&mut app, KeyCode::Char('q'));

        assert!(app.confirm_quit);
        assert!(!app.should_quit);
    }

    #[tokio::test]
    async fn test_handle_quit_confirm_key_y_quits() {
        let mut app = make_test_app().await;
        app.confirm_quit = true;

        handle_quit_confirm_key(&mut app, KeyCode::Char('y'));

        assert!(app.should_quit);
    }

    #[tokio::test]
    async fn test_handle_quit_confirm_key_n_cancels() {
        let mut app = make_test_app().await;
        app.confirm_quit = true;

        handle_quit_confirm_key(&mut app, KeyCode::Char('n'));
        assert!(!app.confirm_quit);

        // Also test Esc
        app.confirm_quit = true;
        handle_quit_confirm_key(&mut app, KeyCode::Esc);
        assert!(!app.confirm_quit);
    }
}
