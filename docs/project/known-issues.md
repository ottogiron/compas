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
**Status:** Open

`--with-worker` spawns the worker as an independent OS process that survives dashboard exit. This is intentional (don't kill running executions when closing the dashboard), but causes problems:
- Stale workers running old code after rebuild
- Multiple orphaned workers accumulating
- Heartbeat guard prevents new worker spawn when stale worker is still alive

**Workaround:** Manually kill worker processes before restarting: `pgrep -fl aster_orch` then `kill <pid>`.

**Planned fix:** Graceful shutdown — dashboard sends SIGTERM on exit, worker finishes current execution then exits. Or embed worker in-process (same tokio runtime).
