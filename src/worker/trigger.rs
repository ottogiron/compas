//! TriggerJob definition and execution logic.
//!
//! Cherry-picked from orchestrator.rs execute_trigger() + extract_json_reply().

use serde::{Deserialize, Serialize};

/// Queue name shared between MCP dispatch (producer) and worker (consumer).
pub const TRIGGER_QUEUE: &str = "trigger-queue";

/// A trigger job represents work the worker needs to do:
/// spawn a CLI backend process for a specific agent to handle a message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggerJob {
    /// Which thread this trigger belongs to
    pub thread_id: String,
    /// Target agent alias (e.g., "focused", "chill", "spark")
    pub agent_alias: String,
    /// The message body the agent should process
    pub message_body: String,
    /// Who sent the message
    pub from_alias: String,
    /// Intent that caused this trigger (dispatch, changes-requested, handoff)
    pub intent: String,
    /// Optional batch/ticket ID
    pub batch_id: Option<String>,
}

/// Output of trigger execution (step 1 result).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggerOutput {
    pub thread_id: String,
    pub agent_alias: String,
    pub raw_output: Option<String>,
    pub success: bool,
    pub error: Option<String>,
    pub session_id: String,
    pub duration_secs: u64,
}

/// Parsed auto-reply from agent output (step 2 result).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ParsedReply {
    /// Agent sent a review-request — route to reviewer
    ReviewRequest {
        thread_id: String,
        from_agent: String,
        to_alias: Option<String>,
        reply_body: String,
    },
    /// Agent sent a completion signal
    Completion {
        thread_id: String,
        from_agent: String,
        reply_body: String,
    },
    /// Agent output had no parseable JSON reply
    NoParseable {
        thread_id: String,
        agent_alias: String,
        raw_output: Option<String>,
    },
    /// Trigger execution failed (non-zero exit, timeout, etc.)
    Failed {
        thread_id: String,
        agent_alias: String,
        error: String,
    },
}

/// Auto-reply JSON shape that agents emit.
#[derive(Debug, Deserialize)]
pub struct AgentAutoReply {
    pub intent: String,
    #[serde(default)]
    pub to: Option<String>,
    pub body: String,
    #[serde(default)]
    pub blocking: Option<bool>,
}

/// Build the instruction prompt sent to the agent.
pub fn build_instruction(job: &TriggerJob) -> String {
    format!(
        "You are '{assignee}'.\nThread: {thread}\nFrom: {from}\nTask:\n{task}\n\nIMPORTANT: When you finish, output a JSON object so the orchestrator can route your reply:\n{{\"intent\":\"review-request\",\"to\":\"{from}\",\"body\":\"<your response>\"}}\n\nValid intents: review-request, handoff, status-update, decision-needed.\nThe \"body\" field should contain your full response. The orchestrator parses this JSON to deliver your reply to the correct recipient.",
        assignee = job.agent_alias,
        thread = job.thread_id,
        from = job.from_alias,
        task = job.message_body,
    )
}

/// Extract a JSON auto-reply from agent output.
/// Agents may wrap JSON in markdown fences or surround it with prose text.
/// Tries direct parse first, then scans for a JSON object containing "intent".
pub fn extract_json_reply(output: &str) -> Option<AgentAutoReply> {
    let trimmed = output.trim();

    // Try direct parse first
    if let Ok(v) = serde_json::from_str::<AgentAutoReply>(trimmed) {
        return Some(v);
    }

    // JSONL line-by-line scan
    if trimmed.contains('\n') {
        for line in trimmed.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Ok(v) = serde_json::from_str::<AgentAutoReply>(line) {
                return Some(v);
            }
            if let Ok(wrapper) = serde_json::from_str::<serde_json::Value>(line) {
                if let Some(v) = extract_json_reply_from_value(&wrapper) {
                    return Some(v);
                }
            }
        }
    }

    // Scan for JSON object in the output (handles markdown fences, surrounding text)
    let mut depth = 0i32;
    let mut start = None;
    for (i, ch) in trimmed.char_indices() {
        match ch {
            '{' => {
                if depth == 0 {
                    start = Some(i);
                }
                depth += 1;
            }
            '}' => {
                depth -= 1;
                if depth == 0 {
                    if let Some(s) = start {
                        let candidate = &trimmed[s..=i];
                        if let Ok(v) = serde_json::from_str::<AgentAutoReply>(candidate) {
                            return Some(v);
                        }
                        if let Ok(wrapper) = serde_json::from_str::<serde_json::Value>(candidate) {
                            if let Some(v) = extract_json_reply_from_value(&wrapper) {
                                return Some(v);
                            }
                        }
                    }
                    start = None;
                }
            }
            _ => {}
        }
    }

    None
}

fn extract_json_reply_from_value(value: &serde_json::Value) -> Option<AgentAutoReply> {
    match value {
        serde_json::Value::String(s) => serde_json::from_str::<AgentAutoReply>(s).ok(),
        serde_json::Value::Array(values) => {
            for v in values {
                if let Some(reply) = extract_json_reply_from_value(v) {
                    return Some(reply);
                }
            }
            None
        }
        serde_json::Value::Object(map) => {
            for key in ["result", "text", "body"] {
                if let Some(s) = map.get(key).and_then(|v| v.as_str()) {
                    if let Ok(reply) = serde_json::from_str::<AgentAutoReply>(s) {
                        return Some(reply);
                    }
                }
            }
            for v in map.values() {
                if let Some(reply) = extract_json_reply_from_value(v) {
                    return Some(reply);
                }
            }
            None
        }
        _ => None,
    }
}

/// Parse trigger output into a structured reply.
pub fn parse_trigger_output(output: &TriggerOutput) -> ParsedReply {
    if !output.success {
        return ParsedReply::Failed {
            thread_id: output.thread_id.clone(),
            agent_alias: output.agent_alias.clone(),
            error: output
                .error
                .clone()
                .unwrap_or_else(|| "unknown error".into()),
        };
    }

    let raw = match &output.raw_output {
        Some(o) => o,
        None => {
            return ParsedReply::NoParseable {
                thread_id: output.thread_id.clone(),
                agent_alias: output.agent_alias.clone(),
                raw_output: None,
            };
        }
    };

    match extract_json_reply(raw) {
        Some(reply) => {
            let intent = reply.intent.as_str();
            match intent {
                "review-request" | "handoff" | "status-update" | "decision-needed" => {
                    ParsedReply::ReviewRequest {
                        thread_id: output.thread_id.clone(),
                        from_agent: output.agent_alias.clone(),
                        to_alias: reply.to,
                        reply_body: reply.body,
                    }
                }
                "completion" => ParsedReply::Completion {
                    thread_id: output.thread_id.clone(),
                    from_agent: output.agent_alias.clone(),
                    reply_body: reply.body,
                },
                _ => ParsedReply::ReviewRequest {
                    thread_id: output.thread_id.clone(),
                    from_agent: output.agent_alias.clone(),
                    to_alias: reply.to,
                    reply_body: reply.body,
                },
            }
        }
        None => ParsedReply::NoParseable {
            thread_id: output.thread_id.clone(),
            agent_alias: output.agent_alias.clone(),
            raw_output: Some(raw.clone()),
        },
    }
}

/// Find the largest byte offset <= `max` that is a valid UTF-8 char boundary.
pub fn truncate_to_char_boundary(s: &str, max: usize) -> usize {
    if max >= s.len() {
        return s.len();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    end
}
