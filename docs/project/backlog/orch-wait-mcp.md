# Re-add orch_wait MCP Tool

Status: Active
Owner: operator
Created: 2026-03-22

## Scope Summary

- Re-register preserved `orch_wait` MCP tool with transport-safe timeout handling
- Add `await_chain` parameter for handoff/fan-out chain settlement
- Add configurable max timeout cap (`mcp_wait_max_timeout_secs`)
- Distinguish chain state on timeout (`chain_pending` field)

## Ticket WAIT-MCP-1 — Re-register orch_wait with transport-safe timeout and await_chain

- Goal: Re-expose the preserved `orch_wait` implementation as an MCP tool with configurable timeout ceiling, `await_chain` support, and `chain_pending` timeout disambiguation.
- In scope:
  - Add `await_chain: Option<bool>` to `WaitParams` (`src/mcp/params.rs`)
  - Add `mcp_wait_max_timeout_secs` config field to `OrchestrationConfig` (`src/config/types.rs`, default 120)
  - Add `chain_pending: bool` to `WaitOutcome::Timeout` variant (`src/wait.rs`)
  - Update `wait_impl` to default timeout to 60s, clamp to config max, pass `await_chain` through (`src/mcp/wait.rs`)
  - Update CLI handler for new `chain_pending` field (`src/bin/compas.rs`)
  - Re-register `#[tool]` stub with `Peer<RoleServer>` + `Meta` extractors for progress notifications (`src/mcp/server.rs`)
  - Update MCP server instructions and `orch_dispatch` description to reference `orch_wait` (`src/mcp/server.rs`)
  - Fix existing test constructions and add new tests for timeout clamping and `chain_pending` (`tests/integration_tests.rs`)
  - Update config docs (`docs/guides/configuration.md`)
  - Add changelog fragment
- Out of scope:
  - Transport timeout discovery/negotiation (not possible in MCP spec)
  - Merging poll/wait return shapes
  - Removing `compas wait` CLI (remains for non-MCP usage)
  - Fan-out settlement metadata (separate ticket WAIT-AX-1)
- Dependencies: none.
- Acceptance criteria:
  - `orch_wait` is registered and callable via MCP
  - Default timeout is 60s, clamped to `mcp_wait_max_timeout_secs` config value
  - `await_chain=true` blocks until handoff/fan-out chain settles
  - Timeout response includes `chain_pending: true` when chain work was still pending
  - Timeout response includes `chain_pending: false` when no chain work was pending
  - Progress notifications sent every 10s to prevent transport timeouts
  - All existing wait tests pass with updated field additions
  - New tests cover timeout clamping and chain_pending behavior
- Verification:
  - `make verify` passes (fmt-check + clippy + test + lint-md)
  - Manual: dispatch work via MCP, call `orch_wait`, confirm found response with full message body
- Status: Done

## Execution Order

1. WAIT-MCP-1

## Tracking Notes

- Backlog-first governance applies.
- Implementation commits should reference ticket IDs.
- Record scope changes/deferrals here.
- Plan reference: `/Users/ottogiron/.claude/plans/tingly-wandering-pebble.md`
