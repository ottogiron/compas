# Aster-Orch Conversation View Polish

Status: Closed
Owner: otto
Created: 2026-03-15

## Scope Summary

- Render markdown in conversation view message bodies (headers, bold, code, lists, tables)
- Fix scroll math to account for wrapped lines
- Replace fixed-width box borders with left-side indicator
- End-to-end verification with real agent output

## Ticket ORCH-CONV-1 — Markdown Rendering in Message Bodies

- Goal: Parse message body markdown and render as styled ratatui text (headers, bold, italic, inline code, code blocks, lists, tables, horizontal rules).
- In scope:
  - Add `pulldown-cmark` dependency to `Cargo.toml`
  - New `markdown_to_lines()` function that converts markdown body text to `Vec<Line<'static>>` with styled spans
  - Rendering targets: `## Header` -> bold+accent, `**bold**` -> bold, `*italic*` -> italic, `` `code` `` -> cyan, code blocks -> dim style, `- lists` -> bullet prefix, `---` -> dim horizontal rule, tables -> pass-through monospace
  - Replace raw `body.lines()` in `push_message_lines()` with `markdown_to_lines()`
- Out of scope:
  - Images, links (display link text only, no interaction)
  - Syntax highlighting inside code blocks
  - HTML tags embedded in markdown
- Dependencies: None
- Acceptance criteria:
  - Agent messages with markdown headers, bold, code blocks, and lists render with visual distinction
  - Plain text messages (no markdown) render identically to current behavior
  - No panics on malformed markdown
- Verification:
  - `make verify` passes
  - Manual: dispatch to an agent that produces markdown output, verify styled rendering in conversation view
- Status: Done

## Ticket ORCH-CONV-2 — Fix Scroll Math for Wrapped Lines

- Goal: Fix scroll offset calculation to account for visual rows after line wrapping, so scrolling works correctly with long messages.
- In scope:
  - Use `Paragraph::line_count(width)` or manual calculation to get actual visual row count after wrapping
  - Use visual row count for `max_offset` instead of `display_lines.len()`
  - Ensure `scroll_up`/`scroll_down`/`scroll_to_top`/`scroll_to_bottom` all work correctly with wrapped content
  - Store `visible_rows` from actual render area for page-scroll sizing
- Out of scope:
  - Horizontal scrolling
  - Per-message scroll (stay with single-paragraph whole-view scroll)
- Dependencies: None (but should be tested after CONV-1 since markdown rendering changes line structure)
- Acceptance criteria:
  - Scrolling to bottom shows the last message fully
  - Scrolling to top shows the first message header
  - Page-up/page-down moves by visible viewport height
  - No scroll overshoot or undershoot with long wrapped lines
- Verification:
  - `make verify` passes
  - Manual: open conversation with long agent messages, scroll top to bottom, verify all content is reachable
- Status: Done

## Ticket ORCH-CONV-3 — Left-Side Indicator Border

- Goal: Replace the fixed-width 56-char box-drawing border with a left-side-only indicator to eliminate width mismatch and save vertical space.
- In scope:
  - Remove top border and bottom border lines from `push_message_lines()`
  - Keep `│ ` prefix on body lines
  - Add a thin dim separator line between messages to replace the visual boundary
  - Adjust scroll math (2 fewer lines per message)
- Out of scope:
  - Colored left-side indicators per sender (future enhancement)
  - Collapsible messages
- Dependencies: None
- Acceptance criteria:
  - Messages render with left prefix on body lines, no top/bottom box borders
  - Visual separation between messages is clear (dim separator or blank line)
  - No hard-coded width constants remain
  - More messages fit on screen (2 lines saved per message)
- Verification:
  - `make verify` passes
  - Manual: view conversation with multiple messages at different terminal widths, verify no visual artifacts
- Status: Done

## Ticket ORCH-CONV-4 — Integration Verification and Edge Cases

- Goal: End-to-end verification of the conversation view with real agent output, plus edge case handling.
- In scope:
  - Test with real agent markdown output (review-request messages, completion reports)
  - Handle edge cases: empty messages, very long single lines, messages with only code blocks, messages with tables, Unicode/emoji content
  - Verify follow mode still auto-scrolls correctly with new rendering
  - Verify live polling does not cause scroll jumps when new messages arrive
  - Fix any visual regressions found during testing
- Out of scope:
  - New features beyond what CONV-1/2/3 deliver
- Dependencies: ORCH-CONV-1, ORCH-CONV-2, ORCH-CONV-3
- Acceptance criteria:
  - Real agent output (like review-request messages with markdown) renders readably
  - No scroll jumps on live message arrival
  - Follow mode tracks new messages
  - No panics on edge case inputs
- Verification:
  - `make verify` passes
  - Manual: dispatch a real task to an agent, observe conversation view throughout execution lifecycle
- Status: Done

## Execution Order

1. ~~ORCH-CONV-3 (border simplification — done)~~
2. ~~ORCH-CONV-1 (markdown rendering — done)~~
3. ~~ORCH-CONV-2 (scroll fix — done)~~
4. ~~ORCH-CONV-4 (integration verification — done)~~

## Tracking Notes

- Backlog-first governance applies.
- Implementation commits should reference ticket IDs.
- Bug found via operator screenshots: long markdown messages are unreadable in conversation view.
- Scroll bug: `line_count` counts logical lines, not visual rows after wrapping — causes scroll overshoot.
- Border bug: hard-coded 56-char box border mismatches body text width.
- All work in `src/dashboard/views/conversation.rs` plus `Cargo.toml` for pulldown-cmark.

## Execution Metrics

- Ticket: ORCH-CONV-1
- Owner: orch-dev
- Complexity: M
- Risk: Medium
- Start: 2026-03-15 21:26 UTC
- End: 2026-03-15 21:33 UTC
- Duration: ~00:07:00
- Notes: pulldown-cmark integration, markdown_to_lines(), 12 new tests. Merged with conflict resolution against CONV-3.

- Ticket: ORCH-CONV-2
- Owner: orch-dev
- Complexity: S
- Risk: Low
- Start: 2026-03-15 21:36 UTC
- End: 2026-03-15 21:40 UTC
- Duration: ~00:04:00
- Notes: paragraph.line_count(inner_width) for visual row count, ratatui unstable-rendered-line-info feature.

- Ticket: ORCH-CONV-3
- Owner: orch-dev-2
- Complexity: S
- Risk: Low
- Start: 2026-03-15 21:26 UTC
- End: 2026-03-15 21:32 UTC
- Duration: ~00:06:00
- Notes: Removed box borders, added dim separator. Agent forgot to commit — operator committed in worktree.

- Ticket: ORCH-CONV-4
- Owner: operator
- Complexity: S
- Risk: Low
- Start: 2026-03-15 21:45 UTC
- End: 2026-03-15 21:50 UTC
- Duration: ~00:05:00
- Notes: Visual verification via real agent output screenshot. Markdown rendering, scrolling, and layout all confirmed working.

## Closure Evidence

- All 4 tickets implemented and merged on main
- Markdown rendering: headers (bold+accent), bold, italic, inline code (cyan), code blocks (dim), bullet lists, thematic breaks
- Scroll fix: visual row count via `paragraph.line_count(width)` replaces logical line count
- Border: box borders removed, left-side `│` indicator + dim `─` separators
- 12 new unit tests for markdown rendering, all existing tests pass (477 total)
- Visual verification: real agent review-request output renders correctly (screenshot confirmed by operator)
- Verification: `make verify` passes (fmt-check + clippy + 362 unit + 22 bin + 93 integration tests)
