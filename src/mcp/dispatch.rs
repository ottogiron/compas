//! orch_dispatch implementation.
//!
//! Dispatch is a pure message-insertion operation. It validates the target
//! agent alias and inserts the message into the store. Trigger eligibility
//! (whether the message should spawn an execution) is determined by the
//! worker poll loop, not here.
//!
//! When `scheduled_for` is provided, dispatch also pre-creates a queued
//! execution with `eligible_at` set so the worker defers pickup until the
//! scheduled time.

use chrono::{DateTime, Utc};
use rmcp::model::CallToolResult;
use serde::Serialize;

use super::params::DispatchParams;
use super::server::{err_text, json_text, OrchestratorMcpServer};

#[derive(Serialize)]
struct DispatchResult {
    thread_id: String,
    message_id: i64,
    /// Concrete CLI command to wait for the agent's response.
    next_step: String,
    /// The scheduled execution time, echoed back when set.
    #[serde(skip_serializing_if = "Option::is_none")]
    scheduled_for: Option<String>,
    /// Pre-created execution ID for scheduled dispatches.
    #[serde(skip_serializing_if = "Option::is_none")]
    execution_id: Option<String>,
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

        // Insert message — trigger eligibility is determined by the worker.
        let message_id = match self
            .store
            .insert_message(
                &thread_id,
                &params.from,
                &params.to,
                &params.intent,
                &params.body,
                params.batch.as_deref(),
                params.summary.as_deref(),
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

        let next_step = format!(
            "compas wait --thread-id {} --since db:{} --timeout 900",
            thread_id, message_id
        );
        Ok(json_text(&DispatchResult {
            thread_id,
            message_id,
            next_step,
            scheduled_for: params.scheduled_for,
            execution_id,
        }))
    }
}
