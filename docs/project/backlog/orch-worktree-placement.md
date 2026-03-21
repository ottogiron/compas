# Worktree Default Placement

Status: Active
Owner: operator
Created: 2026-03-21

## Scope Summary

- Move default worktree placement from outside repo to inside repo

## Ticket WT-PLACE-1 — Move default worktree placement inside repository

- Goal: Eliminate CLI tool permission prompts by placing worktrees inside the repo sandbox
- In scope:
  - Change `worktree_root()` default from `{repo_root}/../.compas-worktrees/` to `{repo_root}/.compas-worktrees/`
  - Add `.compas-worktrees/` to `.gitignore`
  - Update doc comments in `worktree.rs` and `config/types.rs`
  - Update README.md path references
  - Update tests
  - CHANGELOG entry
- Out of scope:
  - Migration code for existing worktrees (pre-v1 clean break)
- Dependencies: none
- Acceptance criteria:
  - `worktree_root(Path::new("/some/repo"), None)` returns `/some/repo/.compas-worktrees`
  - `.compas-worktrees/` is in `.gitignore`
  - All tests pass
  - `make verify` passes
- Verification:
  - `make verify`
- Status: In Progress

## Execution Order

1. WT-PLACE-1

## Tracking Notes

- Backlog-first governance applies.
- Architect consultation completed (thread 01KM8GG91WCVGGG3RAN8EBJRCG) — recommends Option A.

## Execution Metrics
- Ticket: WT-PLACE-1
- Owner: (pending)
- Complexity: (pending)
- Risk: (pending)
- Start: 2026-03-21 15:50 UTC
- End: (pending)
- Duration: (pending)
- Notes: (pending)


