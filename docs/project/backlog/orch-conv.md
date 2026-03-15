# Aster-Orch Conversation View Polish

Status: Active
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
- Status: Todo

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
- Status: In Progress

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
- Status: Todo

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
- Status: Todo

## Execution Order

1. ORCH-CONV-3 (border simplification — smallest change, clears the way)
2. ORCH-CONV-1 (markdown rendering — the main feature)
3. ORCH-CONV-2 (scroll fix — test after markdown changes line structure)
4. ORCH-CONV-4 (integration verification — depends on all above)

## Tracking Notes

- Backlog-first governance applies.
- Implementation commits should reference ticket IDs.
- Bug found via operator screenshots: long markdown messages are unreadable in conversation view.
- Scroll bug: `line_count` counts logical lines, not visual rows after wrapping — causes scroll overshoot.
- Border bug: hard-coded 56-char box border mismatches body text width.
- All work in `src/dashboard/views/conversation.rs` plus `Cargo.toml` for pulldown-cmark.

## Execution Metrics

- Ticket: ORCH-CONV-1
- Owner: TBD
- Complexity: M
- Risk: Medium
- Start:
- End:
- Duration:
- Notes:

- Ticket: ORCH-CONV-2
- Owner: TBD
- Complexity: S
- Risk: Low
- Start:
- End:
- Duration:
- Notes:


- Start: 2026-03-15 21:38 UTC

- Ticket: ORCH-CONV-3
- Owner: TBD
- Complexity: S
- Risk: Low
- Start:
- End:
- Duration:
- Notes:

- Ticket: ORCH-CONV-4
- Owner: TBD
- Complexity: S
- Risk: Low
- Start:
- End:
- Duration:
- Notes:

## Closure Evidence

- (To be filled on batch completion)
