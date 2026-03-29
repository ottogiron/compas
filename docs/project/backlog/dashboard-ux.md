# Dashboard UX Improvements

Status: Done
Owner: operator
Created: 2026-03-29

## Scope Summary

- Fix help dialog overflow on small terminals by making it scrollable
- Add quit confirmation dialog to prevent accidental exits

## Ticket DUX-1 — Fix help dialog overflow on small terminals

- Goal: Make the "?" help overlay scrollable so it renders correctly on small terminals instead of overflowing into underlying content.
- In scope:
  - Add scroll state (`help_scroll`, `help_viewport_height`) to `App` struct
  - Apply `Paragraph::scroll()` in `render_help_overlay_widget`
  - Add j/k and arrow key scrolling in `handle_help_key`
  - Show scroll indicator when content exceeds viewport
  - Reset scroll on toggle
- Out of scope:
  - Changing help content or layout
  - Mouse scroll support in help overlay
- Dependencies: none.
- Acceptance criteria:
  - Help overlay is scrollable with j/k when terminal height < 23 rows
  - Scroll indicator appears when content is clipped
  - Scroll offset resets when help is reopened
  - No overflow or visual bleed into underlying content
- Verification:
  - `make verify`
  - Manual: resize terminal to ~15 rows, press `?`, verify scrollable with j/k
- Status: Done

## Ticket DUX-2 — Add quit confirmation dialog

- Goal: Show an "Are you sure you want to quit?" dialog when pressing `q` instead of quitting immediately.
- In scope:
  - Add `confirm_quit: bool` state to `App` struct
  - Change `q` in `handle_list_key` to set `confirm_quit = true`
  - Add `handle_quit_confirm_key` (y/Y confirms, n/N/Esc cancels)
  - Add `render_quit_confirm_widget` using `centered_rect`
  - Wire into key routing and render pipeline
  - Ctrl+C remains immediate quit (no confirmation)
- Out of scope:
  - Confirmation for Ctrl+C
  - Saving state on quit
- Dependencies: none.
- Acceptance criteria:
  - Pressing `q` shows confirmation dialog, does not quit
  - `y`/`Y` confirms quit
  - `n`/`N`/`Esc` cancels and returns to normal view
  - `Ctrl+C` still quits immediately without confirmation
  - Dialog renders centered with clear y/n prompt
- Verification:
  - `make verify`
  - Manual: press `q`, verify dialog, press `n` (cancels), press `q` then `y` (quits), verify Ctrl+C bypasses
- Status: Done

## Execution Order

1. DUX-1 (parallel)
2. DUX-2 (parallel)

## Tracking Notes

- Backlog-first governance applies.
- Implementation commits should reference ticket IDs.
- Both tickets are independent and can be dispatched in parallel.
- Both modify `src/dashboard/app.rs` but touch different state, render, and key handler sections.

## Execution Metrics

- Ticket: DUX-1
- Owner: worker agent
- Complexity: S
- Risk: Low
- Start:
- End:
- Duration:
- Notes:

- Ticket: DUX-2
- Owner: worker agent
- Complexity: S
- Risk: Low
- Start:
- End:
- Duration:
- Notes:

## Closure Evidence

- DUX-1: Help overlay now scrollable with j/k when terminal < 23 rows, scroll indicator shown, debug_assert guards line count sync
- DUX-2: Quit confirmation dialog on `q` with y/n, Ctrl+C bypasses, help text updated to reflect new behavior
- Both include unit tests and changelog fragments
- `make verify` passes (934 tests, clippy clean)
- Dispatched in parallel, required 1 review round each for changelog + tests, then approved
