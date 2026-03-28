//! Payload formatting helpers for dashboard details panes.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonViewMode {
    Humanized,
    RawPretty,
}

/// Format a payload as display lines.
///
/// - In `Humanized` mode and valid JSON, render flattened key/value lines.
/// - In `RawPretty` mode and valid JSON, render pretty-printed JSON.
/// - Otherwise, return payload as-is.
/// - Always clamp to `max_lines`, appending a truncation marker if needed.
pub fn format_payload_lines(raw: &str, mode: JsonViewMode, max_lines: usize) -> Vec<String> {
    let normalized = raw.replace("\r\n", "\n");
    let rendered = match mode {
        JsonViewMode::Humanized => humanize_json_lines(&normalized)
            .map(|lines| lines.join("\n"))
            .unwrap_or(normalized),
        JsonViewMode::RawPretty => maybe_pretty_json(&normalized).unwrap_or(normalized),
    };

    let mut lines: Vec<String> = rendered.lines().map(str::to_string).collect();
    if lines.is_empty() {
        return vec!["(empty)".to_string()];
    }

    if max_lines == 0 {
        return vec![];
    }
    if lines.len() > max_lines {
        lines.truncate(max_lines.saturating_sub(1).max(1));
        lines.push("... (truncated)".to_string());
    }
    lines
}

/// Pretty-print a single JSON payload string.
pub fn maybe_pretty_json(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    serde_json::to_string_pretty(&value).ok()
}

/// Expand a log line into one or more display lines in the selected JSON mode.
pub fn format_log_line(line: &str, mode: JsonViewMode) -> Vec<String> {
    match mode {
        JsonViewMode::Humanized => {
            humanize_json_lines(line).unwrap_or_else(|| vec![line.to_string()])
        }
        JsonViewMode::RawPretty => match maybe_pretty_json(line) {
            Some(pretty) => pretty.lines().map(str::to_string).collect(),
            None => vec![line.to_string()],
        },
    }
}

/// Extract human-readable content from a JSONL log line.
///
/// Returns `None` for lines that should be skipped (protocol events, tool calls,
/// thinking blocks). Returns `Some(lines)` for meaningful agent narrative text.
///
/// This is used by the Output tab in Humanized mode to show the agent's text
/// instead of raw protocol JSON.
pub fn extract_content_from_log_line(line: &str) -> Option<Vec<String>> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    // stderr lines pass through as-is.
    if trimmed.starts_with("[stderr]") {
        return Some(vec![trimmed.to_string()]);
    }

    // Non-JSON lines pass through as-is.
    let value: serde_json::Value = match serde_json::from_str(trimmed) {
        Ok(v) => v,
        Err(_) => return Some(vec![trimmed.to_string()]),
    };

    let event_type = value.get("type").and_then(|t| t.as_str()).unwrap_or("");

    match event_type {
        // Skip protocol/init events.
        "system" | "rate_limit_event" => None,

        // Skip user messages (tool results — already in Timeline).
        "user" => None,

        // Assistant messages: extract text blocks only.
        "assistant" => {
            let content = value
                .pointer("/message/content")
                .and_then(|c| c.as_array())?;
            let texts: Vec<String> = content
                .iter()
                .filter(|item| item.get("type").and_then(|t| t.as_str()) == Some("text"))
                .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
                .filter(|t| !t.trim().is_empty())
                .flat_map(|t| t.lines().map(str::to_string).collect::<Vec<_>>())
                .collect();
            if texts.is_empty() {
                None
            } else {
                Some(texts)
            }
        }

        // Result event: extract the result field.
        "result" => {
            let result_text = value.get("result").and_then(|r| r.as_str())?;
            if result_text.trim().is_empty() {
                return None;
            }
            Some(result_text.lines().map(str::to_string).collect())
        }

        // OpenCode streaming: extract part.text delta.
        "content.delta" => {
            let text = value.pointer("/part/text").and_then(|v| v.as_str())?;
            if text.is_empty() {
                return None;
            }
            Some(vec![text.to_string()])
        }

        // Codex streaming: extract item.text.
        "item.completed" => {
            let text = value.pointer("/item/text").and_then(|v| v.as_str())?;
            if text.is_empty() {
                return None;
            }
            Some(text.lines().map(str::to_string).collect())
        }

        // Unrecognized JSON event types — skip.
        _ => None,
    }
}

pub fn humanize_json_lines(raw: &str) -> Option<Vec<String>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    let mut out = Vec::new();
    flatten_json("", &value, 0, 5, &mut out);
    if out.is_empty() {
        out.push("(empty)".to_string());
    }
    Some(out)
}

fn flatten_json(
    prefix: &str,
    value: &serde_json::Value,
    depth: usize,
    max_depth: usize,
    out: &mut Vec<String>,
) {
    if depth > max_depth {
        out.push(format!("{}: <max depth reached>", label(prefix)));
        return;
    }
    match value {
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            for key in keys {
                let child = if prefix.is_empty() {
                    key.to_string()
                } else {
                    format!("{}.{}", prefix, key)
                };
                if let Some(v) = map.get(key) {
                    flatten_json(&child, v, depth + 1, max_depth, out);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for (idx, item) in items.iter().enumerate() {
                let child = if prefix.is_empty() {
                    format!("items[{}]", idx)
                } else {
                    format!("{}[{}]", prefix, idx)
                };
                flatten_json(&child, item, depth + 1, max_depth, out);
            }
        }
        _ => {
            out.push(format!("{}: {}", label(prefix), scalar_to_string(value)));
        }
    }
}

fn label(prefix: &str) -> &str {
    if prefix.is_empty() {
        "value"
    } else {
        prefix
    }
}

fn scalar_to_string(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Null => "null".to_string(),
        _ => v.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_payload_pretty_json() {
        let out = format_payload_lines("{\"a\":1}", JsonViewMode::RawPretty, 10);
        assert_eq!(out[0], "{");
        assert!(out.iter().any(|l| l.contains("\"a\": 1")));
    }

    #[test]
    fn test_format_payload_humanized_json() {
        let out = format_payload_lines(
            "{\"meta\":{\"agent\":\"focused\"},\"ok\":true}",
            JsonViewMode::Humanized,
            10,
        );
        assert!(out.contains(&"meta.agent: focused".to_string()));
        assert!(out.contains(&"ok: true".to_string()));
    }

    #[test]
    fn test_format_payload_invalid_json_falls_back() {
        let out = format_payload_lines("{bad", JsonViewMode::Humanized, 10);
        assert_eq!(out, vec!["{bad"]);
    }

    #[test]
    fn test_format_payload_truncates() {
        let out = format_payload_lines("a\nb\nc\nd", JsonViewMode::RawPretty, 3);
        assert_eq!(out.len(), 3);
        assert_eq!(out[2], "... (truncated)");
    }

    #[test]
    fn test_format_log_line_pretty_json() {
        let out = format_log_line("{\"ok\":true}", JsonViewMode::RawPretty);
        assert!(out.len() > 1);
    }

    // ── extract_content_from_log_line tests ─────────────────────────────────

    #[test]
    fn test_extract_content_assistant_text() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Hello world\nSecond line"}]}}"#;
        let result = extract_content_from_log_line(line).unwrap();
        assert_eq!(result, vec!["Hello world", "Second line"]);
    }

    #[test]
    fn test_extract_content_assistant_thinking_only_skipped() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"Let me think..."}]}}"#;
        assert!(extract_content_from_log_line(line).is_none());
    }

    #[test]
    fn test_extract_content_assistant_tool_use_only_skipped() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"ls"}}]}}"#;
        assert!(extract_content_from_log_line(line).is_none());
    }

    #[test]
    fn test_extract_content_assistant_mixed_extracts_text_only() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"hmm"},{"type":"text","text":"The answer is 42"},{"type":"tool_use","name":"Write","input":{}}]}}"#;
        let result = extract_content_from_log_line(line).unwrap();
        assert_eq!(result, vec!["The answer is 42"]);
    }

    #[test]
    fn test_extract_content_result_event() {
        let line = r#"{"type":"result","subtype":"success","result":"All done.\nFiles updated."}"#;
        let result = extract_content_from_log_line(line).unwrap();
        assert_eq!(result, vec!["All done.", "Files updated."]);
    }

    #[test]
    fn test_extract_content_skip_system() {
        let line = r#"{"type":"system","subtype":"init","cwd":"/tmp","tools":["Bash"]}"#;
        assert!(extract_content_from_log_line(line).is_none());
    }

    #[test]
    fn test_extract_content_skip_rate_limit() {
        let line = r#"{"type":"rate_limit_event","retry_after":5}"#;
        assert!(extract_content_from_log_line(line).is_none());
    }

    #[test]
    fn test_extract_content_skip_user() {
        let line =
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result"}]}}"#;
        assert!(extract_content_from_log_line(line).is_none());
    }

    #[test]
    fn test_extract_content_non_json_passthrough() {
        let line = "some plain text output";
        let result = extract_content_from_log_line(line).unwrap();
        assert_eq!(result, vec!["some plain text output"]);
    }

    #[test]
    fn test_extract_content_stderr_passthrough() {
        let line = "[stderr] warning: something happened";
        let result = extract_content_from_log_line(line).unwrap();
        assert_eq!(result, vec!["[stderr] warning: something happened"]);
    }

    #[test]
    fn test_extract_content_opencode_delta() {
        let line = r#"{"type":"content.delta","part":{"text":"streaming chunk"}}"#;
        let result = extract_content_from_log_line(line).unwrap();
        assert_eq!(result, vec!["streaming chunk"]);
    }

    #[test]
    fn test_extract_content_codex_item_completed() {
        let line = r#"{"type":"item.completed","item":{"text":"Done.\nAll good."}}"#;
        let result = extract_content_from_log_line(line).unwrap();
        assert_eq!(result, vec!["Done.", "All good."]);
    }

    #[test]
    fn test_extract_content_unknown_type_skipped() {
        let line = r#"{"type":"some_unknown_event","data":"stuff"}"#;
        assert!(extract_content_from_log_line(line).is_none());
    }

    #[test]
    fn test_extract_content_empty_line_skipped() {
        assert!(extract_content_from_log_line("").is_none());
        assert!(extract_content_from_log_line("   ").is_none());
    }

    #[test]
    fn test_extract_content_result_empty_text_skipped() {
        let line = r#"{"type":"result","result":"  "}"#;
        assert!(extract_content_from_log_line(line).is_none());
    }
}
