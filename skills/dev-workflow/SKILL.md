---
name: dev-workflow
description: Ticket-driven development lifecycle from ticket start through verification and merge.
user-invocable: false
---

# dev-workflow

## Description

Use this skill when starting any implementation work. Ensures ticket tracking, branching, verification gates, and clean closure.
Do not use for read-only exploration or research tasks that produce no code changes.

## Inputs

- Ticket ID (must exist in a backlog file under `docs/project/backlog/`)
- Batch ID (if part of multi-ticket work)

## Workflow

1. Verify ticket exists in backlog.
2. Start session:
   - Ticket mode: `ticket start <ticket-id>`
   - Batch mode: `ticket start <batch-id> --batch`
3. Create branch: `<batch-id>/<ticket-id>` or `<alias>/<ticket-id>`.
4. Implement changes.
5. Run verification gates:
   - Always: `make verify`
6. Commit. Never use `--no-verify`.
7. Close session:
   - Ticket mode: `ticket done <ticket-id>`
   - Batch mode: `ticket done <batch-id> --batch`
8. Merge to main: `git merge --no-ff <branch>` (fast-forward fine for single-commit).
9. Update backlog execution metrics (Start, End, Duration with `YYYY-MM-DD HH:MM UTC`).

## Required Checks

- `make verify` (fmt-check + clippy + test) — **must pass before push**
- CI runs on **Linux (Ubuntu)**. `#[cfg(target_os = "macos")]`-gated code is dead on Linux — clippy will flag it. Always run `make verify` locally to catch cross-platform issues before pushing.
- The pre-commit hook only enforces ticket tracking, NOT quality gates. You must run `make verify` yourself.

## Output Format

- `Ticket ID`
- `Branch`
- `Files Changed`
- `Verification Results` (pass/fail per gate)
- `Merge Status`

## Failure Handling

- **ticket start fails:** Read the error message — it includes the file searched and expected format. Fix the backlog file or create the missing ticket.
- **Verification gate fails:** Fix before proceeding. Do not merge with failing gates.
- **Blocked:** `ticket blocked <ticket-id> "<reason>"`. Record in backlog.
- **Pre-commit hook fails:** Diagnose, fix, and create a new commit. Never use `--no-verify`.
