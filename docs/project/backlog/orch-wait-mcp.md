# MCP Self-Service Tools

Status: Active
Owner: operator
Created: 2026-03-22

## Scope Summary

- Re-register preserved `orch_wait` MCP tool with transport-safe timeout handling (done)
- Add `orch_wait_merge` MCP tool for merge operation waiting
- Add `orch_commit` MCP tool for committing worktree changes from MCP-only agents

## Ticket WAIT-MCP-1 — Re-register orch_wait with transport-safe timeout and await_chain

- Goal: Re-expose the preserved `orch_wait` implementation as an MCP tool with configurable timeout ceiling, `await_chain` support, and `chain_pending` timeout disambiguation.
- In scope:
  - Add `await_chain: Option<bool>` to `WaitParams` (`src/mcp/params.rs`)
  - Add `mcp_wait_max_timeout_secs` config field to `OrchestrationConfig` (`src/config/types.rs`, default 120)
  - Add `chain_pending: bool` to `WaitOutcome::Timeout` variant (`src/wait.rs`)
  - Update `wait_impl` to default timeout to 60s, clamp to config max, pass `await_chain` through (`src/mcp/wait.rs`)
  - Update CLI handler for new `chain_pending` field (`src/bin/compas.rs`)
  - Re-register `#[tool]` stub with `Peer<RoleServer>` + `Meta` extractors for progress notifications (`src/mcp/server.rs`)
  - Update MCP server instructions and `orch_dispatch` description to reference `orch_wait` (`src/mcp/server.rs`)
  - Fix existing test constructions and add new tests for timeout clamping and `chain_pending` (`tests/integration_tests.rs`)
  - Update config docs (`docs/guides/configuration.md`)
  - Add changelog fragment
- Out of scope:
  - Transport timeout discovery/negotiation (not possible in MCP spec)
  - Merging poll/wait return shapes
  - Removing `compas wait` CLI (remains for non-MCP usage)
  - Fan-out settlement metadata (separate ticket WAIT-AX-1)
- Dependencies: none.
- Acceptance criteria:
  - `orch_wait` is registered and callable via MCP
  - Default timeout is 60s, clamped to `mcp_wait_max_timeout_secs` config value
  - `await_chain=true` blocks until handoff/fan-out chain settles
  - Timeout response includes `chain_pending: true` when chain work was still pending
  - Timeout response includes `chain_pending: false` when no chain work was pending
  - Progress notifications sent every 10s to prevent transport timeouts
  - All existing wait tests pass with updated field additions
  - New tests cover timeout clamping and chain_pending behavior
- Verification:
  - `make verify` passes (fmt-check + clippy + test + lint-md)
  - Manual: dispatch work via MCP, call `orch_wait`, confirm found response with full message body
- Status: Done

## Ticket MCP-2 — `orch_wait_merge` MCP tool

- Goal: Allow MCP-connected agents to wait for merge completion without shelling out to `compas wait-merge` CLI.
- In scope:
  - New `orch_wait_merge` MCP tool in `src/mcp/merge.rs`
  - Parameters: `op_id` (required), `timeout_secs` (default 120)
  - Poll `merge_operations` table for terminal status (`completed`, `failed`, `cancelled`)
  - Progress notifications every 10s (same pattern as `orch_wait`)
  - Return merge result (status, source/target branch, conflict info if failed)
- Out of scope:
  - Changing merge queue behavior
- Dependencies: None
- Acceptance criteria:
  - `orch_wait_merge(op_id="<id>")` blocks until merge completes or times out
  - Returns merge status and branch info on completion
  - Progress notifications prevent transport timeout
  - `make verify` passes
- Verification:
  - Integration test: close worktree thread → get merge_op_id → `orch_wait_merge` → verify completed
  - `make verify`
- Status: Todo
- Complexity: S
- Risk: Low

## Ticket MCP-3 — `orch_commit` MCP tool

- Goal: Allow MCP-only agents (e.g., Claude Desktop) to commit changes in their worktree without shell access. Closes the self-service gap documented in known-issues.md.
- In scope:
  - New `orch_commit` MCP tool in `src/mcp/` (new handler or added to existing module)
  - Parameters: `thread_id` (required), `message` (required commit message)
  - Resolves the thread's worktree path from the store
  - Runs `git add -A` + `git commit -m <message>` in the worktree via `std::process::Command` with separate args — do NOT use `sh -c` string interpolation (injection risk)
  - Returns: commit SHA, files changed count, or error if no changes / no worktree
  - Validates thread is Active and has a worktree
- Out of scope:
  - Selective staging (always `add -A`)
  - Amending commits
  - Push to remote
- Dependencies: None
- Acceptance criteria:
  - MCP-only agent can call `orch_commit(thread_id="<id>", message="description")` and get a commit SHA back
  - Subsequent `orch_close(status="completed")` triggers auto-merge with the committed changes
  - Error if thread has no worktree or no uncommitted changes
  - `make verify` passes
- Verification:
  - Integration test: create worktree thread → write file → `orch_commit` → verify commit exists on branch
  - Manual: Claude Desktop agent edits files, calls `orch_commit`, closes thread, changes merge
  - `make verify`
- Status: Todo
- Complexity: S
- Risk: Low
- Notes: Closes known-issues.md "MCP-only agents cannot commit worktree changes"

## Execution Order

1. ~~WAIT-MCP-1~~ (done)
2. MCP-2 (orch_wait_merge)
3. MCP-3 (orch_commit)

## Tracking Notes

- Backlog-first governance applies.
- Implementation commits should reference ticket IDs.
- Record scope changes/deferrals here.
- Implementation plan: local only (not committed)
