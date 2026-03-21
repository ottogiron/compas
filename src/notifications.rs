//! Desktop notifications for orchestrator events.
//!
//! Subscribes to the [`EventBus`] and sends macOS desktop notifications via
//! `osascript` when agent executions complete or fail. Zero external dependencies.
//!
//! Notifications are fire-and-forget: failures are logged at `debug` level and
//! never propagated. Each `osascript` invocation runs in a nested `tokio::spawn`
//! so a hung process cannot block the consumer loop.

use crate::events::{EventBus, OrchestratorEvent};
use tokio::sync::broadcast;

/// Sanitize a string for safe interpolation into AppleScript.
/// Strips characters that could inject AppleScript commands.
// Only called from `send_macos_notification` (cfg(target_os = "macos")),
// but kept visible for tests on all platforms.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn sanitize_applescript(s: &str) -> String {
    s.replace('\\', "")
        .replace('"', "'")
        .replace(['\n', '\r', '\t'], " ")
}

/// Format a duration in milliseconds to human-readable "Xm Ys" or "Xs".
fn format_duration_ms(ms: i64) -> String {
    let secs = ms.max(0) / 1000;
    if secs >= 60 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}s", secs)
    }
}

/// Determine if an event should trigger a notification.
/// Returns `(title, body)` if yes, `None` if no.
fn should_notify(event: &OrchestratorEvent) -> Option<(String, String)> {
    match event {
        OrchestratorEvent::ExecutionCompleted {
            agent_alias,
            success,
            duration_ms,
            thread_summary,
            ..
        } => {
            let status = if *success { "completed" } else { "failed" };
            let duration = format_duration_ms(*duration_ms);
            let body = match thread_summary {
                Some(summary) if !summary.is_empty() => {
                    format!("\"{}\" — {} in {}", summary, status, duration)
                }
                _ => format!("Execution {} in {}", status, duration),
            };
            Some((format!("compas: {} {}", agent_alias, status), body))
        }
        _ => None,
    }
}

/// Send a macOS desktop notification via osascript.
/// Fire-and-forget — spawns a nested task so a hung osascript
/// can't block the notification consumer loop.
#[cfg(target_os = "macos")]
fn send_macos_notification(title: &str, body: &str) {
    let title = sanitize_applescript(title);
    let body = sanitize_applescript(body);
    let script = format!(r#"display notification "{}" with title "{}""#, body, title);
    // Nested tokio::spawn isolates osascript execution
    tokio::spawn(async move {
        match tokio::process::Command::new("osascript")
            .args(["-e", &script])
            .output()
            .await
        {
            Ok(output) if !output.status.success() => {
                tracing::debug!(
                    stderr = %String::from_utf8_lossy(&output.stderr),
                    "osascript notification failed"
                );
            }
            Err(e) => {
                tracing::debug!(error = %e, "failed to spawn osascript");
            }
            _ => {}
        }
    });
}

/// No-op on non-macOS platforms.
#[cfg(not(target_os = "macos"))]
fn send_macos_notification(_title: &str, _body: &str) {
    tracing::debug!("desktop notifications are only supported on macOS");
}

/// Spawn a task that subscribes to the EventBus and sends desktop notifications.
/// Returns the JoinHandle for the consumer task.
pub fn spawn_notification_consumer(event_bus: &EventBus) -> tokio::task::JoinHandle<()> {
    let mut rx = event_bus.subscribe();
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    if let Some((title, body)) = should_notify(&event) {
                        send_macos_notification(&title, &body);
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::debug!(skipped = n, "notification consumer lagged");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::OrchestratorEvent;

    #[test]
    fn test_sanitize_applescript() {
        // Strips backslashes
        assert_eq!(sanitize_applescript(r#"foo\bar"#), "foobar");
        // Replaces double quotes with single quotes
        assert_eq!(sanitize_applescript(r#"say "hello""#), "say 'hello'");
        // Strips newlines
        assert_eq!(sanitize_applescript("line1\nline2"), "line1 line2");
        // Combined
        assert_eq!(sanitize_applescript("a\\b\"c\nd"), "ab'c d");
    }

    #[test]
    fn test_format_duration_ms() {
        // Zero
        assert_eq!(format_duration_ms(0), "0s");
        // Seconds only
        assert_eq!(format_duration_ms(45_000), "45s");
        // Minutes + seconds
        assert_eq!(format_duration_ms(125_000), "2m 5s");
        // Exact minute boundary
        assert_eq!(format_duration_ms(60_000), "1m 0s");
        // Large value
        assert_eq!(format_duration_ms(3_661_000), "61m 1s");
        // Sub-second (rounds down)
        assert_eq!(format_duration_ms(500), "0s");
    }

    #[test]
    fn test_should_notify_execution_completed_success() {
        let event = OrchestratorEvent::ExecutionCompleted {
            execution_id: "exec-1".to_string(),
            thread_id: "thread-1".to_string(),
            agent_alias: "worker-a".to_string(),
            success: true,
            duration_ms: 125_000,
            thread_summary: None,
        };
        let result = should_notify(&event);
        assert!(result.is_some());
        let (title, body) = result.unwrap();
        assert_eq!(title, "compas: worker-a completed");
        assert_eq!(body, "Execution completed in 2m 5s");
    }

    #[test]
    fn test_should_notify_execution_completed_failure() {
        let event = OrchestratorEvent::ExecutionCompleted {
            execution_id: "exec-2".to_string(),
            thread_id: "thread-2".to_string(),
            agent_alias: "worker-b".to_string(),
            success: false,
            duration_ms: 5_000,
            thread_summary: None,
        };
        let result = should_notify(&event);
        assert!(result.is_some());
        let (title, body) = result.unwrap();
        assert_eq!(title, "compas: worker-b failed");
        assert_eq!(body, "Execution failed in 5s");
    }

    #[test]
    fn test_should_notify_execution_completed_with_summary() {
        let event = OrchestratorEvent::ExecutionCompleted {
            execution_id: "exec-3".to_string(),
            thread_id: "thread-3".to_string(),
            agent_alias: "worker-c".to_string(),
            success: true,
            duration_ms: 90_000,
            thread_summary: Some("Add retry logic to payment service".to_string()),
        };
        let result = should_notify(&event);
        assert!(result.is_some());
        let (title, body) = result.unwrap();
        assert_eq!(title, "compas: worker-c completed");
        assert_eq!(
            body,
            "\"Add retry logic to payment service\" — completed in 1m 30s"
        );
    }

    #[test]
    fn test_should_notify_execution_failed_with_summary() {
        let event = OrchestratorEvent::ExecutionCompleted {
            execution_id: "exec-4".to_string(),
            thread_id: "thread-4".to_string(),
            agent_alias: "worker-d".to_string(),
            success: false,
            duration_ms: 3_000,
            thread_summary: Some("Fix auth middleware".to_string()),
        };
        let result = should_notify(&event);
        assert!(result.is_some());
        let (title, body) = result.unwrap();
        assert_eq!(title, "compas: worker-d failed");
        assert_eq!(body, "\"Fix auth middleware\" — failed in 3s");
    }

    #[test]
    fn test_should_notify_empty_summary_falls_back() {
        let event = OrchestratorEvent::ExecutionCompleted {
            execution_id: "exec-5".to_string(),
            thread_id: "thread-5".to_string(),
            agent_alias: "worker-e".to_string(),
            success: true,
            duration_ms: 10_000,
            thread_summary: Some("".to_string()),
        };
        let result = should_notify(&event);
        assert!(result.is_some());
        let (_title, body) = result.unwrap();
        assert_eq!(body, "Execution completed in 10s");
    }

    #[test]
    fn test_should_notify_ignores_other_events() {
        let events = vec![
            OrchestratorEvent::ExecutionStarted {
                execution_id: "exec-1".to_string(),
                thread_id: "thread-1".to_string(),
                agent_alias: "worker-a".to_string(),
            },
            OrchestratorEvent::ExecutionProgress {
                execution_id: "exec-1".to_string(),
                thread_id: "thread-1".to_string(),
                agent_alias: "worker-a".to_string(),
                summary: "doing stuff".to_string(),
            },
            OrchestratorEvent::ThreadStatusChanged {
                thread_id: "thread-1".to_string(),
                new_status: "Active".to_string(),
            },
            OrchestratorEvent::MessageReceived {
                thread_id: "thread-1".to_string(),
                message_id: 1,
                from_alias: "operator".to_string(),
                intent: "dispatch".to_string(),
            },
        ];
        for event in events {
            assert!(
                should_notify(&event).is_none(),
                "should_notify should return None for {:?}",
                event
            );
        }
    }
}
