# Capacity Visibility

Status: Active
Owner: operator
Created: 2026-03-29

## Scope Summary

- Expose per-agent concurrency capacity in `orch_list_agents` MCP tool
- Update orch-dispatch skill with capacity-aware dispatch guidance
- Document concurrency settings in configuration guide

## Ticket CAP-VIS — Agent capacity visibility in orch_list_agents

- Goal: Let operators see how many concurrent dispatch slots each agent has available before dispatching
- In scope:
  - New store method `queued_executions_by_agent()`
  - Enhance `orch_list_agents` response with `max_concurrent`, `active`, `queued`, `available` per agent
  - Add global capacity totals (`global_max_concurrent`, `global_active`, `global_available`)
  - Convert `list_agents_impl` from sync to async
  - Update integration test for new response shape
  - Update orch-dispatch skill with capacity check step and parallel dispatch guidance
  - Add "Concurrency & Capacity" section to `docs/guides/configuration.md`
- Out of scope:
  - Per-agent `max_triggers_per_agent` override on `AgentConfig` (future work)
  - Capacity enforcement at dispatch time (worker handles this)
- Dependencies: none
- Acceptance criteria:
  - `orch_list_agents` response includes per-agent `max_concurrent`, `active`, `queued`, `available` fields
  - `orch_list_agents` response includes `global_max_concurrent`, `global_active`, `global_available`
  - Integration tests pass with new response shape
  - `make verify` passes
  - Skill has capacity check step before dispatch
  - Configuration guide has concurrency section with example output
- Verification:
  - `make verify`
  - Call `orch_list_agents` via dev MCP server and verify capacity fields present
- Status: Done

## Execution Order

1. CAP-VIS

## Tracking Notes

- Backlog-first governance applies.
- Implementation commits should reference ticket IDs.

## Execution Metrics

- Ticket: CAP-VIS
- Owner: compas-dev
- Complexity: M
- Risk: Low
- Start:
- End:
- Duration:
- Notes:
- Start: 2026-03-29 02:21 UTC

- End: 2026-03-29 03:25 UTC


- Duration: 01:03:43

