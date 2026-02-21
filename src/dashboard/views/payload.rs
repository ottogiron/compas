//! Payload formatting helpers for dashboard details panes.

/// Format a payload as display lines.
///
/// - When `pretty_json` is true and the payload parses as JSON, output
///   pretty-printed JSON.
/// - Otherwise, return the payload as-is.
/// - Always clamp to `max_lines`, appending a truncation marker if needed.
pub fn format_payload_lines(raw: &str, pretty_json: bool, max_lines: usize) -> Vec<String> {
    let normalized = raw.replace("\r\n", "\n");
    let rendered = if pretty_json {
        maybe_pretty_json(&normalized).unwrap_or(normalized)
    } else {
        normalized
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

/// Expand a log line into one or more display lines when pretty mode is on.
pub fn format_log_line(line: &str, pretty_json: bool) -> Vec<String> {
    if !pretty_json {
        return vec![line.to_string()];
    }
    match maybe_pretty_json(line) {
        Some(pretty) => pretty.lines().map(str::to_string).collect(),
        None => vec![line.to_string()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_payload_pretty_json() {
        let out = format_payload_lines("{\"a\":1}", true, 10);
        assert_eq!(out[0], "{");
        assert!(out.iter().any(|l| l.contains("\"a\": 1")));
    }

    #[test]
    fn test_format_payload_invalid_json_falls_back() {
        let out = format_payload_lines("{bad", true, 10);
        assert_eq!(out, vec!["{bad"]);
    }

    #[test]
    fn test_format_payload_truncates() {
        let out = format_payload_lines("a\nb\nc\nd", false, 3);
        assert_eq!(out.len(), 3);
        assert_eq!(out[2], "... (truncated)");
    }

    #[test]
    fn test_format_log_line_pretty_json() {
        let out = format_log_line("{\"ok\":true}", true);
        assert!(out.len() > 1);
    }
}
