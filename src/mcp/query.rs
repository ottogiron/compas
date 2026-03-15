//! orch_status, orch_transcript, orch_read, orch_batch_status,
//! orch_tasks, orch_metrics, orch_poll implementations.

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
    thread_status: String,
    agent: Option<String>,
    execution_status: Option<String>,
    started_at: Option<i64>,
    duration_ms: Option<i64>,
    error: Option<String>,
}

impl From<ThreadStatusView> for StatusEntry {
    fn from(v: ThreadStatusView) -> Self {
        Self {
            thread_id: v.thread_id,
            batch_id: v.batch_id,
            thread_status: v.thread_status,
            agent: v.agent_alias,
            execution_status: v.execution_status,
            started_at: v.started_at,
            duration_ms: v.duration_ms,
            error: v.error_detail,
        }
    }
}

impl OrchestratorMcpServer {
    pub async fn status_impl(
        &self,
        params: StatusParams,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        match self
            .store
            .status_view(
                params.thread_id.as_deref(),
                params.agent.as_deref(),
                None,
                50,
            )
            .await
        {
            Ok(rows) => {
                let entries: Vec<StatusEntry> = rows.into_iter().map(StatusEntry::from).collect();
                Ok(json_text(&entries))
            }
            Err(e) => Ok(err_text(format!("status query failed: {}", e))),
        }
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
        }

        #[derive(Serialize)]
        struct Transcript {
            thread_id: String,
            thread_status: Option<String>,
            batch_id: Option<String>,
            messages: Vec<TranscriptMessage>,
            executions: Vec<TranscriptExecution>,
        }

        let transcript = Transcript {
            thread_id: params.thread_id.clone(),
            thread_status: thread.as_ref().map(|t| t.status.clone()),
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
        struct Metrics {
            thread_counts: Vec<(String, i64)>,
            total_messages: i64,
            queue_depth: i64,
            active_by_agent: Vec<(String, i64)>,
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

        Ok(json_text(&Metrics {
            thread_counts,
            total_messages,
            queue_depth,
            active_by_agent,
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
        let config = self.config.load();
        let mgr = crate::worktree::WorktreeManager::new(&config.state_dir);
        match mgr.list_worktrees() {
            Ok(worktrees) => {
                let filtered: Vec<_> = if let Some(ref tid) = params.thread_id {
                    worktrees
                        .into_iter()
                        .filter(|w| w.thread_id == *tid)
                        .collect()
                } else {
                    worktrees
                };
                Ok(json_text(&filtered))
            }
            Err(e) => Ok(err_text(format!("worktree list failed: {}", e))),
        }
    }

    // ── orch_tasks ───────────────────────────────────────────────────────

    pub async fn tasks_impl(&self, params: TasksParams) -> Result<CallToolResult, rmcp::ErrorData> {
        // Query executions — we use status_view as a convenient join
        let limit = params.limit.unwrap_or(20) as i64;
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

        #[derive(Serialize)]
        struct TaskEntry {
            thread_id: String,
            batch_id: Option<String>,
            agent: Option<String>,
            execution_status: Option<String>,
            started_at: Option<i64>,
            finished_at: Option<i64>,
            duration_ms: Option<i64>,
            error: Option<String>,
            prompt_hash: Option<String>,
        }

        let entries: Vec<TaskEntry> = views
            .into_iter()
            .filter(|v| v.execution_id.is_some())
            .map(|v| TaskEntry {
                thread_id: v.thread_id,
                batch_id: v.batch_id,
                agent: v.agent_alias,
                execution_status: v.execution_status,
                started_at: v.started_at,
                finished_at: v.finished_at,
                duration_ms: v.duration_ms,
                error: v.error_detail,
                prompt_hash: v.prompt_hash,
            })
            .collect();

        Ok(json_text(&entries))
    }
}
