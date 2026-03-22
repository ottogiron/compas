# Config-Declared Recurring Schedules

Status: Closed
Owner: operator
Created: 2026-03-21

## Scope Summary

- Add `schedules` section to config for cron-based recurring dispatches
- Worker evaluates cron expressions and creates dispatch messages when due
- Dashboard visibility for configured schedules

## Ticket CRON-1 — Config schema and cron evaluation

- Goal: Add `schedules` config section with cron expression support
- In scope:
  - `ScheduleConfig` struct in `src/config/types.rs`: `name` (String), `agent` (String), `cron` (String, cron expression), `body` (String, dispatch body), `batch` (Option, batch/ticket ID), `max_runs` (u64, safety cap, default 100), `enabled` (bool, default true)
  - `schedules: Option<Vec<ScheduleConfig>>` on `OrchestratorConfig`
  - Cron expression parsing library (e.g., `cron` or `croner` crate)
  - Validation: cron syntax valid, agent alias exists in `agents`, `max_runs > 0`, `name` not empty, no duplicate names
  - Hot-reload: schedules should hot-reload (same as `agents`)
- Out of scope:
  - Worker evaluation loop (CRON-2)
  - Dashboard visibility (CRON-3)
- Dependencies: none (can develop in parallel with SCHED batch)
- Acceptance criteria:
  - Config with `schedules` section parses correctly
  - Invalid cron expressions rejected with clear error
  - Agent alias validated against configured agents
  - Config without `schedules` works (backward compat)
  - `make verify` passes
- Verification:
  - Unit tests for deserialization and validation
  - `make verify`
- Status: Done

## Ticket CRON-2 — Worker schedule evaluation loop

- Goal: Worker evaluates cron schedules and creates dispatch messages when due
- In scope:
  - Schedule evaluation in worker poll loop (or separate interval, e.g., every 60s)
  - On each tick: read schedules from ConfigHandle (hot-reload), check which are due based on cron expression and last-fire time
  - Insert dispatch message with `intent: 'dispatch'` for due schedules, targeting the configured agent
  - Set `scheduled_for` (from SCHED-2) on created execution if needed, or dispatch immediately
  - Dedup: track last-fire time per schedule name in a `schedule_runs` table or in-memory map to prevent double-fires on worker restart
  - Respect `max_runs`: count existing dispatches for the schedule (by batch or schedule name), stop when cap reached
  - Respect `enabled`: skip disabled schedules
  - CHANGELOG entry, DECISIONS.md (new ADR for recurring schedules)
- Out of scope:
  - Dashboard visibility (CRON-3)
  - Event-triggered schedules
- Dependencies: CRON-1, SCHED-2 (for eligible_at infrastructure)
- Acceptance criteria:
  - Cron-scheduled dispatches fire at configured times
  - No double-fires on worker restart
  - `max_runs` cap enforced
  - Disabled schedules don't fire
  - Hot-reload: adding a new schedule fires on next due time without restart
  - `make verify` passes
- Verification:
  - Integration test: configure schedule with short cron interval, verify dispatch message created
  - `make verify`
- Status: Done

## Ticket CRON-2 Execution Metrics

- Ticket: CRON-2
- Owner: compas-dev
- Complexity: Medium
- Risk: Medium (cron evaluation, double-fire prevention)
- Start: 2026-03-21 19:32 UTC
- End: 2026-03-22 00:00 UTC
- Duration: ~4.5h
- Notes: Implemented worker schedule evaluation loop with SQLite-backed run tracking

## Ticket CRON-3 — Dashboard and documentation

- Goal: Show recurring schedules in dashboard, document for users
- In scope:
  - Dashboard: schedule list with name, agent, cron expression, next-fire time, run count / max_runs, enabled status
  - README: document `schedules` section with examples (CI monitoring, periodic health check)
  - `compas doctor`: validate schedule agent aliases exist, validate cron syntax
  - CHANGELOG entry
- Out of scope:
  - Schedule management via MCP tools (create/delete/enable/disable at runtime)
- Dependencies: CRON-1, CRON-2
- Acceptance criteria:
  - Dashboard shows configured schedules with status
  - README has working examples
  - `compas doctor` validates schedule config
  - `make verify` passes
- Verification:
  - Manual: configure a schedule, verify dashboard shows it, verify dispatches fire
  - `make verify`
- Status: Done

## Execution Order

1. CRON-1
2. CRON-2
3. CRON-3

## Tracking Notes

- Backlog-first governance applies.
- Implementation commits should reference ticket IDs.
- Architect consultation: thread `01KM8W2S00PVMAY5N6PM3VJTNW`.
- CRON-2 depends on SCHED-2 (eligible_at infrastructure) — CRON batch can start with CRON-1 in parallel with SCHED batch, but CRON-2 must wait for SCHED-2.
- Prior art: GitHub Actions cron schedules, Buildkite scheduled builds, Airflow DAG schedules.
- Not in scope: agent-initiated recurring tasks (agents don't manage scheduling — ADR-015).

## CRON-1 Execution Metrics

- Ticket: CRON-1
- Owner: compas-dev
- Complexity: Low
- Risk: Low (schema + validation only)
- Start: 2026-03-21 19:32 UTC
- End: 2026-03-21 20:30 UTC
- Duration: ~1h
- Notes: Config schema, validation, cron parsing, tests

## CRON-3 Execution Metrics

- Ticket: CRON-3
- Owner: compas-dev
- Complexity: Medium
- Risk: Low (display-only + documentation)
- Start: 2026-03-22 00:00 UTC
- End: 2026-03-22
- Duration: ~1h
- Notes: Dashboard schedules section in Settings tab, README documentation, compas doctor validation
