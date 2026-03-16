# Aster-Orch Handoff Extensions (Phase 1)

Status: Active
Owner: otto
Created: 2026-03-16

## Scope Summary

- Custom handoff prompt injection (`handoff_prompt` field)
- Multi-target fan-out via batch-linked threads (`HandoffTarget` enum)
- `--await-chain` CLI wait flag for chain settlement
- Updated docs: ADR, README, architecture, example config

## Ticket ORCH-HANDOFF-1 — Custom Handoff Prompt

- Goal: Add `handoff_prompt` field to `HandoffConfig`. When set, prepend it to the auto-generated handoff message body before the "Original dispatch" / "Reply from" sections.
- In scope:
  - `src/config/types.rs`: Add `handoff_prompt: Option<String>` to `HandoffConfig` (now 3 fields)
  - `src/worker/loop_runner.rs`: In `maybe_auto_handoff()`, when building the handoff message body, prepend `handoff_prompt` if present, followed by a blank line separator, then the existing auto-generated context
  - Config validation: `handoff_prompt` is optional, no validation needed beyond serde
  - Integration test: agent with `handoff_prompt` set, verify the handoff message body starts with the custom prompt
  - Unit test: verify body composition order (prompt → separator → original dispatch → reply)
  - `examples/config-generic.yaml`: Add `handoff_prompt` field to the agent config example
- Out of scope:
  - Template variables in the prompt (`{{agent_reply}}` etc.)
  - Per-target prompts (same prompt for all targets)
- Dependencies: None
- Acceptance criteria:
  - Handoff message body starts with `handoff_prompt` content when configured
  - Without `handoff_prompt`, behavior is unchanged
  - `make verify` passes
- Verification:
  - `make verify` passes
  - Integration test: dispatch to agent with `handoff_prompt`, verify handoff message body
- Status: In Progress

## Ticket ORCH-HANDOFF-2 — Multi-Target Fan-Out

- Goal: Support `on_response` as either a single agent alias or a list of aliases. When a list, create separate threads per target linked by batch ID.
- In scope:
  - `src/config/types.rs`: Replace `on_response: Option<String>` with `on_response: Option<HandoffTarget>` where:

    ```rust
    #[derive(Debug, Clone, Deserialize, Serialize)]
    #[serde(untagged)]
    pub enum HandoffTarget {
        Single(String),
        FanOut(Vec<String>),
    }
    ```

  - `src/config/validation.rs`: Validate all targets in `FanOut` are valid agent aliases or "operator". Validate no self-loops. Validate no duplicates. At least 1 target (single-element FanOut degrades to Single behavior). Do NOT require minimum 2.
  - `src/worker/loop_runner.rs`: In `maybe_auto_handoff()`, branch on `Single` vs `FanOut`:
    - `Single`: existing behavior (insert handoff message on same thread)
    - `FanOut` with 1 target: degrade to Single behavior (same-thread handoff)
    - `FanOut` with 2+ targets: for each target, create a new thread with a shared batch ID. Batch ID scheme: inherit from originating thread if exists, else generate `fanout-{thread_id}`. Insert a handoff message on each new thread.
  - Fan-out threads start at depth 0 (independent chains, independent depth counters). Total fan is managed by `max_chain_depth` on the downstream agents.
  - `src/store/mod.rs`: Add `insert_fanout_handoffs()` method that creates N threads + N handoff messages in a single transaction, all sharing a batch ID
  - Config validation tests for `HandoffTarget` enum
  - Integration tests: fan-out creates N threads, each with correct handoff message, each linked by batch
- Out of scope:
  - Join/aggregation mechanism (operator uses `orch_batch_status`)
  - Per-target custom prompts (all targets get the same `handoff_prompt`)
  - Round-robin routing
- Dependencies: ORCH-HANDOFF-1 (custom prompt applies to fan-out messages too)
- Acceptance criteria:
  - `on_response: reviewer` still works (Single variant, backward compatible)
  - `on_response: [reviewer, reviewer-2]` creates 2 new threads with shared batch ID
  - Each fan-out thread has a handoff message with the original context + custom prompt
  - `serde(untagged)` correctly deserializes both string and list YAML
  - `make verify` passes
- Verification:
  - `make verify` passes
  - Integration test: fan-out dispatch, verify thread count, batch linkage, message content
  - Config test: YAML roundtrip for both Single and FanOut variants
- Status: Todo

## Ticket ORCH-HANDOFF-3 — `--await-chain` CLI Wait Flag

- Goal: Add `--await-chain` flag to `aster_orch wait` that keeps polling until the thread has no active/queued executions and there's a non-trigger reply after the last handoff message.
- In scope:
  - `src/bin/aster_orch.rs`: Add `--await-chain` boolean flag to the wait subcommand
  - `src/wait.rs`: When `await_chain` is true, after finding a matching reply, additionally check for pending work on the thread. Only return when pending_work == 0 AND a non-trigger reply exists. Pending work = active executions + untriggered handoff messages (closes the race window where handoff message is inserted but execution not yet enqueued).
  - `src/store/mod.rs`: Add `count_pending_chain_work(thread_id)` method — single query that counts active executions (status in queued/picked_up/executing) PLUS untriggered handoff messages (handoff intent messages with no linked execution):

    ```sql
    SELECT
      (SELECT COUNT(*) FROM executions
       WHERE thread_id = ? AND status IN ('queued', 'picked_up', 'executing')) +
      (SELECT COUNT(*) FROM messages m
       WHERE m.thread_id = ? AND m.intent = 'handoff'
       AND NOT EXISTS (
         SELECT 1 FROM executions e WHERE e.dispatch_message_id = m.id
       ))
    AS pending_work

    ```

  - When chain hits `max_chain_depth` and forces operator review, `--await-chain` returns naturally — the `review-request` message is a non-trigger reply and no further executions are queued
  - Integration tests: await-chain waits through handoff, returns final reply; await-chain returns on depth-limit pause
- Out of scope:
  - `--batch <id>` wait (Phase 2)
  - Fan-out awareness (Phase 2 batch wait covers this)
- Dependencies: None (works with existing single-target chains; fan-out adds value later)
- Acceptance criteria:
  - `aster_orch wait --thread-id X --await-chain` returns the reviewer's reply, not the implementer's reply
  - Without `--await-chain`, behavior is unchanged (returns first non-trigger reply)
  - Chain depth-limit pauses return the escalation message
  - `make verify` passes
- Verification:
  - `make verify` passes
  - Integration test: dispatch → agent completes → handoff → reviewer completes → await-chain returns reviewer's reply
  - Integration test: chain hits depth limit → await-chain returns escalation message
- Status: Todo

## Ticket ORCH-HANDOFF-4 — Update Documentation

- Goal: Update all docs for the handoff extensions: custom prompt, fan-out, await-chain.
- In scope:
  - `README.md`:
    - Update "Auto-Handoff Chains" section with custom prompt example
    - Add fan-out example (`on_response: [reviewer-1, reviewer-2]`)
    - Document `--await-chain` in the CLI wait section
    - Update configuration reference with `handoff_prompt` field
  - `docs/project/architecture.md`:
    - Update dispatch flow for fan-out (creates batch-linked threads)
    - Add fan-out to key design decisions
  - `docs/project/DECISIONS.md`:
    - Amend ADR-014 with fan-out design (batch-linked threads, not same-thread)
    - Add note about `handoff_prompt` composition order
  - `examples/config-generic.yaml`:
    - Add `handoff_prompt` example
    - Add fan-out example
  - `AGENTS.md`:
    - Update if any handoff references are affected
- Out of scope:
  - Phase 2 features (`on_error`, batch wait, `orch_resume`)
- Dependencies: ORCH-HANDOFF-1, ORCH-HANDOFF-2, ORCH-HANDOFF-3
- Acceptance criteria:
  - Fan-out, custom prompt, and await-chain documented in README
  - ADR-014 amended
  - Example config shows all new fields
  - `make verify` passes (lint-md clean)
- Verification:
  - `make verify` passes
  - Grep for stale references
- Status: Todo

## Execution Order

1. ORCH-HANDOFF-1 (custom prompt — smallest, unblocks HANDOFF-2)
2. ORCH-HANDOFF-3 (`--await-chain` — independent, can parallel with HANDOFF-2)
3. ORCH-HANDOFF-2 (fan-out — depends on HANDOFF-1 for prompt injection)
4. ORCH-HANDOFF-4 (docs — after all code is merged)

Parallelization: HANDOFF-1 and HANDOFF-3 can be dispatched in parallel (different files). HANDOFF-2 depends on HANDOFF-1. HANDOFF-4 depends on all code tickets.

## Tracking Notes

- Architect design review: thread 01KKWBP6XNV2MQE5NBNQ29KPV5 (multi-target + custom prompt + wait)
- Key design decision: fan-out via batch-linked threads, NOT same-thread parallel execution
- `HandoffTarget` uses `serde(untagged)` for backward-compatible YAML (string or list)
- `handoff_prompt` composition: custom prompt → blank line → auto-generated context
- `--await-chain` checks: no active executions + non-trigger reply after last handoff
- Phase 2 deferred: `on_error` routing, `--batch` wait, `orch_resume` tool

## Execution Metrics

- Ticket: ORCH-HANDOFF-1
- Owner: TBD
- Complexity: S
- Risk: Low
- Start:
- End:
- Duration:
- Notes:

- Start: 2026-03-16 22:41 UTC

- Ticket: ORCH-HANDOFF-2
- Owner: TBD
- Complexity: M
- Risk: Medium
- Start:
- End:
- Duration:
- Notes:

- Ticket: ORCH-HANDOFF-3
- Owner: TBD
- Complexity: S
- Risk: Low
- Start:
- End:
- Duration:
- Notes:

- Ticket: ORCH-HANDOFF-4
- Owner: TBD
- Complexity: S
- Risk: Low
- Start:
- End:
- Duration:
- Notes:

## Closure Evidence

- (To be filled on batch completion)
