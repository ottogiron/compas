# Backlog Consolidation Guide

Instructions for grooming and consolidating compas backlogs into a coherent roadmap.

**Target audience:** compas-dev agent or operator performing backlog maintenance.

## Current State (2026-03-22)

### Backlog inventory

| File | Status | Open tickets | Closed tickets | Notes |
|------|--------|-------------|----------------|-------|
| `orch-evolution.md` | Active | EVO-6, EVO-10, EVO-11, EVO-14, EVO-16 | EVO-1/2/3/4/5/7/12/13/15 | EVO-8/9 superseded by MFE |
| `multi-frontend.md` | Active | MFE-1, MFE-2, MFE-3 | — | Foundational for Tauri/web UI |
| `multi-project.md` | Active | MPR-1, MPR-2, MPR-3, MPR-4 | — | Projects-as-overlays |
| `orch-observability.md` | Active | OBS-02, OBS-04 | OBS-01, OBS-03 | OBS-04 in progress |
| `lifecycle-hooks.md` | Active | HOOKS-4 | HOOKS-1/2/3 | Filter field only |
| `delayed-dispatch.md` | Active | SCHED-3 | SCHED-1, SCHED-2 | Dashboard visibility |
| `recurring-schedules.md` | Closed | — | CRON-1/2/3 | All done, archived |
| `cli-ux.md` | Active | CLI-1, CLI-2, CLI-3, CLI-4 | — | CLI-1/2/3/4 all Todo. Active backlog. |
| `orch-wait-ax.md` | Active | WAIT-AX-1 | — | Small AX fix |
| `orch-rename-workdir.md` | Active | ORCH-RENAME-1 | — | In progress |
| `orch-worktree-safety.md` | Active | WORKTREE-SAFETY-1 | — | In progress |
| `orch-dashboard-ux.md` | Closed | — | DASH-UX-1 | Done, archived |
| `generic-backend.md` | Closed | — | GBE-1/2/3 | All done, archived |
| `quality-gaps.md` | Active | GAP-1/2/3/4/5/6/7 | — | From quality gap analysis |
| `orch-team.md` | Deferred | TEAM-1/2/3/4/5/7 | — | TEAM-6 extracted to MPR |
| `orch-merge-queue.md` | Complete | — | MERGE-1/2/3/4/5/6/7 | Archived |
| `orch-chain.md` | Closed | — | CHAIN-1/2 | Archived |
| `orch-config.md` | Closed | — | CONFIG-1/2 | Archived |
| `orch-conv.md` | Closed | — | CONV-1/2/3/4 | Archived |
| `orch-docs-sprint.md` | Closed | — | DOCS-1 | Archived |
| `orch-foundation.md` | Closed | — | FOUND-1/2 | Archived |
| `orch-handoff.md` | Closed | — | HANDOFF-1/2/3/4 | Archived |
| `orch-intent.md` | Closed | — | INTENT-1/2/3/4 | Archived |
| `orch-ops.md` | Closed | — | OPS-1/2/3 | Archived |
| `orch-worktree-placement.md` | Closed | — | WT-PLACE-1 | Done, archived |

### Summary

- **~27 open tickets** across 13 active backlogs
- **~45 closed tickets** across 9 closeable/archivable backlogs
- **6 deferred tickets** in orch-team.md (team-scale, not relevant for solo dev)

## Consolidation Tasks

### 1. Archive completed backlogs

Move to `docs/project/backlog/archive/` (or mark Status: Closed at top of each):

- `orch-merge-queue.md` (all 7 tickets done)
- `orch-chain.md` (all done)
- `orch-config.md` (all done)
- `orch-conv.md` (all done)
- `orch-docs-sprint.md` (all done)
- `orch-foundation.md` (all done)
- `orch-handoff.md` (all done)
- `orch-intent.md` (all done)
- `orch-ops.md` (all done)
- `orch-dashboard-ux.md` (DASH-UX-1 done)
- `recurring-schedules.md` (CRON-1/2/3 all done)
- `generic-backend.md` (GBE-1/2/3 all done)

This removes 12 files from the active backlog directory, leaving only files with open work.

### 2. Close nearly-done backlogs

These have 1 ticket left that's small or in progress — close the ticket, then archive:

- `orch-rename-workdir.md` — ORCH-RENAME-1 is in progress. Once merged, archive.
- `orch-worktree-safety.md` — WORKTREE-SAFETY-1 is in progress. Once merged, archive.
- `orch-wait-ax.md` — WAIT-AX-1 is a small AX fix. Ship it, then archive.

### 3. Consolidate orphan tickets into parent backlogs

Some backlogs have 1-2 remaining tickets that belong better elsewhere:

- `delayed-dispatch.md` has only **SCHED-3** (dashboard visibility for scheduled tasks). This is a dashboard UX ticket — consider moving it into `orch-evolution.md` or completing it standalone and archiving.
- `lifecycle-hooks.md` has only **HOOKS-4** (declarative filters). Small standalone ticket — ship and archive, or fold into a "polish" batch.
- `orch-observability.md` has **OBS-02** (aggregation queries) and **OBS-04** (dashboard cost visibility, in progress). These should stay together — keep this backlog active until both are done.

### 4. Validate cross-backlog dependencies

| Ticket | Depends on | Status of dependency |
|--------|-----------|---------------------|
| MFE-2 | MFE-1 | MFE-1 is Todo |
| MFE-3 | MFE-1 | MFE-1 is Todo |
| MPR-2 | MPR-1 | MPR-1 is Todo |
| MPR-3 | MPR-1 + MPR-2 | Both Todo |
| MPR-4 | MPR-2 | MPR-2 is Todo |
| GAP-2 | Soft: EVO-16 | EVO-16 is Todo |
| GAP-3 | OBS-01 (shipped), ideally OBS-02 | OBS-02 is Todo |
| GAP-4 | None | Independent |
| EVO-10 | EVO-2 (shipped) | Ready |
| ORCH-TEAM-3 | MFE-2 + TEAM-2 | Both Todo/Deferred |

No circular dependencies. No blockers preventing immediate work on GAP-1, GAP-4, GAP-5, GAP-7, EVO-6, EVO-10, HOOKS-4.

### 5. Resulting active backlog structure (post-consolidation)

After archiving and consolidating, the active backlog should contain:

```text
docs/project/backlog/
├── cli-ux.md                  # CLI-1, CLI-2, CLI-3, CLI-4 (onboarding)
├── multi-frontend.md          # MFE-1, MFE-2, MFE-3 (foundational)
├── multi-project.md           # MPR-1, MPR-2, MPR-3, MPR-4 (workflow)
├── NEXT.md                    # Priority queue (active / queued / backlog)
├── orch-evolution.md          # EVO-6, EVO-10, EVO-11, EVO-14, EVO-16 (features)
├── orch-observability.md      # OBS-02, OBS-04 (finish in-flight work)
├── quality-gaps.md            # GAP-1 through GAP-7 (quality gaps)
├── orch-team.md               # TEAM-1/2/3/4/5/7 (deferred, team-scale)
├── template.md                # Backlog template
└── archive/                   # All closed backlogs
    ├── orch-merge-queue.md
    ├── orch-chain.md
    ├── ...
```

9 active files (including template and NEXT.md), down from 22.

### 6. Priority tiers for open work

**Tier 0 — Finish in-flight work:**
OBS-04 (in progress), ORCH-RENAME-1 (in progress), WORKTREE-SAFETY-1 (in progress)

**Tier 1 — Quick wins (S complexity, ship this week):**
GAP-2 (stale session recovery), GAP-1 (circuit breaker), GAP-5 (mouse support), WAIT-AX-1 (fan-out metadata), HOOKS-4 (hook filters)

**Tier 2 — Near-term value (M complexity):**
GAP-7 (brew/binstall distribution), EVO-16 (crash session resume), EVO-6 (quick dispatch from TUI), OBS-02 (tool metrics queries), SCHED-3 (schedule dashboard visibility)

**Tier 3 — Foundational (M-L complexity, unlocks next phase):**
MFE-1 → MFE-2 → MFE-3 (service layer → HTTP API → migration)
MPR-1 → MPR-2 → MPR-3/4 (project config → dispatch resolution → handoff/dashboard)

**Tier 4 — Differentiation (M complexity):**
GAP-3 (cost budgets), GAP-4 (audit trail), GAP-6 (shared context), EVO-14 (thread dependencies)

**Tier 5 — Deferred:**
EVO-10 (webhooks — hooks cover 80% of this), EVO-11 (periodic summaries), all ORCH-TEAM-* tickets, visual orchestration canvas, spatial computing

## Verification after consolidation

- `ls docs/project/backlog/` shows only active files + archive/ + template.md
- Every active backlog file has `Status: Active` and at least one Todo ticket
- No ticket ID appears in more than one backlog file
- `quality-gaps.md` cross-references are accurate (check dependency ticket statuses)
- Closed backlogs in archive/ all have `Status: Closed` or `Status: Complete`
- `make verify` passes (markdown lint)
