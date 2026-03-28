# Worktree Cleanup Safety Guard

Status: Done
Owner: operator
Created: 2026-03-21

## Scope Summary

- Add safety guard to prevent worktree cleanup from deleting uncommitted work
- Refactor worktree_status to tri-state return type

## Ticket WORKTREE-SAFETY-1 — Worktree cleanup safety guard

- Goal: Prevent data loss when closing threads with dirty worktrees
- In scope:
  - Refactor worktree_status to Result<Option<String>, String> tri-state
  - Guard in worker loop to skip cleanup on dirty or unverifiable worktrees
  - ADR entry in DECISIONS.md
  - Known-issues entry for retry-forever limitation
- Out of scope:
  - Operator escape hatch for perpetually-dirty worktrees
  - Async git I/O refactor
- Dependencies: none
- Acceptance criteria:
  - make verify passes
  - Dirty worktrees are NOT deleted on close
  - Clean worktrees ARE deleted (existing behavior preserved)
  - Git failures block cleanup (treated as unsafe)
  - Warning log emitted with thread ID, path, and branch name
- Verification:
  - make verify
- Status: Done

## Execution Metrics

- Ticket: WORKTREE-SAFETY-1
- Owner: (pending)
- Complexity: (pending)
- Risk: (pending)
- Start: 2026-03-21 13:28 UTC
- End: (pending)
- Duration: (pending)
- Notes: (pending)
