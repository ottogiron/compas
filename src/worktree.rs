//! Git worktree management for per-thread agent isolation.
//!
//! When an agent's `workspace` config is set to `"worktree"`, the executor
//! creates an isolated git worktree for each thread. This prevents file
//! conflicts when multiple agents work concurrently in the same repository.
//!
//! Default worktree location: `{repo_root}/../.aster-worktrees/{thread_id}/`
//! An optional `worktree_dir` config overrides the parent directory.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Manages git worktrees for isolated agent execution.
///
/// Worktree root is computed per-call from the repo_root (or an override
/// directory), so this struct carries no state.
pub struct WorktreeManager;

/// Information about an active worktree.
#[derive(Debug, Clone, serde::Serialize)]
pub struct WorktreeInfo {
    pub thread_id: String,
    pub path: PathBuf,
}

/// Compute the worktree root directory.
///
/// If `override_dir` is provided, uses that. Otherwise defaults to
/// `{repo_root}/../.aster-worktrees/`.
fn worktree_root(repo_root: &Path, override_dir: Option<&Path>) -> PathBuf {
    match override_dir {
        Some(dir) => dir.to_path_buf(),
        None => repo_root
            .parent()
            .unwrap_or(repo_root)
            .join(".aster-worktrees"),
    }
}

impl WorktreeManager {
    pub fn new() -> Self {
        Self
    }

    /// Ensure a worktree exists for the given thread. Creates one if needed.
    ///
    /// `override_dir`: optional worktree parent directory override from config.
    ///
    /// Returns `Ok(Some(path))` on success, `Ok(None)` if `repo_root` is not
    /// a git repository (graceful fallback to shared mode).
    pub fn ensure_worktree(
        &self,
        repo_root: &Path,
        thread_id: &str,
        override_dir: Option<&Path>,
    ) -> Result<Option<PathBuf>, String> {
        // 1. Check if repo_root is a git repo
        let check = Command::new("git")
            .args(["-C", &repo_root.to_string_lossy(), "rev-parse", "--git-dir"])
            .output()
            .map_err(|e| format!("failed to run git: {}", e))?;

        if !check.status.success() {
            tracing::warn!(
                repo = %repo_root.display(),
                "not a git repository — falling back to shared mode"
            );
            return Ok(None);
        }

        // 2. Worktree path
        let root = worktree_root(repo_root, override_dir);
        let worktree_path = root.join(thread_id);
        if worktree_path.exists() {
            tracing::debug!(
                thread_id = %thread_id,
                path = %worktree_path.display(),
                "reusing existing worktree"
            );
            return Ok(Some(worktree_path));
        }

        // Ensure parent directory exists
        if let Some(parent) = worktree_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create worktree parent dir: {}", e))?;
        }

        // 3. Create worktree with new branch
        let branch_name = format!("aster-orch/{}", thread_id);
        let repo_str = repo_root.to_string_lossy();
        let wt_str = worktree_path.to_string_lossy();

        let result = Command::new("git")
            .args([
                "-C",
                &repo_str,
                "worktree",
                "add",
                "-b",
                &branch_name,
                &wt_str,
                "HEAD",
            ])
            .output()
            .map_err(|e| format!("failed to run git worktree add: {}", e))?;

        if result.status.success() {
            tracing::info!(
                thread_id = %thread_id,
                path = %worktree_path.display(),
                "created worktree"
            );
            return Ok(Some(worktree_path));
        }

        // Branch may already exist (reopened thread) — try without -b
        let stderr = String::from_utf8_lossy(&result.stderr);
        if stderr.contains("already exists") {
            let result2 = Command::new("git")
                .args(["-C", &repo_str, "worktree", "add", &wt_str, &branch_name])
                .output()
                .map_err(|e| format!("failed to run git worktree add (existing branch): {}", e))?;

            if result2.status.success() {
                tracing::info!(
                    thread_id = %thread_id,
                    path = %worktree_path.display(),
                    "created worktree with existing branch"
                );
                return Ok(Some(worktree_path));
            }

            let stderr2 = String::from_utf8_lossy(&result2.stderr);
            return Err(format!(
                "git worktree add failed (existing branch): {}",
                stderr2.trim()
            ));
        }

        Err(format!("git worktree add failed: {}", stderr.trim()))
    }

    /// Remove a worktree and its branch (best-effort).
    pub fn remove_worktree(
        &self,
        repo_root: &Path,
        thread_id: &str,
        override_dir: Option<&Path>,
    ) -> Result<(), String> {
        let root = worktree_root(repo_root, override_dir);
        let worktree_path = root.join(thread_id);
        if !worktree_path.exists() {
            return Ok(());
        }

        let repo_str = repo_root.to_string_lossy();
        let wt_str = worktree_path.to_string_lossy();

        // Remove worktree
        let result = Command::new("git")
            .args(["-C", &repo_str, "worktree", "remove", "--force", &wt_str])
            .output()
            .map_err(|e| format!("failed to run git worktree remove: {}", e))?;

        if !result.status.success() {
            let stderr = String::from_utf8_lossy(&result.stderr);
            tracing::warn!(
                thread_id = %thread_id,
                error = %stderr.trim(),
                "git worktree remove failed — attempting directory cleanup"
            );
            // Best-effort: remove directory manually
            let _ = std::fs::remove_dir_all(&worktree_path);
        }

        // Best-effort branch cleanup
        let branch_name = format!("aster-orch/{}", thread_id);
        let branch_result = Command::new("git")
            .args(["-C", &repo_str, "branch", "-D", &branch_name])
            .output();

        match branch_result {
            Ok(out) if !out.status.success() => {
                tracing::debug!(
                    thread_id = %thread_id,
                    branch = %branch_name,
                    "branch cleanup skipped (may not exist)"
                );
            }
            Err(e) => {
                tracing::debug!(
                    thread_id = %thread_id,
                    error = %e,
                    "branch cleanup command failed"
                );
            }
            _ => {
                tracing::info!(
                    thread_id = %thread_id,
                    branch = %branch_name,
                    "removed worktree branch"
                );
            }
        }

        Ok(())
    }

    /// List active worktrees by reading a worktree directory.
    ///
    /// `root` is the directory to scan (e.g. the resolved worktree root for a
    /// particular repo). Since worktrees may live in different locations
    /// (per-agent workdir), callers must supply the root to scan.
    ///
    /// **Note:** This reads the filesystem directory, not the git worktree
    /// registry. Orphaned directories (e.g. from a crash before cleanup)
    /// may appear as active worktrees.
    pub fn list_worktrees(&self, root: &Path) -> Result<Vec<WorktreeInfo>, String> {
        let entries = match std::fs::read_dir(root) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(format!("failed to read worktree dir: {}", e)),
        };

        let mut result = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Some(name) = entry.file_name().to_str() {
                    result.push(WorktreeInfo {
                        thread_id: name.to_string(),
                        path,
                    });
                }
            }
        }

        Ok(result)
    }
}

impl Default for WorktreeManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_worktree_manager_new() {
        let _mgr = WorktreeManager::new();
        // Unit struct — nothing to assert beyond construction.
    }

    #[test]
    fn test_worktree_root_default() {
        let root = worktree_root(Path::new("/home/user/repo"), None);
        assert_eq!(root, PathBuf::from("/home/user/.aster-worktrees"));
    }

    #[test]
    fn test_worktree_root_override() {
        let root = worktree_root(
            Path::new("/home/user/repo"),
            Some(Path::new("/custom/worktrees")),
        );
        assert_eq!(root, PathBuf::from("/custom/worktrees"));
    }

    #[test]
    fn test_worktree_root_root_level_repo() {
        // When repo_root is `/`, parent() returns None, so fallback to repo_root itself.
        let root = worktree_root(Path::new("/"), None);
        assert_eq!(root, PathBuf::from("/.aster-worktrees"));
    }

    #[test]
    fn test_list_worktrees_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = WorktreeManager::new();
        let list = mgr.list_worktrees(dir.path()).unwrap();
        assert!(list.is_empty());
    }

    #[test]
    fn test_list_worktrees_nonexistent_dir() {
        let mgr = WorktreeManager::new();
        let list = mgr.list_worktrees(Path::new("/nonexistent/path")).unwrap();
        assert!(list.is_empty());
    }

    #[test]
    fn test_remove_worktree_nonexistent_is_ok() {
        let mgr = WorktreeManager::new();
        // Should succeed silently when worktree doesn't exist
        assert!(mgr
            .remove_worktree(Path::new("/tmp"), "nonexistent-thread", None)
            .is_ok());
    }

    #[test]
    fn test_ensure_worktree_non_git_repo() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = WorktreeManager::new();
        // dir.path() is not a git repo
        let result = mgr
            .ensure_worktree(dir.path(), "test-thread", None)
            .unwrap();
        assert!(result.is_none(), "non-git repo should return None");
    }

    #[test]
    fn test_ensure_worktree_git_repo() {
        let dir = tempfile::tempdir().unwrap();

        // Initialize a real git repo
        let init = Command::new("git")
            .args(["init", &dir.path().to_string_lossy()])
            .output()
            .unwrap();
        assert!(init.status.success(), "git init failed");

        // Need at least one commit for HEAD to exist.
        // Set identity via -c flags so tests work in CI without global git config.
        let _ = Command::new("git")
            .args([
                "-C",
                &dir.path().to_string_lossy(),
                "-c",
                "user.email=test@test.com",
                "-c",
                "user.name=Test",
                "commit",
                "--allow-empty",
                "-m",
                "initial",
            ])
            .output()
            .unwrap();

        let mgr = WorktreeManager::new();
        let result = mgr
            .ensure_worktree(dir.path(), "test-thread-123", None)
            .unwrap();
        assert!(result.is_some(), "git repo should create worktree");
        let wt_path = result.unwrap();
        assert!(wt_path.exists(), "worktree path should exist");
        // Default location: repo_root/../.aster-worktrees/thread_id
        let expected = dir
            .path()
            .parent()
            .unwrap()
            .join(".aster-worktrees")
            .join("test-thread-123");
        assert_eq!(wt_path, expected);

        // Calling again should reuse existing
        let result2 = mgr
            .ensure_worktree(dir.path(), "test-thread-123", None)
            .unwrap();
        assert_eq!(result2, Some(wt_path.clone()));

        // Cleanup
        mgr.remove_worktree(dir.path(), "test-thread-123", None)
            .unwrap();
        assert!(!wt_path.exists(), "worktree should be removed");

        // Also clean up the .aster-worktrees directory
        let wt_root = dir.path().parent().unwrap().join(".aster-worktrees");
        let _ = std::fs::remove_dir_all(&wt_root);
    }

    #[test]
    fn test_ensure_worktree_with_override_dir() {
        let dir = tempfile::tempdir().unwrap();
        let override_dir = tempfile::tempdir().unwrap();

        // Initialize a real git repo
        let init = Command::new("git")
            .args(["init", &dir.path().to_string_lossy()])
            .output()
            .unwrap();
        assert!(init.status.success(), "git init failed");

        let _ = Command::new("git")
            .args([
                "-C",
                &dir.path().to_string_lossy(),
                "-c",
                "user.email=test@test.com",
                "-c",
                "user.name=Test",
                "commit",
                "--allow-empty",
                "-m",
                "initial",
            ])
            .output()
            .unwrap();

        let mgr = WorktreeManager::new();
        let result = mgr
            .ensure_worktree(dir.path(), "test-override", Some(override_dir.path()))
            .unwrap();
        assert!(result.is_some(), "git repo should create worktree");
        let wt_path = result.unwrap();
        assert!(wt_path.exists(), "worktree path should exist");
        assert_eq!(wt_path, override_dir.path().join("test-override"));

        // Cleanup
        mgr.remove_worktree(dir.path(), "test-override", Some(override_dir.path()))
            .unwrap();
        assert!(!wt_path.exists(), "worktree should be removed");
    }
}
