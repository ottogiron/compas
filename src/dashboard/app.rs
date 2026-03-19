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

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
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
use crate::dashboard::views::log_viewer::{render_execution_detail, ExecutionDetailState};
use crate::events::{EventBus, OrchestratorEvent};
use crate::store::{ExecutionRow, Store, ThreadStatusView};

// ── Constants ─────────────────────────────────────────────────────────────────

const TABS: &[&str] = &["Ops", "Agents", "History", "Settings"];
const TICK_RATE: Duration = Duration::from_millis(250);
const ACTIVITY_ROW_LIMIT: i64 = 250;
const HISTORY_ROW_LIMIT: i64 = 200;
const HISTORY_GROUP_VISIBLE_LIMIT: usize = 10;
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
    /// Up to 200 most-recent execution rows, newest first.
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
    /// Optional batch drill filter for Ops tab.
    pub drill_batch: Option<String>,
    /// Optional batch drill filter for History tab.
    pub history_drill_batch: Option<String>,
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
            drill_batch: None,
            history_drill_batch: None,
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

                Ok::<_, sqlx::Error>(ActivityData {
                    rows,
                    thread_counts,
                    queue_depth,
                    heartbeat,
                    fetched_at: Instant::now(),
                })
            })
            .await
        });

        match result {
            Ok(Ok(data)) => {
                // Clamp selection to new selectable count.
                let count = activity::ops_selectable_count(
                    &data.rows,
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
                                .get_latest_execution_event(exec_id)
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
                        })
                        .collect();
                    executions_by_agent.push((alias.clone(), summaries));
                }

                Ok::<_, sqlx::Error>(AgentsData {
                    executions_by_agent,
                    active_counts,
                    heartbeat_age_secs,
                    fetched_at: Instant::now(),
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
                // Clamp the selection to the new row count.
                let count = executions::history_selectable_count(
                    &executions,
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
                    .executions_data
                    .as_ref()
                    .map(|d| {
                        executions::history_selectable_count(
                            &d.executions,
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

        let agent_alias = execution.agent_alias.clone();
        let status = execution.status.clone();
        let duration_ms = execution.duration_ms;
        let (input_payload, input_linked) = self.resolve_input_payload_for_execution(&execution);
        let log_path = Some(self.log_dir.join(format!("{}.log", execution.id)));
        let timeline_events = self.handle.block_on(async {
            self.store
                .get_execution_events(&exec_id, None, None, Some(TIMELINE_EVENT_LIMIT))
                .await
                .unwrap_or_default()
        });
        let timeline_truncated = timeline_events.len() as i64 == TIMELINE_EVENT_LIMIT;
        let mut detail = ExecutionDetailState::new(
            execution.id,
            agent_alias,
            status,
            duration_ms,
            log_path,
            input_payload,
            input_linked,
            execution.output_preview.clone(),
        );
        detail.timeline_events = timeline_events;
        detail.timeline_truncated = timeline_truncated;
        self.viewing_log = Some(detail);
    }

    fn open_log_viewer_from_execution(&mut self) {
        let Some(data) = &self.executions_data else {
            return;
        };
        let Some(HistorySelectable::Execution(exec_idx)) = self.selected_history_target() else {
            return;
        };
        let Some(row) = data.executions.get(exec_idx) else {
            return;
        };

        let exec_id = row.id.clone();
        let agent_alias = row.agent_alias.clone();
        let status = row.status.clone();
        let duration_ms = row.duration_ms;
        let (input_payload, input_linked) = self.resolve_input_payload_for_execution(row);

        let log_path = self.log_dir.join(format!("{}.log", exec_id));
        let timeline_events = self.handle.block_on(async {
            self.store
                .get_execution_events(&exec_id, None, None, Some(TIMELINE_EVENT_LIMIT))
                .await
                .unwrap_or_default()
        });
        let timeline_truncated = timeline_events.len() as i64 == TIMELINE_EVENT_LIMIT;
        let mut detail = ExecutionDetailState::new(
            exec_id.clone(),
            agent_alias,
            status,
            duration_ms,
            Some(log_path),
            input_payload,
            input_linked,
            row.output_preview.clone(),
        );
        detail.timeline_events = timeline_events;
        detail.timeline_truncated = timeline_truncated;
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

    /// Open the conversation view for the currently selected thread on the Ops tab.
    pub fn open_conversation(&mut self) {
        if self.active_tab != 0 {
            return;
        }
        let Some(data) = &self.activity_data else {
            return;
        };
        let Some(OpsSelectable::Thread(src_idx)) = activity::ops_selected_target(
            &data.rows,
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
        let thread_id = row.thread_id.clone();
        let batch_id = row.batch_id.clone();
        let thread_status = row.thread_status.clone();

        let store = self.store.clone();
        let tid = thread_id.clone();
        let result = self.handle.block_on(async {
            tokio::time::timeout(REFRESH_TIMEOUT, async {
                let messages = store.get_thread_messages(&tid).await?;
                let executions = store.get_thread_executions(&tid).await?;
                Ok::<_, sqlx::Error>((messages, executions))
            })
            .await
        });
        if let Ok(Ok((messages, executions))) = result {
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
        self.refresh_activity();
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
            self.drill_batch.as_deref(),
            self.activity_selected,
            Self::now_unix(),
            self.stale_active_secs(),
        )
    }

    fn selected_history_target(&self) -> Option<HistorySelectable> {
        let data = self.executions_data.as_ref()?;
        executions::history_selected_target(
            &data.executions,
            self.history_drill_batch.as_deref(),
            self.executions_selected,
            self.history_group_visible_limit(),
        )
    }

    fn toggle_help(&mut self) {
        self.show_help = !self.show_help;
    }

    fn clear_batch_drill(&mut self) {
        if self.drill_batch.is_some() {
            self.drill_batch = None;
            self.activity_selected = 0;
        }
        if self.history_drill_batch.is_some() {
            self.history_drill_batch = None;
            self.executions_selected = 0;
        }
    }

    fn enter_batch_drill(&mut self) {
        let Some(data) = &self.activity_data else {
            return;
        };
        let Some(OpsSelectable::Batch(batch_id)) = activity::ops_selected_target(
            &data.rows,
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
                    .executions_data
                    .as_ref()
                    .map(|d| {
                        executions::history_selectable_count(
                            &d.executions,
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
    let mut app = App::new(store, config, config_path, handle, poll_interval, event_bus);
    let result = event_loop(&mut terminal, &mut app);
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
                    v.exec_id.clone(),
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
                } else if app.show_help {
                    handle_help_key(app, key.code);
                } else {
                    handle_list_key(app, key.code);
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
                viewer.select_prev_section();
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if let Some(ref mut viewer) = app.viewing_log {
                viewer.select_next_section();
            }
        }
        KeyCode::Enter => {
            if let Some(ref mut viewer) = app.viewing_log {
                viewer.toggle_selected_section();
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

/// Handle key events while help overlay is open.
fn handle_help_key(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Esc | KeyCode::Enter | KeyCode::Char('?') => app.toggle_help(),
        _ => {}
    }
}

/// Handle a key event when the normal list/tab view is active.
fn handle_list_key(app: &mut App, code: KeyCode) {
    app.show_hint_banner = false;
    match code {
        KeyCode::Char('q') => app.should_quit = true,
        KeyCode::Char('?') => app.toggle_help(),
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
        KeyCode::Char('g') => app.select_first_row(),
        KeyCode::Char('G') => app.select_last_row(),
        KeyCode::Esc => app.clear_batch_drill(),
        KeyCode::Char('x') => app.clear_batch_drill(),
        // Open conversation view for selected thread (Ops tab only).
        KeyCode::Char('c') => {
            if app.active_tab == 0 {
                app.open_conversation();
            }
        }
        // Enter: drill batch row in Ops/History, otherwise open execution detail.
        KeyCode::Enter => {
            if app.active_tab == 0
                && matches!(
                    app.selected_activity_target(),
                    Some(OpsSelectable::Batch(_))
                )
            {
                app.enter_batch_drill();
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
            _ => {} // Settings + fallback handled by Widget impl
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
            0..=2 => {} // Stateful tabs rendered by render_content_with_frame
            3 => self.render_settings_widget(area, buf),
            _ => {
                let tab_name = TABS[self.active_tab];
                let body = format!("  {} — coming soon", tab_name);
                Paragraph::new(Line::from(body.fg(theme::TEXT_DIM)))
                    .block(theme::panel(tab_name))
                    .render(area, buf);
            }
        }
    }

    fn render_settings_widget(&self, area: Rect, buf: &mut Buffer) {
        let block = theme::panel("Settings");

        let label = |s: &str| s.to_string().fg(theme::TEXT_MUTED).bold();
        let value = |s: String| s.fg(theme::TEXT_NORMAL);

        let poll_secs = self.poll_interval.as_secs();
        let cfg = self.config.load();

        let lines = vec![
            Line::from(vec![
                Span::raw("  "),
                label("Config:       "),
                value(self.config_path.display().to_string()),
            ]),
            Line::from(vec![
                Span::raw("  "),
                label("DB:           "),
                value(cfg.db_path().display().to_string()),
            ]),
            Line::from(vec![
                Span::raw("  "),
                label("Poll interval:"),
                value(format!(" {}s", poll_secs)),
            ]),
            Line::from(vec![
                Span::raw("  "),
                label("Agents:       "),
                value(format!(" {}", cfg.agents.len())),
            ]),
            Line::from(vec![
                Span::raw("  "),
                label("Log dir:      "),
                value(format!(" {}", self.log_dir.display())),
            ]),
        ];

        Paragraph::new(lines)
            .style(Style::new().bg(theme::BG_PANEL).fg(theme::TEXT_NORMAL))
            .block(block)
            .render(area, buf);
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

    fn render_help_overlay_widget(&self, area: Rect, buf: &mut Buffer) {
        let modal = centered_rect(72, 18, area);
        let block = Block::bordered()
            .border_style(Style::new().fg(theme::BORDER_FOCUS))
            .style(Style::new().bg(theme::BG_PANEL).fg(theme::TEXT_NORMAL))
            .title(" Help ");
        let inner = block.inner(modal);
        Clear.render(modal, buf);
        block.render(modal, buf);

        let lines = vec![
            Line::from(" Global"),
            Line::from("   q quit / Ctrl+C quit   ? toggle help   Tab/Shift+Tab switch tabs"),
            Line::from("   1-4 jump tabs   r refresh"),
            Line::from(" Navigation"),
            Line::from("   ↑/↓ or j/k move   g/G first/last"),
            Line::from(" Ops"),
            Line::from("   Enter open log or drill batch   c conversation view"),
            Line::from("   Esc/x back from batch drill"),
            Line::from(" History"),
            Line::from("   Enter drill batch/open execution   Esc back from history batch drill"),
            Line::from(" Execution Detail"),
            Line::from("   ↑/↓ or j/k section   Enter collapse/expand   g/G top/bottom"),
            Line::from("   Esc back   f follow   J view mode"),
            Line::from(" Conversation View"),
            Line::from("   j/k or ↑/↓ scroll   g/G top/bottom   f follow   Esc back"),
            Line::from(""),
            Line::from(" Press Esc, Enter, or ? to close this panel."),
        ];

        Paragraph::new(lines)
            .style(Style::new().bg(theme::BG_PANEL).fg(theme::TEXT_NORMAL))
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
