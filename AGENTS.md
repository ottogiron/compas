# AGENTS.md - Aster Orchestrator (`crates/aster-orch`)

Domain guide for orchestrator work under `crates/aster-orch/**`.

## Scope and Authority

- This file applies to changes in `crates/aster-orch/**`.
- This crate lives in its own repository (`ottogiron/aster-orch`) and is included in the aster workspace as a git submodule.
- When working from within the aster repo, root `AGENTS.md` remains mandatory and authoritative for repo-wide governance.
- This file is additive and must not relax root safety, verification, or release rules.

## Git Workflow (Submodule)

This crate is a standalone git repository. When checked out as a submodule inside aster:

1. Commit and push changes here first (`cd crates/aster-orch && git commit && git push`).
2. Then update the submodule pointer in the parent aster repo (`cd <aster-root> && git add crates/aster-orch && git commit`).

When working on this repo independently (cloned standalone), use normal git workflow.

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
- Both processes use the same SQLite database at `{state_dir}/jobs.sqlite`.
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

### CI Pipeline (`.github/workflows/ci.yml`)

CI runs on every push to `main` and every PR. It executes `make verify` which is:

1. `make fmt-check` — `cargo fmt --all -- --check`
2. `make clippy` — `cargo clippy --all-targets -- -D warnings`
3. `make test` — `cargo test`

**All three checks must pass locally before pushing.** The most common CI failure is formatting — always run `cargo fmt --all` (or `make fmt`) before committing.

### Standalone (working directly in `ottogiron/aster-orch`)

```bash
make fmt           # apply rustfmt (do this before committing)
make verify        # fmt-check + clippy --all-targets + test (matches CI)
```

### From within the `aster` parent repo

```bash
cargo fmt --all                    # format from workspace root
cargo test -p aster-orch           # run aster-orch tests via workspace
make verify                        # full aster workspace quality gate
make perf-baseline
make perf-check
```

### Pre-push Checklist

Before pushing to `ottogiron/aster-orch`:

1. `make fmt` — apply formatting
2. `make verify` — run the full CI gate locally
3. If working as a submodule, push the submodule first, then update the pointer in aster

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
  - remove SQLite DB files under state dir (`<state_dir>/jobs.sqlite*`)
  - restart worker and MCP server

## Design Bias

- Prefer clear, machine-parseable diagnostics over implicit behavior.
- Preserve AX principles: resilient contracts, actionable failures, explicit operator guidance.
- Favor small, composable MCP/CLI contracts over hidden convenience behavior.
