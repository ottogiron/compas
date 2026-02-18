use crate::model::message::Intent;

/// Check if an intent should trigger a notification.
pub fn should_notify(intent: &Intent) -> bool {
    matches!(
        intent,
        Intent::ReviewRequest | Intent::Completion | Intent::DecisionNeeded
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_notification_filter() {
        assert!(should_notify(&Intent::ReviewRequest));
        assert!(should_notify(&Intent::Completion));
        assert!(should_notify(&Intent::DecisionNeeded));
        assert!(!should_notify(&Intent::Dispatch));
        assert!(!should_notify(&Intent::Handoff));
        assert!(!should_notify(&Intent::StatusUpdate));
        assert!(!should_notify(&Intent::Approved));
        assert!(!should_notify(&Intent::ChangesRequested));
    }
}
