//! Lifecycle tool implementations: approve, reject, complete, reopen, abandon.

use rmcp::model::CallToolResult;
use sqlx::{Executor, Row};

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
        let triggered = match self
            .maybe_push_trigger(
                &params.thread_id,
                &params.from,
                &params.to,
                "changes-requested",
                &params.feedback,
                batch_id.as_deref(),
            )
            .await
        {
            Ok(job_id) => job_id,
            Err(e) => return Ok(err_text(e)),
        };

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
        let mut tx = match self.store.pool().begin().await {
            Ok(tx) => tx,
            Err(e) => return Ok(err_text(e)),
        };

        let approval_row: (i64, String, String) = match sqlx::query_as(
            "SELECT id, thread_id, to_alias
             FROM messages
             WHERE review_token = ? AND intent = 'approved'
             ORDER BY id DESC
             LIMIT 1",
        )
        .bind(&params.token)
        .fetch_optional(&mut *tx)
        .await
        {
            Ok(Some(row)) => row,
            Ok(None) => return Ok(err_text("invalid review token: not found or already used")),
            Err(e) => return Ok(err_text(e)),
        };

        let approval_id = approval_row.0;
        let approval_thread_id = approval_row.1;
        let issued_to = approval_row.2;

        if approval_thread_id != params.thread_id {
            return Ok(err_text("invalid review token: token does not belong to this thread"));
        }
        if issued_to != params.from {
            return Ok(err_text(
                "invalid review token: token was not issued to this agent",
            ));
        }

        let consumed = match sqlx::query(
            "UPDATE messages
             SET review_token = NULL
             WHERE id = ? AND review_token = ?",
        )
        .bind(approval_id)
        .bind(&params.token)
        .execute(&mut *tx)
        .await
        {
            Ok(done) => done,
            Err(e) => return Ok(err_text(e)),
        };
        if consumed.rows_affected() != 1 {
            return Ok(err_text("invalid review token: not found or already used"));
        }

        let completion_body = format!("Thread completed by {}", params.from);
        let completion_row = match sqlx::query(
            "INSERT INTO messages (thread_id, from_alias, to_alias, intent, body, batch_id)
             VALUES (?, ?, ?, 'completion', ?, NULL)
             RETURNING id",
        )
        .bind(&params.thread_id)
        .bind(&params.from)
        .bind(&params.from)
        .bind(&completion_body)
        .fetch_one(&mut *tx)
        .await
        {
            Ok(row) => row,
            Err(e) => return Ok(err_text(e)),
        };
        let message_id: i64 = completion_row.get(0);

        if let Err(e) = tx
            .execute(
                sqlx::query("UPDATE threads SET status = 'Completed' WHERE thread_id = ?")
                    .bind(&params.thread_id),
            )
            .await
        {
            return Ok(err_text(e));
        }
        if let Err(e) = tx.commit().await {
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

#[cfg(test)]
mod tests {
    use crate::backend::registry::BackendRegistry;
    use crate::config::types::{AgentConfig, AgentRole, OrchestratorConfig, OrchestrationConfig};
    use crate::mcp::params::{ApproveParams, CompleteParams};
    use crate::mcp::server::OrchestratorMcpServer;
    use crate::store::Store;
    use apalis_sqlite::SqlitePool;

    async fn test_server() -> OrchestratorMcpServer {
        let pool = SqlitePool::connect("sqlite::memory:")
            .await
            .expect("sqlite::memory should connect");
        let store = Store::new(pool);
        store.setup().await.expect("store setup should succeed");

        let config = OrchestratorConfig {
            state_dir: "/tmp/test-orch".into(),
            db_path: ".aster-orch/jobs.sqlite".into(),
            poll_interval_secs: 1,
            models: None,
            agents: vec![AgentConfig {
                alias: "focused".into(),
                identity: "Test Agent".into(),
                backend: "stub".into(),
                role: AgentRole::Worker,
                model: None,
                models: None,
                preferred_models: None,
                prompt: None,
                prompt_file: None,
                timeout_secs: None,
                backend_args: None,
                env: None,
            }],
            orchestration: OrchestrationConfig::default(),
            apalis: Default::default(),
            telegram: None,
            audit_log_path: None,
        };

        OrchestratorMcpServer::new(config, store, BackendRegistry::new())
    }

    async fn approve_and_get_token(
        server: &OrchestratorMcpServer,
        thread_id: &str,
        reviewer: &str,
        author: &str,
    ) -> String {
        server
            .approve_impl(ApproveParams {
                thread_id: thread_id.to_string(),
                from: reviewer.to_string(),
                to: author.to_string(),
            })
            .await
            .expect("approve call should return");

        let msgs = server
            .store
            .get_thread_messages(thread_id)
            .await
            .expect("thread messages should load");
        msgs.into_iter()
            .find(|m| m.intent == "approved")
            .and_then(|m| m.review_token)
            .expect("approval token should exist")
    }

    #[tokio::test]
    async fn test_complete_requires_valid_token() {
        let server = test_server().await;
        let thread_id = "t-complete-invalid-token";
        let _token = approve_and_get_token(&server, thread_id, "operator", "focused").await;

        server
            .complete_impl(CompleteParams {
                thread_id: thread_id.to_string(),
                from: "focused".to_string(),
                token: "not-a-real-token".to_string(),
            })
            .await
            .expect("complete call should return");

        let status = server
            .store
            .get_thread_status(thread_id)
            .await
            .expect("status read should succeed");
        assert_eq!(status.as_deref(), Some("Active"));
        let msgs = server
            .store
            .get_thread_messages(thread_id)
            .await
            .expect("thread messages should load");
        assert_eq!(msgs.iter().filter(|m| m.intent == "completion").count(), 0);
    }

    #[tokio::test]
    async fn test_complete_rejects_token_for_other_agent() {
        let server = test_server().await;
        let thread_id = "t-complete-wrong-agent";
        let token = approve_and_get_token(&server, thread_id, "operator", "focused").await;

        server
            .complete_impl(CompleteParams {
                thread_id: thread_id.to_string(),
                from: "spark".to_string(),
                token,
            })
            .await
            .expect("complete call should return");

        let status = server
            .store
            .get_thread_status(thread_id)
            .await
            .expect("status read should succeed");
        assert_eq!(status.as_deref(), Some("Active"));
        let msgs = server
            .store
            .get_thread_messages(thread_id)
            .await
            .expect("thread messages should load");
        assert_eq!(msgs.iter().filter(|m| m.intent == "completion").count(), 0);
    }

    #[tokio::test]
    async fn test_complete_consumes_token() {
        let server = test_server().await;
        let thread_id = "t-complete-consume-token";
        let token = approve_and_get_token(&server, thread_id, "operator", "focused").await;

        server
            .complete_impl(CompleteParams {
                thread_id: thread_id.to_string(),
                from: "focused".to_string(),
                token: token.clone(),
            })
            .await
            .expect("first complete call should return");

        let status = server
            .store
            .get_thread_status(thread_id)
            .await
            .expect("status read should succeed");
        assert_eq!(status.as_deref(), Some("Completed"));

        let msgs = server
            .store
            .get_thread_messages(thread_id)
            .await
            .expect("thread messages should load");
        let approval = msgs
            .iter()
            .find(|m| m.intent == "approved")
            .expect("approval message should exist");
        assert_eq!(approval.review_token, None);
        assert_eq!(msgs.iter().filter(|m| m.intent == "completion").count(), 1);

        server
            .complete_impl(CompleteParams {
                thread_id: thread_id.to_string(),
                from: "focused".to_string(),
                token,
            })
            .await
            .expect("second complete call should return");

        let msgs_after = server
            .store
            .get_thread_messages(thread_id)
            .await
            .expect("thread messages should load");
        assert_eq!(
            msgs_after.iter().filter(|m| m.intent == "completion").count(),
            1
        );
    }
}
