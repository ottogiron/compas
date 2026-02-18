use crate::error::{OrchestratorError, Result};
use crate::model::message::Intent;

/// Valid intent transitions in the orchestration workflow.
/// None -> any starting intent is allowed.
/// From a specific intent, only certain next intents are valid.
pub fn validate_transition(from: Option<&Intent>, to: &Intent) -> Result<()> {
    match from {
        None => {
            // Starting intents: dispatch, handoff, status-update, decision-needed
            match to {
                Intent::Dispatch
                | Intent::Handoff
                | Intent::StatusUpdate
                | Intent::DecisionNeeded => Ok(()),
                _ => Err(OrchestratorError::InvalidTransition {
                    from: "none".into(),
                    to: to.to_string(),
                }),
            }
        }
        Some(prev) => {
            if matches!(to, Intent::StatusUpdate | Intent::DecisionNeeded) {
                return Ok(());
            }
            let valid = match prev {
                Intent::Dispatch => matches!(
                    to,
                    Intent::Handoff
                        | Intent::ReviewRequest
                        | Intent::StatusUpdate
                        | Intent::DecisionNeeded
                ),
                Intent::Handoff => matches!(
                    to,
                    Intent::ReviewRequest
                        | Intent::StatusUpdate
                        | Intent::DecisionNeeded
                        | Intent::Handoff
                ),
                Intent::ReviewRequest => matches!(
                    to,
                    Intent::Approved | Intent::ChangesRequested | Intent::StatusUpdate
                ),
                Intent::Approved => {
                    matches!(to, Intent::Completion | Intent::StatusUpdate)
                }
                Intent::ChangesRequested => matches!(
                    to,
                    Intent::ReviewRequest
                        | Intent::StatusUpdate
                        | Intent::Handoff
                        | Intent::DecisionNeeded
                ),
                Intent::Completion => {
                    matches!(to, Intent::StatusUpdate | Intent::DecisionNeeded)
                }
                Intent::StatusUpdate | Intent::DecisionNeeded => true,
            };

            if valid {
                Ok(())
            } else {
                Err(OrchestratorError::InvalidTransition {
                    from: prev.to_string(),
                    to: to.to_string(),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_starting_dispatch() {
        assert!(validate_transition(None, &Intent::Dispatch).is_ok());
    }

    #[test]
    fn test_starting_handoff() {
        assert!(validate_transition(None, &Intent::Handoff).is_ok());
    }

    #[test]
    fn test_starting_review_request_invalid() {
        assert!(validate_transition(None, &Intent::ReviewRequest).is_err());
    }

    #[test]
    fn test_starting_approved_invalid() {
        assert!(validate_transition(None, &Intent::Approved).is_err());
    }

    #[test]
    fn test_dispatch_to_handoff() {
        assert!(validate_transition(Some(&Intent::Dispatch), &Intent::Handoff).is_ok());
    }

    #[test]
    fn test_dispatch_to_review_request() {
        assert!(validate_transition(Some(&Intent::Dispatch), &Intent::ReviewRequest).is_ok());
    }

    #[test]
    fn test_dispatch_to_approved_invalid() {
        assert!(validate_transition(Some(&Intent::Dispatch), &Intent::Approved).is_err());
    }

    #[test]
    fn test_handoff_to_review_request() {
        assert!(validate_transition(Some(&Intent::Handoff), &Intent::ReviewRequest).is_ok());
    }

    #[test]
    fn test_review_request_to_approved() {
        assert!(validate_transition(Some(&Intent::ReviewRequest), &Intent::Approved).is_ok());
    }

    #[test]
    fn test_review_request_to_changes_requested() {
        assert!(
            validate_transition(Some(&Intent::ReviewRequest), &Intent::ChangesRequested).is_ok()
        );
    }

    #[test]
    fn test_approved_to_completion() {
        assert!(validate_transition(Some(&Intent::Approved), &Intent::Completion).is_ok());
    }

    #[test]
    fn test_approved_to_dispatch_invalid() {
        assert!(validate_transition(Some(&Intent::Approved), &Intent::Dispatch).is_err());
    }

    #[test]
    fn test_changes_requested_to_review_request() {
        assert!(
            validate_transition(Some(&Intent::ChangesRequested), &Intent::ReviewRequest).is_ok()
        );
    }

    #[test]
    fn test_completion_to_dispatch_invalid() {
        assert!(validate_transition(Some(&Intent::Completion), &Intent::Dispatch).is_err());
    }

    #[test]
    fn test_status_update_allows_anything() {
        assert!(validate_transition(Some(&Intent::StatusUpdate), &Intent::Dispatch).is_ok());
        assert!(validate_transition(Some(&Intent::StatusUpdate), &Intent::Completion).is_ok());
    }

    #[test]
    fn test_full_happy_path() {
        // dispatch -> handoff -> review-request -> approved -> completion
        assert!(validate_transition(None, &Intent::Dispatch).is_ok());
        assert!(validate_transition(Some(&Intent::Dispatch), &Intent::Handoff).is_ok());
        assert!(validate_transition(Some(&Intent::Handoff), &Intent::ReviewRequest).is_ok());
        assert!(validate_transition(Some(&Intent::ReviewRequest), &Intent::Approved).is_ok());
        assert!(validate_transition(Some(&Intent::Approved), &Intent::Completion).is_ok());
    }

    #[test]
    fn test_rejection_loop() {
        // review-request -> changes-requested -> review-request -> approved
        assert!(
            validate_transition(Some(&Intent::ReviewRequest), &Intent::ChangesRequested).is_ok()
        );
        assert!(
            validate_transition(Some(&Intent::ChangesRequested), &Intent::ReviewRequest).is_ok()
        );
        assert!(validate_transition(Some(&Intent::ReviewRequest), &Intent::Approved).is_ok());
    }
}
