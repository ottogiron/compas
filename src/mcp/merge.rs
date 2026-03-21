//! orch_merge, orch_merge_status, orch_merge_cancel implementations.
//!
//! Exposes merge queue operations to operators via MCP tools. Merge
//! execution itself happens in the worker — these tools only queue,
//! query, and cancel operations.

use std::collections::HashMap;

use rmcp::model::CallToolResult;
use serde::Serialize;

use super::params::{MergeCancelParams, MergeParams, MergeStatusParams};
use super::server::{err_text, json_text, OrchestratorMcpServer};
use crate::merge::MergeExecutor;
use crate::store::MergeOperation;

// ── orch_merge result ────────────────────────────────────────────────────

#[derive(Serialize)]
struct MergeQueuedResult {
    op_id: String,
    thread_id: String,
    source_branch: String,
    target_branch: String,
    strategy: String,
    status: String,
    queue_depth: i64,
    next_step: String,
}

// ── orch_merge_status results ────────────────────────────────────────────

#[derive(Serialize)]
struct MergeOpDetail {
    op_id: String,
    thread_id: String,
    source_branch: String,
    target_branch: String,
    strategy: String,
    requested_by: String,
    status: String,
    queued_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    started_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    finished_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    duration_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result_summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    conflict_files: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    suggested_actions: Option<Vec<String>>,
}

#[derive(Serialize)]
struct MergeOpSummary {
    op_id: String,
    thread_id: String,
    target_branch: String,
    strategy: String,
    status: String,
    queued_at: i64,
}

#[derive(Serialize)]
struct MergeOverview {
    counts: HashMap<String, i64>,
    recent: Vec<MergeOpSummary>,
}

// ── orch_merge_cancel result ─────────────────────────────────────────────

#[derive(Serialize)]
struct MergeCancelResult {
    op_id: String,
    cancelled: bool,
    reason: Option<String>,
}

// ── Conversions ──────────────────────────────────────────────────────────

fn op_to_detail(op: &MergeOperation) -> MergeOpDetail {
    let conflict_files: Option<Vec<String>> = op
        .conflict_files
        .as_ref()
        .and_then(|cf| serde_json::from_str(cf).ok());

    let suggested_actions = if op.status == "failed" {
        let mut actions = Vec::new();
        if conflict_files.is_some() {
            actions.push("Resolve conflicts in the source branch, then re-queue the merge".into());
        }
        actions.push(format!(
            "Re-queue with: orch_merge(thread_id=\"{}\", target_branch=\"{}\")",
            op.thread_id, op.target_branch
        ));
        actions.push(format!(
            "Cancel with: orch_merge_cancel(op_id=\"{}\")",
            op.id
        ));
        Some(actions)
    } else {
        None
    };

    MergeOpDetail {
        op_id: op.id.clone(),
        thread_id: op.thread_id.clone(),
        source_branch: op.source_branch.clone(),
        target_branch: op.target_branch.clone(),
        strategy: op.merge_strategy.clone(),
        requested_by: op.requested_by.clone(),
        status: op.status.clone(),
        queued_at: op.queued_at,
        started_at: op.started_at,
        finished_at: op.finished_at,
        duration_ms: op.duration_ms,
        result_summary: op.result_summary.clone(),
        error_detail: op.error_detail.clone(),
        conflict_files,
        suggested_actions,
    }
}

fn op_to_summary(op: &MergeOperation) -> MergeOpSummary {
    MergeOpSummary {
        op_id: op.id.clone(),
        thread_id: op.thread_id.clone(),
        target_branch: op.target_branch.clone(),
        strategy: op.merge_strategy.clone(),
        status: op.status.clone(),
        queued_at: op.queued_at,
    }
}

// ── Implementations ──────────────────────────────────────────────────────

impl OrchestratorMcpServer {
    // ── orch_merge ────────────────────────────────────────────────────────

    pub async fn merge_impl(&self, params: MergeParams) -> Result<CallToolResult, rmcp::ErrorData> {
        let config = self.config.load();

        let target_branch = params.target_branch.unwrap_or_else(|| "main".to_string());
        let strategy = params
            .strategy
            .unwrap_or_else(|| config.orchestration.default_merge_strategy.clone());

        // Validate strategy
        if !["merge", "rebase", "squash"].contains(&strategy.as_str()) {
            return Ok(err_text(format!(
                "invalid merge strategy '{}' — must be one of: merge, rebase, squash",
                strategy
            )));
        }

        // Resolve repo_root from thread's worktree_repo_root (per-agent workdir),
        // falling back to config.default_workdir for shared-workspace or legacy threads.
        let repo_root = match self.store.get_thread_worktree_info(&params.thread_id).await {
            Ok(Some((_, root))) => root,
            Ok(None) => config.default_workdir.clone(),
            Err(e) => {
                tracing::warn!(thread_id = %params.thread_id, error = %e,
                    "get_thread_worktree_info failed, falling back to default_workdir");
                config.default_workdir.clone()
            }
        };
        let preflight = match MergeExecutor::preflight_check(
            &self.store,
            &params.thread_id,
            &target_branch,
            &repo_root,
        )
        .await
        {
            Ok(pf) => pf,
            Err(e) => return Ok(err_text(e)),
        };

        // Generate ULID and insert
        let op_id = ulid::Ulid::new().to_string();
        let now = chrono::Utc::now().timestamp();

        let op = MergeOperation {
            id: op_id.clone(),
            thread_id: params.thread_id.clone(),
            source_branch: preflight.source_branch.clone(),
            target_branch: target_branch.clone(),
            merge_strategy: strategy.clone(),
            requested_by: params.from,
            status: "queued".to_string(),
            push_requested: false,
            queued_at: now,
            claimed_at: None,
            started_at: None,
            finished_at: None,
            duration_ms: None,
            result_summary: None,
            error_detail: None,
            conflict_files: None,
        };

        if let Err(e) = self.store.insert_merge_op(&op).await {
            return Ok(err_text(format!("failed to queue merge: {}", e)));
        }

        let queue_depth = match self.store.count_queued_merge_ops(&target_branch).await {
            Ok(depth) => depth,
            Err(e) => {
                tracing::warn!(target_branch = %target_branch, error = %e, "count_queued_merge_ops failed, defaulting to 1");
                1
            }
        };

        let next_step = format!("compas wait-merge --op-id {} --timeout 120", op_id);

        Ok(json_text(&MergeQueuedResult {
            op_id,
            thread_id: params.thread_id,
            source_branch: preflight.source_branch,
            target_branch,
            strategy,
            status: "queued".to_string(),
            queue_depth,
            next_step,
        }))
    }

    // ── orch_merge_status ────────────────────────────────────────────────

    pub async fn merge_status_impl(
        &self,
        params: MergeStatusParams,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        // Detail view: specific op_id
        if let Some(op_id) = &params.op_id {
            return match self.store.get_merge_op(op_id).await {
                Ok(Some(op)) => Ok(json_text(&op_to_detail(&op))),
                Ok(None) => Ok(err_text(format!("merge operation '{}' not found", op_id))),
                Err(e) => Ok(err_text(format!("failed to query merge op: {}", e))),
            };
        }

        // Overview: aggregate counts + recent preview
        let counts = match self
            .store
            .count_merge_ops_by_status(params.target_branch.as_deref(), params.thread_id.as_deref())
            .await
        {
            Ok(c) => c,
            Err(e) => return Ok(err_text(format!("failed to count merge operations: {}", e))),
        };

        let recent = match self
            .store
            .list_merge_ops(
                params.target_branch.as_deref(),
                None,
                params.thread_id.as_deref(),
                20,
            )
            .await
        {
            Ok(ops) => ops.iter().map(op_to_summary).collect(),
            Err(e) => return Ok(err_text(format!("failed to list merge operations: {}", e))),
        };

        Ok(json_text(&MergeOverview { counts, recent }))
    }

    // ── orch_merge_cancel ────────────────────────────────────────────────

    pub async fn merge_cancel_impl(
        &self,
        params: MergeCancelParams,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        // Check existence first for a clear "not found" error
        if matches!(self.store.get_merge_op(&params.op_id).await, Ok(None)) {
            return Ok(err_text(format!(
                "merge operation '{}' not found",
                params.op_id
            )));
        }

        match self.store.cancel_merge_op(&params.op_id).await {
            Ok(true) => Ok(json_text(&MergeCancelResult {
                op_id: params.op_id,
                cancelled: true,
                reason: None,
            })),
            Ok(false) => {
                // Re-fetch to get the actual current status (avoids TOCTOU with the pre-check)
                let current_status = self
                    .store
                    .get_merge_op(&params.op_id)
                    .await
                    .ok()
                    .flatten()
                    .map(|op| op.status)
                    .unwrap_or_else(|| "unknown".to_string());
                Ok(err_text(format!(
                    "cannot cancel merge operation '{}' — current status is '{}' (only 'queued' operations can be cancelled)",
                    params.op_id, current_status
                )))
            }
            Err(e) => Ok(err_text(format!("failed to cancel merge op: {}", e))),
        }
    }
}
