//! Query tool implementations: status, transcript, read, metrics, diagnose, batch_status.

use rmcp::model::CallToolResult;

use super::params::{
    BatchStatusParams, DiagnoseParams, MetricsParams, ReadParams, StatusParams, TranscriptParams,
};
use super::server::{err_text, json_text, OrchestratorMcpServer};
use crate::store;

impl OrchestratorMcpServer {
    pub(crate) async fn status_impl(
        &self,
        params: StatusParams,
    ) -> Result<CallToolResult, rmcp::Error> {
        match self
            .store
            .list_messages(params.agent.as_deref(), params.thread_id.as_deref())
            .await
        {
            Ok(messages) => {
                let summaries: Vec<serde_json::Value> = messages
                    .iter()
                    .map(|m| {
                        serde_json::json!({
                            "id": m.id,
                            "from": m.from_alias,
                            "to": m.to_alias,
                            "intent": m.intent,
                            "thread_id": m.thread_id,
                            "status": m.status,
                            "message_ref": store::message_ref(m.id),
                        })
                    })
                    .collect();
                Ok(json_text(&summaries))
            }
            Err(e) => Ok(err_text(e)),
        }
    }

    pub(crate) async fn transcript_impl(
        &self,
        params: TranscriptParams,
    ) -> Result<CallToolResult, rmcp::Error> {
        match self.store.get_thread_messages(&params.thread_id).await {
            Ok(messages) => {
                let summaries: Vec<serde_json::Value> = messages
                    .iter()
                    .map(|m| {
                        serde_json::json!({
                            "id": m.id,
                            "from": m.from_alias,
                            "to": m.to_alias,
                            "intent": m.intent,
                            "body": m.body,
                            "message_ref": store::message_ref(m.id),
                        })
                    })
                    .collect();
                Ok(json_text(&summaries))
            }
            Err(e) => Ok(err_text(e)),
        }
    }

    pub(crate) async fn read_impl(
        &self,
        params: ReadParams,
    ) -> Result<CallToolResult, rmcp::Error> {
        let id = match store::parse_message_ref(&params.reference) {
            Ok(id) => id,
            Err(e) => return Ok(err_text(e)),
        };
        match self.store.get_message(id).await {
            Ok(Some(msg)) => {
                let val = serde_json::json!({
                    "id": msg.id,
                    "from": msg.from_alias,
                    "to": msg.to_alias,
                    "intent": msg.intent,
                    "thread_id": msg.thread_id,
                    "batch": msg.batch_id,
                    "status": msg.status,
                    "body": msg.body,
                    "message_ref": store::message_ref(msg.id),
                });
                Ok(json_text(&val))
            }
            Ok(None) => Ok(err_text(format!("message {} not found", params.reference))),
            Err(e) => Ok(err_text(e)),
        }
    }

    pub(crate) async fn metrics_impl(
        &self,
        _params: MetricsParams,
    ) -> Result<CallToolResult, rmcp::Error> {
        match self.store.metrics().await {
            Ok(m) => Ok(json_text(&serde_json::json!({
                "total_messages": m.total_messages,
                "pending_messages": m.pending_messages,
                "active_threads": m.active_threads,
                "completed_threads": m.completed_threads,
                "failed_threads": m.failed_threads,
                "abandoned_threads": m.abandoned_threads,
            }))),
            Err(e) => Ok(err_text(e)),
        }
    }

    pub(crate) async fn diagnose_impl(
        &self,
        params: DiagnoseParams,
    ) -> Result<CallToolResult, rmcp::Error> {
        let thread_id = &params.thread_id;
        let status = self.store.get_thread_status(thread_id).await.ok().flatten();
        let messages = self
            .store
            .get_thread_messages(thread_id)
            .await
            .unwrap_or_default();
        let last_msg = messages.last();
        let last_intent = self
            .store
            .latest_thread_intent(thread_id)
            .await
            .ok()
            .flatten();

        let val = serde_json::json!({
            "thread_id": thread_id,
            "thread_status": status.unwrap_or_else(|| "Active".into()),
            "message_count": messages.len(),
            "last_intent": last_intent,
            "last_message_from": last_msg.map(|m| &m.from_alias),
            "last_message_to": last_msg.map(|m| &m.to_alias),
        });
        Ok(json_text(&val))
    }

    pub(crate) async fn batch_status_impl(
        &self,
        params: BatchStatusParams,
    ) -> Result<CallToolResult, rmcp::Error> {
        let threads = match self.store.get_batch_threads(&params.batch_id).await {
            Ok(t) => t,
            Err(e) => return Ok(err_text(e)),
        };
        let messages = match self.store.get_batch_messages(&params.batch_id).await {
            Ok(m) => m,
            Err(e) => return Ok(err_text(e)),
        };

        let thread_summaries: Vec<serde_json::Value> = threads
            .iter()
            .map(|t| {
                serde_json::json!({
                    "thread_id": t.thread_id,
                    "status": t.status,
                })
            })
            .collect();

        let val = serde_json::json!({
            "batch_id": params.batch_id,
            "thread_count": threads.len(),
            "message_count": messages.len(),
            "threads": thread_summaries,
        });
        Ok(json_text(&val))
    }
}
