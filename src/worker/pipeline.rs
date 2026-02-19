//! Stepped workflow pipeline: execute → parse → dispatch.
//!
//! Each step is persisted to SQLite via apalis. If the worker crashes between
//! steps, it resumes at the failed step instead of re-triggering the agent.
//!
//! Step 1 (execute_trigger): Spawn CLI process, wait for output  [slow: 10s-300s]
//! Step 2 (parse_reply):     Parse output for JSON auto-reply    [fast: <1ms]
//! Step 3 (dispatch_result): Update thread state, return reply   [fast: <10ms]
//!
//! Job routing (pushing follow-up trigger jobs for review-requests, handoffs, etc.)
//! is handled by the caller/handler, not the pipeline. The pipeline returns a
//! `ParsedReply` so the handler can make routing decisions with access to the
//! apalis storage handle.

use super::context::TriggerContext;
use super::trigger::{
    build_instruction, parse_trigger_output, ParsedReply, TriggerJob, TriggerOutput,
};
use apalis::prelude::*;
use tracing;

/// Step 1: Execute the CLI trigger.
///
/// Resolves agent config → backend, starts/reuses session, builds instruction,
/// calls `backend.trigger()`, and captures the output.
pub async fn execute_trigger(
    job: TriggerJob,
    ctx: Data<TriggerContext>,
) -> Result<TriggerOutput, BoxDynError> {
    tracing::info!(
        phase = "picked",
        agent_alias = %job.agent_alias,
        thread_id = %job.thread_id,
        intent = %job.intent,
        "trigger job picked by worker"
    );

    // 1. Resolve agent, backend, cached session
    let (agent, backend, existing_session) = ctx.resolve(&job.agent_alias, None)?;

    // 2. Start or reuse session
    let session = match existing_session {
        Some(s) => {
            tracing::debug!(
                phase = "backend_session",
                agent_alias = %job.agent_alias,
                thread_id = %job.thread_id,
                session_id = %s.id,
                "reusing cached backend session"
            );
            s
        }
        None => {
            tracing::info!(
                phase = "backend_session",
                agent_alias = %job.agent_alias,
                thread_id = %job.thread_id,
                "opening new backend session"
            );
            let new_session = backend.start_session(&agent).await.map_err(|e| {
                tracing::error!(
                    phase = "backend_session",
                    agent_alias = %job.agent_alias,
                    thread_id = %job.thread_id,
                    error = %e,
                    "failed to open backend session"
                );
                e
            })?;
            ctx.cache_session(&job.agent_alias, new_session.clone());
            tracing::info!(
                phase = "backend_session",
                agent_alias = %job.agent_alias,
                thread_id = %job.thread_id,
                session_id = %new_session.id,
                "opened new backend session"
            );
            new_session
        }
    };

    // 3. Build instruction
    let instruction = build_instruction(&job);

    // 4. Call backend
    tracing::info!(
        phase = "backend_execute",
        agent_alias = %job.agent_alias,
        thread_id = %job.thread_id,
        session_id = %session.id,
        "starting backend trigger execution"
    );
    let start = std::time::Instant::now();
    let result = backend
        .trigger(&agent, &session, Some(&instruction))
        .await;
    let duration = start.elapsed();

    match result {
        Ok(trigger_result) => {
            tracing::info!(
                phase = "backend_execute",
                agent_alias = %job.agent_alias,
                thread_id = %job.thread_id,
                session_id = %trigger_result.session_id,
                success = trigger_result.success,
                duration_secs = duration.as_secs(),
                "backend trigger execution finished"
            );
            Ok(TriggerOutput {
                thread_id: job.thread_id,
                agent_alias: job.agent_alias,
                raw_output: trigger_result.output,
                success: trigger_result.success,
                error: if trigger_result.success {
                    None
                } else {
                    Some("backend reported failure".into())
                },
                session_id: trigger_result.session_id,
                duration_secs: duration.as_secs(),
            })
        }
        Err(e) => {
            tracing::error!(
                phase = "backend_execute",
                agent_alias = %job.agent_alias,
                thread_id = %job.thread_id,
                session_id = %session.id,
                error = %e,
                "backend trigger execution failed"
            );
            Ok(TriggerOutput {
                thread_id: job.thread_id,
                agent_alias: job.agent_alias,
                raw_output: None,
                success: false,
                error: Some(e.to_string()),
                session_id: session.id,
                duration_secs: duration.as_secs(),
            })
        }
    }
}

/// Step 2: Parse the trigger output for a JSON auto-reply.
pub async fn parse_reply(output: TriggerOutput) -> Result<ParsedReply, BoxDynError> {
    let reply = parse_trigger_output(&output);
    let parsed_intent = match &reply {
        ParsedReply::ReviewRequest { .. } => "review-request",
        ParsedReply::Completion { .. } => "completion",
        ParsedReply::NoParseable { .. } => "no-parseable",
        ParsedReply::Failed { .. } => "failed",
    };
    let parseable = !matches!(
        &reply,
        ParsedReply::NoParseable { .. } | ParsedReply::Failed { .. }
    );

    tracing::info!(
        phase = "parse",
        agent_alias = %output.agent_alias,
        thread_id = %output.thread_id,
        success = output.success,
        parseable,
        parsed_intent,
        "trigger output parsed"
    );

    Ok(reply)
}

/// Step 3: Update thread state based on the parsed reply.
///
/// Returns the `ParsedReply` so the caller can handle routing (pushing follow-up
/// jobs for review-requests, handoffs, etc.).
pub async fn dispatch_result(
    reply: ParsedReply,
    ctx: Data<TriggerContext>,
) -> Result<ParsedReply, BoxDynError> {
    match &reply {
        ParsedReply::ReviewRequest {
            thread_id,
            from_agent,
            to_alias,
            reply_body,
        } => {
            let to = to_alias.as_deref().unwrap_or("operator");
            tracing::info!(
                phase = "persist",
                thread_id = %thread_id,
                from = %from_agent,
                to = %to,
                intent = "review-request",
                "persisting parsed reply"
            );
            ctx.store
                .insert_message(thread_id, from_agent, to, "review-request", reply_body, None)
                .await?;
            ctx.store
                .update_thread_status(thread_id, "ReviewPending")
                .await?;
        }
        ParsedReply::Completion {
            thread_id,
            from_agent,
            reply_body,
        } => {
            tracing::info!(
                phase = "persist",
                thread_id = %thread_id,
                from = %from_agent,
                intent = "completion",
                "persisting parsed reply and marking thread complete"
            );
            ctx.store
                .insert_message(thread_id, from_agent, "operator", "completion", reply_body, None)
                .await?;
            ctx.store
                .update_thread_status(thread_id, "Completed")
                .await?;
        }
        ParsedReply::NoParseable {
            thread_id,
            agent_alias,
            raw_output,
        } => {
            let body = raw_output.as_deref().unwrap_or("(no parseable output)");
            tracing::warn!(
                phase = "persist",
                thread_id = %thread_id,
                agent_alias = %agent_alias,
                intent = "status-update",
                "no parseable reply; persisting raw output"
            );
            ctx.store
                .insert_message(thread_id, agent_alias, "operator", "status-update", body, None)
                .await?;
            // Thread stays Active — operator can inspect and decide
        }
        ParsedReply::Failed {
            thread_id,
            agent_alias,
            error,
        } => {
            tracing::error!(
                phase = "persist",
                thread_id = %thread_id,
                agent_alias = %agent_alias,
                error = %error,
                intent = "status-update",
                "trigger failed; persisting failure status-update and marking thread failed"
            );
            ctx.store
                .insert_message(thread_id, agent_alias, "operator", "status-update", error, None)
                .await?;
            ctx.store
                .update_thread_status(thread_id, "Failed")
                .await?;
        }
    }
    Ok(reply)
}
