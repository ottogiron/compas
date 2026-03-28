//! orch_commit implementation — commit worktree changes for MCP-only agents.

use rmcp::model::CallToolResult;
use serde::Serialize;

use super::params::*;
use super::server::{err_text, json_text, OrchestratorMcpServer};

#[derive(Serialize)]
struct CommitResult {
    commit_sha: String,
    files_changed: usize,
    thread_id: String,
    worktree_path: String,
}

impl OrchestratorMcpServer {
    pub async fn commit_impl(
        &self,
        params: CommitParams,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        // 1. Validate thread exists and is Active
        let status = self
            .store
            .get_thread_status(&params.thread_id)
            .await
            .map_err(|e| format!("failed to get thread status: {}", e));

        let status = match status {
            Ok(Some(s)) => s,
            Ok(None) => {
                return Ok(err_text(format!("thread '{}' not found", params.thread_id)));
            }
            Err(e) => return Ok(err_text(e)),
        };

        if status != "Active" {
            return Ok(err_text(format!(
                "thread '{}' is {} — only Active threads can be committed",
                params.thread_id, status
            )));
        }

        // 2. Get worktree path
        let worktree_path = match self.store.get_thread_worktree_path(&params.thread_id).await {
            Ok(Some(p)) => p,
            Ok(None) => {
                return Ok(err_text(format!(
                    "thread '{}' has no worktree path set",
                    params.thread_id
                )));
            }
            Err(e) => return Ok(err_text(e)),
        };

        if !worktree_path.exists() {
            return Ok(err_text(format!(
                "worktree path does not exist: {}",
                worktree_path.display()
            )));
        }

        // 3. Run git commands in a blocking task
        let thread_id = params.thread_id.clone();
        let message = params.message.clone();
        let wt_path = worktree_path.clone();

        let result = tokio::task::spawn_blocking(move || -> Result<CommitResult, String> {
            // git add -A
            let add_output = std::process::Command::new("git")
                .args(["add", "-A"])
                .current_dir(&wt_path)
                .output()
                .map_err(|e| format!("git add failed: {}", e))?;

            if !add_output.status.success() {
                let stderr = String::from_utf8_lossy(&add_output.stderr);
                return Err(format!("git add failed: {}", stderr));
            }

            // git commit -m <message>
            let commit_output = std::process::Command::new("git")
                .args(["commit", "-m", &message])
                .current_dir(&wt_path)
                .output()
                .map_err(|e| format!("git commit failed: {}", e))?;

            if !commit_output.status.success() {
                let stdout = String::from_utf8_lossy(&commit_output.stdout);
                let stderr = String::from_utf8_lossy(&commit_output.stderr);
                let combined = format!("{}{}", stdout, stderr);
                if combined.contains("nothing to commit") {
                    return Err("nothing to commit — working tree clean".to_string());
                }
                return Err(format!("git commit failed: {}", combined.trim()));
            }

            // git rev-parse HEAD — get commit SHA
            let sha_output = std::process::Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&wt_path)
                .output()
                .map_err(|e| format!("git rev-parse failed: {}", e))?;

            if !sha_output.status.success() {
                let stderr = String::from_utf8_lossy(&sha_output.stderr);
                return Err(format!("git rev-parse HEAD failed: {}", stderr.trim()));
            }

            let commit_sha = String::from_utf8_lossy(&sha_output.stdout)
                .trim()
                .to_string();

            // Count files changed — may fail on first commit (no HEAD~1), so fall back to 0
            let files_changed = std::process::Command::new("git")
                .args(["diff", "--stat", "HEAD~1..HEAD"])
                .current_dir(&wt_path)
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| {
                    let text = String::from_utf8_lossy(&o.stdout);
                    // Per-file stat lines contain " | " (e.g. " file.rs | 5 +++-")
                    text.lines().filter(|l| l.contains(" | ")).count()
                })
                .unwrap_or(0);

            Ok(CommitResult {
                commit_sha,
                files_changed,
                thread_id,
                worktree_path: wt_path.to_string_lossy().to_string(),
            })
        })
        .await
        .map_err(|e| format!("spawn_blocking failed: {}", e));

        match result {
            Ok(Ok(commit_result)) => Ok(json_text(&commit_result)),
            Ok(Err(e)) => Ok(err_text(e)),
            Err(e) => Ok(err_text(e)),
        }
    }
}
