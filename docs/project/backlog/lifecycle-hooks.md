# Lifecycle Hooks

Status: Active
Owner: operator
Created: 2026-03-21

## Scope Summary

- Add git-hook style lifecycle hooks to compas: named scripts triggered at execution/thread events
- Fire-and-forget subprocess execution with JSON event payload on stdin
- Any language, debuggable in isolation, webhook support built on top

## Ticket HOOKS-1 — Config schema and hook runner

- Goal: Add `hooks` config section and implement hook execution engine
- In scope:
  - `HooksConfig` struct in `src/config/types.rs` with named hook points: `on_execution_started`, `on_execution_completed`, `on_thread_closed`, `on_thread_failed`
  - Each hook point is `Vec<HookEntry>` (supports multiple hooks per event from the start)
  - `HookEntry` struct: `command` (path/name), `args` (optional), `timeout_secs` (default 10), `env` (optional extra env vars)
  - New `src/hooks.rs` module: `HookRunner` that spawns subprocess, passes event JSON on stdin, enforces timeout with SIGTERM → grace period → SIGKILL (reuse `wait_with_timeout` pattern from `src/backend/process.rs`)
  - Fire-and-forget semantics: hook failure logged as warning, never blocks execution
  - Hook working directory: `default_workdir` by default
  - Hot-reload: hooks should hot-reload (read from ConfigHandle on each event, not cached at startup) — same as `agents`
- Out of scope:
  - Blocking/interceptor hooks (deferred to future phase)
  - Webhook HTTP endpoints (users write `curl` in hook scripts)
  - `on_thread_abandoned` and `on_execution_retrying` hook points (Phase 2)
- Dependencies: none
- Acceptance criteria:
  - Config with `hooks` section (Vec per hook point) parses correctly
  - Config without `hooks` still works (backward compat)
  - HookRunner spawns subprocess, passes JSON on stdin, enforces timeout
  - Hook failure logged as warning, doesn't affect execution
  - `make verify` passes
- Verification:
  - Unit tests for HookRunner with stub scripts
  - `make verify`
- Status: In Progress

## Ticket HOOKS-2 — EventBus integration

- Goal: Wire hook execution into the worker's event loop
- In scope:
  - Subscribe HookRunner to EventBus in worker startup (follow `spawn_notification_consumer` in `src/notifications.rs` pattern)
  - Event → hook point mapping:
    - `ExecutionStarted` → `on_execution_started`
    - `ExecutionCompleted` → `on_execution_completed`
    - `ThreadStatusChanged` (status=Completed) → `on_thread_closed`
    - `ThreadStatusChanged` (status=Failed) → `on_thread_failed`
  - Concrete JSON payload schemas per hook point (nullable optional fields):
    - `on_execution_started`: `{"event": "execution_started", "thread_id": "...", "execution_id": "...", "agent_alias": "...", "timestamp": "..."}`
    - `on_execution_completed`: `{"event": "execution_completed", "thread_id": "...", "execution_id": "...", "agent_alias": "...", "success": true, "duration_ms": 12345, "timestamp": "..."}`
    - `on_thread_closed`: `{"event": "thread_closed", "thread_id": "...", "new_status": "Completed", "timestamp": "..."}`
    - `on_thread_failed`: `{"event": "thread_failed", "thread_id": "...", "new_status": "Failed", "timestamp": "..."}`
  - Subscriber architecture: long-lived task with `loop { rx.recv() }`, spawns per-event task for sequential hook execution (don't block subscriber loop)
  - Multiple hooks per event run sequentially in config order within the spawned task
  - Document: `Abandoned` and `ExecutionRetrying` events are not hooked in Phase 1
- Out of scope:
  - Parallel hook execution
  - Hook result feedback to the event system
- Dependencies: HOOKS-1
- Acceptance criteria:
  - Hooks fire on configured events with correct JSON payload
  - Slow hooks don't block execution pipeline
  - Multiple hooks per event run in config order
  - Hooks fire for both built-in and generic backend executions
  - `make verify` passes
- Verification:
  - Integration test: stub hook script writes event JSON to file, verify contents
  - `make verify`
- Status: In Progress

## Ticket HOOKS-3 — Documentation and examples

- Goal: Document lifecycle hooks for users
- In scope:
  - README: document `hooks` section with examples (Slack notification, PagerDuty alert, audit log)
  - Example hook scripts in `examples/hooks/` (notify-slack.sh, log-to-file.sh)
  - CHANGELOG entry, DECISIONS.md (new ADR for lifecycle hooks)
  - `compas doctor` awareness: validate hook commands exist on PATH
- Out of scope:
  - Built-in webhook support (users write `curl` in hook scripts)
- Dependencies: HOOKS-1, HOOKS-2
- Acceptance criteria:
  - README has working examples
  - Example scripts are functional
  - `make verify` passes
- Verification:
  - Manual: configure a hook, trigger an execution, verify hook fires
  - `make verify`
- Status: Todo

## Execution Order

1. HOOKS-1
2. HOOKS-2
3. HOOKS-3

## Tracking Notes

- Backlog-first governance applies.
- Implementation commits should reference ticket IDs.
- Architect consultation: thread `01KM8CW0R6CKD5J4T7JWXSSSG1` (design), thread `01KM8DQ5X0BW0AB3F1Q79D6QMQ` (backlog review).
- Prior art: Buildkite agent hooks (config-defined CLI commands, JSON on stdin, fire-and-forget).
- GBE and HOOKS backlogs are fully independent — can be developed in parallel.
- Phase 2 extensions (deferred): `on_thread_abandoned`, `on_execution_retrying`, blocking hooks, per-hook `workdir` override.

## Execution Metrics

- Ticket: HOOKS-2
- Owner: (pending)
- Complexity: (pending)
- Risk: (pending)
- Start: 2026-03-21 15:55 UTC
- End: (pending)
- Duration: (pending)
- Notes: (pending)

- Ticket: HOOKS-1
- Owner: (pending)
- Complexity: (pending)
- Risk: (pending)
- Start: 2026-03-21 15:30 UTC
- End: 2026-03-21 15:14 UTC
- Duration: 00:07:54
- Notes: (pending)
