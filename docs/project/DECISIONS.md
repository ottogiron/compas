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
