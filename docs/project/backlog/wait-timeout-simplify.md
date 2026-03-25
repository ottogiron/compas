# Wait Timeout Simplification

Status: Active
Owner: operator
Created: 2026-03-25

## Scope Summary

- Remove `mcp_wait_max_timeout_secs` config knob
- Derive `orch_wait` ceiling automatically from `execution_timeout_secs`
- Add transparent clamping feedback in timeout responses

## Ticket WAIT-TIMEOUT-1 — Remove mcp_wait_max_timeout_secs and derive wait ceiling

- Goal: Simplify timeout configuration from 3 interacting values to 1 source of truth (`execution_timeout_secs`).
- In scope:
  - Remove `mcp_wait_max_timeout_secs` from `OrchestrationConfig` (field, default fn, serde, Default impl)
  - Derive wait ceiling in `src/mcp/wait.rs`: `exec_timeout + 30` (no chain), `exec_timeout * 3 + 30` (await_chain)
  - Default `timeout_secs` param to derived ceiling when omitted
  - Clamp caller's `timeout_secs` to derived ceiling when it exceeds it
  - Add `effective_timeout_secs`, `clamped` (bool), and `hint` (string) to `WaitTimeout` response when clamped
  - Handle existing configs that still have the removed field (ignore unknown field gracefully)
  - Update `WaitParams` description to document derived default
  - Update `docs/guides/configuration.md` to remove `mcp_wait_max_timeout_secs`
  - Update CLI wait in `src/bin/compas.rs` if it references the removed config
  - Changelog fragment
- Out of scope:
  - Changing `execution_timeout_secs` itself
  - Per-agent wait timeout overrides
  - `orch_dispatch_and_wait` combined tool
- Dependencies: none.
- Acceptance criteria:
  - `mcp_wait_max_timeout_secs` no longer exists in config schema
  - `orch_wait(timeout_secs=300)` with `execution_timeout_secs=1800` is NOT clamped
  - `orch_wait(timeout_secs=5000)` IS clamped, response includes `clamped: true`, `effective_timeout_secs`, and `hint`
  - `orch_wait()` with no timeout defaults to derived ceiling
  - Old configs with `mcp_wait_max_timeout_secs` still parse without error
  - MCP and CLI wait behavior are consistent
- Verification:
  - `make verify`
  - Smoke test: dispatch + orch_wait with various timeout values
- Status: Done

## Execution Order

1. WAIT-TIMEOUT-1

## Tracking Notes

- Backlog-first governance applies.
- Implementation commits should reference ticket IDs.
- Design rationale from architect: thread `01KMKJVV421WADH2WQNY1HYAA1`.
- Discovered during WAIT-AX-1 smoke testing: `orch_wait(timeout_secs=900)` was silently clamped to 120s, requiring 3+ re-waits.

## Execution Metrics
- Ticket: WAIT-TIMEOUT-1
- Owner: compas-dev (Opus 4.6)
- Complexity: Medium
- Risk: Low
- Start: 2026-03-25 22:58 UTC
- End: 2026-03-25 23:16 UTC
- Duration: 00:17:46
- Notes: Single dispatch, reviewer approved with minor suggestion (missing await_chain=true integration test). Architect design review in thread 01KMKJVV421WADH2WQNY1HYAA1.


