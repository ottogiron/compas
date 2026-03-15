# Known Issues — Aster Orchestrator

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
**Status:** Open

If Claude exits 0 but produces non-JSON output (rare — startup failures, rate-limit splash pages), the internal UUID is saved as `backend_session_id`. The next dispatch passes `-r <uuid>` to Claude CLI, which rejects it as a non-existent session, causing execution failure.

**Workaround:** Abandon the thread and re-dispatch. The next execution starts a fresh session.

**Planned fix:** Compare `result.session_id` against the internal `session.id` before persisting — only save IDs that came from actual backend JSON output.

## Stale backend session IDs cause hard execution failures

**Severity:** Low
**Status:** Open

If a persisted backend session ID has expired or been pruned by the provider (overnight expiry, key rotation, provider-side cleanup), the CLI rejects the resume attempt and the execution fails. There is no automatic "session not found → retry as fresh session" fallback.

**Affects:** Claude, Codex, OpenCode (all backends with session resume).

**Workaround:** Abandon the thread and re-dispatch, or close and open a new thread for the same task.

**Planned fix:** Per-backend session-not-found detection — if resume fails with a recognizable error pattern, retry as a fresh session automatically.

## Worker processes orphaned on dashboard exit

**Severity:** Medium
**Status:** Fixed on Unix (clean exit path). Non-unix platforms remain unresolved.

Dashboard now sends SIGTERM on exit. Worker drains in-flight executions (up to `execution_timeout_secs`) then exits cleanly. The dashboard waits up to 10s for the worker to exit before returning.

**Remaining edge case:** If the dashboard crashes or is killed with SIGKILL before the cleanup block runs, the worker remains orphaned. Crash recovery on next startup (`mark_orphaned_executions_crashed`) handles the execution state, but the stale worker process must be killed manually.

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

Notifications say "aster-orch: focused completed / Execution completed in 2m 15s" but don't include **what** the agent was working on. The dispatch body (task description) or batch ID would make notifications actionable without switching to the dashboard.

**Root cause:** `ExecutionCompleted` event only carries `agent_alias`, `success`, `duration_ms` — no task description. Including context requires a store lookup by `thread_id` to fetch the original dispatch message, adding a store dependency to the notification consumer.

**Options:**
- Add `batch_id` and/or a short `description` field to `ExecutionCompleted` event (enriches the event at emission time)
- Notification consumer does a store lookup on each completion (adds coupling)
- Include first N chars of the dispatch body in `ExecutionStarted` and carry forward

## Dashboard: No mouse support

**Severity:** Low
**Status:** Open

The TUI dashboard is keyboard-only. Mouse support for clicking list items, drilling down into threads, and selecting tabs would improve ergonomics, especially for operators used to GUI tools. Ratatui supports mouse events via crossterm.
