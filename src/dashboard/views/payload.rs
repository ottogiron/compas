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
}
