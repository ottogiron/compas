# AGENTS.md — Compas

Operational guide for agents working in this repository.

## Project Overview

Compas is a multi-agent orchestration system for AI-assisted software development. It dispatches work to AI coding agents (Claude, Codex, Gemini, OpenCode), manages execution lifecycle, and provides a TUI dashboard for monitoring.

This is a standalone repository (`ottogiron/compas`).

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
- `src/worktree.rs` — git worktree creation, cleanup, and path resolution
- `src/merge.rs` — merge executor, temporary worktree merge, conflict detection
- `src/events.rs` — EventBus and execution telemetry pipeline
- `src/bin/compas.rs` — CLI entrypoints (`worker`, `mcp-server`, `dashboard`, `wait`, `wait-merge`)
- `tests/integration_tests.rs` — orchestrator integration tests

## Build Commands

```bash
make build          # cargo build
make test           # cargo test
make fmt            # cargo fmt --all
make verify         # fmt-check + clippy + test + lint-md (matches CI)
make changelog      # changie dry-run preview of next version
make setup-hooks    # install pre-commit hook
make worker         # run worker
make dashboard      # run dashboard
make dashboard-dev  # dashboard + embedded worker (dev DB)
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

**Never bypass the pre-commit hook with `--no-verify`.** Run `make setup-hooks` after cloning to install the hook.

## Quality Gates (Required Before Every Push)

```bash
make verify    # always — fmt-check + clippy + test + lint-md
```

This matches the CI pipeline (`.github/workflows/ci.yml`). All four checks must pass locally before pushing. **CI runs on Linux (Ubuntu).** Code that compiles on macOS may fail CI due to `#[cfg(target_os = "macos")]` gating — clippy will flag dead code that is only reachable on macOS. Always run `make verify` to catch these issues locally.

### Pre-commit hook vs CI

The pre-commit hook (`scripts/hooks/pre-commit`) enforces **ticket tracking only** — it does NOT run fmt, clippy, or tests. Those checks are enforced by CI. Agents MUST run `make verify` themselves before pushing.

### Pre-push Checklist (Mandatory)

1. `make fmt` — apply formatting
2. Add a changelog fragment: `changie new -k <Added|Changed|Fixed|Removed> -b "<description>"` (no CI enforcement — agent discipline only)
3. `make verify` — run the full CI gate (`fmt-check` + `clippy` + `test` + `lint-md`). **Do not push if this fails.**
4. Push to remote after verification passes

### Code Review Policy

Operator-authored changes (not just dispatched work) must be sent to `compas-reviewer` for approval before committing. Use Mode B from the `orch-dispatch` skill: gather the diff, dispatch to the reviewer, act on findings.

## Release Checklist

When tagging a new release:

1. `changie batch <major|minor|patch>` — assemble fragments into a versioned entry
2. `changie merge` — regenerate `CHANGELOG.md` from all version files
3. Update the version in `Cargo.toml` to match
4. Update the install tag in `README.md` (`--tag vX.Y.Z` in both the install and build-from-source commands)
5. Commit, tag (`git tag vX.Y.Z`), push with tags (`git push origin main --tags`)

## Impact Update Matrix

If you change a layer, update/review the paired artifacts in the same commit set.

- MCP tools (`src/mcp/*`): `README.md`, integration tests, changelog fragment (`changie new`)
- Worker/executor (`src/worker/*`): integration tests, `docs/project/DECISIONS.md` for behavioral changes, changelog fragment (`changie new`)
- Dashboard (`src/dashboard/*`): visual verification, changelog fragment (`changie new`)
- Backends (`src/backend/*`): backend-specific tests, `README.md`, changelog fragment (`changie new`)
- Config (`src/config/*`): validation tests, `README.md`, changelog fragment (`changie new`)
- Store/DB (`src/store/*`): migration handling, integration tests, changelog fragment (`changie new`)
- Merge executor (`src/merge.rs`): integration tests, `docs/project/DECISIONS.md`, changelog fragment (`changie new`)
- ADRs or known-issues updates (`docs/project/DECISIONS.md`, `docs/project/known-issues.md`): changelog fragment (`changie new`)

## Development Workflow

### Dev MCP server

A repo-level dev MCP server config is available for testing MCP changes during development. Copy `.mcp.json.example` to `.mcp.json` and update the paths to your local checkout. The dev instance uses `.compas/config.yaml` with an isolated state directory (`.compas/state/`), completely separate from any production install.

```bash
make dashboard-dev   # dashboard + embedded worker on dev DB
```

### Testing MCP changes

1. Edit source code (e.g., `src/mcp/*.rs`)
2. `cargo build`
3. Call the changed tool via the dev MCP server — it uses `cargo run` and picks up your build
4. Verify results in the dev dashboard (`make dashboard-dev`)
5. `make verify` before committing

### Git workflow

Standard git workflow. Commit, push, PR. See [CONTRIBUTING.md](CONTRIBUTING.md) for contribution guidelines.

## Failure and Recovery Guidance

- Diagnose stuck threads: `orch_diagnose`, `orch_tasks`, `orch_health`
- Diagnose stuck merges: `orch_merge_status` to check operation state, `orch_merge_cancel` to cancel queued ops
- Stale state reset: stop processes → remove `<state_dir>/jobs.sqlite*` → restart
- Stale/orphaned merge ops are auto-marked failed on worker restart (no manual intervention needed)

## Design Bias

- Prefer clear, machine-parseable diagnostics over implicit behavior.
- Preserve AX principles: resilient contracts, actionable failures, explicit operator guidance.
- Favor small, composable MCP/CLI contracts over hidden convenience behavior.

## Skills

Available skills in `skills/`:

- `dev-workflow` — Ticket-driven development lifecycle
- `backlog-setup` — Create backlog artifacts before implementation
- `orch-dispatch` — Operator dispatch-review-complete loop for delegating work via the orchestrator. **Load before any dispatch workflow.**
- `stop-and-think` — Behavioral guardrail (always active)
