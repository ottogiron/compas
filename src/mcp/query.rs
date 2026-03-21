//! orch_status, orch_transcript, orch_read, orch_batch_status,
//! orch_tasks, orch_metrics, orch_poll implementations.

use std::collections::HashMap;

use chrono::DateTime;
use rmcp::model::CallToolResult;
use serde::Serialize;

use super::params::*;
use super::server::{err_text, json_text, OrchestratorMcpServer};
use crate::store::{self, MessageRow, ThreadStatusView};

// ── orch_status ──────────────────────────────────────────────────────────

#[derive(Serialize)]
struct StatusEntry {
    thread_id: String,
    batch_id: Option<String>,
    summary: Option<String>,
    thread_status: String,
    agent: Option<String>,
    execution_status: Option<String>,
    started_at: Option<i64>,
    duration_ms: Option<i64>,
    error: Option<String>,
    prompt_hash: Option<String>,
}

impl From<ThreadStatusView> for StatusEntry {
    fn from(v: ThreadStatusView) -> Self {
        Self {
            thread_id: v.thread_id,
            batch_id: v.batch_id,
            summary: v.summary,
            thread_status: v.thread_status,
            agent: v.agent_alias,
            execution_status: v.execution_status,
            started_at: v.started_at,
            duration_ms: v.duration_ms,
            error: v.error_detail,
            prompt_hash: v.prompt_hash,
        }
    }
}

impl OrchestratorMcpServer {
    pub async fn status_impl(
        &self,
        params: StatusParams,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let rows = match self
            .store
            .status_view(
                params.thread_id.as_deref(),
                params.agent.as_deref(),
                None,
                50,
            )
            .await
        {
            Ok(r) => r,
            Err(e) => return Ok(err_text(format!("status query failed: {}", e))),
        };

        let scheduled_count = self.store.count_scheduled_executions().await.unwrap_or(0);

        let entries: Vec<StatusEntry> = rows.into_iter().map(StatusEntry::from).collect();

        #[derive(Serialize)]
        struct StatusResponse {
            threads: Vec<StatusEntry>,
            scheduled_count: i64,
        }

        Ok(json_text(&StatusResponse {
            threads: entries,
            scheduled_count,
        }))
    }

    // ── orch_transcript ──────────────────────────────────────────────────

    pub async fn transcript_impl(
        &self,
        params: TranscriptParams,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let messages = match self.store.get_thread_messages(&params.thread_id).await {
            Ok(m) => m,
            Err(e) => return Ok(err_text(format!("transcript query failed: {}", e))),
        };

        let executions = match self.store.get_thread_executions(&params.thread_id).await {
            Ok(e) => e,
            Err(e) => return Ok(err_text(format!("executions query failed: {}", e))),
        };

        let thread = self
            .store
            .get_thread(&params.thread_id)
            .await
            .ok()
            .flatten();

        #[derive(Serialize)]
        struct TranscriptMessage {
            id: i64,
            reference: String,
            from: String,
            to: String,
            intent: String,
            body: String,
            created_at: i64,
        }

        #[derive(Serialize)]
        struct TranscriptExecution {
            id: String,
            agent: String,
            dispatch_message_id: Option<i64>,
            status: String,
            queued_at: i64,
            started_at: Option<i64>,
            finished_at: Option<i64>,
            duration_ms: Option<i64>,
            exit_code: Option<i32>,
            error: Option<String>,
            prompt_hash: Option<String>,
        }

        #[derive(Serialize)]
        struct Transcript {
            thread_id: String,
            thread_status: Option<String>,
            summary: Option<String>,
            batch_id: Option<String>,
            messages: Vec<TranscriptMessage>,
            executions: Vec<TranscriptExecution>,
        }

        let transcript = Transcript {
            thread_id: params.thread_id.clone(),
            thread_status: thread.as_ref().map(|t| t.status.clone()),
            summary: thread.as_ref().and_then(|t| t.summary.clone()),
            batch_id: thread.and_then(|t| t.batch_id),
            messages: messages
                .into_iter()
                .map(|m| TranscriptMessage {
                    id: m.id,
                    reference: store::message_ref(m.id),
                    from: m.from_alias,
                    to: m.to_alias,
                    intent: m.intent,
                    body: m.body,
                    created_at: m.created_at,
                })
                .collect(),
            executions: executions
                .into_iter()
                .map(|e| TranscriptExecution {
                    id: e.id,
                    agent: e.agent_alias,
                    dispatch_message_id: e.dispatch_message_id,
                    status: e.status,
                    queued_at: e.queued_at,
                    started_at: e.started_at,
                    finished_at: e.finished_at,
                    duration_ms: e.duration_ms,
                    exit_code: e.exit_code,
                    error: e.error_detail,
                    prompt_hash: e.prompt_hash,
                })
                .collect(),
        };

        Ok(json_text(&transcript))
    }

    // ── orch_read ────────────────────────────────────────────────────────

    pub async fn read_impl(&self, params: ReadParams) -> Result<CallToolResult, rmcp::ErrorData> {
        let id = match store::parse_message_ref(&params.reference) {
            Ok(id) => id,
            Err(e) => return Ok(err_text(e)),
        };

        match self.store.get_message(id).await {
            Ok(Some(msg)) => {
                #[derive(Serialize)]
                struct MessageDetail {
                    id: i64,
                    reference: String,
                    thread_id: String,
                    from: String,
                    to: String,
                    intent: String,
                    body: String,
                    created_at: i64,
                }
                Ok(json_text(&MessageDetail {
                    id: msg.id,
                    reference: store::message_ref(msg.id),
                    thread_id: msg.thread_id,
                    from: msg.from_alias,
                    to: msg.to_alias,
                    intent: msg.intent,
                    body: msg.body,
                    created_at: msg.created_at,
                }))
            }
            Ok(None) => Ok(err_text(format!("message not found: {}", params.reference))),
            Err(e) => Ok(err_text(format!("read failed: {}", e))),
        }
    }

    // ── orch_metrics ─────────────────────────────────────────────────────

    pub async fn metrics_impl(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        #[derive(Serialize)]
        struct CostOverview {
            total_cost_usd: f64,
            total_tokens_in: i64,
            total_tokens_out: i64,
            executions_with_cost: i64,
        }

        #[derive(Serialize)]
        struct Metrics {
            thread_counts: Vec<(String, i64)>,
            total_messages: i64,
            queue_depth: i64,
            active_by_agent: Vec<(String, i64)>,
            cost: CostOverview,
        }

        let thread_counts = match self.store.thread_counts().await {
            Ok(v) => v,
            Err(e) => return Ok(err_text(format!("metrics query failed: {}", e))),
        };
        let total_messages = match self.store.message_count().await {
            Ok(v) => v,
            Err(e) => return Ok(err_text(format!("metrics query failed: {}", e))),
        };
        let queue_depth = match self.store.queue_depth().await {
            Ok(v) => v,
            Err(e) => return Ok(err_text(format!("metrics query failed: {}", e))),
        };
        let active_by_agent = match self.store.active_executions_by_agent().await {
            Ok(v) => v,
            Err(e) => return Ok(err_text(format!("metrics query failed: {}", e))),
        };
        let cost_summary = match self.store.cost_summary(None).await {
            Ok(v) => v,
            Err(e) => return Ok(err_text(format!("metrics query failed: {}", e))),
        };

        Ok(json_text(&Metrics {
            thread_counts,
            total_messages,
            queue_depth,
            active_by_agent,
            cost: CostOverview {
                total_cost_usd: cost_summary.total_cost_usd,
                total_tokens_in: cost_summary.total_tokens_in,
                total_tokens_out: cost_summary.total_tokens_out,
                executions_with_cost: cost_summary.executions_with_cost,
            },
        }))
    }

    // ── orch_tool_stats ──────────────────────────────────────────────────────

    pub async fn tool_stats_impl(
        &self,
        params: ToolStatsParams,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let alias = params.agent_alias.as_deref();

        // Get error rates (includes call_count from the inner join)
        let error_rates: HashMap<String, (i64, f64)> =
            match self.store.tool_error_rates(alias).await {
                Ok(v) => v
                    .into_iter()
                    .map(|s| (s.tool_name, (s.error_count, s.error_rate)))
                    .collect(),
                Err(e) => return Ok(err_text(format!("tool stats query failed: {}", e))),
            };

        // Call counts as the primary list (defines the set of tool names)
        let call_counts = match self.store.tool_call_counts(alias).await {
            Ok(v) => v,
            Err(e) => return Ok(err_text(format!("tool stats query failed: {}", e))),
        };

        #[derive(Serialize)]
        struct ToolStat {
            tool_name: String,
            call_count: i64,
            error_count: i64,
            error_rate: f64,
        }

        // Merge call_counts with error_rates by tool_name
        let tool_stats: Vec<ToolStat> = call_counts
            .into_iter()
            .map(|s| {
                let (error_count, error_rate) =
                    error_rates.get(&s.tool_name).copied().unwrap_or((0, 0.0));
                ToolStat {
                    tool_name: s.tool_name,
                    call_count: s.call_count,
                    error_count,
                    error_rate,
                }
            })
            .collect();

        // Tool usage broken down by agent (filter in Rust if alias is set)
        #[derive(Serialize)]
        struct AgentToolUsage {
            agent_alias: String,
            tool_name: String,
            call_count: i64,
        }

        let usage_by_agent: Vec<AgentToolUsage> = match self.store.tool_usage_by_agent().await {
            Ok(rows) => rows
                .into_iter()
                .filter(|(a, _, _)| alias.is_none_or(|f| a == f))
                .map(|(agent_alias, tool_name, call_count)| AgentToolUsage {
                    agent_alias,
                    tool_name,
                    call_count,
                })
                .collect(),
            Err(e) => return Ok(err_text(format!("tool usage query failed: {}", e))),
        };

        // Cost per agent (filter in Rust if alias is set)
        #[derive(Serialize)]
        struct AgentCost {
            agent_alias: String,
            total_cost_usd: f64,
            total_tokens_in: i64,
            total_tokens_out: i64,
            execution_count: i64,
        }

        let cost_by_agent: Vec<AgentCost> = match self.store.cost_by_agent().await {
            Ok(rows) => rows
                .into_iter()
                .filter(|c| alias.is_none_or(|f| c.agent_alias == f))
                .map(|c| AgentCost {
                    agent_alias: c.agent_alias,
                    total_cost_usd: c.total_cost_usd,
                    total_tokens_in: c.total_tokens_in,
                    total_tokens_out: c.total_tokens_out,
                    execution_count: c.execution_count,
                })
                .collect(),
            Err(e) => return Ok(err_text(format!("cost by agent query failed: {}", e))),
        };

        #[derive(Serialize)]
        struct ToolStatsResponse {
            tool_stats: Vec<ToolStat>,
            tool_usage_by_agent: Vec<AgentToolUsage>,
            cost_by_agent: Vec<AgentCost>,
        }

        Ok(json_text(&ToolStatsResponse {
            tool_stats,
            tool_usage_by_agent: usage_by_agent,
            cost_by_agent,
        }))
    }

    // ── orch_poll ────────────────────────────────────────────────────────

    pub async fn poll_impl(&self, params: PollParams) -> Result<CallToolResult, rmcp::ErrorData> {
        let thread = match self.store.get_thread(&params.thread_id).await {
            Ok(Some(t)) => t,
            Ok(None) => {
                return Ok(err_text(format!("thread not found: {}", params.thread_id)));
            }
            Err(e) => return Ok(err_text(format!("poll failed: {}", e))),
        };

        // Resolve since cursor
        let since_id = match params.since_reference.as_deref() {
            Some(r) => match store::parse_message_ref(r) {
                Ok(id) => id,
                Err(e) => return Ok(err_text(e)),
            },
            None => 0,
        };

        let messages = match self
            .store
            .get_messages_since(&params.thread_id, since_id)
            .await
        {
            Ok(m) => m,
            Err(e) => return Ok(err_text(format!("poll messages failed: {}", e))),
        };

        // Filter by intent if specified, or auto-exclude trigger intents.
        //
        // When neither `intent` nor `since_reference` is provided, trigger
        // trigger intents are auto-excluded
        // so the caller gets the agent's response, not their own dispatch.
        let config = self.config.load();
        let filtered: Vec<&MessageRow> = if let Some(ref intent) = params.intent {
            messages.iter().filter(|m| m.intent == *intent).collect()
        } else if params.since_reference.is_none() {
            let trigger_intents = &config.orchestration.trigger_intents;
            messages
                .iter()
                .filter(|m| !trigger_intents.contains(&m.intent))
                .collect()
        } else {
            messages.iter().collect()
        };

        let latest_exec = self
            .store
            .latest_execution(&params.thread_id)
            .await
            .ok()
            .flatten();

        #[derive(Serialize)]
        struct PollResult {
            thread_id: String,
            thread_status: String,
            summary: Option<String>,
            matched_messages: usize,
            latest_message_id: Option<i64>,
            latest_message_ref: Option<String>,
            latest_intent: Option<String>,
            latest_body_preview: Option<String>,
            execution_status: Option<String>,
        }

        let latest = filtered.last();
        Ok(json_text(&PollResult {
            thread_id: params.thread_id,
            thread_status: thread.status,
            summary: thread.summary,
            matched_messages: filtered.len(),
            latest_message_id: latest.map(|m| m.id),
            latest_message_ref: latest.map(|m| store::message_ref(m.id)),
            latest_intent: latest.map(|m| m.intent.clone()),
            latest_body_preview: latest.map(|m| {
                if m.body.len() > 200 {
                    format!("{}...", &m.body[..200])
                } else {
                    m.body.clone()
                }
            }),
            execution_status: latest_exec.map(|e| e.status),
        }))
    }

    // ── orch_batch_status ────────────────────────────────────────────────

    pub async fn batch_status_impl(
        &self,
        params: BatchStatusParams,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let threads = match self
            .store
            .list_threads(Some(&params.batch_id), None, 100)
            .await
        {
            Ok(t) => t,
            Err(e) => return Ok(err_text(format!("batch query failed: {}", e))),
        };

        #[derive(Serialize)]
        struct BatchThread {
            thread_id: String,
            summary: Option<String>,
            status: String,
        }

        #[derive(Serialize)]
        struct BatchStatus {
            batch_id: String,
            thread_count: usize,
            threads: Vec<BatchThread>,
        }

        Ok(json_text(&BatchStatus {
            batch_id: params.batch_id,
            thread_count: threads.len(),
            threads: threads
                .into_iter()
                .map(|t| BatchThread {
                    thread_id: t.thread_id,
                    summary: t.summary,
                    status: t.status,
                })
                .collect(),
        }))
    }

    // ── orch_execution_events ─────────────────────────────────────────────

    pub async fn execution_events_impl(
        &self,
        params: ExecutionEventsParams,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let limit = params.limit.unwrap_or(100).min(1000);
        match self
            .store
            .get_execution_events(
                &params.execution_id,
                params.since_timestamp,
                params.since_event_index,
                Some(limit),
            )
            .await
        {
            Ok(events) => {
                #[derive(Serialize)]
                struct EventEntry {
                    id: i64,
                    event_type: String,
                    summary: String,
                    detail: Option<String>,
                    timestamp_ms: i64,
                    event_index: i32,
                    #[serde(skip_serializing_if = "Option::is_none")]
                    tool_name: Option<String>,
                }
                let entries: Vec<EventEntry> = events
                    .into_iter()
                    .map(|e| EventEntry {
                        id: e.id,
                        event_type: e.event_type,
                        summary: e.summary,
                        detail: e.detail,
                        timestamp_ms: e.timestamp_ms,
                        event_index: e.event_index,
                        tool_name: e.tool_name,
                    })
                    .collect();
                Ok(json_text(&entries))
            }
            Err(e) => Ok(err_text(format!("execution events query failed: {}", e))),
        }
    }

    // ── orch_worktrees ────────────────────────────────────────────────────

    pub async fn worktrees_impl(
        &self,
        params: WorktreesParams,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        // Query worktree paths from the threads table (DB is the source of truth
        // since worktrees can live in different locations per-agent workdir).
        match self.store.threads_with_worktree_paths().await {
            Ok(entries) => {
                let filtered: Vec<_> = if let Some(ref tid) = params.thread_id {
                    entries
                        .into_iter()
                        .filter(|w| w.thread_id == *tid)
                        .collect()
                } else {
                    entries
                };
                let infos: Vec<crate::worktree::WorktreeInfo> = filtered
                    .into_iter()
                    .map(|e| crate::worktree::WorktreeInfo {
                        thread_id: e.thread_id,
                        path: std::path::PathBuf::from(e.worktree_path),
                    })
                    .collect();
                Ok(json_text(&infos))
            }
            Err(e) => Ok(err_text(format!("worktree list failed: {}", e))),
        }
    }

    // ── orch_read_log ─────────────────────────────────────────────────────

    pub async fn read_log_impl(
        &self,
        params: ReadLogParams,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let limit = (params.limit.unwrap_or(200) as usize).min(1000);
        let tail = params.tail.unwrap_or(false);
        let requested_offset = params.offset.unwrap_or(0) as usize;

        let config = self.config.load();
        let log_path = config
            .log_dir()
            .join(format!("{}.log", params.execution_id));

        #[derive(Serialize)]
        struct ReadLogResponse {
            execution_id: String,
            total_lines: usize,
            returned_lines: usize,
            offset: usize,
            has_more: bool,
            source: String,
            lines: Vec<String>,
        }

        let (all_lines, source) = if log_path.exists() {
            match tokio::fs::read_to_string(&log_path).await {
                Ok(content) => {
                    let lines: Vec<String> = content.lines().map(String::from).collect();
                    (lines, "log_file")
                }
                Err(e) => {
                    return Ok(err_text(format!(
                        "failed to read log file {}: {}",
                        log_path.display(),
                        e
                    )));
                }
            }
        } else {
            // Fallback to output_preview from DB
            match self.store.get_execution(&params.execution_id).await {
                Ok(Some(exec)) => {
                    let lines: Vec<String> = exec
                        .output_preview
                        .as_deref()
                        .unwrap_or("")
                        .lines()
                        .map(String::from)
                        .collect();
                    (lines, "output_preview")
                }
                Ok(None) => {
                    return Ok(err_text(format!(
                        "execution not found: {}",
                        params.execution_id
                    )));
                }
                Err(e) => {
                    return Ok(err_text(format!("execution query failed: {}", e)));
                }
            }
        };

        let total_lines = all_lines.len();
        let (offset, selected) = if tail {
            let start = total_lines.saturating_sub(limit);
            let slice = &all_lines[start..];
            (start, slice.to_vec())
        } else {
            let start = requested_offset.min(total_lines);
            let end = (start + limit).min(total_lines);
            let slice = &all_lines[start..end];
            (start, slice.to_vec())
        };

        let returned_lines = selected.len();
        let has_more = (offset + returned_lines) < total_lines;

        Ok(json_text(&ReadLogResponse {
            execution_id: params.execution_id,
            total_lines,
            returned_lines,
            offset,
            has_more,
            source: source.to_string(),
            lines: selected,
        }))
    }

    // ── orch_tasks ───────────────────────────────────────────────────────

    pub async fn tasks_impl(&self, params: TasksParams) -> Result<CallToolResult, rmcp::ErrorData> {
        // Validate filter param.
        if let Some(ref f) = params.filter {
            if f != "scheduled" {
                return Ok(err_text(format!(
                    "unknown filter '{}'. Supported filters: 'scheduled'",
                    f
                )));
            }
        }

        let limit = params.limit.unwrap_or(20) as i64;

        #[derive(Serialize)]
        struct TaskEntry {
            thread_id: String,
            batch_id: Option<String>,
            summary: Option<String>,
            agent: Option<String>,
            execution_status: Option<String>,
            started_at: Option<i64>,
            finished_at: Option<i64>,
            duration_ms: Option<i64>,
            error: Option<String>,
            prompt_hash: Option<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            eligible_at: Option<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            eligible_reason: Option<String>,
        }

        if params.filter.as_deref() == Some("scheduled") {
            // Dedicated DB query — avoids status_view limit truncation.
            let execs = match self
                .store
                .get_scheduled_executions(params.alias.as_deref(), limit)
                .await
            {
                Ok(v) => v,
                Err(e) => return Ok(err_text(format!("tasks query failed: {}", e))),
            };

            let entries: Vec<TaskEntry> = execs
                .into_iter()
                .map(|se| {
                    let e = se.execution;
                    let eligible_at_iso = e.eligible_at.map(|ts| {
                        DateTime::from_timestamp(ts, 0)
                            .map(|dt| dt.to_rfc3339())
                            .unwrap_or_else(|| ts.to_string())
                    });
                    TaskEntry {
                        thread_id: e.thread_id,
                        batch_id: e.batch_id,
                        summary: se.summary,
                        agent: Some(e.agent_alias),
                        execution_status: Some(e.status),
                        started_at: e.started_at,
                        finished_at: e.finished_at,
                        duration_ms: e.duration_ms,
                        error: e.error_detail,
                        prompt_hash: e.prompt_hash,
                        eligible_at: eligible_at_iso,
                        eligible_reason: e.eligible_reason,
                    }
                })
                .collect();

            return Ok(json_text(&entries));
        }

        // Default path: use status_view join.
        let views = match self
            .store
            .status_view(
                None,
                params.alias.as_deref(),
                params.batch_id.as_deref(),
                limit,
            )
            .await
        {
            Ok(v) => v,
            Err(e) => return Ok(err_text(format!("tasks query failed: {}", e))),
        };

        let entries: Vec<TaskEntry> = views
            .into_iter()
            .filter(|v| v.execution_id.is_some())
            .map(|v| {
                let eligible_at_iso = v.eligible_at.map(|ts| {
                    DateTime::from_timestamp(ts, 0)
                        .map(|dt| dt.to_rfc3339())
                        .unwrap_or_else(|| ts.to_string())
                });
                TaskEntry {
                    thread_id: v.thread_id,
                    batch_id: v.batch_id,
                    summary: v.summary,
                    agent: v.agent_alias,
                    execution_status: v.execution_status,
                    started_at: v.started_at,
                    finished_at: v.finished_at,
                    duration_ms: v.duration_ms,
                    error: v.error_detail,
                    prompt_hash: v.prompt_hash,
                    eligible_at: eligible_at_iso,
                    eligible_reason: v.eligible_reason,
                }
            })
            .collect();

        Ok(json_text(&entries))
    }
}
