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
**Status:** Open

Observed on thread `01KMBF0AR215Q733NA0APTD9AC` (compas-architect). The agent completed its response (telemetry shows `turn_complete` with `subtype: success`), the response message was inserted into the thread (db:1830), but the execution row was never finalized — `status` remains `executing`, `finished_at` is NULL.

The Claude CLI process has already exited (no orphaned process). The response is not lost — it's in the thread. But the execution is permanently stuck, blocking health/task reporting.

**Symptoms:**
- `orch_tasks` shows execution as `executing` indefinitely
- `orch_diagnose` says "execution in progress"
- `orch_poll` returns the response message (work is done)
- No Claude process running for the execution

**Suspected cause:** The post-execution finalization path in `handle_trigger_output()` (loop_runner.rs) failed silently after inserting the response message but before calling `store.mark_execution_completed()`. Possible causes:
- New circuit breaker integration (GAP-1, v0.4.0) panicking or erroring in the success path
- A store write error being swallowed
- Race condition between telemetry consumer and executor finalization

**Workaround:** Manually update the execution row: `UPDATE executions SET status = 'completed', finished_at = unixepoch() WHERE id = '<exec_id>'`

**Investigation:** Check `handle_trigger_output()` success path for any new code (circuit breaker state update, DB persist) that could fail silently after response insertion.

## Dashboard: No mouse support

**Severity:** Low
**Status:** Open

The TUI dashboard is keyboard-only. Mouse support for clicking list items, drilling down into threads, and selecting tabs would improve ergonomics, especially for operators used to GUI tools. Ratatui supports mouse events via crossterm.
