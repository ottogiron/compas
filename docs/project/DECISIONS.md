# Architecture Decision Records — Aster Orchestrator

## ADR-001: SQLite as sole persistence backend

**Date:** 2024-12
**Status:** Active

SQLite in WAL mode provides concurrent read/write from worker + MCP server processes without external dependencies. Scales to hundreds of threads/executions. No need for a database server.

## ADR-002: Two-process model (worker + MCP server)

**Date:** 2024-12
**Status:** Active

MCP server handles operator-facing tools (dispatch, close, status). Worker handles background execution (polling, triggering backends, writing results). Both share SQLite. Dashboard optionally embeds the worker (`--with-worker`).

This separation keeps MCP responses fast and worker execution unblocked.

## ADR-003: Backend CLI abstraction

**Date:** 2025-01
**Status:** Active

All AI backends (Claude, Codex, Gemini, OpenCode) are invoked as CLI subprocesses. The `Backend` trait normalizes args, session management, and output parsing. Adding a new backend means implementing one trait.

This avoids SDK dependencies and works with any tool that has a CLI.

## ADR-004: Parallel ticket sessions

**Date:** 2026-03
**Status:** Active

Moved from single `.session` file to `.sessions/` directory with per-key YAML files. Multiple batches (e.g., compiler + orchestrator) can run concurrently. Pre-commit hook validates any active session (permissive policy).

## ADR-005: Standalone ticket-tracker repo

**Date:** 2026-03
**Status:** Active

Extracted ticket-tracker to its own repo (`ottogiron/ticket-tracker`). Installed globally via `cargo install`. Generic tool usable across any project — not coupled to aster or aster-orch.

## ADR-006: Standalone repo with independent dev infrastructure

**Date:** 2026-03
**Status:** Active

Extracted aster-orch from aster as a fully independent repository with its own development infrastructure: ticket system, backlogs, pre-commit hooks, skills, governance docs, and MCP server configs.

**Why:** Submodule git workflow (two-step commits, detached HEAD) added friction. Parallel development on aster (compiler) and aster-orch (orchestrator) was blocked by the single-session ticket system. Independent repos enable independent development cadences.

**How it works:**
- Production orch (`aster-orch` MCP server) dispatches agents to work on any repo, including aster-orch itself.
- Dev orch (`aster-orch-dev` MCP server, via `cargo run`) uses a local state directory (`.aster-orch/state/`) for testing MCP changes.
- Both MCP servers are configured globally (user scope) in Claude Code, Codex, and OpenCode — available from any project.
- `make dashboard-dev` runs the dashboard with an embedded worker on the dev DB.

**Trade-off:** Loses the convenience of `cargo test -p aster-orch` from the aster workspace. Gained: independent git history, parallel ticket sessions, no submodule friction, self-contained dev infrastructure.

## ADR-007: Graceful worker shutdown via SIGTERM + semaphore drain

**Date:** 2026-03
**Status:** Active

`--with-worker` previously spawned the worker as a fully independent OS process (`process_group(0)`, `kill_on_drop(false)`) that survived dashboard exit indefinitely. This caused orphaned workers running stale code after rebuilds, and heartbeat guards preventing new workers from spawning.

**Decision:** Dashboard sends SIGTERM to the worker on exit. Worker handles SIGTERM (and SIGINT) by breaking its poll loop and draining in-flight executions via semaphore permit acquisition with a timeout of `execution_timeout_secs`.

**Alternatives considered:**
- Kill worker immediately on dashboard exit — rejected because it kills running agent executions mid-task.
- Embed worker in-process (same tokio runtime) — rejected because dashboard exit always kills the worker, even during long executions.

**Accepted residual risk:** If the dashboard crashes (SIGKILL, panic) before the cleanup block runs, the worker remains orphaned. Crash recovery on next startup (`mark_orphaned_executions_crashed`) handles the execution state; the stale process must be killed manually. This is the same behavior as before — the fix only covers the clean exit path.
