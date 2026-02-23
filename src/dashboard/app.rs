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

use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Tabs},
    Frame, Terminal,
};
use std::{
    io::{self, Stdout},
    path::PathBuf,
    time::{Duration, Instant},
};
use tokio::runtime::Handle;

use crate::config::ConfigHandle;
use crate::dashboard::views::activity::{self, render_activity, OpsSelectable};
use crate::dashboard::views::agents;
use crate::dashboard::views::executions::{self, HistorySelectable};
use crate::dashboard::views::log_viewer::{render_execution_detail, ExecutionDetailState};
use crate::lifecycle::LifecycleService;
use crate::store::{ExecutionRow, Store, ThreadStatusView};

// ── Constants ─────────────────────────────────────────────────────────────────

const TABS: &[&str] = &["Ops", "Agents", "History", "Settings"];
const TICK_RATE: Duration = Duration::from_millis(250);
const ACTIVITY_ROW_LIMIT: i64 = 250;
const HISTORY_ROW_LIMIT: i64 = 200;
const HISTORY_GROUP_VISIBLE_LIMIT: usize = 10;

#[derive(Debug, Clone, Copy)]
enum AdminActionKind {
    Abandon,
    Reopen,
    AbandonStaleActive,
}

#[derive(Debug, Clone)]
struct ActionMenuState {
    thread_id: String,
    options: Vec<AdminActionKind>,
    selected: usize,
}

#[derive(Debug, Clone)]
struct PendingAdminAction {
    kind: AdminActionKind,
    target_label: String,
    thread_ids: Vec<String>,
    impact_summary: String,
    guardrail: String,
}

impl PendingAdminAction {
    fn verb(&self) -> &'static str {
        match self.kind {
            AdminActionKind::Abandon => "abandon",
            AdminActionKind::Reopen => "reopen",
            AdminActionKind::AbandonStaleActive => "abandon stale active",
        }
    }

    fn title(&self) -> &'static str {
        match self.kind {
            AdminActionKind::Abandon => " Confirm Abandon ",
            AdminActionKind::Reopen => " Confirm Reopen ",
            AdminActionKind::AbandonStaleActive => " Confirm Stale Cleanup ",
        }
    }

    fn prompt(&self) -> String {
        match self.kind {
            AdminActionKind::Abandon => format!("Abandon thread {}?", self.target_label),
            AdminActionKind::Reopen => {
                format!("Reopen thread {} to Active?", self.target_label)
            }
            AdminActionKind::AbandonStaleActive => format!(
                "Abandon {} stale active thread(s) in {}?",
                self.thread_ids.len(),
                self.target_label
            ),
        }
    }
}

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
    /// Whether the help overlay is visible.
    pub show_help: bool,
    /// Optional action menu state for guided admin actions.
    action_menu: Option<ActionMenuState>,
    /// Pending admin action waiting for explicit confirmation.
    pending_admin_action: Option<PendingAdminAction>,
    /// Last admin action result shown in the status bar.
    admin_notice: Option<String>,
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

    pub fn new(
        store: Store,
        config: ConfigHandle,
        config_path: PathBuf,
        handle: Handle,
        poll_interval: Duration,
    ) -> Self {
        let log_dir = config.load().log_dir();
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
            show_help: false,
            action_menu: None,
            pending_admin_action: None,
            admin_notice: None,
            drill_batch: None,
            history_drill_batch: None,
            show_hint_banner: true,
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
                .status_view(None, None, None, ACTIVITY_ROW_LIMIT)
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
    }

    /// Fetch fresh per-agent metrics from SQLite and update `agents_data`.
    ///
    /// Uses `Handle::block_on` to drive async queries from the synchronous
    /// TUI thread. Silently swallows errors — stale data is preferable to a
    /// panic inside the event loop.
    pub fn refresh_agents(&mut self) {
        // Snapshot live config for this refresh cycle.
        let cfg = self.config.load();
        // Collect aliases up-front to avoid holding a borrow into config
        // across the async block.
        let aliases: Vec<String> = cfg.agents.iter().map(|a| a.alias.clone()).collect();

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
        let agent_count = cfg.agents.len();
        if agent_count > 0 {
            self.agents_selected = self.agents_selected.min(agent_count - 1);
        } else {
            self.agents_selected = 0;
        }
    }

    /// Fetch the most recent executions from SQLite and update `executions_data`.
    ///
    /// Ordered by `queued_at DESC`. Silently swallows errors — stale data is
    /// preferable to a panic inside the event loop.
    pub fn refresh_executions(&mut self) {
        let store = &self.store;
        let handle = &self.handle;

        let executions = handle.block_on(async {
            store
                .recent_executions(HISTORY_ROW_LIMIT)
                .await
                .unwrap_or_default()
        });

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
            self.admin_notice =
                Some("Selected thread has no execution to inspect yet.".to_string());
            return;
        };

        let execution = self
            .handle
            .block_on(async { self.store.get_execution(&exec_id).await.ok().flatten() });
        let Some(execution) = execution else {
            self.admin_notice = Some(format!("Execution {} was not found.", exec_id));
            return;
        };

        let agent_alias = execution.agent_alias.clone();
        let status = execution.status.clone();
        let duration_ms = execution.duration_ms;
        let (input_payload, input_linked) = self.resolve_input_payload_for_execution(&execution);
        let log_path = Some(self.log_dir.join(format!("{}.log", execution.id)));
        self.viewing_log = Some(ExecutionDetailState::new(
            execution.id,
            agent_alias,
            status,
            duration_ms,
            log_path,
            input_payload,
            input_linked,
            execution.output_preview.clone(),
        ));
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
        self.viewing_log = Some(ExecutionDetailState::new(
            exec_id,
            agent_alias,
            status,
            duration_ms,
            Some(log_path),
            input_payload,
            input_linked,
            row.output_preview.clone(),
        ));
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

    /// Close the detail view and return to the list view.
    pub fn close_log_viewer(&mut self) {
        self.viewing_log = None;
    }

    // ── Admin actions ────────────────────────────────────────────────────────

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

    fn selected_thread_row_from_ops(&self) -> Option<ThreadStatusView> {
        let data = self.activity_data.as_ref()?;
        let OpsSelectable::Thread(src_idx) = self.selected_activity_target()? else {
            return None;
        };
        data.rows.get(src_idx).cloned()
    }

    fn selected_thread_id_and_status(&self) -> Option<(String, String)> {
        match self.active_tab {
            0 => self
                .selected_thread_row_from_ops()
                .map(|r| (r.thread_id, r.thread_status)),
            2 => self
                .executions_data
                .as_ref()
                .and_then(|d| match self.selected_history_target() {
                    Some(HistorySelectable::Execution(exec_idx)) => d.executions.get(exec_idx),
                    _ => None,
                })
                .map(|e| {
                    let status = self.handle.block_on(async {
                        self.store
                            .get_thread(&e.thread_id)
                            .await
                            .ok()
                            .flatten()
                            .map(|t| t.status)
                    });
                    let status = status.unwrap_or_else(|| "Unknown".to_string());
                    (e.thread_id.clone(), status)
                }),
            _ => None,
        }
    }

    fn action_allowed(kind: AdminActionKind, thread_status: &str) -> Result<(), &'static str> {
        match kind {
            AdminActionKind::Abandon => {
                if thread_status == "Abandoned" {
                    Err("thread is already Abandoned")
                } else {
                    Ok(())
                }
            }
            AdminActionKind::Reopen => {
                if matches!(thread_status, "Completed" | "Failed" | "Abandoned") {
                    Ok(())
                } else {
                    Err("reopen is only valid for terminal threads")
                }
            }
            AdminActionKind::AbandonStaleActive => Ok(()),
        }
    }

    fn estimate_cancellable_executions(&self, thread_id: &str) -> usize {
        let store = self.store.clone();
        let tid = thread_id.to_string();
        self.handle
            .block_on(async { store.get_thread_executions(&tid).await.unwrap_or_default() })
            .into_iter()
            .filter(|e| matches!(e.status.as_str(), "queued" | "picked_up" | "executing"))
            .count()
    }

    fn queue_admin_action(&mut self, kind: AdminActionKind) {
        let Some((thread_id, thread_status)) = self.selected_thread_id_and_status() else {
            self.admin_notice =
                Some("No thread selected. Pick a thread row in Ops/History first.".to_string());
            return;
        };
        if let Err(reason) = Self::action_allowed(kind, &thread_status) {
            self.admin_notice = Some(format!("Cannot {}: {}", action_name(kind), reason));
            return;
        }

        let (impact_summary, guardrail) = match kind {
            AdminActionKind::Abandon => {
                let cancellable = self.estimate_cancellable_executions(&thread_id);
                (
                    format!("Will cancel {} queued/running executions.", cancellable),
                    "Use abandon only when work should stop immediately.".to_string(),
                )
            }
            AdminActionKind::Reopen => (
                format!("Will move {} -> Active.", thread_status),
                "Reopened threads can be re-triggered by operator actions.".to_string(),
            ),
            AdminActionKind::AbandonStaleActive => (
                "Will abandon stale active threads.".to_string(),
                "Use the stale cleanup shortcut from Ops.".to_string(),
            ),
        };

        self.pending_admin_action = Some(PendingAdminAction {
            kind,
            target_label: thread_id.clone(),
            thread_ids: vec![thread_id],
            impact_summary,
            guardrail,
        });
        self.action_menu = None;
    }

    fn stale_active_thread_ids(&self) -> Vec<String> {
        let now_unix = Self::now_unix();
        self.activity_data
            .as_ref()
            .map(|d| {
                activity::stale_active_thread_ids(
                    &d.rows,
                    self.drill_batch.as_deref(),
                    now_unix,
                    self.stale_active_secs(),
                )
            })
            .unwrap_or_default()
    }

    fn queue_stale_active_cleanup(&mut self) {
        if self.active_tab != 0 {
            self.admin_notice = Some("Stale cleanup is available on the Ops tab.".to_string());
            return;
        }

        let thread_ids = self.stale_active_thread_ids();
        if thread_ids.is_empty() {
            self.admin_notice = Some(format!(
                "No stale active threads found (age >= {}s, excluding queued/running).",
                self.stale_active_secs()
            ));
            return;
        }

        let target_label = self
            .drill_batch
            .as_deref()
            .map(|b| format!("batch {}", b))
            .unwrap_or_else(|| "all visible threads".to_string());
        let count = thread_ids.len();

        self.pending_admin_action = Some(PendingAdminAction {
            kind: AdminActionKind::AbandonStaleActive,
            target_label: target_label.clone(),
            thread_ids,
            impact_summary: format!(
                "Will abandon {} stale active thread(s) in {}.",
                count, target_label
            ),
            guardrail: format!(
                "Stale means Active for at least {}s with no queued/picked_up/executing execution.",
                self.stale_active_secs()
            ),
        });
        self.action_menu = None;
    }

    fn open_action_menu(&mut self) {
        let Some((thread_id, thread_status)) = self.selected_thread_id_and_status() else {
            self.admin_notice = Some("Action menu requires a selected thread row.".to_string());
            return;
        };

        let mut options = Vec::new();
        if Self::action_allowed(AdminActionKind::Abandon, &thread_status).is_ok() {
            options.push(AdminActionKind::Abandon);
        }
        if Self::action_allowed(AdminActionKind::Reopen, &thread_status).is_ok() {
            options.push(AdminActionKind::Reopen);
        }

        if options.is_empty() {
            self.admin_notice =
                Some("No admin actions available for the selected thread status.".to_string());
            return;
        }

        self.action_menu = Some(ActionMenuState {
            thread_id,
            options,
            selected: 0,
        });
    }

    fn close_action_menu(&mut self) {
        self.action_menu = None;
    }

    fn action_menu_prev(&mut self) {
        if let Some(menu) = &mut self.action_menu {
            menu.selected = menu.selected.saturating_sub(1);
        }
    }

    fn action_menu_next(&mut self) {
        if let Some(menu) = &mut self.action_menu {
            let max = menu.options.len().saturating_sub(1);
            menu.selected = (menu.selected + 1).min(max);
        }
    }

    fn action_menu_confirm(&mut self) {
        let Some(menu) = self.action_menu.as_ref() else {
            return;
        };
        let Some(&kind) = menu.options.get(menu.selected) else {
            return;
        };
        self.queue_admin_action(kind);
    }

    fn cancel_admin_action(&mut self) {
        self.pending_admin_action = None;
        self.admin_notice = Some("Admin action cancelled.".to_string());
    }

    fn execute_admin_action(&mut self) {
        let Some(action) = self.pending_admin_action.take() else {
            return;
        };

        let cfg = self.config.load();
        let svc = LifecycleService::new(self.store.clone(), cfg.agents.as_slice());
        match action.kind {
            AdminActionKind::Abandon => {
                let thread_id = action.thread_ids.first().cloned().unwrap_or_default();
                let result = self
                    .handle
                    .block_on(async { svc.abandon(&thread_id).await });
                match result {
                    Ok(out) => {
                        self.admin_notice = Some(format!(
                            "Thread {} abandoned ({} executions cancelled).",
                            out.thread_id, out.executions_cancelled
                        ));
                    }
                    Err(e) => {
                        self.admin_notice = Some(format!("Failed to {}: {}", action.verb(), e));
                    }
                }
            }
            AdminActionKind::Reopen => {
                let thread_id = action.thread_ids.first().cloned().unwrap_or_default();
                let result = self.handle.block_on(async { svc.reopen(&thread_id).await });
                match result {
                    Ok(out) => {
                        self.admin_notice = Some(format!(
                            "Thread {} reopened ({} → {}).",
                            out.thread_id, out.previous_status, out.new_status
                        ));
                    }
                    Err(e) => {
                        self.admin_notice = Some(format!("Failed to {}: {}", action.verb(), e));
                    }
                }
            }
            AdminActionKind::AbandonStaleActive => {
                let mut abandoned = 0usize;
                let mut cancelled_total = 0u64;
                let mut failed = 0usize;

                for thread_id in &action.thread_ids {
                    match self.handle.block_on(async { svc.abandon(thread_id).await }) {
                        Ok(out) => {
                            abandoned += 1;
                            cancelled_total += out.executions_cancelled;
                        }
                        Err(_) => failed += 1,
                    }
                }

                self.admin_notice = Some(format!(
                    "Stale cleanup complete: {} abandoned, {} failed ({} executions cancelled).",
                    abandoned, failed, cancelled_total
                ));
            }
        }

        // Refresh impacted views after a state mutation.
        self.refresh_activity();
        self.refresh_executions();
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

/// Set up the terminal, run the event loop, and restore the terminal on exit
/// (including on panic).
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
                    if key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        app.should_quit = true;
                        continue;
                    }
                    if app.viewing_log.is_some() {
                        handle_log_viewer_key(app, key.code);
                    } else if app.show_help {
                        handle_help_key(app, key.code);
                    } else if app.pending_admin_action.is_some() {
                        handle_admin_confirm_key(app, key.code);
                    } else if app.action_menu.is_some() {
                        handle_action_menu_key(app, key.code);
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

/// Handle key events while help overlay is open.
fn handle_help_key(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Esc | KeyCode::Enter | KeyCode::Char('?') => app.toggle_help(),
        _ => {}
    }
}

/// Handle key events while action menu overlay is open.
fn handle_action_menu_key(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Esc => app.close_action_menu(),
        KeyCode::Up | KeyCode::Char('k') => app.action_menu_prev(),
        KeyCode::Down | KeyCode::Char('j') => app.action_menu_next(),
        KeyCode::Enter => app.action_menu_confirm(),
        _ => {}
    }
}

/// Handle key events while an admin action confirmation modal is open.
fn handle_admin_confirm_key(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
            app.execute_admin_action();
        }
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
            app.cancel_admin_action();
        }
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
        // Guided admin menu.
        KeyCode::Char('a') => app.open_action_menu(),
        // Lifecycle admin actions.
        KeyCode::Char('b') => app.queue_admin_action(AdminActionKind::Abandon),
        KeyCode::Char('o') => app.queue_admin_action(AdminActionKind::Reopen),
        KeyCode::Char('s') => app.queue_stale_active_cleanup(),
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

// ── Rendering ─────────────────────────────────────────────────────────────────

fn draw(f: &mut Frame, app: &mut App) {
    // When the execution detail view is open, render it full-screen for
    // maximum space.
    if let Some(ref mut viewer) = app.viewing_log {
        render_execution_detail(f, viewer, f.area());
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
    render_status_bar(f, app, chunks[2]);

    if app.show_help {
        render_help_overlay(f, area);
    } else if app.action_menu.is_some() {
        render_action_menu_modal(f, app, area);
    } else if app.pending_admin_action.is_some() {
        render_admin_action_modal(f, app, area);
    }
}

fn render_tab_bar(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let tab_titles: Vec<Line> = TABS.iter().map(|&t| Line::from(Span::raw(t))).collect();

    let tabs = Tabs::new(tab_titles)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .style(Style::default().bg(Color::Black).fg(Color::White))
                .title(Span::styled(
                    " aster-orch dashboard ",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )),
        )
        .select(app.active_tab)
        .style(Style::default().bg(Color::Black).fg(Color::White))
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
            .style(Style::default().bg(Color::Black).fg(Color::White))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .style(Style::default().bg(Color::Black)),
            );

            f.render_widget(content, area);
        }
    }
}

fn render_settings(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .style(Style::default().bg(Color::Black).fg(Color::White))
        .title(" Settings ");

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
    let cfg = app.config.load();

    let lines = vec![
        Line::from(vec![
            Span::raw("  "),
            label("Config:       "),
            value(app.config_path.display().to_string()),
        ]),
        Line::from(vec![
            Span::raw("  "),
            label("DB:           "),
            value(cfg.db_path.display().to_string()),
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
            value(format!(" {}", app.log_dir.display())),
        ]),
    ];

    let p = Paragraph::new(lines)
        .style(Style::default().bg(Color::Black).fg(Color::White))
        .block(block);
    f.render_widget(p, area);
}

fn render_status_bar(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let key = |s: &'static str| {
        Span::styled(
            s,
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
    };
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

    match app.active_tab {
        0 => {
            spans.push(sep());
            spans.push(key("Enter"));
            spans.push(Span::raw(": open/drill"));
            spans.push(sep());
            spans.push(key("a"));
            spans.push(Span::raw(": actions"));
            spans.push(sep());
            spans.push(key("Esc"));
            spans.push(Span::raw(": back batch"));
            spans.push(sep());
            spans.push(key("s"));
            spans.push(Span::raw(": stale cleanup"));
        }
        2 => {
            spans.push(sep());
            spans.push(key("Enter"));
            spans.push(Span::raw(": drill/open"));
            spans.push(sep());
            spans.push(key("Esc"));
            spans.push(Span::raw(": back batch"));
            spans.push(sep());
            spans.push(key("a"));
            spans.push(Span::raw(": actions"));
        }
        1 => {
            spans.push(sep());
            spans.push(key("j/k"));
            spans.push(Span::raw(": select agent"));
        }
        _ => {}
    }

    if let Some(msg) = &app.admin_notice {
        let mut notice = msg.clone();
        if notice.chars().count() > 64 {
            notice = format!("{}…", notice.chars().take(63).collect::<String>());
        }
        spans.push(sep());
        spans.push(Span::styled("last:", Style::default().fg(Color::Cyan)));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(notice, Style::default().fg(Color::White)));
    }

    if app.show_hint_banner {
        spans.push(sep());
        spans.push(Span::styled(
            "Tip: press ? for keymap, a for guided actions",
            Style::default().fg(Color::Cyan),
        ));
    }

    let status = Paragraph::new(Line::from(spans))
        .style(Style::default().bg(Color::DarkGray).fg(Color::White));

    f.render_widget(status, area);
}

fn render_admin_action_modal(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let Some(action) = &app.pending_admin_action else {
        return;
    };

    let modal = centered_rect(74, 10, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .style(Style::default().bg(Color::Black).fg(Color::White))
        .title(action.title());
    let inner = block.inner(modal);
    f.render_widget(Clear, modal);
    f.render_widget(block, modal);

    let lines = vec![
        Line::from(vec![
            Span::raw(" "),
            Span::styled(action.prompt(), Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::raw(" "),
            Span::styled("Impact: ", Style::default().fg(Color::Cyan)),
            Span::styled(
                action.impact_summary.clone(),
                Style::default().fg(Color::White),
            ),
        ]),
        Line::from(vec![
            Span::raw(" "),
            Span::styled("Guardrail: ", Style::default().fg(Color::Cyan)),
            Span::styled(
                action.guardrail.clone(),
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        Line::from(Span::raw("")),
        Line::from(vec![
            Span::raw(" "),
            Span::styled(
                "Enter",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(": confirm  "),
            Span::styled(
                "Esc",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(": cancel"),
        ]),
    ];

    f.render_widget(
        Paragraph::new(lines).style(Style::default().bg(Color::Black).fg(Color::White)),
        inner,
    );
}

fn render_action_menu_modal(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let Some(menu) = &app.action_menu else {
        return;
    };

    let modal = centered_rect(58, 8, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .style(Style::default().bg(Color::Black).fg(Color::White))
        .title(" Actions ")
        .title_bottom(Line::from(vec![
            Span::raw(" "),
            Span::styled(
                "↑/↓",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(": choose  "),
            Span::styled(
                "Enter",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(": continue  "),
            Span::styled(
                "Esc",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(": close "),
        ]));
    let inner = block.inner(modal);
    f.render_widget(Clear, modal);
    f.render_widget(block, modal);

    let mut items = vec![ListItem::new(Line::from(vec![
        Span::raw(" "),
        Span::styled("Thread: ", Style::default().fg(Color::Cyan)),
        Span::styled(menu.thread_id.clone(), Style::default().fg(Color::White)),
    ]))];

    for (idx, action) in menu.options.iter().enumerate() {
        items.push(ListItem::new(Line::from(vec![
            Span::raw("   "),
            Span::styled(
                action_name(*action).to_string(),
                Style::default().fg(Color::White),
            ),
        ])));
        if idx + 1 < menu.options.len() {
            items.push(ListItem::new(Line::from(Span::raw("   "))));
        }
    }

    let mut state = ListState::default();
    state.select(Some(1 + menu.selected.saturating_mul(2)));

    let list = List::new(items)
        .highlight_symbol(" > ")
        .highlight_style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
        .style(Style::default().bg(Color::Black).fg(Color::White));
    f.render_stateful_widget(list, inner, &mut state);
}

fn render_help_overlay(f: &mut Frame, area: ratatui::layout::Rect) {
    let modal = centered_rect(72, 16, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .style(Style::default().bg(Color::Black).fg(Color::White))
        .title(" Help ");
    let inner = block.inner(modal);
    f.render_widget(Clear, modal);
    f.render_widget(block, modal);

    let lines = vec![
        Line::from(" Global"),
        Line::from("   q quit / Ctrl+C quit   ? toggle help   Tab/Shift+Tab switch tabs"),
        Line::from("   1-4 jump tabs   r refresh"),
        Line::from(" Navigation"),
        Line::from("   ↑/↓ or j/k move   g/G first/last"),
        Line::from(" Ops"),
        Line::from("   Enter open log or drill batch"),
        Line::from("   a action menu   b/o quick action aliases   s stale cleanup"),
        Line::from("   Esc back from batch drill"),
        Line::from(" History"),
        Line::from("   Enter drill batch/open execution   Esc back from history batch drill"),
        Line::from(" Execution Detail"),
        Line::from("   ↑/↓ or j/k section   Enter collapse/expand   g/G top/bottom"),
        Line::from("   Esc back   f follow   J view mode"),
        Line::from(""),
        Line::from(" Press Esc, Enter, or ? to close this panel."),
    ];

    f.render_widget(
        Paragraph::new(lines).style(Style::default().bg(Color::Black).fg(Color::White)),
        inner,
    );
}

fn action_name(kind: AdminActionKind) -> &'static str {
    match kind {
        AdminActionKind::Abandon => "abandon",
        AdminActionKind::Reopen => "reopen",
        AdminActionKind::AbandonStaleActive => "abandon stale active",
    }
}

fn centered_rect(
    percent_x: u16,
    height_rows: u16,
    r: ratatui::layout::Rect,
) -> ratatui::layout::Rect {
    let modal_height = height_rows.min(r.height.max(1));
    let top_pad = r.height.saturating_sub(modal_height) / 2;
    let pct = percent_x.clamp(1, 100);
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(top_pad),
            Constraint::Length(modal_height),
            Constraint::Min(0),
        ])
        .split(r);

    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - pct) / 2),
            Constraint::Percentage(pct),
            Constraint::Min(0),
        ])
        .split(vertical[1]);

    horizontal[1]
}
