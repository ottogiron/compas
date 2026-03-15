# Aster-Orch Ops Dashboard Redesign

Status: Active
Owner: otto
Created: 2026-03-15

## Scope Summary

- Remove Context sidebar panel — replace with inline detail row on selected item
- Remove admin action keybindings and dead code (action menu, abandon, reopen, stale cleanup)
- Responsive column widths for narrow/portrait terminals
- Collapse empty sections to save vertical space
- Keep only `c` (conversation) and `Enter` (drill) as interactive keybindings

## Ticket ORCH-OPS-1 — Remove Context Panel and Add Inline Detail Row

- Goal: Remove the right-side Context panel. Make the list full-width. Show context info as an inline sub-line below the selected item.
- In scope:
  - Remove the `Layout::horizontal([Fill(64), Fill(36)])` split in `render_activity()`
  - Remove `render_context_panel()` function entirely
  - Pass full inner area to `render_ops_list()`
  - Add inline detail sub-line for selected threads: `└─ {intent} │ [c] conversation`
  - Add inline detail sub-line for selected batches: `└─ a:{active} c:{completed} f:{failed} │ [Enter] drill`
  - Running threads keep existing progress sub-line, append `│ [c] conversation` to it
  - Remove `kv_line()` and `action_line()` helper functions if no longer used
- Out of scope:
  - Responsive column widths (OPS-2)
  - Removing admin keybindings (OPS-3)
- Dependencies: None
- Acceptance criteria:
  - No Context panel rendered at any terminal width
  - Selected thread shows inline detail with intent and `[c]` hint
  - Selected batch shows inline detail with counts and `[Enter]` hint
  - List uses full available width
  - `make verify` passes
- Verification:
  - `make verify` passes
  - Manual: view Ops tab in portrait and horizontal, verify no Context panel, inline details visible
- Status: Todo

## Ticket ORCH-OPS-2 — Responsive Columns and Collapse Empty Sections

- Goal: Adapt thread/batch row column widths based on terminal width. Collapse empty active sections to save vertical space.
- In scope:
  - Define two column profiles based on available width:
    - Wide (>=100): current widths (icon 3, thread 18, status 16, agent 12, batch 14, duration variable)
    - Narrow (<100): compressed (icon 3, thread 12, status 12, agent 10, duration variable; batch column hidden)
  - Pass available width to `make_thread_line()` and `make_batch_line()`
  - For batch rows at narrow widths: drop `a:N c:N f:N` inline counts (shown on detail row instead)
  - Collapse empty sections: when Running, Active Batches, and Active Threads are ALL empty, show single dim line `no active work` instead of 3 headers with "none"
  - When some active sections have items, only show the non-empty ones (skip empty section headers entirely)
- Out of scope:
  - Changing the data model or queries
  - Changing horizontal-mode layout (it benefits from same changes)
- Dependencies: ORCH-OPS-1 (context panel removal changes the available width)
- Acceptance criteria:
  - At narrow widths (<100 cols), thread rows fit without clipping important columns
  - Batch column is hidden at narrow widths but visible at wide widths
  - Empty active sections are collapsed, saving 6+ lines of vertical space
  - `make verify` passes
- Verification:
  - `make verify` passes
  - Manual: resize terminal to various widths, verify columns adapt and no clipping
- Status: Todo

## Ticket ORCH-OPS-3 — Remove Admin Action Keybindings and Dead Code

- Goal: Remove the admin action system (action menu, abandon, reopen, stale cleanup keybindings) from the Ops tab and clean up dead code.
- In scope:
  - Remove keybindings from `handle_list_key()` in `app.rs`: `a` (action menu), `b` (abandon), `o` (reopen), `s` (stale cleanup)
  - Remove methods: `open_action_menu()`, `queue_admin_action()`, `queue_stale_active_cleanup()`
  - Remove types if no longer referenced: `ActionMenuState`, `PendingAdminAction`, `AdminActionKind`, `action_name()`
  - Remove action menu rendering code (the overlay that was broken/overlapping)
  - Update footer hint bar: remove `a: actions`, `s: stale cleanup`, `Esc: back batch` references to admin actions
  - Keep `c` (conversation), `Enter` (drill/log viewer), `Esc`/`x` (back from drill), navigation keys
  - Clean up any orphaned imports
- Out of scope:
  - Removing the underlying lifecycle MCP tools (orch_close, orch_abandon, orch_reopen) — those stay
  - Removing admin action execution logic if it's shared with MCP handlers
- Dependencies: ORCH-OPS-1 (context panel removal eliminates action hints there)
- Acceptance criteria:
  - Keys `a`, `b`, `o`, `s` do nothing on the Ops tab
  - No action menu overlay renders
  - No dead code (clippy clean)
  - Footer hint bar is simplified
  - `make verify` passes
- Verification:
  - `make verify` passes
  - Manual: press `a`, `b`, `o`, `s` on Ops tab — no action, no crash
- Status: Todo

## Execution Order

1. ORCH-OPS-1 (context panel removal — foundational layout change)
2. ORCH-OPS-3 (admin action removal — can run in parallel with OPS-2 since it touches app.rs, not activity.rs)
3. ORCH-OPS-2 (responsive columns — builds on OPS-1 layout)

Note: OPS-1 and OPS-3 touch different primary files (activity.rs vs app.rs) and can be dispatched in parallel.

## Tracking Notes

- Motivated by portrait mode usability: Context panel wastes space, columns clip, empty sections waste lines.
- Horizontal mode also benefits (more list width, inline context closer to items).
- Admin actions had low usage — operator uses MCP tools or lets agents handle lifecycle.
- Action menu (`a`) was broken (overlay overlaps list content).
- Batch row `a:N c:N f:N` counts were unreadable at narrow widths.

## Execution Metrics

- Ticket: ORCH-OPS-1
- Owner: TBD
- Complexity: M
- Risk: Medium
- Start:
- End:
- Duration:
- Notes:

- Ticket: ORCH-OPS-2
- Owner: TBD
- Complexity: M
- Risk: Low
- Start:
- End:
- Duration:
- Notes:

- Ticket: ORCH-OPS-3
- Owner: TBD
- Complexity: M
- Risk: Low
- Start:
- End:
- Duration:
- Notes:

## Closure Evidence

- (To be filled on batch completion)
