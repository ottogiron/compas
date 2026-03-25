# Wait AX Improvements

Status: Active
Owner: operator
Created: 2026-03-17

## Scope Summary

- Improve `--await-chain` output to include fan-out settlement metadata
- Make wait output unambiguous for agent and human consumers

## Ticket WAIT-AX-1 — Add fan-out settlement metadata to --await-chain output

- Goal: When `--await-chain` blocks for fan-out child threads, the output should include metadata showing what was waited on and when settlement occurred.
- In scope:
  - Add `fanout_children_awaited=N` to wait output when fan-out children exist
  - Add `settled_at=<unix_timestamp>` showing when the wait actually exited (wall-clock)
  - Only emit these fields when `--await-chain` is active and fan-out children were found
- Out of scope:
  - Changing the matched message format
  - Adding per-child-thread detail (thread IDs, durations)
- Dependencies: none (ADR-014 Phase 2 is merged).
- Acceptance criteria:
  - `--await-chain` output includes `fanout_children_awaited` and `settled_at` when fan-out children exist
  - Output is unchanged when no fan-out children exist (backward compatible)
  - An agent consumer can distinguish "wait returned immediately" from "wait blocked for N seconds on fan-out"
- Verification:
  - `make verify`
  - Manual: dispatch to fan-out agent, run `--await-chain`, confirm metadata appears in output
- Status: Done

## Execution Order

1. WAIT-AX-1

## Tracking Notes

- Backlog-first governance applies.
- Implementation commits should reference ticket IDs.
- Discovered during ADR-014 Phase 2 smoke test: operator (agent) misread `created_at` as the exit timestamp and incorrectly concluded `--await-chain` returned early.

## Execution Metrics

- Ticket: WAIT-AX-1
- Owner: compas-dev (Opus 4.6)
- Complexity: Low
- Risk: Low
- Start: 2026-03-25 21:33 UTC
- End: 2026-03-25 21:56 UTC
- Duration: 00:23:01
- Notes: Single dispatch, reviewer approved with no blocking issues. Worker left changes uncommitted — manual commit + orch_merge required.
