# Config Defaults

Status: Active
Owner: operator
Created: 2026-03-29

## Scope Summary

- Add `agent_defaults` top-level config block for shared agent settings
- Field-by-field merge with per-agent overrides
- Establish reusable merge primitive for future MPR-1 (multi-project)

## Ticket CFG-DEFAULTS — Agent defaults for config DRY

- Goal: Reduce agent config repetition by providing shared defaults that agents inherit from
- In scope:
  - `AgentDefaults` struct (all-optional mirror of `AgentConfig`)
  - `agent_defaults` field on `OrchestratorConfig`
  - `apply_agent_defaults()` merge function in config loading pipeline
  - Make `AgentConfig.backend` `Option<String>` with post-merge validation
  - Merge rules: env=shallow merge, backend_args/handoff=replace, all others=field-by-field
  - Helper method `AgentConfig::backend()` for safe access post-validation
  - Unit tests for all merge rules and edge cases
  - Integration test with `orch_list_agents`
  - Documentation in `docs/guides/configuration.md`
  - Hot-reload support (automatic via load pipeline)
  - Changelog fragment
- Out of scope:
  - Named profiles (future, if needed at 50+ agents)
  - `compas doctor` cross-backend `backend_args` warning (follow-up)
  - Multi-project overlays (MPR-1, depends on this)
- Dependencies: none
- Acceptance criteria:
  - Config with `agent_defaults` loads and resolves correctly
  - Agents inherit defaults for all supported fields
  - Per-agent overrides take precedence
  - `env` shallow-merges (defaults base, agent keys win)
  - `backend_args` and `handoff` replace entirely
  - Missing `backend` post-merge produces AX-friendly error
  - Existing configs without `agent_defaults` work unchanged
  - `make verify` passes
- Verification:
  - `make verify`
  - Manual: config with `agent_defaults`, verify `orch_list_agents` shows resolved values
- Status: Done

## Execution Order

1. CFG-DEFAULTS

## Tracking Notes

- Architect consultation validated design (thread 01KMVYT2T5CHHFWHTBYBNY69YQ)
- MPR-1 should reuse the merge function from this ticket

## Execution Metrics

- Ticket: CFG-DEFAULTS
- Owner: compas-dev
- Complexity: M
- Risk: Low
- Start: 2026-03-29 12:28 UTC
- Duration: 00:47:03

- End: 2026-03-29 13:15 UTC

