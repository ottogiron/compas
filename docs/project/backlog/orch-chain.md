# Aster-Orch Handoff Chains (Phase 1)

Status: Closed
Owner: otto
Created: 2026-03-16

## Scope Summary

- Config-driven auto-chains: agents hand off to other agents based on reply intent
- `handoff` section on agent config with `on_<intent>: <target>` routing
- `max_chain_depth` safety limit with forced operator escalation
- Forward-compatible schema (serde untagged enum for future gated handoffs)
- `escalation` intent for agent-to-operator "help" signals

## Ticket ORCH-CHAIN-1 — Config Schema and Handoff Routing

- Goal: Add `handoff` config section to agent config and implement auto-chain routing in the worker.
- In scope:
  - Add `HandoffConfig` struct to `src/config/types.rs`:

    ```rust
    #[derive(Deserialize)]
    pub struct HandoffConfig {
        pub on_success: Option<HandoffTarget>,
        pub on_review_request: Option<HandoffTarget>,
        pub on_response: Option<HandoffTarget>,
        pub on_changes_requested: Option<HandoffTarget>,
        pub on_escalation: Option<HandoffTarget>,
        pub max_chain_depth: Option<u32>,  // default 3
    }
    
    #[derive(Deserialize)]
    #[serde(untagged)]
    pub enum HandoffTarget {
        Simple(String),
        Gated { target: String, gate: String, gate_timeout_secs: Option<u64> },
    }
    ```

  - Add `handoff: Option<HandoffConfig>` to `AgentConfig`
  - Config validation: reject `Gated` variant with clear error ("gate conditions are not yet supported")
  - Validate handoff targets reference valid agent aliases or "operator"
  - Validate `max_chain_depth` is between 1 and 20
  - In `handle_trigger_output` (src/worker/loop_runner.rs): after inserting the reply message on success, check if the agent has a matching `handoff.on_<reply_intent>` route
  - If target is another agent (not "operator"), insert a new message with `from: <current_agent>, to: <target_agent>, intent: handoff, body: <context>`
  - The handoff message body should include the previous agent's output and chain context
  - Track chain depth by counting `handoff`-intent messages in the thread
  - If chain depth >= `max_chain_depth`, route to operator with `intent: review-request` and a body explaining the chain was interrupted
  - "operator" as a target means "stop the chain" — no message inserted, operator decides
  - Unrecognized/unmapped intents default to operator (safe fallback)
  - Add `escalation` as a recognized intent in the intent parsing logic
- Out of scope:
  - Gated handoffs (Phase 2)
  - `aster_orch signal` CLI command (Phase 1.5)
  - Thread dependencies (EVO-14)
  - Parallel fan-out (single target only)
  - Cross-thread handoffs
- Dependencies: None
- Acceptance criteria:
  - Agent with `handoff.on_response: reviewer` auto-dispatches to reviewer on completion
  - Chain `dev -> reviewer -> dev -> reviewer -> operator` works with correct routing
  - `max_chain_depth` stops the chain and escalates to operator with explanation
  - Unrecognized intents route to operator
  - `Gated` config variant is rejected at validation time with clear error
  - Handoff targets that don't match a valid agent alias are rejected at validation
  - `make verify` passes
- Verification:
  - `make verify` passes
  - Integration tests: basic chain, loop prevention at max_depth, escalation to operator
  - Config validation tests: gated rejection, invalid target, depth bounds
- Status: Done

## Ticket ORCH-CHAIN-2 — Diagnostics and Dashboard Integration

- Goal: Add chain visibility to orch_diagnose and the dashboard. Ensure handoff events are visible in the conversation view and Ops tab.
- In scope:
  - `orch_diagnose`: report chain state — current depth, agents involved, route taken, whether approaching `max_chain_depth`
  - Dashboard conversation view: handoff messages render with clear visual indicator (e.g., "orch-dev -> reviewer [handoff]" header)
  - Dashboard Ops tab: running chains show chain context in inline detail row
  - `orch_transcript`: handoff messages appear naturally (no changes needed — they're already messages)
  - Event bus: `MessageReceived` events already fire for handoff messages (verify this works)
  - Desktop notifications: handoff completions fire `ExecutionCompleted` events (verify)
- Out of scope:
  - New MCP tools for chain management
  - Dashboard chain visualization (graph/diagram)
  - Chain-level metrics
- Dependencies: ORCH-CHAIN-1 (handoff messages must exist)
- Acceptance criteria:
  - `orch_diagnose` on a thread with handoff chain shows depth, agents, and depth warning if approaching limit
  - Conversation view renders handoff messages distinctly
  - `make verify` passes
- Verification:
  - `make verify` passes
  - Manual: run a handoff chain, view in dashboard conversation view, check orch_diagnose output
- Status: Done

## Execution Order

1. ORCH-CHAIN-1 (config schema + worker routing — the core feature)
2. ORCH-CHAIN-2 (diagnostics + dashboard — builds on CHAIN-1)

## Tracking Notes

- Architect design review: thread 01KKVYNZHQCTPQ6RBE5KQJ1BPW (both initial proposal and wait follow-up)
- Phase 1 only — no gated handoffs, no signal CLI, no thread dependencies
- Forward-compatible schema: `HandoffTarget::Gated` parsed but rejected in Phase 1
- Default `max_chain_depth`: 3 (conservative, per architect recommendation)
- Key AX principle: every auto-handoff visible as explicit message in transcript
- `escalation` intent: "I need help" vs `review-request`: "please review my work"
- Phase 1.5 (`aster_orch signal` CLI) is independent and can be a separate ticket later

## Execution Metrics

- Ticket: ORCH-CHAIN-1
- Owner: TBD
- Complexity: M
- Risk: Medium
- Start:
- End:
- Duration:
- Notes:

- Ticket: ORCH-CHAIN-2
- Owner: TBD
- Complexity: S
- Risk: Low
- Start:
- End:
- Duration:
- Notes:

## Closure Evidence

- (To be filled on batch completion)
