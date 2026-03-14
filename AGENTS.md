# AGENTS.md — Aster Orchestrator

Operational guide for agents working in this repository.

## Project Overview

Aster Orchestrator is a multi-agent orchestration system for AI-assisted software development. It dispatches work to AI coding agents (Claude, Codex, Gemini, OpenCode), manages execution lifecycle, and provides a TUI dashboard for monitoring.

This is a standalone repository (`ottogiron/aster-orch`). It is also included as a git submodule in the aster compiler repo (`ottogiron/aster`).

## Project Principles

- Prioritize correctness, consistency, and maintainability.
- **AX (Agent Experience).** Tools and APIs must serve agents as primary consumers — diagnostic errors, resilient contracts, no escape hatches. See `docs/project/AX.md`.
- **Active development, no backward-compatibility burden.** Pre-v1, rapid iteration. The only stability contract is passing tests and verification gates.

## Module Overview

- `src/mcp/*` — MCP tools and handlers
- `src/worker/*` — background trigger execution loop
- `src/store/*` — SQLite persistence and lifecycle state
- `src/backend/*` — backend integrations (`claude`, `codex`, `gemini`, `opencode`)
- `src/config/*` — orchestrator configuration schema and validation
- `src/dashboard/*` — TUI dashboard (ratatui)
- `src/bin/aster_orch.rs` — CLI entrypoints (`worker`, `mcp-server`, `dashboard`, `wait`)
- `tests/integration_tests.rs` — orchestrator integration tests

## Build Commands

```bash
make build          # cargo build
make test           # cargo test
make fmt            # cargo fmt --all
make verify         # fmt-check + clippy + test (matches CI)
make setup-hooks    # install pre-commit hook
make worker         # run worker
make dashboard      # run dashboard
make mcp-server     # run MCP server
```

## Code Style

- Follow `rustfmt` defaults.
- Use `Result<T, String>` for recoverable errors.
- Use `unwrap()` only in tests.
- All clippy warnings are errors (`-D warnings`).
- Test naming: `test_<component>_<feature>`.

## Architecture Constraints

- Two-process model: MCP server + worker. Both share SQLite via WAL mode.
- Thread, message, and execution lifecycle consistency is required.
- All AI backends are CLI subprocess invocations via the `Backend` trait.

## Ticket Workflow

This project uses `ticket` (installed via `cargo install --git https://github.com/ottogiron/ticket-tracker`) for backlog governance.

```bash
ticket start <ticket-id>           # start a ticket session
ticket start <batch-id> --batch    # start a batch session
ticket done <ticket-id>            # close a ticket
ticket done <batch-id> --batch     # close a batch
ticket status                      # show active sessions
ticket blocked <id> "<reason>"     # mark as blocked
ticket note <id> "<note>"          # add tracking note
```

Backlogs live in `docs/project/backlog/`. See `docs/project/backlog/template.md` for the required format.

**Never bypass the pre-commit hook with `--no-verify`.** Run `make setup-hooks` after cloning to install the hook.

## Quality Gates (Required Before Merge)

```bash
make verify    # always — fmt-check + clippy + test
```

This matches the CI pipeline (`.github/workflows/ci.yml`). All three checks must pass locally before pushing.

### Pre-push Checklist

1. `make fmt` — apply formatting
2. `make verify` — run the full CI gate locally
3. If working as a submodule, push here first, then update the pointer in aster

## Impact Update Matrix

If you change a layer, update/review the paired artifacts in the same commit set.

- MCP tools (`src/mcp/*`): `README.md`, integration tests
- Worker/executor (`src/worker/*`): integration tests, `docs/project/DECISIONS.md` for behavioral changes
- Dashboard (`src/dashboard/*`): visual verification
- Backends (`src/backend/*`): backend-specific tests, `README.md`
- Config (`src/config/*`): validation tests, `README.md`
- Store/DB (`src/store/*`): migration handling, integration tests

## Git Workflow

### Standalone (normal development)

Standard git workflow. Commit, push, PR.

### As submodule in aster

1. Commit and push changes here first.
2. Then update the submodule pointer in aster: `cd <aster-root> && git add crates/aster-orch && git commit`

## Failure and Recovery Guidance

- Diagnose stuck threads: `orch_diagnose`, `orch_tasks`, `orch_health`
- Stale state reset: stop processes → remove `<state_dir>/jobs.sqlite*` → restart

## Design Bias

- Prefer clear, machine-parseable diagnostics over implicit behavior.
- Preserve AX principles: resilient contracts, actionable failures, explicit operator guidance.
- Favor small, composable MCP/CLI contracts over hidden convenience behavior.

## Skills

Available skills in `skills/`:

- `dev-workflow` — Ticket-driven development lifecycle
- `backlog-setup` — Create backlog artifacts before implementation
- `stop-and-think` — Behavioral guardrail (always active)
