# Merge Queue for Worktree Branch Integration

Status: Complete
Owner: operator
Created: 2026-03-21

## Scope Summary

- FIFO merge queue backed by SQLite for serialized worktree branch merges
- Worker-executed merges in temporary worktrees (never touches main checkout)
- MCP tools for operator merge requests, status, and cancellation
- EventBus integration and dashboard visibility
- CLI wait support for scripting

## Ticket MERGE-1 — Store schema and methods for merge_operations

- Goal: Add the `merge_operations` table and all store methods needed by the merge queue
- In scope:
  - `merge_operations` table creation in `Store::setup()` (CREATE TABLE IF NOT EXISTS)
  - Index: `idx_merge_ops_target_status` on `(target_branch, status)`
  - Index: `idx_merge_ops_thread` on `(thread_id)`
  - `MergeOperationStatus` enum: `Queued`, `Claimed`, `Executing`, `Completed`, `Failed`, `Cancelled`
  - `MergeOperation` struct matching table columns
  - Store methods: `insert_merge_op`, `claim_next_merge_op` (atomic select-update serialized per target_branch), `update_merge_op_status`, `get_merge_op`, `list_merge_ops` (filterable by target_branch/status/thread_id), `mark_stale_merge_ops_failed` (timeout-based), `cancel_merge_op` (queued only), `has_pending_merge_for_thread` (for worktree cleanup guard)
  - Modify `threads_with_stale_worktrees` query to exclude threads with pending merge ops
  - Unit tests for all store methods, especially claim serialization per target_branch
  - CHANGELOG entry
- Out of scope:
  - MergeExecutor logic (MERGE-2)
  - MCP tool handlers (MERGE-4)
  - Worker integration (MERGE-3)
- Dependencies: none
- Acceptance criteria:
  - `claim_next_merge_op` only returns an op when no other op for the same `target_branch` is `claimed` or `executing`
  - Concurrent claims for same target_branch are serialized (only one succeeds)
  - `threads_with_stale_worktrees` excludes threads with queued/claimed/executing merge ops
  - All store methods have unit tests
  - `make verify` passes
- Verification:
  - `make verify`
- Status: Done

## Ticket MERGE-2 — MergeExecutor with temporary worktree isolation

- Goal: Implement git merge operations in temporary worktrees, with conflict detection
- In scope:
  - New module `src/merge.rs`
  - `MergeExecutor` struct with methods: `preflight_check`, `execute`
  - Preflight checks: thread is Completed or Failed, source branch exists, worktree clean, no duplicate pending op
  - Temporary merge worktree: `git worktree add .compas-worktrees/merge-{op_id} {target_branch}`
  - Merge strategies: merge (`git merge --no-edit`), rebase (`git rebase`), squash (`git merge --squash`)
  - Conflict detection: `git diff --name-only --diff-filter=U` on conflict, then `git merge --abort`
  - Merge worktree cleanup on success or failure (always remove temp worktree)
  - Orphaned merge worktree detection and cleanup (for crash recovery)
  - Unit tests with temp git repos (merge, rebase, squash, conflict, missing branch)
  - CHANGELOG entry
- Out of scope:
  - Push support (deferred to v2, requires auth story)
  - Conflict resolution workflow (deferred)
  - Source branch auto-delete (deferred)
- Dependencies: MERGE-1
- Acceptance criteria:
  - Merge executes in `.compas-worktrees/merge-{op_id}` — operator's main checkout is never touched
  - On conflict: merge is aborted, conflicting files are returned, source branch is intact
  - On success: target branch contains merged changes
  - Temp merge worktree is cleaned up in all code paths (success, failure, error)
  - All three strategies (merge, rebase, squash) work
  - `make verify` passes
- Verification:
  - `make verify`
- Status: Done

## Ticket MERGE-3 — Worker integration for merge queue polling

- Goal: Add merge queue polling to the worker loop with crash recovery and stale detection
- In scope:
  - New poll arm in `WorkerRunner::run` select! loop: `merge_interval.tick()`
  - `poll_merge_ops` method: claim next op, dispatch to `spawn_blocking` for MergeExecutor
  - Update merge op status through lifecycle: claimed → executing → completed/failed
  - Crash recovery on startup: mark claimed/executing merge ops as failed
  - Orphaned merge worktree cleanup on startup (via MergeExecutor)
  - Stale merge op detection: ops in claimed/executing beyond `merge_timeout_secs` (default 30s)
  - CHANGELOG entry
- Out of scope:
  - MCP tools (MERGE-4)
  - EventBus events (MERGE-5)
  - Dashboard rendering (MERGE-5)
- Dependencies: MERGE-1, MERGE-2
- Acceptance criteria:
  - Worker polls and executes queued merge operations
  - Only one merge per target_branch executes at a time
  - On worker restart, orphaned merge ops are marked failed
  - Orphaned merge worktrees (`merge-*`) are cleaned up on startup
  - Stale merge ops (>30s in claimed/executing) are marked failed
  - `make verify` passes
- Verification:
  - `make verify`
- Status: Done

## Ticket MERGE-4 — MCP tools for merge operations

- Goal: Expose merge queue to operators via 3 MCP tools
- In scope:
  - `orch_merge` tool: validate params, run preflight, insert merge op, return op_id + wait command
  - `orch_merge_status` tool: query merge queue state or specific operation detail
  - `orch_merge_cancel` tool: cancel queued merge op
  - Params structs in `src/mcp/params.rs`
  - Handler implementations in `src/mcp/merge.rs`
  - Tool registration in `src/mcp/server.rs`
  - `suggested_actions` in status response on failure (guide operator to resolve conflicts)
  - Integration tests
  - README.md update (new tools section)
  - CHANGELOG entry
- Out of scope:
  - Push support in orch_merge params (deferred)
  - Batch merge operations
- Dependencies: MERGE-1, MERGE-2
- Acceptance criteria:
  - `orch_merge` rejects Active/Abandoned threads, missing branches, dirty worktrees, duplicate requests
  - `orch_merge` returns op_id and next_step wait command
  - `orch_merge_status` shows queue overview and per-op detail with conflict_files
  - `orch_merge_cancel` only cancels queued ops
  - Integration tests cover happy path and error cases
  - `make verify` passes
- Verification:
  - `make verify`
- Status: Done

## Ticket MERGE-5 — EventBus events and dashboard merge queue view

- Goal: Observable merge operations via events and dashboard UI
- In scope:
  - `MergeQueued`, `MergeStarted`, `MergeCompleted` event variants in `OrchestratorEvent`
  - Emit events from worker merge poll loop (MERGE-3 integration)
  - Dashboard: merge queue section in Activity tab (executing + queued ops, recent results)
  - Dashboard subscription to merge events for live updates
  - CHANGELOG entry
- Out of scope:
  - Desktop notifications for merge completion (follow-up)
  - Detailed merge diff view in dashboard
- Dependencies: MERGE-3
- Acceptance criteria:
  - Events emitted at each merge lifecycle transition
  - Dashboard shows active/queued merge operations
  - Dashboard updates live on merge events (no polling delay)
  - `make verify` passes
- Verification:
  - `make verify`
  - Visual verification of dashboard merge queue section
- Status: Done

## Ticket MERGE-6 — CLI wait-merge subcommand

- Goal: Blocking CLI wait for merge completion, for scripting and operator workflows
- In scope:
  - `compas wait-merge --op-id <id> --timeout <secs>` subcommand in `src/bin/compas.rs`
  - Poll `merge_operations` table for terminal status (completed/failed/cancelled)
  - Output merge result on completion (summary, conflict_files on failure)
  - Follows same pattern as existing `compas wait`
  - CHANGELOG entry
- Out of scope:
  - Wait for all merges in a batch
- Dependencies: MERGE-1
- Acceptance criteria:
  - Blocks until merge op reaches terminal status or timeout
  - Outputs result summary on completion
  - Outputs conflict_files and error_detail on failure
  - Exits with non-zero on failure/timeout
  - `make verify` passes
- Verification:
  - `make verify`
- Status: Done

## Ticket MERGE-7 — ADR-019 and config additions

- Goal: Document architectural decisions and add configuration
- In scope:
  - ADR-019 in DECISIONS.md: merge queue design, queue-over-lock rationale, temporary worktree isolation, per-target serialization
  - Config fields in `OrchestrationConfig`: `merge_timeout_secs` (default 30), `default_merge_strategy` (default "merge")
  - Config validation and defaults
  - CHANGELOG entry
- Out of scope:
  - `merge_auto_push` config (deferred with push support)
- Dependencies: none
- Acceptance criteria:
  - ADR-019 documents all key decisions from architect review
  - Config fields parse correctly with defaults
  - `make verify` passes
- Verification:
  - `make verify`
- Status: Done

## Execution Order

1. MERGE-7 (ADR + config — can start immediately, no code deps)
2. MERGE-1 (Store schema — foundational)
3. MERGE-2 + MERGE-6 (MergeExecutor + CLI wait — parallelizable after MERGE-1)
4. MERGE-3 + MERGE-4 (Worker integration + MCP tools — parallelizable after MERGE-1 + MERGE-2)
5. MERGE-5 (EventBus + Dashboard — after MERGE-3)

## Dispatch Plan

| Ticket | Agent | Rationale |
|---|---|---|
| MERGE-7 | compas-dev-2 | Low-risk docs + config, sonnet-capable |
| MERGE-1 | compas-dev | Foundational store work, follows existing patterns, opus for correctness |
| MERGE-2 | compas-dev | Core merge logic, new module, opus for safety |
| MERGE-6 | compas-dev-2 | Follows existing wait pattern, sonnet-capable |
| MERGE-3 | compas-dev | Worker integration, opus for correctness |
| MERGE-4 | compas-dev-2 | MCP tools follow established patterns, sonnet-capable |
| MERGE-5 | compas-dev-2 | EventBus + dashboard, sonnet-capable |

## Tracking Notes

- Backlog-first governance applies.
- Implementation commits should reference ticket IDs.
- Architect review completed (thread 01KM8KDH2TWQBGYC5W4PC7NDDP) — approved with modifications (all incorporated).
- Key architect recommendations incorporated: focused table (no generalization), temporary worktree isolation, no push in v1, 30s timeout, Failed threads eligible.
