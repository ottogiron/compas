# Ops Dashboard UX Improvements

Status: Active
Owner: operator
Created: 2026-03-21

## Scope Summary

- Improve readability of Ops dashboard activity view

## Ticket DASH-UX-1 — P0+P1 Ops view readability fixes

- Goal: Fix column collisions, noisy progress lines, wasted ID space, and cryptic batch stats
- In scope:
  - Shorten thread IDs (show last N chars instead of first N)
  - Add column separators between agent/summary/batch
  - Filter tool_result events from progress display
  - Use readable batch stats labels with semantic colors
  - ADR for telemetry filter decision
  - CHANGELOG entries
- Out of scope:
  - Section dividers (P2)
  - Status color differentiation (P2)
  - Portrait responsive improvements (P3)
- Dependencies: none
- Acceptance criteria:
  - make verify passes
  - Thread IDs show truncated-left format
  - Columns visually separated
  - Progress shows tool names not API IDs
  - Batch stats use full labels
- Verification:
  - make verify
  - Visual check via make dashboard-dev
- Status: Done

## Execution Order

1. DASH-UX-1

## Tracking Notes

- Design reviewed by compas-ux (thread 01KM8QHXVHZBZTB90RK5W88V4F)
- Implementation by compas-dev, reviewed by compas-reviewer

## Execution Metrics

- Ticket: DASH-UX-1
- Owner: (pending)
- Complexity: (pending)
- Risk: (pending)
- Start: 2026-03-21 18:04 UTC
- End: 2026-03-21 18:05 UTC
- Duration: 00:01:06
- Notes: (pending)
