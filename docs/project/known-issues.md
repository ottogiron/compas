# Known Issues — Compas

## MCP transport latency on large transcripts

**Severity:** Low
**Status:** Open

`orch_transcript` for threads with many long messages can be slow due to JSON serialization over stdio MCP transport. Not a problem for typical thread sizes (<50 messages).

**Workaround:** Use `orch_poll` with `since_reference` for incremental reads instead of full transcript.

## Dashboard polling overhead

**Severity:** Low
**Status:** Fixed (ORCH-EVO-2)

Dashboard now uses `tokio::broadcast` event channel for push-based updates. SQLite polling is supplementary (1.5s debounce for progress summaries). Resolved by ORCH-EVO-2 (Event Broadcast Channel).

## Claude: internal UUID may be saved as backend session ID

**Severity:** Low
**Status:** Fixed (GAP-2a)

The executor now compares `result.session_id` against the internal `session.id` before persisting. Only IDs that came from actual backend JSON output are saved. If they match (meaning no real session ID was extracted), the persist is skipped.

## Stale backend session IDs cause hard execution failures

**Severity:** Low
**Status:** Fixed (GAP-2b)

Claude stale session errors (`"No conversation found with session ID"`) are now classified as `StaleSession` (retryable). When detected, the worker clears the persisted session ID and retries with a fresh session. Codex self-heals (silently starts fresh), so no fix needed. OpenCode hangs on invalid sessions — see separate issue below.

## OpenCode hangs on invalid session IDs

**Severity:** Low
**Status:** Open

When OpenCode receives an invalid or expired session ID, it hangs indefinitely instead of returning an error. The execution timeout catches this eventually, but the error is classified as `Unknown` (not retryable) rather than `StaleSession` because OpenCode produces no matchable error pattern.

**Workaround:** The execution timeout (`execution_timeout_secs`) catches the hang. The thread fails and can be re-dispatched.

## Worker processes orphaned on dashboard exit

**Severity:** Medium
**Status:** Fixed on Unix (clean exit path). Non-unix platforms remain unresolved.

Dashboard now sends SIGTERM on exit. Worker drains in-flight executions (up to `execution_timeout_secs`) then exits cleanly. The dashboard waits up to 10s for the worker to exit before returning.

**Remaining edge case:** If the dashboard crashes or is killed with SIGKILL before the cleanup block runs, the worker remains orphaned. The singleton guard (ADR-016) now prevents the worst outcome: a second worker starting and blanket-crashing the first worker's in-flight executions via `mark_orphaned_executions_crashed`. The next `compas worker` or `compas dashboard` startup detects the orphaned worker via lockfile + heartbeat/PID check and fails fast with an actionable error (PID, heartbeat age, kill hint). The stale process must still be killed manually.

## Stale worker heartbeat prevents new worker spawn

**Severity:** High
**Status:** Fixed

`is_worker_alive()` now checks both heartbeat freshness AND process liveness via `kill(pid, 0)` with ESRCH/EPERM handling. A stale heartbeat from a dead process is correctly detected and a new worker is spawned. No manual heartbeat clearing needed.

## Dashboard: Active threads section always appears empty

**Severity:** Medium
**Status:** Fixed

The `is_active_waiting` filter excluded threads where the latest execution was completed. Active threads waiting for operator review (execution done, thread still Active) fell through to "recently completed" instead. Fixed by removing the completed-execution exclusion.

## Desktop notifications lack task context

**Severity:** Low
**Status:** Open

Notifications say "compas: focused completed / Execution completed in 2m 15s" but don't include **what** the agent was working on. The dispatch body (task description) or batch ID would make notifications actionable without switching to the dashboard.

**Root cause:** `ExecutionCompleted` event only carries `agent_alias`, `success`, `duration_ms` — no task description. Including context requires a store lookup by `thread_id` to fetch the original dispatch message, adding a store dependency to the notification consumer.

**Options:**

- Add `batch_id` and/or a short `description` field to `ExecutionCompleted` event (enriches the event at emission time)
- Notification consumer does a store lookup on each completion (adds coupling)
- Include first N chars of the dispatch body in `ExecutionStarted` and carry forward

## Dirty worktree cleanup retries forever with no operator escape hatch

**Severity:** Low
**Status:** Open

When a thread is closed but its worktree has uncommitted changes, the worker cleanup loop skips deletion and retries every ~60 seconds indefinitely. There is no MCP tool flag to force-delete a dirty worktree — the only recourses are:

1. Commit or stash the changes in the worktree branch (`compas/{thread_id}`)
2. Manually delete the worktree directory and run `git worktree prune` in the repo root
3. Stop the worker, delete the worktree manually, restart

**Root cause:** ADR-017 intentionally treats dirty worktrees as unsafe to delete. An escape hatch (`force_cleanup` flag on `orch_close`) was deferred as out of scope for the initial fix.

**Planned fix:** Add `force_cleanup: bool` to `orch_close` to allow operators to override the guard explicitly when they know the changes can be discarded.

## Execution stuck in "executing" after agent completes (v0.4.0)

**Severity:** Medium
**Status:** Fixed

Observed on thread `01KMBF0AR215Q733NA0APTD9AC` (compas-architect). The agent completed its response (telemetry shows `turn_complete` with `subtype: success`), the response message was inserted into the thread (db:1830), but the execution row was never finalized — `status` remains `executing`, `finished_at` is NULL.

The Claude CLI process has already exited (no orphaned process). The response is not lost — it's in the thread. But the execution is permanently stuck, blocking health/task reporting.

**Symptoms:**

- `orch_tasks` shows execution as `executing` indefinitely
- `orch_diagnose` says "execution in progress"
- `orch_poll` returns the response message (work is done)
- No Claude process running for the execution

**Root cause:** `PRAGMA busy_timeout` is a per-connection SQLite setting but was only set on one connection during `store.setup()`. The pool has up to 32 connections; the other 31 used the default `busy_timeout=0`, causing immediate `SQLITE_BUSY` errors under any write contention (telemetry flush, heartbeat, stale checker, MCP server). The `Err` from `complete_execution` was logged at `warn` level and silently swallowed, leaving the execution row permanently stuck.

**Fix:** Set `busy_timeout` via `SqliteConnectOptions::pragma()` so every pool connection inherits it, and added `finalize_with_retry` with exponential backoff as defense-in-depth. Finalization failures now log at `error` level.

## MCP-only agents cannot commit worktree changes

**Severity:** Medium
**Status:** Open

Agents connected via MCP (e.g., Claude Desktop) can read files, edit files, and call all `orch_*` tools — but they have no shell access. They cannot run `git commit` in their worktree. This means:

- The agent finishes editing files in the worktree
- `orch_merge` produces a no-op merge (changes are uncommitted, so there's nothing to merge)
- `orch_close(status="completed")` is then refused because no completed merge exists (merge-before-close gate)
- The operator must manually commit in the worktree before merging and closing the thread

This breaks the self-service loop for MCP-only agents. CLI-based agents (Claude Code, Codex, OpenCode) don't have this problem — they commit as part of their execution.

**Workaround:** The operator commits on the agent's behalf:

```bash
git -C .compas-worktrees/<thread-id> add -A && git -C .compas-worktrees/<thread-id> commit -m "<description>"
```

Then close the thread normally.

**Possible fix:** Add an `orch_commit(thread_id, message)` MCP tool that commits all changes in the thread's worktree. This would close the self-service gap for MCP-only agents.

## Dispatch → wait flow needs redesign for MCP agents

**Severity:** Medium
**Status:** Open

The `orch_dispatch` → `orch_wait` flow has friction for MCP-connected agents:

1. **`next_step` hint is ambiguous** — the natural language hint tells agents to use `orch_wait` with `await_chain=true`, but agents interpret conditional phrasing ("if the agent uses auto-handoff") as "doesn't apply" and skip the wait entirely.

2. **Dispatch and wait are separate calls** — agents must remember to call `orch_wait` after every `orch_dispatch`. The orch-dispatch skill says "always wait after dispatch" but agents without the skill loaded don't know this. A combined `dispatch_and_wait` tool or an auto-wait option on dispatch would eliminate the gap.

3. **`await_chain` requires knowledge the caller doesn't have** — the caller doesn't know if the target agent has handoff config. Making `await_chain=true` the safe default helps but is still a parameter the caller must remember.

4. ~~**Timeout ceiling mismatch**~~ — **Resolved.** The wait ceiling is now derived from `execution_timeout_secs` (non-chain: `exec + 30`, chain: `exec * 3 + 30`). Clamped requests include `clamped`, `effective_timeout_secs`, `requested_timeout_secs`, and a `hint` in the response so agents know their wait was shortened.

**Additional friction reported by Codex operator (2026-03-22):**

- **Agent discovery** — the agent had to guess `compas-worker` and fail before learning aliases. Listing available aliases in the `orch_dispatch` tool description (or a lighter-weight discovery call) would save a round-trip.
- **Response attribution is surprising** — dispatched to `compas-dev` but the terminal response came from `compas-reviewer` (via handoff chain). With `await_chain=true`, intermediate hops are invisible. A chain trace in the wait result (e.g., `chain: [compas-dev → compas-reviewer]`) would make the flow legible.
- **Tool loading ceremony** — agent had to ToolSearch twice (dispatch, then wait) with a confirmation round-trip each time. Pre-loading the core `dispatch/wait/status` trio would make the common path smoother.

**Possible fixes:**

- `orch_dispatch_and_wait` combined tool that dispatches and blocks in one call
- `auto_wait: true` parameter on `orch_dispatch` that makes it block until response
- Remove `next_step` field entirely — rely on skill instructions and tool descriptions instead
- Make `await_chain=true` the server-side default (not a caller responsibility)
- Include available agent aliases in `orch_dispatch` tool description
- Add `chain_trace` field to `orch_wait` response showing the handoff path

**Design goal:** The basic dispatch → wait → act protocol should work without loading any skill. Tool descriptions, parameter defaults, and response hints should be sufficient for any MCP-connected agent to use compas correctly on first contact. The `orch-dispatch` skill should only add advanced workflows (reviewer loops, ticket lifecycle, merge queue), not basic operational knowledge.

## Dashboard: No mouse support

**Severity:** Low
**Status:** Open

The TUI dashboard is keyboard-only. Mouse support for clicking list items, drilling down into threads, and selecting tabs would improve ergonomics, especially for operators used to GUI tools. Ratatui supports mouse events via crossterm.
