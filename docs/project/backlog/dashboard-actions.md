# Dashboard Actions

Status: Active
Owner: operator
Created: 2026-03-29

## Scope Summary

- Context-sensitive action menu (`a` key) in the TUI dashboard with lazygit-style interaction
- Lifecycle actions: abandon thread/batch, close thread, reopen thread, cancel merge, queue merge
- Shared service extraction: `LifecycleService.abandon_batch()`, `MergeService` for merge queueing
- Action state machine (Idle → Menu → Confirming → Feedback) with status bar feedback
- MCP handler refactoring to delegate to shared services

## Ticket DASH-ACT-1 — Action Types & Execution Engine

- Goal: Create the action type system, state machine, and async execution engine for dashboard actions.
- In scope:
  - `src/dashboard/actions.rs` new module
  - `ActionState`, `ActionTarget`, `PendingAction`, `ActionEntry`, `ActionResult` types
  - `available_actions()` — state-aware action filtering per target
  - `execute_action()` — async dispatcher calling LifecycleService/MergeService with 5s timeout
  - `needs_confirmation()`, `confirmation_prompt()`, `action_for_key()` helpers
  - `resolve_action_target()` — converts `OpsSelectable` to `ActionTarget` with async worktree check
  - In-module unit tests for all pure functions
- Out of scope:
  - Rendering (WS-4)
  - Integration into `app.rs` event loop (WS-5)
- Dependencies: DASH-ACT-2, DASH-ACT-3 (for `abandon_batch` and `MergeService` types, but can use stub/placeholder imports during development).
- Acceptance criteria:
  - `available_actions()` returns correct action sets for all target/state combinations
  - `execute_action()` dispatches to correct service method for each `PendingAction` variant
  - `needs_confirmation()` returns false only for `ReopenThread`
  - All unit tests pass
- Verification:
  - `cargo test --lib dashboard::actions`
  - `make verify`
- Status: Done

## Ticket DASH-ACT-2 — LifecycleService.abandon_batch()

- Goal: Add batch abandon to the shared lifecycle service and refactor the MCP handler to use it.
- In scope:
  - `AbandonBatchOutcome` type in `src/lifecycle/mod.rs`
  - `LifecycleService::abandon_batch()` method — iterates batch threads, calls `abandon()` per non-terminal thread, aggregates results
  - Refactor `src/mcp/lifecycle.rs` `abandon_batch_impl()` to delegate to `LifecycleService::abandon_batch()`
  - Unit tests in `src/lifecycle/mod.rs`
- Out of scope:
  - Dashboard integration
  - MergeService extraction
- Dependencies: none.
- Acceptance criteria:
  - `abandon_batch()` abandons all non-terminal threads in a batch
  - Already-terminal threads are counted in `threads_already_terminal`
  - Per-thread errors are collected, not fatal
  - Empty/nonexistent batch returns zero counts (not an error)
  - MCP handler produces identical behavior after refactor
- Verification:
  - `cargo test --lib lifecycle`
  - `cargo test abandon_batch`
  - `make verify`
- Status: Done

## Ticket DASH-ACT-3 — MergeService Extraction

- Goal: Extract merge queueing logic from the MCP handler into a shared service so dashboard can queue merges without MCP coupling.
- In scope:
  - `src/lifecycle/merge.rs` new module with `MergeService`, `QueueMergeOutcome`, `MergeError`
  - `MergeService::queue_merge()` — validates strategy, resolves repo_root, runs preflight, inserts op
  - `pub mod merge;` declaration in `src/lifecycle/mod.rs`
  - Refactor `src/mcp/merge.rs::merge_impl()` to delegate to `MergeService::queue_merge()`
  - Unit tests for validation and error paths
- Out of scope:
  - Merge execution (worker-side)
  - Wait-merge blocking
  - Dashboard integration
- Dependencies: none.
- Acceptance criteria:
  - `queue_merge()` produces identical merge operations as the current MCP handler
  - Invalid strategy returns `MergeError::InvalidStrategy`
  - Abandoned threads rejected by preflight
  - MCP handler produces identical behavior after refactor
- Verification:
  - `cargo test --lib lifecycle::merge`
  - `cargo test merge`
  - `make verify`
- Status: Done

## Ticket DASH-ACT-4 — Action Menu View Widget

- Goal: Create rendering components for the action menu overlay, confirmation bar, and feedback flash.
- In scope:
  - `src/dashboard/views/action_menu.rs` new module
  - `render_action_menu()` — centered overlay listing available actions with key labels
  - `confirmation_line()` — status bar Line for y/n confirmation prompts
  - `feedback_line()` — status bar Line for success/error feedback flash
  - `pub mod action_menu;` in `src/dashboard/views/mod.rs`
  - Styling using existing `theme.rs` constants (BG_PANEL, BORDER_FOCUS, ACCENT, WARNING, SUCCESS, FAILURE, MARKER_COMPLETED, MARKER_FAILED)
  - Uses existing `centered_rect` helper pattern
  - Unit tests verifying no panics and correct styling
- Out of scope:
  - Key handling logic
  - Action execution
  - Integration into app.rs render loop
- Dependencies: none (only uses existing theme constants and ratatui types).
- Acceptance criteria:
  - `render_action_menu` renders without panic for 0-6 action entries
  - `confirmation_line` produces styled spans with WARNING color
  - `feedback_line` uses SUCCESS/FAILURE colors correctly
  - All rendering functions are pure (no state mutation)
- Verification:
  - `cargo test --lib dashboard::views::action_menu`
  - `make verify`
- Status: Done

## Ticket DASH-ACT-5 — Dashboard Integration

- Goal: Wire action types, execution engine, and view widgets into the dashboard event loop and render pipeline.
- In scope:
  - Add `ActionState` field to `App` struct (initialized to `Idle`)
  - Add `config` field to `App` (for `MergeService` construction)
  - Key dispatch hierarchy: `viewing_conversation → viewing_log → show_help → action_state → handle_list_key`
  - Wire `'a'` key in Ops tab and History tab to open action menu
  - Action menu rendering overlay in main render function
  - Status bar: confirmation prompt, feedback flash, `a: actions` hint
  - Help overlay: add Actions section
  - Auto-clear feedback after 3 seconds in tick handler
  - `pub mod actions;` in `src/dashboard/mod.rs`
- Out of scope:
  - Quick dispatch text input (EVO-6)
  - Interactive sessions (ISESS)
- Dependencies: DASH-ACT-1, DASH-ACT-2, DASH-ACT-3, DASH-ACT-4.
- Acceptance criteria:
  - Pressing `a` on a running thread shows action menu with Abandon option
  - Pressing `a` on a terminal thread shows Reopen option
  - Pressing `a` on a batch shows Abandon batch option
  - Pressing `a` on a queued merge op shows Cancel merge option
  - Confirmation prompt appears for destructive actions
  - Reopen executes without confirmation
  - Feedback flash appears for 3 seconds after action execution
  - Help overlay includes Actions section
  - Status bar shows `a: actions` hint on Ops tab
- Verification:
  - `make verify`
  - Manual test with `make dashboard-dev`
- Status: Todo

## Ticket DASH-ACT-6 — End-to-End Tests

- Goal: Integration tests exercising the full action → lifecycle → store path.
- In scope:
  - `test_dashboard_abandon_thread_action`
  - `test_dashboard_abandon_batch_action`
  - `test_dashboard_close_completed_action`
  - `test_dashboard_close_worktree_requires_merge`
  - `test_dashboard_reopen_action`
  - `test_dashboard_cancel_merge_action`
  - `test_dashboard_queue_merge_action`
  - `test_available_actions_state_filtering`
- Out of scope:
  - TUI rendering tests (manual verification)
- Dependencies: DASH-ACT-5.
- Acceptance criteria:
  - All integration tests pass
  - `make verify` passes
- Verification:
  - `cargo test dashboard`
  - `make verify`
- Status: Todo

## Execution Order

1. DASH-ACT-1, DASH-ACT-2, DASH-ACT-3, DASH-ACT-4 (parallel — Batch A)
2. DASH-ACT-5 (sequential — Batch B, depends on all of Batch A)
3. DASH-ACT-6 (sequential — Batch C, depends on Batch B)

## Tracking Notes

- Backlog-first governance applies.
- Implementation commits should reference ticket IDs.
- Batch A workstreams touch independent files — safe for parallel worktree dispatch.
- WS-2 and WS-3 both add to `src/lifecycle/mod.rs` (pub mod declaration) — merge order matters; WS-5 resolves.
- Record scope changes/deferrals here.

## Execution Metrics

- Ticket: DASH-ACT-1
- Owner: worker
- Complexity: M
- Risk: Low
- Start:
- End:
- Duration:
- Notes:

- Ticket: DASH-ACT-2
- Owner: worker
- Complexity: M
- Risk: Low
- Start:
- End:
- Duration:
- Notes:

- Ticket: DASH-ACT-3
- Owner: worker
- Complexity: M
- Risk: Medium
- Start:
- End:
- Duration:
- Notes: MCP refactor risk — must preserve identical behavior

- Ticket: DASH-ACT-4
- Owner: worker
- Complexity: S
- Risk: Low
- Start:
- End:
- Duration:
- Notes:

- Ticket: DASH-ACT-5
- Owner: worker
- Complexity: L
- Risk: Medium
- Start:
- End:
- Duration:
- Notes: Largest workstream — touches app.rs (~2800 lines)

- Ticket: DASH-ACT-6
- Owner: worker
- Complexity: M
- Risk: Low
- Start:
- End:
- Duration:
- Notes:

## Closure Evidence

-
