# AGENTS.md - Aster Orchestrator (`crates/aster-orch`)

Domain guide for orchestrator work under `crates/aster-orch/**`.

## Scope and Authority

- This file applies to changes in `crates/aster-orch/**`.
- Root `AGENTS.md` remains mandatory and authoritative for repo-wide governance.
- This file is additive and must not relax root safety, verification, or release rules.

## Module Overview

Primary local modules:

- `src/mcp/*` — MCP tools and handlers
- `src/worker/*` — background trigger execution loop
- `src/store/*` — SQLite persistence and lifecycle state
- `src/backend/*` — backend integrations (`claude`, `codex`, `gemini`, `opencode`)
- `src/config/*` — orchestrator configuration schema and validation
- `src/bin/aster_orch.rs` — CLI entrypoints (`worker`, `mcp-server`, `dashboard`, `wait`)
- `tests/integration_tests.rs` — orchestrator integration tests

## Architecture Constraints

- Two-process model is required:
  - MCP server process (`mcp-server`)
  - worker process (`worker`)
- Both processes use the same SQLite database configured via `db_path`.
- WAL mode and safe concurrent read/write behavior must be preserved.
- Thread, message, and execution lifecycle consistency is required for all MCP workflows.

## Operational Policy

- Use MCP tools (`orch_*`) for instant operations.
- Use CLI wait for blocking waits:
  - `aster_orch wait --config .aster-orch/config.yaml ...`
- Keep lifecycle transitions coherent:
  - close should finalize threads with explicit terminal status
  - abandon should cancel queued/running executions
  - reopen should only apply to terminal threads

## Verification for Orchestrator Changes

Minimum local checks for `crates/aster-orch/**` work:

```bash
cargo test -p aster-orch
make verify
make perf-baseline
make perf-check
```

Behavioral changes to orchestrator workflows/tools must also run benchmark flow:

- apply `/orch-benchmark`
- update benchmark evidence in `docs/project/benchmarks/orchestrator/` when appropriate

## Required Documentation Parity

When orchestrator behavior changes, update impacted docs in the same change set:

- `crates/aster-orch/README.md` (tooling/architecture/commands)
- `docs/project/DECISIONS.md` (ADR for meaningful behavioral/policy changes)
- `docs/project/known-issues/` when operational risks or constraints change
- relevant skill docs in `skills/` if operator workflow expectations change

## Failure and Recovery Guidance

- Diagnose stuck threads with:
  - `orch_diagnose`
  - `orch_tasks`
  - `orch_health`
- Stale state reset procedure:
  - stop worker/MCP processes
  - remove configured SQLite DB files (`~/.aster/orch/jobs.sqlite*` in this repo config)
  - restart worker and MCP server

## Design Bias

- Prefer clear, machine-parseable diagnostics over implicit behavior.
- Preserve AX principles: resilient contracts, actionable failures, explicit operator guidance.
- Favor small, composable MCP/CLI contracts over hidden convenience behavior.
