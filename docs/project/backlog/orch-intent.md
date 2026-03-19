# Aster-Orch Intent Simplification

Status: Closed
Owner: otto
Created: 2026-03-16

## Scope Summary

- Remove agent intent management — agents reply naturally, no REPLY PROTOCOL
- Remove `parse_intent_from_text()` — all agent replies default to `response`
- Simplify `HandoffConfig` — remove intent-based routing fields (later extended to 3 fields by ORCH-HANDOFF-1/2)
- Remove deprecated handoff fields: `on_review_request`, `on_escalation`, `on_changes_requested`
- Dashboard coloring: source-based (operator/agent/system) instead of intent-based
- Add `changes-requested` to default trigger intents
- Update all docs: README, architecture, DECISIONS, skills, example config

## Ticket ORCH-INTENT-1 — Remove Agent Intent Parsing

- Goal: Remove `parse_intent_from_text()` and all agent intent annotation. All successful agent replies get `intent: "response"` automatically.
- In scope:
  - `src/backend/mod.rs`: Delete `parse_intent_from_text()` function and all its tests
  - `src/backend/claude.rs`: Remove `parse_intent_from_text(&result_text)` call, set `parsed_intent: None`
  - `src/backend/codex.rs`: Same removal
  - `src/backend/opencode.rs`: Same removal
  - `src/backend/gemini.rs`: Same removal
  - Remove the `parse_intent_from_text` import from all 4 backend files
  - In `src/worker/loop_runner.rs`: Where `parsed_intent` is used for handoff routing and reply message insertion, default to `"response"` when `None`
  - Clean up any orphaned imports
- Out of scope:
  - Changing operator intents (dispatch, handoff, changes-requested) — those stay
  - HandoffConfig simplification (INTENT-2)
  - Dashboard coloring (INTENT-3)
- Dependencies: None
- Acceptance criteria:
  - `parse_intent_from_text` function and all 7 tests deleted
  - All 4 backends set `parsed_intent: None` in BackendOutput
  - Worker defaults reply intent to `"response"` for all successful agent completions
  - Existing handoff routing still works (via the current `on_review_request` etc. until INTENT-2 cleans them up)
  - `make verify` passes
- Verification:
  - `make verify` passes
  - Integration test: dispatch to agent, verify reply message has `intent: "response"`
- Status: Done

## Ticket ORCH-INTENT-2 — Simplify HandoffConfig to 2 Fields

- Goal: Remove `on_review_request`, `on_escalation`, `on_changes_requested` from HandoffConfig. Keep only `on_response` and `max_chain_depth`.
- In scope:
  - `src/config/types.rs`: Remove `on_review_request`, `on_escalation`, `on_changes_requested` fields from `HandoffConfig`. Remove `HandoffTarget::Gated` variant (no longer needed for forward compat — the gated concept is dead). `HandoffTarget` becomes just a `String` (alias or "operator"). Simplified struct:

    ```rust
    pub struct HandoffConfig {
        pub on_response: Option<String>,
        pub max_chain_depth: Option<u32>,
    }
    ```

  - `src/config/validation.rs`: Remove validation for deleted fields (gated target rejection, all `on_*` field validations except `on_response`). Simplify to: validate `on_response` is a valid agent alias or "operator", validate `max_chain_depth` bounds, validate no self-loop. Remove all tests for deleted fields.
  - `src/worker/loop_runner.rs`: Simplify `maybe_auto_handoff()` — only check `on_response` route. Remove all other intent-to-route matching branches.
  - `src/store/mod.rs`: Keep `count_handoff_messages` and `insert_handoff_if_under_depth` (still used).
  - Add `changes-requested` to `default_trigger_intents()` in `src/config/types.rs` — currently only `dispatch` and `handoff`. This ensures operator `changes-requested` dispatches trigger agent execution.
- Out of scope:
  - Removing operator intents — `changes-requested` stays as operator dispatch intent
  - Dashboard changes (INTENT-3)
  - Doc updates (INTENT-4)
- Dependencies: ORCH-INTENT-1 (parse_intent removal means all replies are `response`)
- Acceptance criteria:
  - `HandoffConfig` has exactly 2 fields: `on_response` (Option<String>) and `max_chain_depth` (Option<u32>)
  - `HandoffTarget` enum removed — `on_response` is just a `String`
  - `default_trigger_intents` returns `["dispatch", "handoff", "changes-requested"]`
  - Agent with `handoff.on_response: reviewer` auto-dispatches on completion
  - Operator can dispatch with `intent: changes-requested` and it triggers the target agent
  - `make verify` passes
- Verification:
  - `make verify` passes
  - Integration tests updated for simplified config
  - Config validation tests updated
- Status: Done

## Ticket ORCH-INTENT-3 — Dashboard Source-Based Coloring

- Goal: Replace intent-based message coloring in the conversation view with source-based coloring.
- In scope:
  - `src/dashboard/views/conversation.rs`: Replace `intent_color()` function with `source_color()` that colors by:
    - Operator messages (`from == "operator"`) → Accent/highlight color
    - Agent replies (success, `from` is an agent alias) → Green
    - Agent replies (failure) → Red
    - System messages (handoff, chain-depth interrupt) → Dim/gray
  - The intent badge text still shows the intent string (e.g., "[dispatch]", "[response]") — just the COLOR changes to be source-based
  - Update or replace the `intent_color` tests with `source_color` tests
  - Keep intent badge text for operator messages showing `[dispatch]`, `[changes-requested]` etc. — useful for distinguishing operator actions
- Out of scope:
  - Ops tab changes — already uses source-based display
  - Changing message data model
- Dependencies: ORCH-INTENT-1 (intent field is now always `response` for agents)
- Acceptance criteria:
  - Operator messages have accent/highlight color
  - Agent reply messages have green color
  - System/handoff messages have dim color
  - Intent badge text still visible (shows the intent string)
  - `make verify` passes
- Verification:
  - `make verify` passes
  - Manual: view conversation with mixed operator/agent messages, verify color distinction
- Status: Done

## Ticket ORCH-INTENT-4 — Update All Documentation

- Goal: Update all docs to reflect the simplified intent model.
- In scope:
  - `README.md`:
    - Update handoff config section — show 2-field config
    - Update auto-handoff section — remove references to `on_review_request`, `on_escalation`
    - Update configuration reference — simplified handoff fields
    - Add note about `changes-requested` in trigger_intents
  - `docs/project/architecture.md`:
    - Update dispatch flow — agent replies always `response`, routing via `on_response` only
    - Update key design decisions — intent simplification
  - `docs/project/DECISIONS.md`:
    - Add ADR-015: Intent simplification — agents don't manage intents
    - Amend ADR-014: Note simplified HandoffConfig (2 fields)
  - `examples/config-generic.yaml`:
    - Update handoff section — show only `on_response` + `max_chain_depth`
    - Remove commented `on_review_request` etc.
  - `skills/orch-dispatch/SKILL.md` and `examples/skills/orch-dispatch/SKILL.md`:
    - Remove any references to agent REPLY PROTOCOL
    - Note that agents reply naturally, no intent annotation needed
  - `AGENTS.md`: Remove any references to agent intent protocol if present
- Out of scope:
  - Agent prompt changes (those are in production config, not docs)
- Dependencies: ORCH-INTENT-1, ORCH-INTENT-2
- Acceptance criteria:
  - No references to `parse_intent_from_text`, `on_review_request`, `on_escalation`, agent REPLY PROTOCOL in docs
  - ADR-015 exists
  - Handoff config examples show 2-field model
  - `make verify` passes
- Verification:
  - `make verify` passes
  - Grep for stale references: `on_review_request`, `on_escalation`, `REPLY PROTOCOL`, `parse_intent_from_text`
- Status: Done

## Execution Order

1. ORCH-INTENT-1 (remove parse_intent — zero risk, enables everything else)
2. ORCH-INTENT-2 (simplify HandoffConfig — depends on INTENT-1)
3. ORCH-INTENT-3 + ORCH-INTENT-4 in parallel (dashboard coloring + docs — independent of each other, both depend on INTENT-1/2)

## Tracking Notes

- Architect design review: thread 01KKW66XNM6QEFCWKK060SEV4Q (initial proposal + validation)
- Operator directive: "I didn't sign up for intent management"
- No backward compat needed — single operator, pre-v1
- Agent prompts (REPLY PROTOCOL removal) done manually by operator in production config after code ships
- `HandoffTarget` enum fully removed — `on_response` is just a String alias
- `changes-requested` added to default trigger intents to support operator → agent dispatch loop

## Execution Metrics

- Ticket: ORCH-INTENT-1
- Owner: TBD
- Complexity: S
- Risk: Low
- Start:
- End:
- Duration:
- Notes:

- Ticket: ORCH-INTENT-2
- Owner: TBD
- Complexity: M
- Risk: Medium
- Start:
- End:
- Duration:
- Notes:

- Ticket: ORCH-INTENT-3
- Owner: TBD
- Complexity: S
- Risk: Low
- Start:
- End:
- Duration:
- Notes:

- Ticket: ORCH-INTENT-4
- Owner: TBD
- Complexity: S
- Risk: Low
- Start:
- End:
- Duration:
- Notes:

## Closure Evidence

- All 4 tickets shipped and merged on main
- INTENT-1: parse_intent_from_text() deleted from all 4 backends. All agent replies default to `intent: "response"`.
- INTENT-2: HandoffConfig simplified to 2 fields (on_response, max_chain_depth). HandoffTarget enum removed. `changes-requested` added to default_trigger_intents.
- INTENT-3: Dashboard source-based coloring (operator=accent, agent=green, system=dim).
- INTENT-4: README, architecture, DECISIONS, example config, and skills updated. ADR-015 added.
- Verification:
  - `make verify`: fmt-check + clippy + test pass
  - Grep for stale refs (on_review_request, on_escalation, REPLY PROTOCOL, parse_intent_from_text): clean
