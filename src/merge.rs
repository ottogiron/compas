//! Merge execution engine with temporary worktree isolation.
//!
//! All merge operations run in disposable worktrees under
//! `{repo_root}/.compas-worktrees/merge-{op_id}/`. After a successful merge,
//! the operator's working tree is synced via `git reset --keep` when the target
//! branch is currently checked out.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::store::{MergeOperation, Store};
use crate::worktree::WorktreeManager;

/// Result of a merge execution attempt.
#[derive(Debug, Clone)]
pub struct MergeResult {
    pub success: bool,
    pub summary: Option<String>,
    pub error: Option<String>,
    pub conflict_files: Option<Vec<String>>,
}

/// Result of a preflight check — metadata needed to queue a merge.
#[derive(Debug, Clone)]
pub struct PreflightResult {
    pub source_branch: String,
    pub target_branch: String,
    pub thread_id: String,
}

/// Merge executor — runs git merge operations in temporary worktrees.
pub struct MergeExecutor;

impl MergeExecutor {
    /// Validate preconditions before queuing a merge.
    ///
    /// Checks:
    /// 1. Thread exists and is `Active`, `Completed`, or `Failed` (not Abandoned)
    /// 2. Source branch `compas/{thread_id}` exists in the git repo
    /// 3. Thread's worktree is clean (no uncommitted changes)
    /// 4. No existing queued/claiming/executing merge for same (thread_id, target_branch)
    pub async fn preflight_check(
        store: &Store,
        thread_id: &str,
        target_branch: &str,
        repo_root: &Path,
    ) -> Result<PreflightResult, String> {
        // 1. Thread must exist and be in a merge-eligible status
        let status = store
            .get_thread_status(thread_id)
            .await
            .map_err(|e| format!("failed to query thread status: {}", e))?
            .ok_or_else(|| format!("thread '{}' not found", thread_id))?;

        match status.as_str() {
            "Active" | "Completed" | "Failed" => {} // eligible
            "Abandoned" => {
                return Err(format!(
                    "thread '{}' is Abandoned and cannot be merged",
                    thread_id
                ));
            }
            other => {
                return Err(format!(
                    "thread '{}' has unexpected status '{}' — only Active, Completed, or Failed threads can be merged",
                    thread_id, other
                ));
            }
        }

        // 2. Source branch must exist in the git repo
        let source_branch = format!("compas/{}", thread_id);
        let repo_str = repo_root.to_string_lossy().to_string();

        let branch_check = Command::new("git")
            .args(["-C", &repo_str, "rev-parse", "--verify", &source_branch])
            .output()
            .map_err(|e| format!("failed to check source branch: {}", e))?;

        if !branch_check.status.success() {
            return Err(format!(
                "source branch '{}' does not exist in repository",
                source_branch
            ));
        }

        // 3. Thread's worktree must be clean (if it exists)
        let worktree_path = store
            .get_thread_worktree_path(thread_id)
            .await
            .map_err(|e| format!("failed to query worktree path: {}", e))?;

        if let Some(wt_path) = worktree_path {
            // worktree_status runs blocking git commands — move off the async runtime
            let wt_status =
                tokio::task::spawn_blocking(move || WorktreeManager::worktree_status(&wt_path))
                    .await
                    .map_err(|e| format!("worktree status task panicked: {}", e))?;

            match wt_status {
                Ok(Some(status_detail)) => {
                    return Err(format!(
                        "thread '{}' worktree has uncommitted changes — commit or discard before merging\n{}",
                        thread_id, status_detail
                    ));
                }
                Ok(None) => {} // clean or missing — ok
                Err(e) => {
                    return Err(format!(
                        "failed to check worktree status for thread '{}': {}",
                        thread_id, e
                    ));
                }
            }
        }

        // 4. No pending merge for same (thread_id, target_branch)
        let has_pending = store
            .has_pending_merge_for_thread(thread_id, target_branch)
            .await?;

        if has_pending {
            return Err(format!(
                "thread '{}' already has a pending merge to '{}' — wait for it to complete or cancel it",
                thread_id, target_branch
            ));
        }

        Ok(PreflightResult {
            source_branch,
            target_branch: target_branch.to_string(),
            thread_id: thread_id.to_string(),
        })
    }

    /// Count commits on `source` not yet in `target`.
    /// Returns 0 when source is identical to or behind target.
    fn commits_ahead(repo_root: &Path, target: &str, source: &str) -> Result<u64, String> {
        let repo_str = repo_root.to_string_lossy();
        let range = format!("{}..{}", target, source);
        let output = Command::new("git")
            .args(["-C", &repo_str, "rev-list", "--count", &range])
            .output()
            .map_err(|e| format!("failed to run git rev-list: {}", e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("git rev-list failed: {}", stderr.trim()));
        }

        let count_str = String::from_utf8_lossy(&output.stdout);
        count_str
            .trim()
            .parse::<u64>()
            .map_err(|e| format!("failed to parse rev-list count: {}", e))
    }

    /// Execute a merge operation in a temporary worktree.
    ///
    /// The merge happens in `.compas-worktrees/merge-{op.id}` using a temporary
    /// branch (git forbids two worktrees on the same branch). On success:
    ///
    /// - If the target branch is checked out in the main repo, `git reset --keep`
    ///   advances the ref, index, and working tree together. Aborts safely if
    ///   uncommitted changes conflict with merged files.
    /// - If the target branch is NOT checked out, `git update-ref` moves only
    ///   the branch pointer (no working tree to sync).
    ///
    /// `thread_worktree_path` is the agent's worktree (if any). When provided,
    /// the executor checks for uncommitted changes before proceeding.
    pub fn execute(
        op: &MergeOperation,
        repo_root: &Path,
        thread_worktree_path: Option<&Path>,
    ) -> Result<MergeResult, String> {
        // Pre-merge validation: check for uncommitted changes in the agent worktree
        if let Some(wt_path) = thread_worktree_path {
            match WorktreeManager::dirty_files(wt_path) {
                Ok(Some(files)) => {
                    return Ok(MergeResult {
                        success: false,
                        summary: None,
                        error: Some(
                            "Source branch has uncommitted changes — agent did not commit its work"
                                .to_string(),
                        ),
                        conflict_files: Some(files),
                    });
                }
                Err(e) => {
                    tracing::warn!(
                        op_id = %op.id,
                        worktree = %wt_path.display(),
                        error = %e,
                        "dirty_files check failed — blocking merge for safety"
                    );
                    return Ok(MergeResult {
                        success: false,
                        summary: None,
                        error: Some(format!(
                            "Failed to check worktree for uncommitted changes: {}",
                            e
                        )),
                        conflict_files: None,
                    });
                }
                Ok(None) => {} // clean — proceed
            }
        }

        // Pre-merge validation: check divergence (no-op detection)
        let ahead = Self::commits_ahead(repo_root, &op.target_branch, &op.source_branch)?;
        if ahead == 0 {
            return Ok(MergeResult {
                success: false,
                summary: None,
                error: Some(
                    "No commits to merge — source branch is identical to target".to_string(),
                ),
                conflict_files: None,
            });
        }

        let worktree_path = repo_root
            .join(".compas-worktrees")
            .join(format!("merge-{}", op.id));

        let temp_branch = format!("_compas_merge_{}", op.id);

        // Ensure the cleanup runs in all code paths
        let _guard = WorktreeCleanupGuard {
            repo_root: repo_root.to_path_buf(),
            worktree_path: worktree_path.clone(),
            temp_branch: Some(temp_branch.clone()),
        };

        // Ensure parent directory exists
        if let Some(parent) = worktree_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create merge worktree parent: {}", e))?;
        }

        let repo_str = repo_root.to_string_lossy().to_string();
        let wt_str = worktree_path.to_string_lossy().to_string();

        // 1. Create temporary worktree with a temp branch based on the target branch.
        //    This avoids the "branch already checked out" error when the target
        //    branch is active in the main repo.
        let add_result = Command::new("git")
            .args([
                "-C",
                &repo_str,
                "worktree",
                "add",
                "-b",
                &temp_branch,
                &wt_str,
                &op.target_branch,
            ])
            .output()
            .map_err(|e| format!("failed to create merge worktree: {}", e))?;

        if !add_result.status.success() {
            let stderr = String::from_utf8_lossy(&add_result.stderr);
            return Err(format!(
                "git worktree add failed for merge-{}: {}",
                op.id,
                stderr.trim()
            ));
        }

        // 2. Execute the merge based on strategy
        let result = match op.merge_strategy.as_str() {
            "merge" => Self::execute_merge(op, &worktree_path),
            "rebase" => Self::execute_rebase(op, &worktree_path),
            "squash" => Self::execute_squash(op, &worktree_path),
            other => Err(format!("unsupported merge strategy: '{}'", other)),
        }?;

        // 3. On success, update the real target branch to point at the merge result
        if result.success {
            // Get the commit SHA from the temp branch
            let rev_output = Command::new("git")
                .args(["-C", &wt_str, "rev-parse", "HEAD"])
                .output()
                .map_err(|e| format!("failed to get merge commit SHA: {}", e))?;

            if !rev_output.status.success() {
                return Err("failed to read merge result commit".to_string());
            }

            let new_sha = String::from_utf8_lossy(&rev_output.stdout)
                .trim()
                .to_string();

            // Check if the target branch is currently checked out in the main repo.
            // This determines which strategy we use to advance the branch:
            //   - Checked out: `git reset --keep` moves the ref AND syncs the working
            //     tree + index in one step. --keep aborts if uncommitted changes to
            //     tracked files would be overwritten (safe default).
            //   - Not checked out: `git update-ref` moves only the ref (no working
            //     tree to sync).
            //   - Detached HEAD: `rev-parse --abbrev-ref` returns "HEAD", which won't
            //     match any target branch name, so falls through to update-ref.
            let current_branch = Command::new("git")
                .args(["-C", &repo_str, "rev-parse", "--abbrev-ref", "HEAD"])
                .output()
                .ok()
                .and_then(|o| {
                    if o.status.success() {
                        Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
                    } else {
                        None
                    }
                });

            if current_branch.as_deref() == Some(&op.target_branch) {
                // Target branch is checked out — use reset --keep to advance the
                // ref, index, and working tree together.
                let reset = Command::new("git")
                    .args(["-C", &repo_str, "reset", "--keep", &new_sha])
                    .output()
                    .map_err(|e| format!("failed to reset working tree: {}", e))?;

                if !reset.status.success() {
                    let stderr = String::from_utf8_lossy(&reset.stderr);
                    // Fall back to update-ref so the merge isn't lost, but warn
                    // that the working tree is out of sync.
                    tracing::warn!(
                        target_branch = %op.target_branch,
                        error = %stderr.trim(),
                        "reset --keep failed (uncommitted changes conflict with \
                         merged files); falling back to update-ref — working tree \
                         will be out of sync; run `git stash && git reset --keep HEAD \
                         && git stash pop` to sync, or `git reset --hard HEAD` to \
                         discard local changes"
                    );
                    let update_ref = Command::new("git")
                        .args([
                            "-C",
                            &repo_str,
                            "update-ref",
                            &format!("refs/heads/{}", op.target_branch),
                            &new_sha,
                        ])
                        .output()
                        .map_err(|e| format!("failed to update target branch ref: {}", e))?;

                    if !update_ref.status.success() {
                        let stderr = String::from_utf8_lossy(&update_ref.stderr);
                        return Err(format!(
                            "failed to update '{}' to merge result: {}",
                            op.target_branch,
                            stderr.trim()
                        ));
                    }
                }
            } else {
                // Target branch is NOT checked out — just move the ref.
                let update_ref = Command::new("git")
                    .args([
                        "-C",
                        &repo_str,
                        "update-ref",
                        &format!("refs/heads/{}", op.target_branch),
                        &new_sha,
                    ])
                    .output()
                    .map_err(|e| format!("failed to update target branch ref: {}", e))?;

                if !update_ref.status.success() {
                    let stderr = String::from_utf8_lossy(&update_ref.stderr);
                    return Err(format!(
                        "failed to update '{}' to merge result: {}",
                        op.target_branch,
                        stderr.trim()
                    ));
                }
            }
        }

        Ok(result)
    }

    /// Standard merge: `git merge {source_branch} -m <message>`
    fn execute_merge(op: &MergeOperation, worktree_path: &Path) -> Result<MergeResult, String> {
        let wt_str = worktree_path.to_string_lossy().to_string();

        let commit_msg = op
            .commit_message
            .clone()
            .unwrap_or_else(|| format!("Merge {} into {}", op.source_branch, op.target_branch));

        let merge_output = Command::new("git")
            .args(["-C", &wt_str, "merge", &op.source_branch, "-m", &commit_msg])
            .output()
            .map_err(|e| format!("failed to run git merge: {}", e))?;

        if merge_output.status.success() {
            return Ok(MergeResult {
                success: true,
                summary: Some(format!(
                    "Merged {} into {}",
                    op.source_branch, op.target_branch
                )),
                error: None,
                conflict_files: None,
            });
        }

        // Merge conflict — extract conflicting files and abort
        let conflict_files = Self::get_conflict_files_merge(worktree_path);

        // Abort the merge
        let _ = Command::new("git")
            .args(["-C", &wt_str, "merge", "--abort"])
            .output();

        let file_count = conflict_files.as_ref().map_or(0, |f| f.len());
        Ok(MergeResult {
            success: false,
            summary: None,
            error: Some(format!("Merge conflict in {} file(s)", file_count)),
            conflict_files,
        })
    }

    /// Rebase: `git rebase {source_branch}` (rebase target onto source)
    fn execute_rebase(op: &MergeOperation, worktree_path: &Path) -> Result<MergeResult, String> {
        let wt_str = worktree_path.to_string_lossy().to_string();

        let rebase_output = Command::new("git")
            .args(["-C", &wt_str, "rebase", &op.source_branch])
            .output()
            .map_err(|e| format!("failed to run git rebase: {}", e))?;

        if rebase_output.status.success() {
            return Ok(MergeResult {
                success: true,
                summary: Some(format!(
                    "Rebased {} onto {}",
                    op.target_branch, op.source_branch
                )),
                error: None,
                conflict_files: None,
            });
        }

        // Rebase conflict — extract info from stderr and abort
        let stderr = String::from_utf8_lossy(&rebase_output.stderr).to_string();

        // Try to get conflict files via diff
        let conflict_files = Self::get_conflict_files_merge(worktree_path);

        // Abort the rebase
        let _ = Command::new("git")
            .args(["-C", &wt_str, "rebase", "--abort"])
            .output();

        let file_count = conflict_files.as_ref().map_or(0, |f| f.len());
        let error_msg = if file_count > 0 {
            format!("Rebase conflict in {} file(s)", file_count)
        } else {
            format!("Rebase failed: {}", stderr.trim())
        };

        Ok(MergeResult {
            success: false,
            summary: None,
            error: Some(error_msg),
            conflict_files,
        })
    }

    /// Squash merge: `git merge --squash {source_branch}` then commit
    fn execute_squash(op: &MergeOperation, worktree_path: &Path) -> Result<MergeResult, String> {
        let wt_str = worktree_path.to_string_lossy().to_string();

        let squash_output = Command::new("git")
            .args(["-C", &wt_str, "merge", "--squash", &op.source_branch])
            .output()
            .map_err(|e| format!("failed to run git merge --squash: {}", e))?;

        if !squash_output.status.success() {
            // Squash merge conflict
            let conflict_files = Self::get_conflict_files_merge(worktree_path);

            // Reset the failed squash
            let _ = Command::new("git")
                .args(["-C", &wt_str, "merge", "--abort"])
                .output();
            // Also reset any partial squash state
            let _ = Command::new("git")
                .args(["-C", &wt_str, "reset", "--hard"])
                .output();

            let file_count = conflict_files.as_ref().map_or(0, |f| f.len());
            return Ok(MergeResult {
                success: false,
                summary: None,
                error: Some(format!("Squash merge conflict in {} file(s)", file_count)),
                conflict_files,
            });
        }

        // Squash succeeded — now commit
        let commit_msg = op.commit_message.clone().unwrap_or_else(|| {
            format!(
                "Squash merge {} into {}",
                op.source_branch, op.target_branch
            )
        });
        let commit_output = Command::new("git")
            .args(["-C", &wt_str, "commit", "-m", &commit_msg])
            .output()
            .map_err(|e| format!("failed to commit squash merge: {}", e))?;

        if !commit_output.status.success() {
            let stderr = String::from_utf8_lossy(&commit_output.stderr);
            return Err(format!("squash merge commit failed: {}", stderr.trim()));
        }

        Ok(MergeResult {
            success: true,
            summary: Some(format!(
                "Squash merged {} into {}",
                op.source_branch, op.target_branch
            )),
            error: None,
            conflict_files: None,
        })
    }

    /// Extract conflicting file names from a merge/squash conflict state.
    fn get_conflict_files_merge(worktree_path: &Path) -> Option<Vec<String>> {
        let wt_str = worktree_path.to_string_lossy().to_string();
        let output = Command::new("git")
            .args(["-C", &wt_str, "diff", "--name-only", "--diff-filter=U"])
            .output()
            .ok()?;

        if !output.status.success() {
            return None;
        }

        let files: Vec<String> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| l.to_string())
            .collect();

        if files.is_empty() {
            None
        } else {
            Some(files)
        }
    }

    /// Clean up orphaned merge worktrees from `.compas-worktrees/`.
    ///
    /// Scans for directories matching `merge-*` that don't correspond to any
    /// active merge operation and removes them. Used for crash recovery — these
    /// should not persist beyond a single merge operation.
    ///
    /// `active_merge_ids` contains the IDs of merge operations currently in
    /// `claimed` or `executing` status. Worktrees whose ID suffix matches an
    /// active operation are left alone.
    pub fn cleanup_orphaned_merge_worktrees(
        repo_root: &Path,
        active_merge_ids: &[String],
    ) -> Result<u64, String> {
        let worktree_root = repo_root.join(".compas-worktrees");
        let entries = match std::fs::read_dir(&worktree_root) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(format!("failed to read worktree dir: {}", e)),
        };

        let repo_str = repo_root.to_string_lossy().to_string();
        let mut cleaned = 0u64;

        for entry in entries.flatten() {
            let name = match entry.file_name().to_str() {
                Some(n) => n.to_string(),
                None => continue,
            };

            if !name.starts_with("merge-") || !entry.path().is_dir() {
                continue;
            }

            // Extract the op ID from "merge-{op_id}" and skip if active
            let op_id = &name["merge-".len()..];
            if active_merge_ids.iter().any(|id| id == op_id) {
                tracing::debug!(
                    path = %name,
                    op_id = %op_id,
                    "skipping active merge worktree"
                );
                continue;
            }

            let wt_path = entry.path();
            let wt_str = wt_path.to_string_lossy().to_string();

            tracing::info!(
                path = %wt_str,
                "removing orphaned merge worktree"
            );

            // Try git worktree remove first
            let remove_result = Command::new("git")
                .args(["-C", &repo_str, "worktree", "remove", "--force", &wt_str])
                .output();

            match remove_result {
                Ok(out) if out.status.success() => {
                    cleaned += 1;
                }
                _ => {
                    // Fallback: manual removal + prune
                    if std::fs::remove_dir_all(&wt_path).is_ok() {
                        let _ = Command::new("git")
                            .args(["-C", &repo_str, "worktree", "prune"])
                            .output();
                        cleaned += 1;
                    } else {
                        tracing::warn!(
                            path = %wt_str,
                            "failed to remove orphaned merge worktree"
                        );
                    }
                }
            }
        }

        Ok(cleaned)
    }
}

/// RAII guard that ensures a temporary merge worktree and its temp branch
/// are cleaned up.
///
/// On drop, removes the worktree via `git worktree remove --force`, falling
/// back to `fs::remove_dir_all` + `git worktree prune`, then deletes the
/// temporary branch.
struct WorktreeCleanupGuard {
    repo_root: PathBuf,
    worktree_path: PathBuf,
    temp_branch: Option<String>,
}

impl Drop for WorktreeCleanupGuard {
    fn drop(&mut self) {
        let repo_str = self.repo_root.to_string_lossy().to_string();

        if self.worktree_path.exists() {
            let wt_str = self.worktree_path.to_string_lossy().to_string();

            let result = Command::new("git")
                .args(["-C", &repo_str, "worktree", "remove", "--force", &wt_str])
                .output();

            let removed = matches!(result, Ok(ref out) if out.status.success());

            if !removed {
                // Fallback: manual removal + prune
                let _ = std::fs::remove_dir_all(&self.worktree_path);
                let _ = Command::new("git")
                    .args(["-C", &repo_str, "worktree", "prune"])
                    .output();
            }
        }

        // Clean up the temporary branch (best-effort)
        if let Some(ref branch) = self.temp_branch {
            let _ = Command::new("git")
                .args(["-C", &repo_str, "branch", "-D", branch])
                .output();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{MergeOperation, ThreadStatus};

    /// Create a temporary git repo with an initial commit and local user config.
    fn init_test_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let dir_str = dir.path().to_string_lossy().to_string();

        // git init
        let init = Command::new("git")
            .args(["init", &dir_str])
            .output()
            .unwrap();
        assert!(init.status.success(), "git init failed");

        // Set local git config so commits/merges work without -c overrides
        let cfg_email = Command::new("git")
            .args(["-C", &dir_str, "config", "user.email", "test@test.com"])
            .output()
            .unwrap();
        assert!(cfg_email.status.success(), "git config user.email failed");
        let cfg_name = Command::new("git")
            .args(["-C", &dir_str, "config", "user.name", "Test"])
            .output()
            .unwrap();
        assert!(cfg_name.status.success(), "git config user.name failed");

        // Initial commit
        let commit = Command::new("git")
            .args([
                "-C",
                &dir_str,
                "commit",
                "--allow-empty",
                "-m",
                "initial commit",
            ])
            .output()
            .unwrap();
        assert!(commit.status.success(), "initial commit failed");

        dir
    }

    /// Create a file, add, and commit it in the given repo/worktree path.
    fn commit_file(repo_path: &Path, filename: &str, content: &str, message: &str) {
        let path_str = repo_path.to_string_lossy().to_string();

        std::fs::write(repo_path.join(filename), content).unwrap();

        let add = Command::new("git")
            .args(["-C", &path_str, "add", filename])
            .output()
            .unwrap();
        assert!(add.status.success(), "git add failed for {}", filename);

        let commit = Command::new("git")
            .args([
                "-C",
                &path_str,
                "-c",
                "user.email=test@test.com",
                "-c",
                "user.name=Test",
                "commit",
                "-m",
                message,
            ])
            .output()
            .unwrap();
        assert!(
            commit.status.success(),
            "git commit failed: {}",
            String::from_utf8_lossy(&commit.stderr)
        );
    }

    /// Create a source branch with a committed file, starting from the current HEAD.
    fn create_source_branch(repo_path: &Path, branch_name: &str, filename: &str, content: &str) {
        let path_str = repo_path.to_string_lossy().to_string();

        // Create and checkout branch
        let checkout = Command::new("git")
            .args(["-C", &path_str, "checkout", "-b", branch_name])
            .output()
            .unwrap();
        assert!(
            checkout.status.success(),
            "checkout -b failed: {}",
            String::from_utf8_lossy(&checkout.stderr)
        );

        commit_file(
            repo_path,
            filename,
            content,
            &format!("add {} on {}", filename, branch_name),
        );

        // Switch back to the original branch (main/master)
        let back = Command::new("git")
            .args(["-C", &path_str, "checkout", "-"])
            .output()
            .unwrap();
        assert!(
            back.status.success(),
            "checkout back failed: {}",
            String::from_utf8_lossy(&back.stderr)
        );
    }

    /// Get the default branch name (main or master) of a test repo.
    fn default_branch(repo_path: &Path) -> String {
        let path_str = repo_path.to_string_lossy().to_string();
        let output = Command::new("git")
            .args(["-C", &path_str, "rev-parse", "--abbrev-ref", "HEAD"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn make_test_merge_op(
        id: &str,
        source_branch: &str,
        target_branch: &str,
        strategy: &str,
    ) -> MergeOperation {
        MergeOperation {
            id: id.to_string(),
            thread_id: "test-thread".to_string(),
            source_branch: source_branch.to_string(),
            target_branch: target_branch.to_string(),
            merge_strategy: strategy.to_string(),
            requested_by: "operator".to_string(),
            status: "executing".to_string(),
            push_requested: false,
            queued_at: 1000,
            claimed_at: Some(1001),
            started_at: Some(1002),
            finished_at: None,
            duration_ms: None,
            result_summary: None,
            error_detail: None,
            conflict_files: None,
            commit_message: None,
        }
    }

    #[test]
    fn test_merge_execute_success() {
        let repo = init_test_repo();
        let target = default_branch(repo.path());

        // Add a file on the default branch so there's a base
        commit_file(repo.path(), "base.txt", "base content", "add base file");

        // Create a source branch with a non-conflicting file
        create_source_branch(
            repo.path(),
            "compas/test-thread",
            "feature.txt",
            "feature content",
        );

        let op = make_test_merge_op("merge-ok", "compas/test-thread", &target, "merge");
        let result = MergeExecutor::execute(&op, repo.path(), None).unwrap();

        assert!(result.success, "merge should succeed");
        assert!(result.summary.is_some());
        assert!(result.summary.unwrap().contains("Merged"));
        assert!(result.error.is_none());
        assert!(result.conflict_files.is_none());
    }

    #[test]
    fn test_merge_commit_uses_custom_message() {
        let repo = init_test_repo();
        let target = default_branch(repo.path());

        commit_file(repo.path(), "base.txt", "base content", "add base file");
        create_source_branch(
            repo.path(),
            "compas/msg-thread",
            "feature.txt",
            "feature content",
        );
        // Add a commit on target to create divergence (prevents fast-forward)
        commit_file(repo.path(), "target.txt", "target content", "target work");

        let mut op = make_test_merge_op("msg-ok", "compas/msg-thread", &target, "merge");
        op.commit_message = Some("feat: add widget support".to_string());
        let result = MergeExecutor::execute(&op, repo.path(), None).unwrap();
        assert!(result.success, "merge should succeed");

        // Verify the merge commit message matches the custom commit_message
        let repo_str = repo.path().to_string_lossy().to_string();
        let log = Command::new("git")
            .args(["-C", &repo_str, "log", "-1", "--format=%s"])
            .output()
            .unwrap();
        let msg = String::from_utf8_lossy(&log.stdout).trim().to_string();
        assert_eq!(msg, "feat: add widget support");
    }

    #[test]
    fn test_merge_commit_fallback_message_when_none() {
        let repo = init_test_repo();
        let target = default_branch(repo.path());

        commit_file(repo.path(), "base.txt", "base content", "add base file");
        create_source_branch(
            repo.path(),
            "compas/fallback-thread",
            "feature.txt",
            "feature content",
        );
        // Add a commit on target to create divergence (prevents fast-forward)
        commit_file(repo.path(), "target.txt", "target content", "target work");

        let op = make_test_merge_op("fallback-ok", "compas/fallback-thread", &target, "merge");
        // commit_message is None — should use fallback
        assert!(op.commit_message.is_none());
        let result = MergeExecutor::execute(&op, repo.path(), None).unwrap();
        assert!(result.success, "merge should succeed");

        let repo_str = repo.path().to_string_lossy().to_string();
        let log = Command::new("git")
            .args(["-C", &repo_str, "log", "-1", "--format=%s"])
            .output()
            .unwrap();
        let msg = String::from_utf8_lossy(&log.stdout).trim().to_string();
        let expected = format!("Merge compas/fallback-thread into {}", target);
        assert_eq!(msg, expected);
    }

    #[test]
    fn test_merge_commit_author_is_not_compas_localhost() {
        let repo = init_test_repo();
        let target = default_branch(repo.path());

        commit_file(repo.path(), "base.txt", "base content", "add base file");
        create_source_branch(
            repo.path(),
            "compas/author-thread",
            "feature.txt",
            "feature content",
        );
        // Add a commit on target to create divergence (prevents fast-forward)
        commit_file(repo.path(), "target.txt", "target content", "target work");

        let op = make_test_merge_op("author-ok", "compas/author-thread", &target, "merge");
        let result = MergeExecutor::execute(&op, repo.path(), None).unwrap();
        assert!(result.success, "merge should succeed");

        // Verify the merge commit author is NOT compas@localhost
        let repo_str = repo.path().to_string_lossy().to_string();
        let log = Command::new("git")
            .args(["-C", &repo_str, "log", "-1", "--format=%ae"])
            .output()
            .unwrap();
        let email = String::from_utf8_lossy(&log.stdout).trim().to_string();
        assert_ne!(
            email, "compas@localhost",
            "merge commit should use repo's git user, not compas@localhost"
        );
        assert_eq!(email, "test@test.com");
    }

    #[test]
    fn test_squash_commit_uses_custom_message() {
        let repo = init_test_repo();
        let target = default_branch(repo.path());

        commit_file(repo.path(), "base.txt", "base content", "add base file");
        create_source_branch(
            repo.path(),
            "compas/squash-msg-thread",
            "feature.txt",
            "feature content",
        );

        let mut op = make_test_merge_op(
            "squash-msg-ok",
            "compas/squash-msg-thread",
            &target,
            "squash",
        );
        op.commit_message = Some("feat: squashed widget support".to_string());
        let result = MergeExecutor::execute(&op, repo.path(), None).unwrap();
        assert!(result.success, "squash merge should succeed");

        let repo_str = repo.path().to_string_lossy().to_string();
        let log = Command::new("git")
            .args(["-C", &repo_str, "log", "-1", "--format=%s"])
            .output()
            .unwrap();
        let msg = String::from_utf8_lossy(&log.stdout).trim().to_string();
        assert_eq!(msg, "feat: squashed widget support");
    }

    #[test]
    fn test_squash_commit_fallback_message_when_none() {
        let repo = init_test_repo();
        let target = default_branch(repo.path());

        commit_file(repo.path(), "base.txt", "base content", "add base file");
        create_source_branch(
            repo.path(),
            "compas/squash-fallback-thread",
            "feature.txt",
            "feature content",
        );

        let op = make_test_merge_op(
            "squash-fallback-ok",
            "compas/squash-fallback-thread",
            &target,
            "squash",
        );
        assert!(op.commit_message.is_none());
        let result = MergeExecutor::execute(&op, repo.path(), None).unwrap();
        assert!(result.success, "squash merge should succeed");

        let repo_str = repo.path().to_string_lossy().to_string();
        let log = Command::new("git")
            .args(["-C", &repo_str, "log", "-1", "--format=%s"])
            .output()
            .unwrap();
        let msg = String::from_utf8_lossy(&log.stdout).trim().to_string();
        let expected = format!("Squash merge compas/squash-fallback-thread into {}", target);
        assert_eq!(msg, expected);
    }

    #[test]
    fn test_squash_commit_author_is_not_compas_localhost() {
        let repo = init_test_repo();
        let target = default_branch(repo.path());

        commit_file(repo.path(), "base.txt", "base content", "add base file");
        create_source_branch(
            repo.path(),
            "compas/squash-author-thread",
            "feature.txt",
            "feature content",
        );

        let op = make_test_merge_op(
            "squash-author-ok",
            "compas/squash-author-thread",
            &target,
            "squash",
        );
        let result = MergeExecutor::execute(&op, repo.path(), None).unwrap();
        assert!(result.success, "squash merge should succeed");

        let repo_str = repo.path().to_string_lossy().to_string();
        let log = Command::new("git")
            .args(["-C", &repo_str, "log", "-1", "--format=%ae"])
            .output()
            .unwrap();
        let email = String::from_utf8_lossy(&log.stdout).trim().to_string();
        assert_ne!(
            email, "compas@localhost",
            "squash commit should use repo's git user, not compas@localhost"
        );
        assert_eq!(email, "test@test.com");
    }

    #[test]
    fn test_merge_execute_syncs_working_tree() {
        let repo = init_test_repo();
        let target = default_branch(repo.path());

        // Add a base file on the default branch
        commit_file(repo.path(), "base.txt", "base content", "add base file");

        // Create a source branch with a new file
        create_source_branch(
            repo.path(),
            "compas/sync-thread",
            "synced.txt",
            "synced content",
        );

        let op = make_test_merge_op("sync-ok", "compas/sync-thread", &target, "merge");
        let result = MergeExecutor::execute(&op, repo.path(), None).unwrap();

        assert!(result.success, "merge should succeed");

        // The merged file should exist in the working tree — not just in the git ref
        let synced_path = repo.path().join("synced.txt");
        assert!(
            synced_path.exists(),
            "merged file should be present in working tree after merge"
        );
        assert_eq!(
            std::fs::read_to_string(&synced_path).unwrap(),
            "synced content",
            "working tree file content should match the merged commit"
        );
    }

    #[test]
    fn test_merge_execute_fallback_when_dirty_working_tree() {
        let repo = init_test_repo();
        let target = default_branch(repo.path());
        let repo_str = repo.path().to_string_lossy().to_string();

        // Add a base file on the default branch
        commit_file(repo.path(), "shared.txt", "original", "add shared file");

        // Create a source branch that modifies the same file
        create_source_branch(
            repo.path(),
            "compas/dirty-thread",
            "new-file.txt",
            "new content",
        );
        // Also modify shared.txt on the source branch
        let source_path = repo.path().to_string_lossy().to_string();
        let _ = Command::new("git")
            .args(["-C", &source_path, "checkout", "compas/dirty-thread"])
            .output()
            .unwrap();
        commit_file(
            repo.path(),
            "shared.txt",
            "from source",
            "modify shared on source",
        );
        let _ = Command::new("git")
            .args(["-C", &source_path, "checkout", "-"])
            .output()
            .unwrap();

        // Create uncommitted changes to shared.txt in the working tree
        // (this will cause reset --keep to fail)
        std::fs::write(repo.path().join("shared.txt"), "uncommitted local edit").unwrap();

        let op = make_test_merge_op("dirty-ok", "compas/dirty-thread", &target, "merge");
        let result = MergeExecutor::execute(&op, repo.path(), None).unwrap();

        assert!(
            result.success,
            "merge should still succeed via update-ref fallback"
        );

        // The ref should be advanced even though working tree sync failed
        let rev = Command::new("git")
            .args([
                "-C",
                &repo_str,
                "rev-parse",
                &format!("refs/heads/{}", target),
            ])
            .output()
            .unwrap();
        let ref_sha = String::from_utf8_lossy(&rev.stdout).trim().to_string();

        // Verify the ref points to a commit that contains new-file.txt
        let show = Command::new("git")
            .args([
                "-C",
                &repo_str,
                "show",
                &format!("{}:new-file.txt", ref_sha),
            ])
            .output()
            .unwrap();
        assert!(
            show.status.success(),
            "target branch ref should contain the merged file"
        );
    }

    #[test]
    fn test_merge_execute_syncs_working_tree_rebase() {
        let repo = init_test_repo();
        let target = default_branch(repo.path());

        commit_file(repo.path(), "base.txt", "base content", "add base file");
        create_source_branch(
            repo.path(),
            "compas/rebase-sync",
            "rebased.txt",
            "rebased content",
        );

        let op = make_test_merge_op("rebase-sync", "compas/rebase-sync", &target, "rebase");
        let result = MergeExecutor::execute(&op, repo.path(), None).unwrap();

        assert!(result.success, "rebase should succeed");
        let rebased_path = repo.path().join("rebased.txt");
        assert!(
            rebased_path.exists(),
            "rebased file should be present in working tree"
        );
    }

    #[test]
    fn test_merge_execute_syncs_working_tree_squash() {
        let repo = init_test_repo();
        let target = default_branch(repo.path());

        commit_file(repo.path(), "base.txt", "base content", "add base file");
        create_source_branch(
            repo.path(),
            "compas/squash-sync",
            "squashed.txt",
            "squashed content",
        );

        let op = make_test_merge_op("squash-sync", "compas/squash-sync", &target, "squash");
        let result = MergeExecutor::execute(&op, repo.path(), None).unwrap();

        assert!(result.success, "squash should succeed");
        let squashed_path = repo.path().join("squashed.txt");
        assert!(
            squashed_path.exists(),
            "squashed file should be present in working tree"
        );
    }

    #[test]
    fn test_merge_execute_success_non_fast_forward() {
        let repo = init_test_repo();
        let target = default_branch(repo.path());

        // Add a base file on the default branch
        commit_file(repo.path(), "base.txt", "base content", "add base file");

        // Create a source branch with a new file
        create_source_branch(
            repo.path(),
            "compas/nff-thread",
            "feature.txt",
            "feature content",
        );

        // Commit on target AFTER branching — forces a real merge commit
        commit_file(
            repo.path(),
            "other.txt",
            "other content",
            "add other file on target",
        );

        let op = make_test_merge_op("nff-merge", "compas/nff-thread", &target, "merge");
        let result = MergeExecutor::execute(&op, repo.path(), None).unwrap();

        assert!(result.success, "non-fast-forward merge should succeed");
        assert!(result.summary.is_some());
        assert!(result.summary.unwrap().contains("Merged"));
        assert!(result.error.is_none());
        assert!(result.conflict_files.is_none());

        // Verify that the target branch now contains both files
        let repo_str = repo.path().to_string_lossy().to_string();
        let log = Command::new("git")
            .args(["-C", &repo_str, "log", "--oneline", "-1", &target])
            .output()
            .unwrap();
        let log_msg = String::from_utf8_lossy(&log.stdout).to_string();
        assert!(
            log_msg.contains("Merge"),
            "should have a merge commit, got: {}",
            log_msg
        );
    }

    #[test]
    fn test_merge_execute_conflict() {
        let repo = init_test_repo();
        let target = default_branch(repo.path());

        // Create a file on the default branch
        commit_file(repo.path(), "shared.txt", "base content", "add shared file");

        // Create source branch modifying the same file
        create_source_branch(
            repo.path(),
            "compas/conflict-thread",
            "shared.txt",
            "source content",
        );

        // Modify the same file on the target branch to create a conflict
        commit_file(
            repo.path(),
            "shared.txt",
            "target content",
            "modify shared on target",
        );

        let op = make_test_merge_op("merge-conflict", "compas/conflict-thread", &target, "merge");
        let result = MergeExecutor::execute(&op, repo.path(), None).unwrap();

        assert!(!result.success, "merge should fail due to conflict");
        assert!(result.error.is_some());
        assert!(result.error.as_ref().unwrap().contains("conflict"));
        assert!(result.conflict_files.is_some());
        let files = result.conflict_files.unwrap();
        assert!(
            files.contains(&"shared.txt".to_string()),
            "conflict files should include shared.txt, got: {:?}",
            files
        );

        // Verify source branch is still intact
        let repo_str = repo.path().to_string_lossy().to_string();
        let verify = Command::new("git")
            .args([
                "-C",
                &repo_str,
                "rev-parse",
                "--verify",
                "compas/conflict-thread",
            ])
            .output()
            .unwrap();
        assert!(verify.status.success(), "source branch should still exist");
    }

    #[test]
    fn test_merge_execute_rebase_success() {
        let repo = init_test_repo();
        let target = default_branch(repo.path());

        // Add a base file
        commit_file(repo.path(), "base.txt", "base content", "add base file");

        // Create source branch with a different file
        create_source_branch(
            repo.path(),
            "compas/rebase-thread",
            "rebase-feature.txt",
            "rebase content",
        );

        let op = make_test_merge_op("rebase-ok", "compas/rebase-thread", &target, "rebase");
        let result = MergeExecutor::execute(&op, repo.path(), None).unwrap();

        assert!(result.success, "rebase should succeed");
        assert!(result.summary.is_some());
        assert!(result.summary.unwrap().contains("Rebased"));
        assert!(result.error.is_none());
        assert!(result.conflict_files.is_none());
    }

    #[test]
    fn test_merge_execute_squash_success() {
        let repo = init_test_repo();
        let target = default_branch(repo.path());

        // Add a base file
        commit_file(repo.path(), "base.txt", "base content", "add base file");

        // Create source branch with a different file
        create_source_branch(
            repo.path(),
            "compas/squash-thread",
            "squash-feature.txt",
            "squash content",
        );

        let op = make_test_merge_op("squash-ok", "compas/squash-thread", &target, "squash");
        let result = MergeExecutor::execute(&op, repo.path(), None).unwrap();

        assert!(result.success, "squash merge should succeed");
        assert!(result.summary.is_some());
        assert!(result.summary.unwrap().contains("Squash merged"));
        assert!(result.error.is_none());
        assert!(result.conflict_files.is_none());
    }

    #[test]
    fn test_merge_execute_missing_branch() {
        let repo = init_test_repo();
        let target = default_branch(repo.path());

        // Add a base file so the repo isn't empty
        commit_file(repo.path(), "base.txt", "content", "add base");

        let op = make_test_merge_op("missing-branch", "compas/nonexistent", &target, "merge");
        let result = MergeExecutor::execute(&op, repo.path(), None);

        // The merge will fail because the source branch doesn't exist
        // This could manifest as a merge error or a git worktree error
        match result {
            Ok(merge_result) => {
                assert!(
                    !merge_result.success,
                    "merge with missing branch should not succeed"
                );
                assert!(merge_result.error.is_some());
            }
            Err(e) => {
                // Also acceptable — the git command itself failed
                assert!(
                    e.contains("nonexistent")
                        || e.contains("merge")
                        || e.contains("pathspec")
                        || e.contains("not something"),
                    "error should mention the missing branch, got: {}",
                    e
                );
            }
        }
    }

    #[test]
    fn test_merge_cleanup_temp_worktree() {
        let repo = init_test_repo();
        let target = default_branch(repo.path());

        // Add a base file
        commit_file(repo.path(), "base.txt", "base", "base");

        // Create a source branch
        create_source_branch(
            repo.path(),
            "compas/cleanup-thread",
            "clean.txt",
            "clean content",
        );

        let worktree_path = repo
            .path()
            .join(".compas-worktrees")
            .join("merge-cleanup-test");

        // Test success path — worktree should be cleaned up
        let op = make_test_merge_op("cleanup-test", "compas/cleanup-thread", &target, "merge");
        let result = MergeExecutor::execute(&op, repo.path(), None).unwrap();
        assert!(result.success);
        assert!(
            !worktree_path.exists(),
            "merge worktree should be cleaned up after success"
        );

        // Test failure path — create a conflict scenario
        commit_file(
            repo.path(),
            "conflict-file.txt",
            "target version",
            "target change",
        );
        create_source_branch(
            repo.path(),
            "compas/cleanup-conflict",
            "conflict-file.txt",
            "source version",
        );
        commit_file(
            repo.path(),
            "conflict-file.txt",
            "target diverged",
            "diverge target",
        );

        let conflict_worktree = repo
            .path()
            .join(".compas-worktrees")
            .join("merge-cleanup-conflict");

        let op2 = make_test_merge_op(
            "cleanup-conflict",
            "compas/cleanup-conflict",
            &target,
            "merge",
        );
        let result2 = MergeExecutor::execute(&op2, repo.path(), None).unwrap();
        assert!(!result2.success);
        assert!(
            !conflict_worktree.exists(),
            "merge worktree should be cleaned up after conflict"
        );
    }

    #[test]
    fn test_cleanup_orphaned_merge_worktrees() {
        let repo = init_test_repo();

        // Create some orphaned merge worktree directories
        let worktree_root = repo.path().join(".compas-worktrees");
        std::fs::create_dir_all(&worktree_root).unwrap();

        std::fs::create_dir_all(worktree_root.join("merge-orphan1")).unwrap();
        std::fs::create_dir_all(worktree_root.join("merge-orphan2")).unwrap();
        // Non-merge directory should NOT be cleaned up
        std::fs::create_dir_all(worktree_root.join("some-thread-id")).unwrap();

        let cleaned = MergeExecutor::cleanup_orphaned_merge_worktrees(repo.path(), &[]).unwrap();

        assert_eq!(cleaned, 2, "should clean up 2 orphaned merge worktrees");
        assert!(
            !worktree_root.join("merge-orphan1").exists(),
            "orphaned merge worktree 1 should be removed"
        );
        assert!(
            !worktree_root.join("merge-orphan2").exists(),
            "orphaned merge worktree 2 should be removed"
        );
        assert!(
            worktree_root.join("some-thread-id").exists(),
            "non-merge worktree should NOT be removed"
        );
    }

    #[test]
    fn test_cleanup_orphaned_skips_active_merge() {
        let repo = init_test_repo();

        let worktree_root = repo.path().join(".compas-worktrees");
        std::fs::create_dir_all(&worktree_root).unwrap();

        // "merge-active-op" corresponds to active op ID "active-op"
        std::fs::create_dir_all(worktree_root.join("merge-active-op")).unwrap();
        std::fs::create_dir_all(worktree_root.join("merge-orphan")).unwrap();

        let active_ids = vec!["active-op".to_string()];
        let cleaned =
            MergeExecutor::cleanup_orphaned_merge_worktrees(repo.path(), &active_ids).unwrap();

        assert_eq!(cleaned, 1, "should only clean up the orphaned worktree");
        assert!(
            worktree_root.join("merge-active-op").exists(),
            "active merge worktree should NOT be removed"
        );
        assert!(
            !worktree_root.join("merge-orphan").exists(),
            "orphaned merge worktree should be removed"
        );
    }

    #[test]
    fn test_cleanup_orphaned_no_worktree_dir() {
        // When .compas-worktrees doesn't exist, should return 0
        let dir = tempfile::tempdir().unwrap();
        let cleaned = MergeExecutor::cleanup_orphaned_merge_worktrees(dir.path(), &[]).unwrap();
        assert_eq!(cleaned, 0);
    }

    // ── No-op and dirty-worktree detection ──────────────────────────────────

    #[test]
    fn test_merge_execute_noop_identical_branches() {
        let repo = init_test_repo();
        let target = default_branch(repo.path());

        // Create source branch at same commit as target (no additional commits)
        let repo_str = repo.path().to_string_lossy().to_string();
        let checkout = Command::new("git")
            .args(["-C", &repo_str, "checkout", "-b", "compas/noop-thread"])
            .output()
            .unwrap();
        assert!(checkout.status.success());
        let back = Command::new("git")
            .args(["-C", &repo_str, "checkout", "-"])
            .output()
            .unwrap();
        assert!(back.status.success());

        let op = make_test_merge_op("noop-merge", "compas/noop-thread", &target, "merge");
        let result = MergeExecutor::execute(&op, repo.path(), None).unwrap();

        assert!(!result.success, "no-op merge should fail");
        assert!(result.error.is_some());
        assert!(
            result
                .error
                .as_ref()
                .unwrap()
                .contains("No commits to merge"),
            "error should mention no commits, got: {}",
            result.error.unwrap()
        );
        assert!(result.conflict_files.is_none());
    }

    #[test]
    fn test_merge_execute_dirty_worktree_blocks_merge() {
        let repo = init_test_repo();
        let target = default_branch(repo.path());

        // Create source branch at same commit (no divergence)
        let repo_str = repo.path().to_string_lossy().to_string();
        let checkout = Command::new("git")
            .args(["-C", &repo_str, "checkout", "-b", "compas/dirty-thread-2"])
            .output()
            .unwrap();
        assert!(checkout.status.success());
        let back = Command::new("git")
            .args(["-C", &repo_str, "checkout", "-"])
            .output()
            .unwrap();
        assert!(back.status.success());

        // Create a worktree to act as the agent's working directory
        let wt_path = repo.path().join(".compas-worktrees").join("dirty-thread-2");
        std::fs::create_dir_all(wt_path.parent().unwrap()).unwrap();
        let wt_add = Command::new("git")
            .args([
                "-C",
                &repo_str,
                "worktree",
                "add",
                &wt_path.to_string_lossy(),
                "compas/dirty-thread-2",
            ])
            .output()
            .unwrap();
        assert!(
            wt_add.status.success(),
            "worktree add failed: {}",
            String::from_utf8_lossy(&wt_add.stderr)
        );

        // Leave uncommitted changes in the worktree
        std::fs::write(wt_path.join("uncommitted.txt"), "oops").unwrap();

        let op = make_test_merge_op("dirty-merge", "compas/dirty-thread-2", &target, "merge");
        let result = MergeExecutor::execute(&op, repo.path(), Some(&wt_path)).unwrap();

        assert!(!result.success, "dirty worktree should block merge");
        assert!(result.error.is_some());
        assert!(
            result
                .error
                .as_ref()
                .unwrap()
                .contains("uncommitted changes"),
            "error should mention uncommitted changes, got: {}",
            result.error.unwrap()
        );
        assert!(result.conflict_files.is_some());
        let files = result.conflict_files.unwrap();
        assert!(
            files.iter().any(|f| f.contains("uncommitted.txt")),
            "conflict_files should list the dirty file, got: {:?}",
            files
        );
    }

    #[test]
    fn test_merge_execute_dirty_worktree_blocks_even_when_ahead() {
        let repo = init_test_repo();
        let target = default_branch(repo.path());

        // Create source branch with a real commit ahead of target
        create_source_branch(
            repo.path(),
            "compas/partial-thread",
            "committed.txt",
            "committed content",
        );

        // Create a worktree for the source branch
        let repo_str = repo.path().to_string_lossy().to_string();
        let wt_path = repo.path().join(".compas-worktrees").join("partial-thread");
        std::fs::create_dir_all(wt_path.parent().unwrap()).unwrap();
        let wt_add = Command::new("git")
            .args([
                "-C",
                &repo_str,
                "worktree",
                "add",
                &wt_path.to_string_lossy(),
                "compas/partial-thread",
            ])
            .output()
            .unwrap();
        assert!(
            wt_add.status.success(),
            "worktree add failed: {}",
            String::from_utf8_lossy(&wt_add.stderr)
        );

        // Leave uncommitted changes in the worktree even though branch is ahead
        std::fs::write(wt_path.join("not-committed.txt"), "forgot to commit").unwrap();

        let op = make_test_merge_op("partial-merge", "compas/partial-thread", &target, "merge");
        let result = MergeExecutor::execute(&op, repo.path(), Some(&wt_path)).unwrap();

        assert!(
            !result.success,
            "dirty worktree should block merge even when source is ahead"
        );
        assert!(result.error.is_some());
        assert!(
            result
                .error
                .as_ref()
                .unwrap()
                .contains("uncommitted changes"),
            "error should mention uncommitted changes, got: {}",
            result.error.unwrap()
        );
        assert!(result.conflict_files.is_some());
        let files = result.conflict_files.unwrap();
        assert!(
            files.iter().any(|f| f.contains("not-committed.txt")),
            "conflict_files should list the dirty file, got: {:?}",
            files
        );
    }

    #[test]
    fn test_merge_execute_clean_worktree_succeeds() {
        // Verify that passing a clean worktree path doesn't block a normal merge
        let repo = init_test_repo();
        let target = default_branch(repo.path());

        commit_file(repo.path(), "base.txt", "base content", "add base file");

        create_source_branch(
            repo.path(),
            "compas/clean-wt-thread",
            "feature.txt",
            "feature content",
        );

        // Create a worktree for the source branch (clean)
        let repo_str = repo.path().to_string_lossy().to_string();
        let wt_path = repo
            .path()
            .join(".compas-worktrees")
            .join("clean-wt-thread");
        std::fs::create_dir_all(wt_path.parent().unwrap()).unwrap();
        let wt_add = Command::new("git")
            .args([
                "-C",
                &repo_str,
                "worktree",
                "add",
                &wt_path.to_string_lossy(),
                "compas/clean-wt-thread",
            ])
            .output()
            .unwrap();
        assert!(
            wt_add.status.success(),
            "worktree add failed: {}",
            String::from_utf8_lossy(&wt_add.stderr)
        );

        let op = make_test_merge_op("clean-wt-merge", "compas/clean-wt-thread", &target, "merge");
        let result = MergeExecutor::execute(&op, repo.path(), Some(&wt_path)).unwrap();

        assert!(
            result.success,
            "clean worktree with commits ahead should succeed"
        );
        assert!(result.summary.is_some());
    }

    // ── Preflight tests (using real store) ─────────────────────────────────

    async fn test_store() -> Store {
        let pool = sqlx::sqlite::SqlitePool::connect("sqlite::memory:")
            .await
            .unwrap();
        let store = Store::new(pool);
        store.setup().await.unwrap();
        store
    }

    #[tokio::test]
    async fn test_preflight_accepts_active_thread() {
        let store = test_store().await;
        let repo = init_test_repo();

        // Create an Active thread with a source branch
        store
            .ensure_thread("active-thread", None, None)
            .await
            .unwrap();
        create_source_branch(
            repo.path(),
            "compas/active-thread",
            "feature.txt",
            "content",
        );

        let result =
            MergeExecutor::preflight_check(&store, "active-thread", "main", repo.path()).await;

        assert!(
            result.is_ok(),
            "Active threads should be eligible for merge, got: {:?}",
            result.unwrap_err()
        );
    }

    #[tokio::test]
    async fn test_preflight_rejects_abandoned_thread() {
        let store = test_store().await;
        let repo = init_test_repo();

        // Create an Abandoned thread
        store
            .ensure_thread("abandoned-thread", None, None)
            .await
            .unwrap();
        store
            .update_thread_status("abandoned-thread", ThreadStatus::Abandoned)
            .await
            .unwrap();

        let result =
            MergeExecutor::preflight_check(&store, "abandoned-thread", "main", repo.path()).await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("Abandoned"),
            "error should mention Abandoned status, got: {}",
            err
        );
    }

    #[tokio::test]
    async fn test_preflight_accepts_completed_thread() {
        let store = test_store().await;
        let repo = init_test_repo();

        // Create a Completed thread with a source branch
        store
            .ensure_thread("completed-thread", None, None)
            .await
            .unwrap();
        store
            .update_thread_status("completed-thread", ThreadStatus::Completed)
            .await
            .unwrap();

        // Create the source branch in the repo
        create_source_branch(
            repo.path(),
            "compas/completed-thread",
            "feature.txt",
            "content",
        );

        let result =
            MergeExecutor::preflight_check(&store, "completed-thread", "main", repo.path()).await;

        assert!(
            result.is_ok(),
            "preflight should pass for Completed: {:?}",
            result
        );
        let preflight = result.unwrap();
        assert_eq!(preflight.source_branch, "compas/completed-thread");
        assert_eq!(preflight.target_branch, "main");
        assert_eq!(preflight.thread_id, "completed-thread");
    }

    #[tokio::test]
    async fn test_preflight_accepts_failed_thread() {
        let store = test_store().await;
        let repo = init_test_repo();

        // Create a Failed thread with a source branch
        store
            .ensure_thread("failed-thread", None, None)
            .await
            .unwrap();
        store
            .update_thread_status("failed-thread", ThreadStatus::Failed)
            .await
            .unwrap();

        // Create the source branch
        create_source_branch(
            repo.path(),
            "compas/failed-thread",
            "fix.txt",
            "fix content",
        );

        let result =
            MergeExecutor::preflight_check(&store, "failed-thread", "main", repo.path()).await;

        assert!(
            result.is_ok(),
            "preflight should pass for Failed: {:?}",
            result
        );
    }

    #[tokio::test]
    async fn test_preflight_rejects_missing_thread() {
        let store = test_store().await;
        let repo = init_test_repo();

        let result =
            MergeExecutor::preflight_check(&store, "nonexistent", "main", repo.path()).await;

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found"));
    }

    #[tokio::test]
    async fn test_preflight_rejects_missing_branch() {
        let store = test_store().await;
        let repo = init_test_repo();

        // Thread exists and is Completed, but no branch
        store.ensure_thread("no-branch", None, None).await.unwrap();
        store
            .update_thread_status("no-branch", ThreadStatus::Completed)
            .await
            .unwrap();

        let result = MergeExecutor::preflight_check(&store, "no-branch", "main", repo.path()).await;

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("does not exist"));
    }

    #[tokio::test]
    async fn test_preflight_rejects_pending_merge() {
        let store = test_store().await;
        let repo = init_test_repo();

        // Thread exists, Completed, with a branch
        store
            .ensure_thread("pending-merge", None, None)
            .await
            .unwrap();
        store
            .update_thread_status("pending-merge", ThreadStatus::Completed)
            .await
            .unwrap();
        create_source_branch(
            repo.path(),
            "compas/pending-merge",
            "pending.txt",
            "content",
        );

        // Insert a pending merge operation
        let op = MergeOperation {
            id: "existing-merge".to_string(),
            thread_id: "pending-merge".to_string(),
            source_branch: "compas/pending-merge".to_string(),
            target_branch: "main".to_string(),
            merge_strategy: "merge".to_string(),
            requested_by: "operator".to_string(),
            status: "queued".to_string(),
            push_requested: false,
            queued_at: 1000,
            claimed_at: None,
            started_at: None,
            finished_at: None,
            duration_ms: None,
            result_summary: None,
            error_detail: None,
            conflict_files: None,
            commit_message: None,
        };
        store.insert_merge_op(&op).await.unwrap();

        let result =
            MergeExecutor::preflight_check(&store, "pending-merge", "main", repo.path()).await;

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("pending merge"));
    }

    #[tokio::test]
    async fn test_preflight_rejects_dirty_worktree() {
        let store = test_store().await;
        let repo = init_test_repo();

        // Create a Completed thread with a source branch
        store.ensure_thread("dirty-wt", None, None).await.unwrap();
        store
            .update_thread_status("dirty-wt", ThreadStatus::Completed)
            .await
            .unwrap();
        create_source_branch(repo.path(), "compas/dirty-wt", "feature.txt", "content");

        // Create a worktree for this thread
        let mgr = WorktreeManager::new();
        let wt_path = mgr
            .ensure_worktree(repo.path(), "dirty-wt", None)
            .unwrap()
            .unwrap();

        // Record the worktree path in the store
        store
            .set_thread_worktree_path("dirty-wt", &wt_path, repo.path())
            .await
            .unwrap();

        // Make the worktree dirty by adding an uncommitted file
        std::fs::write(wt_path.join("uncommitted.txt"), "dirty").unwrap();

        let result = MergeExecutor::preflight_check(&store, "dirty-wt", "main", repo.path()).await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("uncommitted changes"),
            "error should mention uncommitted changes, got: {}",
            err
        );

        // Clean up
        mgr.remove_worktree(repo.path(), "dirty-wt", None).unwrap();
    }
}
