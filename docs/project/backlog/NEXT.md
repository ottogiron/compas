# Priority Queue

Last updated: 2026-03-29

## Active

- CAP-VIS — Agent capacity visibility in orch_list_agents (`capacity-visibility.md`)

> List concrete in-flight tickets only. Use backlog status and current work reality as the source of truth; use `ticket status` as a hint, not as a mechanical source. Exclude stale sessions and completed tickets.

## Queue

Ordered by priority. Work top item when capacity is available.

1. SEC-6 — Recursive dispatch protection (`security-hardening.md`)
2. EVO-16 — Session resume after crash (`orch-evolution.md`)
3. ISESS-SPIKE — Interactive sessions validation spike (`interactive-sessions.md`)
4. EVO-6 — Quick dispatch from TUI (`orch-evolution.md`)
5. OBS-02 — Tool metrics aggregation (`orch-observability.md`)
6. SCHED-3 — Schedule dashboard visibility (`delayed-dispatch.md`)

## Backlog

Unordered. Pick when queue is empty. See `docs/project/backlog-consolidation-guide.md` for tier rationale.

- MFE-1, MFE-2, MFE-3 — Multi-frontend service layer (`multi-frontend.md`)
- MPR-1, MPR-2, MPR-3, MPR-4 — Multi-project support (`multi-project.md`)
- GAP-3 — Cost budget controls (`quality-gaps.md`)
- GAP-4 — Governance audit trail (`quality-gaps.md`)
- GAP-6 — Shared context store (`quality-gaps.md`)
- EVO-14 — Thread dependencies (`orch-evolution.md`)
- ISESS-1/2/3/4 — Interactive sessions Phase 1: tmux batch mode (`interactive-sessions.md`)
- ISESS-5/6/7/8 — Interactive sessions Phase 2: interactive mode (`interactive-sessions.md`)
