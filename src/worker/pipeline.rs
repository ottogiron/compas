//! Stepped workflow pipeline: execute → parse → dispatch.
//!
//! Each step is persisted to SQLite via apalis. If the worker crashes between
//! steps, it resumes at the failed step instead of re-triggering the agent.
//!
//! Step 1 (execute_trigger): Spawn CLI process, wait for output  [slow: 10s-300s]
//! Step 2 (parse_reply):     Parse output for JSON auto-reply    [fast: <1ms]
//! Step 3 (dispatch_result): Route reply / update thread state    [fast: <10ms]

use super::trigger::{parse_trigger_output, ParsedReply, TriggerJob, TriggerOutput};
use apalis::prelude::*;
use tracing;

/// Step 1: Execute the CLI trigger.
///
/// In the real implementation this calls `backend.trigger(&agent, &session, instruction)`.
/// For now, this is the seam where we'll wire in the backend registry.
pub async fn execute_trigger(job: TriggerJob) -> Result<TriggerOutput, BoxDynError> {
    tracing::info!(
        agent = %job.agent_alias,
        thread = %job.thread_id,
        intent = %job.intent,
        "trigger:execute starting"
    );

    // TODO: Wire in real backend execution:
    //   1. Resolve agent config + backend from registry
    //   2. Start or reuse session
    //   3. Build instruction via build_instruction(&job)
    //   4. Call backend.trigger(&agent, &session, Some(&instruction))
    //   5. Capture output + timing

    // Placeholder: simulate execution
    let start = std::time::Instant::now();
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    let duration = start.elapsed();

    let simulated_output = format!(
        r#"{{"intent":"review-request","to":"operator","body":"Completed: {}"}}"#,
        job.message_body
    );

    Ok(TriggerOutput {
        thread_id: job.thread_id,
        agent_alias: job.agent_alias,
        raw_output: Some(simulated_output),
        success: true,
        error: None,
        session_id: "sim-session".to_string(),
        duration_secs: duration.as_secs(),
    })
}

/// Step 2: Parse the trigger output for a JSON auto-reply.
pub async fn parse_reply(output: TriggerOutput) -> Result<ParsedReply, BoxDynError> {
    tracing::info!(
        agent = %output.agent_alias,
        thread = %output.thread_id,
        success = output.success,
        "trigger:parse"
    );

    Ok(parse_trigger_output(&output))
}

/// Step 3: Dispatch the parsed reply (route to reviewer, mark complete, etc.).
pub async fn dispatch_result(reply: ParsedReply) -> Result<(), BoxDynError> {
    match &reply {
        ParsedReply::ReviewRequest {
            thread_id,
            from_agent,
            to_alias,
            reply_body,
        } => {
            tracing::info!(
                thread = %thread_id,
                from = %from_agent,
                to = ?to_alias,
                "trigger:dispatch review-request"
            );
            // TODO: Push a new TriggerJob for the reviewer agent,
            //       or dispatch via the store
            let _ = reply_body;
        }
        ParsedReply::Completion {
            thread_id,
            from_agent,
            ..
        } => {
            tracing::info!(
                thread = %thread_id,
                from = %from_agent,
                "trigger:dispatch completion → mark thread complete"
            );
            // TODO: UPDATE threads SET status = 'Completed' WHERE thread_id = ?
        }
        ParsedReply::NoParseable {
            thread_id,
            agent_alias,
            ..
        } => {
            tracing::warn!(
                thread = %thread_id,
                agent = %agent_alias,
                "trigger:dispatch no parseable reply"
            );
        }
        ParsedReply::Failed {
            thread_id,
            agent_alias,
            error,
        } => {
            tracing::error!(
                thread = %thread_id,
                agent = %agent_alias,
                error = %error,
                "trigger:dispatch failed"
            );
            // TODO: UPDATE threads SET status = 'Failed' WHERE thread_id = ?
        }
    }
    Ok(())
}
