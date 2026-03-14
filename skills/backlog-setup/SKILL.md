---
name: backlog-setup
description: Create a backlog artifact for multi-ticket work before any implementation begins.
---

# backlog-setup

## Description

Use this skill when starting a new roadmap phase or batch that spans multiple tickets. The backlog file must exist before any implementation work begins.
Do not use for one-off or trivial changes that do not require multi-ticket governance.

## Inputs

- Phase or batch name
- Batch ID prefix (used in ticket IDs)
- List of planned tickets with goals and scope

## Workflow

1. Create backlog file at `docs/project/backlog/<phase-or-batch>.md`.
2. Author from canonical template: `docs/project/backlog/template.md`.
3. For every ticket section, keep parser-required fields exactly:
   - `Goal`
   - `In scope`
   - `Out of scope`
   - `Dependencies`
   - `Acceptance criteria`
   - `Verification`
   - `Status`
4. Set ticket status to `Todo` and batch header `Status: Active`.
5. Validate batch session resolution:

   ```bash
   ticket start <batch-id> --batch
   ticket done <batch-id> --batch
   ```

6. Validate duplicate ticket headings manually:

   ```bash
   rg -o "^## Ticket [A-Z0-9-]+" docs/project/backlog/<phase-or-batch>.md | sort | uniq -d
   ```

   This command must produce no output.
7. Commit the backlog file before starting implementation work.

## Required Checks

- Backlog file exists under `docs/project/backlog/`
- Batch session command works (`ticket start <batch-id> --batch`)
- Required ticket fields are present with exact names
- Duplicate ticket heading check is clean

## Output Format

- `Batch ID`
- `Backlog Path`
- `Ticket Count`
- `Validation Result`

## Failure Handling

- **Batch start fails:** Read error output and fix file naming, batch ID prefix, or ticket heading format.
- **Missing required fields:** Update ticket sections to parser-required field names.
- **Duplicate ticket IDs:** Rename duplicate heading IDs before proceeding.
- **Missing dependencies:** Mark dependent tickets as `Blocked` with reason.
