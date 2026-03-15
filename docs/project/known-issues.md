# Known Issues — Aster Orchestrator

## MCP transport latency on large transcripts

**Severity:** Low
**Status:** Open

`orch_transcript` for threads with many long messages can be slow due to JSON serialization over stdio MCP transport. Not a problem for typical thread sizes (<50 messages).

**Workaround:** Use `orch_poll` with `since_reference` for incremental reads instead of full transcript.

## Dashboard polling overhead

**Severity:** Low
**Status:** Open (addressed by ORCH-EVO-2)

Dashboard polls SQLite at a fixed interval. No push-based updates. Can feel sluggish for real-time monitoring of fast-moving executions.

**Planned fix:** ORCH-EVO-2 (Event Broadcast Channel) will enable push-based dashboard updates.

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
**Status:** Open

When a worker process dies (killed manually, crashed, old binary replaced) but its heartbeat record remains in SQLite, the dashboard's `--with-worker` flag detects the heartbeat and skips spawning a new worker. The result: dispatches queue but never execute — the worker is gone but the system thinks it's alive.

This happens frequently during development: deploy a new binary, restart the dashboard, but the old worker's heartbeat persists. The dashboard starts without a worker and dispatches silently pile up.

**How to detect:** `orch_health()` shows a heartbeat with a stale `started_at` timestamp. `orch_metrics()` shows `queue_depth > 0` but no running executions. Dispatches stay in "Active" with 0 executions.

**Workaround:** Manually clear heartbeats: `sqlite3 <state_dir>/jobs.sqlite "DELETE FROM worker_heartbeats;"` then restart the dashboard.

**Planned fix:** The heartbeat guard should validate that the worker process is actually alive (e.g., `kill(pid, 0)` check) before deciding not to spawn. If the process doesn't exist, the stale heartbeat should be cleared automatically. Alternatively, the heartbeat should include a TTL — if no heartbeat update within 30s (2x the 10s heartbeat interval), consider the worker dead.

## Dashboard: Active threads section always appears empty

**Severity:** Medium
**Status:** Open

The Ops tab "Active" section shows no threads. Threads in Active state with no running execution don't appear — only "Running" (executing) and completed threads are visible. Active threads waiting for operator action (review, re-dispatch) should show in the Active section.

## Dashboard: No mouse support

**Severity:** Low
**Status:** Open

The TUI dashboard is keyboard-only. Mouse support for clicking list items, drilling down into threads, and selecting tabs would improve ergonomics, especially for operators used to GUI tools. Ratatui supports mouse events via crossterm.
