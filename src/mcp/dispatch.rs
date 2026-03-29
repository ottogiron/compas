//! orch_dispatch implementation.
//!
//! Dispatch validates the target agent alias, auto-reopens threads in terminal
//! states (Completed/Failed/Abandoned), and inserts the message into the store.
//! Trigger eligibility (whether the message should spawn an execution) is
//! determined by the worker poll loop, not here.
//!
//! When `scheduled_for` is provided, dispatch also pre-creates a queued
//! execution with `eligible_at` set so the worker defers pickup until the
//! scheduled time.

use chrono::{DateTime, Utc};
use rmcp::model::CallToolResult;
use serde::Serialize;

use super::params::DispatchParams;
use super::server::{err_text, json_text, OrchestratorMcpServer};
use crate::config::types::HandoffTarget;
use crate::store::ThreadStatus;

#[derive(Serialize)]
struct DispatchResult {
    thread_id: String,
    message_id: i64,
    /// MCP tool hint for waiting on the agent's response.
    next_step: String,
    /// The scheduled execution time, echoed back when set.
    #[serde(skip_serializing_if = "Option::is_none")]
    scheduled_for: Option<String>,
    /// Pre-created execution ID for scheduled dispatches.
    #[serde(skip_serializing_if = "Option::is_none")]
    execution_id: Option<String>,
    /// True when the thread was in a terminal state and was auto-reopened.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    reopened: bool,
    /// The thread's status before it was reopened.
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_status: Option<String>,
}

impl OrchestratorMcpServer {
    pub async fn dispatch_impl(
        &self,
        params: DispatchParams,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        // Snapshot live config for alias validation.
        let config = self.config.load();

        // Validate target agent exists
        if !config.agents.iter().any(|a| a.alias == params.to) {
            return Ok(err_text(format!(
                "unknown agent alias: '{}'. available: {}",
                params.to,
                config
                    .agents
                    .iter()
                    .map(|a| a.alias.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }

        // SEC-6: Queue depth guard
        if let Some(max) = config.orchestration.max_queued_executions {
            match self.store.total_inflight_executions().await {
                Ok(current) if current as usize >= max => {
                    return Ok(err_text(format!(
                        "Queue depth limit reached: {current}/{max} executions are in-flight \
                         (queued + active). Wait for running tasks to complete, or abandon \
                         stale threads with orch_abandon before dispatching new work. \
                         Check current load with orch_metrics."
                    )));
                }
                Err(e) => {
                    tracing::warn!(error = %e, "queue depth check failed, allowing dispatch");
                    // Fail-open: don't block dispatch on transient DB error
                }
                _ => {}
            }
        }

        // Parse and validate scheduled_for if provided.
        let eligible_at = match &params.scheduled_for {
            Some(ts_str) => {
                let parsed: DateTime<Utc> = match ts_str.parse::<DateTime<Utc>>() {
                    Ok(dt) => dt,
                    Err(_) => {
                        // Try RFC 3339 with fixed offset and convert to UTC.
                        match DateTime::parse_from_rfc3339(ts_str) {
                            Ok(dt) => dt.with_timezone(&Utc),
                            Err(e) => {
                                return Ok(err_text(format!(
                                    "invalid scheduled_for timestamp '{}': {}. \
                                     Expected ISO 8601 format (e.g. '2026-03-21T20:00:00Z').",
                                    ts_str, e
                                )));
                            }
                        }
                    }
                };
                let now = Utc::now();
                if parsed <= now {
                    return Ok(err_text(format!(
                        "scheduled_for must be in the future. Got '{}' but current time is '{}'.",
                        parsed.to_rfc3339(),
                        now.to_rfc3339()
                    )));
                }
                Some(parsed)
            }
            None => None,
        };

        // Generate thread_id if not provided
        let thread_id = params
            .thread_id
            .unwrap_or_else(|| ulid::Ulid::new().to_string());

        // If dispatching to an existing thread in a terminal state, auto-reopen it.
        // This intentionally diverges from `lifecycle::reopen()` by also cancelling
        // any stale queued/active executions — defensive cleanup to prevent
        // double-execution when the worker polls immediately after the new message.
        let mut reopened = false;
        let mut previous_status: Option<String> = None;
        if let Ok(Some(thread)) = self.store.get_thread(&thread_id).await {
            let status: ThreadStatus = thread.status.parse().unwrap_or(ThreadStatus::Active);
            if status.is_terminal() {
                let _ = self.store.cancel_thread_executions(&thread_id).await;
                if let Err(e) = self
                    .store
                    .update_thread_status(&thread_id, ThreadStatus::Active)
                    .await
                {
                    return Ok(err_text(format!("failed to reopen thread: {}", e)));
                }
                previous_status = Some(thread.status.clone());
                reopened = true;
            }
        }

        // Insert message — trigger eligibility is determined by the worker.
        let skip_handoff = params.skip_handoff.unwrap_or(false);
        let message_id = match self
            .store
            .insert_dispatch_message(
                &thread_id,
                &params.from,
                &params.to,
                &params.intent,
                &params.body,
                params.batch.as_deref(),
                params.summary.as_deref(),
                skip_handoff,
            )
            .await
        {
            Ok(id) => id,
            Err(e) => return Ok(err_text(format!("failed to insert message: {}", e))),
        };

        // When scheduled_for is set, pre-create the execution with eligible_at
        // so it is linked to the dispatch message (dedup-safe) and the worker
        // won't claim it until the scheduled time.
        let execution_id = if let Some(dt) = eligible_at {
            match self
                .store
                .insert_execution_scheduled(
                    &thread_id,
                    &params.to,
                    Some(message_id),
                    None, // prompt_hash — worker will populate on claim
                    Some(dt.timestamp()),
                    Some("scheduled"),
                )
                .await
            {
                Ok(Some(exec_id)) => Some(exec_id),
                Ok(None) => None, // dedup — already enqueued
                Err(e) => {
                    return Ok(err_text(format!(
                        "failed to create scheduled execution: {}",
                        e
                    )));
                }
            }
        } else {
            None
        };

        // Build target-agent-aware wait hint based on handoff config.
        let target_agent = config.agents.iter().find(|a| a.alias == params.to);
        let has_downstream = target_agent
            .and_then(|a| a.handoff.as_ref())
            .and_then(|h| h.on_response.as_ref())
            .is_some_and(|target| !matches!(target, HandoffTarget::Single(s) if s == "operator"));

        let next_step = if has_downstream {
            let handoff = target_agent.unwrap().handoff.as_ref().unwrap();
            let depth = handoff.max_chain_depth.unwrap_or(3);
            let target_desc = match handoff.on_response.as_ref().unwrap() {
                HandoffTarget::Single(t) => format!("\"{}\"", t),
                HandoffTarget::FanOut(ts) => {
                    let names: Vec<String> = ts.iter().map(|t| format!("\"{}\"", t)).collect();
                    names.join(", ")
                }
            };
            format!(
                "Use orch_wait with thread_id=\"{}\", since_reference=\"db:{}\", and await_chain=true to wait for the full chain to settle. \"{}\" auto-handoffs to {} (chain depth limit: {}).",
                thread_id, message_id, params.to, target_desc, depth
            )
        } else {
            format!(
                "Use orch_wait with thread_id=\"{}\", since_reference=\"db:{}\" to wait for the response.",
                thread_id, message_id
            )
        };
        Ok(json_text(&DispatchResult {
            thread_id,
            message_id,
            next_step,
            scheduled_for: params.scheduled_for,
            execution_id,
            reopened,
            previous_status,
        }))
    }
}
