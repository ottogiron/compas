# Ticket-Tracker SQLite Migration

Status: Active
Owner: operator
Created: 2026-03-29

## Scope Summary

- Migrate ticket-tracker execution state from filesystem (YAML sessions + mutable markdown fields) to SQLite
- Keep backlog .md files as immutable specs in git
- Eliminate concurrent file modification issues with parallel worktree agents
- Update compas-side artifacts (AGENTS.md, skills, hooks) to match new workflow

## Context

- Architecture decision from compas-architect consultation (2026-03-29)
- Root cause: planning state (specs, ACs) and execution state (status, metrics, sessions) conflated in one storage medium
- Pattern follows compas itself: config in git, state in SQLite (WAL mode)
- ticket-tracker is a standalone Rust CLI at github.com/ottogiron/ticket-tracker

## Ticket TDB-1 — SQLite store module

- Goal: Add SQLite persistence layer to ticket-tracker
- In scope:
  - rusqlite with bundled feature, store module, schema, WAL mode, CRUD ops
- Out of scope:
  - CLI command changes, data migration
- Dependencies: none
- Acceptance criteria:
  - Store module with typed CRUD for all 4 tables, WAL + FK enforcement, 24 unit tests
- Verification:
  - `make verify` in ticket-tracker
- Status: Done

## Ticket TDB-2 — Migrate CLI commands to SQLite backend

- Goal: Refactor start/done/status/blocked/note/reconcile to use SQLite instead of YAML/markdown
- In scope:
  - All commands write to SQLite, stop writing YAML and modifying .md
  - Backward compat: auto-migrate .sessions/*.yaml to SQLite on first run
  - Done-guard checks SQLite before .md fallback
- Out of scope:
  - Template changes, import command
- Dependencies: TDB-1
- Acceptance criteria:
  - All CLI commands work against SQLite, no YAML/md mutations, 14 new tests
- Verification:
  - `make verify` in ticket-tracker
- Status: Done

## Ticket TDB-3 — Import command and data migration

- Goal: One-shot `ticket import` that parses backlog .md files and populates SQLite
- In scope:
  - Parse tickets, status, metrics, notes, closure evidence from .md files
  - Idempotent upsert semantics
- Out of scope:
  - Modifying .md files, CLI command changes
- Dependencies: TDB-1
- Acceptance criteria:
  - Import populates SQLite from all backlog files, re-run safe, 10 new tests
- Verification:
  - `make verify` in ticket-tracker
- Status: Done

## Ticket TDB-4 — Backlog template reform and report command

- Goal: Strip mutable execution fields from backlog .md template and add `ticket report` command
- In scope:
  - Update template.md — remove Status from ticket sections, remove Execution Metrics, remove Closure Evidence
  - Add `ticket report <ticket-id>` and `ticket report --batch <batch-id>`
  - Update ticket-tracker README
- Out of scope:
  - Retroactively stripping fields from existing backlog files
- Dependencies: TDB-2, TDB-3
- Acceptance criteria:
  - Template has no mutable fields
  - `ticket report` generates formatted output from SQLite
  - README updated
- Verification:
  - `make verify` in ticket-tracker
- Status: Todo

## Ticket TDB-5 — Compas-side updates (hook, AGENTS.md, skills)

- Goal: Update all compas artifacts to reflect the new SQLite-backed ticket workflow
- In scope:
  - Pre-commit hook: replace .sessions/ YAML check with `ticket reconcile --json --strict`
  - AGENTS.md: update Ticket Workflow section (no more .md Status mutations, SQLite is truth source)
  - `backlog-setup` skill: update template references (no mutable fields)
  - `orch-dispatch` skill: update Step 9 ticket closure instructions (no .md status updates)
  - Any other docs referencing `.sessions/` YAML or .md Status field mutations
- Out of scope:
  - CI pipeline changes
- Dependencies: TDB-2
- Acceptance criteria:
  - Pre-commit hook uses `ticket reconcile` CLI
  - All agent instructions reflect SQLite-backed workflow
  - `make verify` in compas
- Verification:
  - `make verify` in compas
- Status: Todo

## Execution Order

1. TDB-1 (done)
2. TDB-2 + TDB-3 (done, parallel)
3. TDB-4 + TDB-5 (parallel — TDB-4 targets ticket-tracker, TDB-5 targets compas)

## Tracking Notes

- Backlog-first governance applies.
- ticket-tracker repo: github.com/ottogiron/ticket-tracker
- Architecture decision: Option B from compas-architect consultation (2026-03-29)

## Execution Metrics

## Closure Evidence
