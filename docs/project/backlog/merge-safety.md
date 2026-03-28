# Merge Safety Improvements

Status: Active
Owner: otto
Created: 2026-03-28

## Scope Summary

- Detect and report no-op merges where source branch has no commits ahead of target
- Prevent silent data loss when agent leaves changes uncommitted in worktree

## Ticket MERGE-1 — Detect No-Op Merges and Uncommitted Worktree Changes

- Goal: Prevent the merge executor from reporting `status=completed` when no work was actually merged, and detect uncommitted changes in the worktree before merge.
- In scope:
  - Before running `git merge`, always check for uncommitted changes in the agent worktree (dirty-worktree check runs first, regardless of divergence).
  - If worktree has uncommitted changes (whether or not source is ahead of target): report merge as `failed` with `error: "Source branch has uncommitted changes — agent did not commit its work"` and list the dirty files in `conflict_files` (reuse the field for diagnostic visibility).
  - If source == target (no divergence) AND worktree is clean: report merge as `failed` with `error: "No commits to merge — source branch is identical to target"`.
  - `orch_merge_status` and `wait merge` surface the error clearly so the operator can take corrective action (commit the work, re-queue merge).
  - Same detection applies to all strategies (merge, rebase, squash).
- Out of scope:
  - Auto-committing uncommitted changes (operator decision).
  - Retry/re-queue logic (operator handles manually).
- Dependencies: None
- Acceptance criteria:
  - Merge of identical branches returns `status=failed` with descriptive error, not `status=completed`.
  - Merge where worktree has uncommitted changes returns `status=failed` with file list — regardless of whether source is ahead of target.
  - `wait merge` shows the error in its output.
  - Normal merges (source ahead of target, clean worktree) are unaffected.
  - `make verify` passes.
- Verification:
  - Unit test: merge with identical branches → failed with "no commits" error.
  - Unit test: merge with uncommitted changes (no divergence) → failed with file list.
  - Unit test: merge with uncommitted changes (source ahead) → failed with file list.
  - Unit test: merge with clean worktree and commits ahead → succeeds normally.
  - Integration test: no-op clean worktree → failed with "No commits to merge".
  - Integration test: no-op dirty worktree → failed with "uncommitted changes" and file list.
  - Integration test: partial commit dirty worktree → failed with "uncommitted changes" and file list.
  - Integration test: existing merge tests pass unchanged.
  - `make verify` (fmt-check + clippy + test + lint-md).
- Status: In Progress
- Complexity: S
- Risk: Low
- Notes: Discovered during GAP-5 dispatch — worker left all changes uncommitted, merge reported `completed` with `duration_ms=0`. The operator had no signal that the merge was empty until manually checking `git log`. The `git merge` command exits 0 with "Already up to date." when branches are identical, so the current success check (`merge_output.status.success()`) is insufficient. The fix is a pre-merge divergence check via `git rev-list --count {target}..{source}`.

## Execution Order

1. MERGE-1

## Tracking Notes

- Backlog-first governance applies.
- Implementation commits should reference ticket IDs (MERGE-N).
- Record scope changes/deferrals here.

## Execution Metrics

- Ticket: MERGE-1
- Owner: otto
- Complexity: S
- Risk: Low
- Start:
- End:
- Duration:
- Notes:

## Closure Evidence

- MERGE-1 implemented: pre-merge validation detects no-op merges and uncommitted worktree changes
- Behavior: `MergeExecutor::execute()` returns `MergeResult { success: false }` with descriptive error for (1) dirty worktrees (file list in `conflict_files`), (2) zero-divergence branches, (3) dirty_files check failures (defense-in-depth). Normal merges unaffected.
- ADR-025 in DECISIONS.md covers conflict_files reuse, check ordering, TOCTOU rationale
- Changelog fragment via `changie new`
- Verification:
  - `make verify`: 174 tests passed, 0 failed; fmt-check, clippy, lint-md all green
- Deferred:
  - None
