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
  - Before running `git merge`, check if the source branch has commits ahead of the target. If not, check for uncommitted changes in the worktree.
  - If source == target (no divergence) AND worktree is clean: report merge as `failed` with `error: "No commits to merge — source branch is identical to target"`.
  - If source == target AND worktree has uncommitted changes: report merge as `failed` with `error: "Source branch has uncommitted changes — agent did not commit its work"` and list the dirty files in `conflict_files` (reuse the field for diagnostic visibility).
  - `orch_merge_status` and `wait-merge` surface the error clearly so the operator can take corrective action (commit the work, re-queue merge).
  - Same detection applies to all strategies (merge, rebase, squash).
- Out of scope:
  - Auto-committing uncommitted changes (operator decision).
  - Retry/re-queue logic (operator handles manually).
- Dependencies: None
- Acceptance criteria:
  - Merge of identical branches returns `status=failed` with descriptive error, not `status=completed`.
  - Merge where worktree has uncommitted changes returns `status=failed` with file list.
  - `wait-merge` shows the error in its output.
  - Normal merges (source ahead of target, clean worktree) are unaffected.
  - `make verify` passes.
- Verification:
  - Unit test: merge with identical branches → failed with "no commits" error.
  - Unit test: merge with uncommitted changes → failed with file list.
  - Integration test: existing merge tests pass unchanged.
  - `make verify` (fmt-check + clippy + test + lint-md).
- Status: Todo
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

- <ticket completion summary>
- <behavior delivered>
- <docs/ADR/changelog parity summary>
- Verification:
  - `<command>`: <result>
  - `<command>`: <result>
- Deferred:
  - <deferred item and why>
