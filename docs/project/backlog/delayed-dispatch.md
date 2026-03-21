# Delayed Dispatch

Status: Active
Owner: operator
Created: 2026-03-21

## Scope Summary

- Unify `retry_after` and `scheduled_for` into a single `eligible_at` column
- Add `scheduled_for` parameter to `orch_dispatch` for operator-initiated delayed execution
- Dashboard and MCP visibility for scheduled tasks

## Ticket SCHED-1 — Schema migration: unify retry_after → eligible_at

- Goal: Rename `retry_after` to `eligible_at`, add `eligible_reason` column
- In scope:
  - Migration in `src/store/mod.rs`: rename column `retry_after` → `eligible_at`, add `eligible_reason TEXT` (nullable)
  - Update all store methods that read/write `retry_after` (~5 call sites)
  - Update `claim_next_execution()` WHERE clause (column rename only — logic unchanged)
  - Update retry logic in `src/worker/loop_runner.rs` to write `eligible_reason = 'retry_backoff'`
  - Update `ExecutionRetrying` event field name in `src/events.rs`
  - Update integration tests referencing `retry_after`
- Out of scope:
  - Dispatch parameter changes (SCHED-2)
  - Dashboard changes (SCHED-3)
  - MCP tool changes (SCHED-2)
- Dependencies: none
- Acceptance criteria:
  - Existing retry behavior unchanged (behavioral no-op — same timing, same backoff)
  - `eligible_at` column used everywhere `retry_after` was
  - `eligible_reason` populated as `'retry_backoff'` for retries
  - `make verify` passes
- Verification:
  - Retry integration tests pass with new column name
  - `make verify`
- Status: Done

## Ticket SCHED-2 — Delayed dispatch via orch_dispatch

- Goal: Add `scheduled_for` parameter to `orch_dispatch` for time-delayed execution
- In scope:
  - `DispatchParams` gains `scheduled_for: Option<String>` (ISO 8601 timestamp)
  - `dispatch_impl()`: parse ISO 8601 timestamp, set `eligible_at` on created execution, set `eligible_reason = 'scheduled'`
  - Validation: `scheduled_for` must be in the future (reject past timestamps with clear error)
  - If `scheduled_for` is None, behavior unchanged (immediate eligibility, `eligible_at` = NULL)
  - `orch_dispatch` response includes `scheduled_for` in output when set
  - Update `orch_dispatch` tool description to document the new parameter
  - CHANGELOG entry, DECISIONS.md (new ADR for delayed dispatch and eligible_at unification)
- Out of scope:
  - Recurring schedules (CRON batch)
  - Dashboard changes (SCHED-3)
  - Agent-initiated scheduling
- Dependencies: SCHED-1
- Acceptance criteria:
  - `orch_dispatch` with `scheduled_for` creates execution not claimable until that time
  - `orch_dispatch` without `scheduled_for` works exactly as before
  - Worker claims scheduled execution only after its eligible time
  - Past timestamps rejected with clear error message
  - `make verify` passes
- Verification:
  - Integration test: dispatch with `scheduled_for` 5 seconds in future, verify execution not claimed immediately, verify claimed after delay
  - `make verify`
- Status: Done

## Ticket SCHED-3 — Dashboard and MCP visibility

- Goal: Make scheduled tasks visible to operators
- In scope:
  - `orch_tasks`: add filter for scheduled executions (queued with future `eligible_at`)
  - `orch_status`: include count of pending scheduled executions in status output
  - Dashboard Ops tab: "Scheduled" section showing pending scheduled work with due time
  - Verify `orch_abandon` works for scheduled executions (already cancels queued — confirm)
  - README: document `scheduled_for` parameter with examples
  - CHANGELOG entry
- Out of scope:
  - Recurring schedule visibility (CRON-3)
- Dependencies: SCHED-2
- Acceptance criteria:
  - `orch_tasks` can list scheduled-but-not-yet-eligible executions
  - Dashboard shows scheduled tasks with their due time
  - `orch_abandon` cancels scheduled executions
  - README has working examples
  - `make verify` passes
- Verification:
  - Manual: dispatch with `scheduled_for`, verify visibility in dashboard and `orch_tasks`
  - `make verify`
- Status: Todo

## Execution Order

1. SCHED-1
2. SCHED-2
3. SCHED-3

## Tracking Notes

- Backlog-first governance applies.
- Implementation commits should reference ticket IDs.
- Architect consultation: thread `01KM8W2S00PVMAY5N6PM3VJTNW`.
- Key design decision: unify `retry_after` + `scheduled_for` into `eligible_at` (one column, one concept).
- SCHED batch is independent of CRON batch — CRON depends on SCHED-2 but not vice versa.
- Phase 3 (agent-initiated scheduling) explicitly deferred per architect recommendation.

## Execution Metrics

- Ticket: SCHED-2
- Owner: compas-dev
- Complexity: M
- Risk: Low
- Start: 2026-03-21 20:02 UTC
- End: 2026-03-21 20:11 UTC
- Duration: 00:12:01
- Notes: Recovered from lost session. Worktree verified by reviewer, merged by operator.

- Ticket: SCHED-1
- Owner: (pending)
- Complexity: (pending)
- Risk: (pending)
- Start: 2026-03-21 19:32 UTC
- End: 2026-03-21 19:36 UTC
- Duration: 00:04:30
- Notes: (pending)
