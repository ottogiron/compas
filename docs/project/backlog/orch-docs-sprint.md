# Aster-Orch Docs Sprint — Feature Documentation Parity

Status: Closed
Owner: orch-ux
Created: 2026-03-15

## Scope Summary

- Update README.md, AGENTS.md, architecture.md, and DECISIONS.md to reflect recently shipped features
- Document worktree isolation, retry with error classification, execution telemetry, desktop notifications, session continuity, and EventBus
- Add ADR-011 and ADR-012 to DECISIONS.md
- Update skills/orch-dispatch/SKILL.md with new operational patterns

## Ticket ORCH-DOCS-1 — Documentation sprint for shipped features

- Goal: Bring all docs and skills up to date with features shipped in the 2026-03 cycle.
- In scope:
  - README.md: add `max_retries`, `retry_backoff_secs`, `log_retention_count` to config reference; add `orch_execution_events` and `orch_worktrees` to MCP tools tables; add "Retry on Transient Failure" section
  - AGENTS.md: add `src/worktree.rs` and `src/events.rs` to Module Overview
  - skills/orch-dispatch/SKILL.md: add Worktree Isolation and Automatic Retry sections; add execution_events debug tip to Failure Handling
  - docs/project/architecture.md: update tool count (15→17), add execution_events table, update Key Design Decisions, update module tree, update status lifecycle diagram
  - docs/project/DECISIONS.md: add ADR-011 (retry with error classification) and ADR-012 (execution telemetry pipeline)
  - examples/config-generic.yaml: add `max_retries`, `retry_backoff_secs`, `log_retention_count` comments
- Out of scope:
  - Code changes
  - New features
- Dependencies: None
- Acceptance criteria:
  - All six files updated per task specification
  - `make verify` passes (no code changes, so fmt/clippy/test are unaffected)
  - MCP tool count is consistent across README.md and architecture.md (17)
  - ADR-011 and ADR-012 present in DECISIONS.md
- Verification:
  - `make verify`
- Status: Done

## Execution Order

1. ORCH-DOCS-1

## Tracking Notes

- Backlog-first governance applies.
- Implementation commits should reference ticket IDs.

## Execution Metrics

- Ticket: ORCH-DOCS-1
- Owner: orch-ux
- Complexity: S
- Risk: Low
- Start: 2026-03-15 16:18 UTC
- End: 2026-03-15 16:19 UTC
- Duration: 00:01:10
- Notes: docs-only sprint, no code changes
