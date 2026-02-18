use crate::model::message::Message;

/// Format a message for notification (plain text).
pub fn format_notification(message: &Message) -> String {
    let fm = &message.frontmatter;
    let mut lines = vec![format!(
        "[{}] {} -> {}",
        fm.intent, fm.from_alias, fm.to_alias
    )];

    if let Some(ref batch) = fm.task_batch {
        lines.push(format!("Batch: {}", batch));
    }
    if let Some(ref thread) = fm.thread_id {
        lines.push(format!("Thread: {}", thread));
    }

    let body_preview = message.body.trim();
    if !body_preview.is_empty() {
        let preview: String = body_preview.chars().take(200).collect();
        lines.push(String::new());
        lines.push(preview);
        if body_preview.len() > 200 {
            lines.push("...".to_string());
        }
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::message::{Intent, MessageFrontmatter, MessageStatus};

    fn test_message(body: &str) -> Message {
        Message {
            frontmatter: MessageFrontmatter {
                session_namespace_id: "default".into(),
                from_alias: "focused".into(),
                to_alias: "operator".into(),
                intent: Intent::ReviewRequest,
                task_batch: Some("B5-T2".into()),
                thread_id: Some("B5-T2-abc123".into()),
                blocking: false,
                status: MessageStatus::New,
                review_token: None,
            },
            body: body.to_string(),
            file_path: None,
            timestamp: None,
        }
    }

    #[test]
    fn test_format_notification_basic() {
        let msg = test_message("## TL;DR\n- Did the thing");
        let formatted = format_notification(&msg);
        assert!(formatted.contains("[review-request]"));
        assert!(formatted.contains("focused -> operator"));
        assert!(formatted.contains("Batch: B5-T2"));
        assert!(formatted.contains("Thread: B5-T2-abc123"));
        assert!(formatted.contains("Did the thing"));
    }

    #[test]
    fn test_format_notification_empty_body() {
        let msg = test_message("");
        let formatted = format_notification(&msg);
        assert!(formatted.contains("[review-request]"));
        assert!(!formatted.contains("\n\n"));
    }
}
