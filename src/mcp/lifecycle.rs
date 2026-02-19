//! Lifecycle tool implementations: approve, reject, complete, reopen, abandon.

use rmcp::model::CallToolResult;

use super::params::{AbandonParams, ApproveParams, CompleteParams, RejectParams, ReopenParams};
use super::server::{err_text, json_text, OrchestratorMcpServer};
use crate::store;

impl OrchestratorMcpServer {
    pub(crate) async fn approve_impl(
        &self,
        params: ApproveParams,
    ) -> Result<CallToolResult, rmcp::Error> {
        // Insert approval message
        let message_id = match self
            .store
            .insert_message(
                &params.thread_id,
                &params.from,
                &params.to,
                "approved",
                &format!("Approved by {}", params.from),
                None,
            )
            .await
        {
            Ok(id) => id,
            Err(e) => return Ok(err_text(e)),
        };

        // Generate review token
        let token = uuid::Uuid::new_v4().to_string();
        if let Err(e) = self
            .store
            .set_message_review_token(message_id, &token)
            .await
        {
            return Ok(err_text(e));
        }

        self.wait_registry
            .notify(params.thread_id.clone(), message_id);

        Ok(json_text(&serde_json::json!({
            "status": "approved",
            "thread_id": params.thread_id,
            "token": token,
            "message_ref": store::message_ref(message_id),
        })))
    }

    pub(crate) async fn reject_impl(
        &self,
        params: RejectParams,
    ) -> Result<CallToolResult, rmcp::Error> {
        // Look up batch_id from thread for trigger job context
        let batch_id = self
            .store
            .get_thread_batch_id(&params.thread_id)
            .await
            .ok()
            .flatten();

        let message_id = match self
            .store
            .insert_message(
                &params.thread_id,
                &params.from,
                &params.to,
                "changes-requested",
                &params.feedback,
                batch_id.as_deref(),
            )
            .await
        {
            Ok(id) => id,
            Err(e) => return Ok(err_text(e)),
        };

        self.wait_registry
            .notify(params.thread_id.clone(), message_id);

        // Push trigger job so the agent picks up the rejection and reworks
        let triggered = self
            .maybe_push_trigger(
                &params.thread_id,
                &params.from,
                &params.to,
                "changes-requested",
                &params.feedback,
                batch_id.as_deref(),
            )
            .await;

        let mut val = serde_json::json!({
            "status": "changes_requested",
            "thread_id": params.thread_id,
            "message_ref": store::message_ref(message_id),
        });
        if let Some(job_id) = triggered {
            val["trigger_job_id"] = serde_json::Value::String(job_id);
        }
        Ok(json_text(&val))
    }

    pub(crate) async fn complete_impl(
        &self,
        params: CompleteParams,
    ) -> Result<CallToolResult, rmcp::Error> {
        // TODO: Validate the review token against the stored token
        // For now, trust the caller.

        // Insert completion message
        let message_id = match self
            .store
            .insert_message(
                &params.thread_id,
                &params.from,
                &params.from, // completion is self-directed
                "completion",
                &format!("Thread completed by {}", params.from),
                None,
            )
            .await
        {
            Ok(id) => id,
            Err(e) => return Ok(err_text(e)),
        };

        // Mark thread as completed
        if let Err(e) = self
            .store
            .update_thread_status(&params.thread_id, "Completed")
            .await
        {
            return Ok(err_text(e));
        }

        self.wait_registry
            .notify(params.thread_id.clone(), message_id);

        Ok(json_text(&serde_json::json!({
            "status": "completed",
            "thread_id": params.thread_id,
        })))
    }

    pub(crate) async fn reopen_impl(
        &self,
        params: ReopenParams,
    ) -> Result<CallToolResult, rmcp::Error> {
        if let Err(e) = self
            .store
            .update_thread_status(&params.thread_id, "Active")
            .await
        {
            return Ok(err_text(e));
        }
        Ok(json_text(&serde_json::json!({
            "status": "reopened",
            "thread_id": params.thread_id,
        })))
    }

    pub(crate) async fn abandon_impl(
        &self,
        params: AbandonParams,
    ) -> Result<CallToolResult, rmcp::Error> {
        if let Err(e) = self
            .store
            .update_thread_status(&params.thread_id, "Abandoned")
            .await
        {
            return Ok(err_text(e));
        }
        Ok(json_text(&serde_json::json!({
            "status": "abandoned",
            "thread_id": params.thread_id,
        })))
    }
}
