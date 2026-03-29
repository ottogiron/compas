# Prompt Optimization

Status: Active
Owner: operator
Created: 2026-03-29

## Scope Summary

- Per-task prompt context injection into agent system prompts at dispatch time
- Content-addressable prompt storage for version correlation
- Composite outcome scoring from existing execution telemetry
- OPRO-lite prompt optimizer (LLM-as-judge suggestions, operator-gated)

## Ticket OPTIM-1 — Per-task prompt context injection + prompt storage

- Goal: Enable injecting per-dispatch system prompt context and store full prompt text for correlation.
- In scope:
  - Add `prompt_context: Option<String>` to `DispatchParams` (MCP param)
  - Add `prompt_context` column to `messages` table (ALTER TABLE migration)
  - Add `prompts` table: `(hash TEXT PK, text TEXT, source TEXT, created_at INTEGER)` for content-addressable prompt storage
  - Thread `prompt_context` through dispatch handler to `insert_dispatch_message()`
  - Compose effective prompt at execution time: `base_prompt + "\n\n---\n\n" + prompt_context`
  - Defer `prompt_hash` computation from enqueue time to execution time (hash the composed prompt)
  - Upsert composed prompt text into `prompts` table at execution time
  - Propagate `prompt_context` through single handoffs and fan-out handoffs
  - Validate `prompt_context` size limit (10KB) at dispatch time
  - Add `orch_prompt_history` MCP tool: list prompts by hash with execution counts
  - Add `Store::upsert_prompt()`, `Store::get_prompt()`, `Store::prompt_history()` queries
- Out of scope:
  - Prompt templating or variable substitution
  - Automatic prompt changes (optimizer is Phase 3)
  - Dashboard prompt visibility (future ticket)
- Dependencies: None.
- Acceptance criteria:
  - `orch_dispatch` accepts optional `prompt_context` parameter
  - Dispatching with `prompt_context` composes it with agent's base prompt as the `--system-prompt` value
  - `prompt_hash` on execution reflects the composed prompt (base + context), not just the base
  - Full prompt text stored in `prompts` table, retrievable by hash
  - Handoff messages carry `prompt_context` from originating dispatch
  - `orch_prompt_history` returns prompt hashes with execution counts and outcome summaries
  - Dispatching without `prompt_context` behaves identically to today (backward compatible)
  - `prompt_context` > 10KB rejected with diagnostic error
- Verification:
  - `make verify` passes
  - Integration test: dispatch with `prompt_context`, verify composed prompt hash differs from base-only hash
  - Integration test: dispatch without `prompt_context`, verify backward-compatible behavior
  - Manual: dispatch via MCP, check execution log shows composed `--system-prompt`
- Status: In Progress

## Ticket OPTIM-3 — Outcome scoring engine

- Goal: Compute composite quality score per execution from existing telemetry signals.
- In scope:
  - Add `outcome_score REAL`, `score_components TEXT` (JSON), `scored_at INTEGER` columns to `executions` table
  - New `src/scoring.rs` module with `ScoreComponents` struct and `compute_score()` function
  - Composite formula: `success_gate(100) - cost_penalty - turn_penalty - retry_penalty + merge_bonus + review_bonus`, clamped 0-100
  - Score computation after `complete_execution()` in worker loop (initial score, no merge signals)
  - Score recomputation after merge completion in `poll_merge_ops()` (adds merge bonus)
  - Agent median cost/turn computation for normalization (from recent execution history)
  - Store queries: `compute_and_store_score()`, `recompute_score_for_thread()`
- Out of scope:
  - MCP tool exposure (OPTIM-4)
  - Dashboard visibility (OPTIM-4)
  - Configurable scoring formula (future)
  - LLM-as-judge scoring (Phase 3)
- Dependencies: OPTIM-1 (prompt_hash from composed prompts for correlation).
- Acceptance criteria:
  - Every completed execution gets an `outcome_score` value
  - Score components stored as inspectable JSON
  - Successful execution with low cost/turns scores higher than one with many retries
  - Score recomputed when merge completes (merge_bonus applied)
  - Failed executions score 0
  - Scoring does not block execution completion (best-effort, log errors)
- Verification:
  - `make verify` passes
  - Unit tests for scoring formula edge cases (zero cost, null tokens, max retries)
  - Integration test: complete execution, verify score populated
  - Integration test: complete merge, verify score recomputed with bonus
- Status: Todo

## Ticket OPTIM-4 — Score MCP tools and dashboard visibility

- Goal: Expose outcome scores via MCP tools and surface in the TUI dashboard.
- In scope:
  - Add `orch_scores` MCP tool: query scores by agent, prompt_hash, with limit
  - Add `orch_prompt_compare` MCP tool: side-by-side metrics for two prompt hashes
  - Store queries: `score_summary_by_agent()`, `score_by_prompt_hash()`, `recent_scores()`
  - Dashboard: score column in execution list view
  - Dashboard: score summary in metrics/ops footer
- Out of scope:
  - Prompt optimizer (OPTIM-5)
  - Score-based alerting
  - Historical trend visualization
- Dependencies: OPTIM-3 (scoring engine must exist).
- Acceptance criteria:
  - `orch_scores` returns per-execution scores with component breakdown
  - `orch_prompt_compare` returns side-by-side average score, cost, turns, success rate for two prompt hashes
  - Dashboard shows score column in execution list (color-coded: green >70, yellow 40-70, red <40)
  - Dashboard metrics view includes average score per agent
- Verification:
  - `make verify` passes
  - Manual: call `orch_scores` after several executions, verify output
  - Manual: call `orch_prompt_compare` with two different prompt hashes
  - Visual: dashboard shows score column and color coding
- Status: Todo

## Ticket OPTIM-5 — OPRO-lite prompt optimizer tool

- Goal: LLM-as-judge analyzes execution history and suggests prompt improvements. Operator reviews and applies.
- In scope:
  - Add `OptimizerConfig` to `OrchestrationConfig` (enabled, backend, model, history_window)
  - New `src/optimizer.rs` module with `suggest_prompt()` function
  - Meta-prompt construction: current prompt + scored execution history + transcript summaries
  - Add `orch_suggest_prompt` MCP tool: returns suggested prompt modification + rationale
  - Calls configured LLM backend for meta-prompt evaluation
  - Focus parameter: "cost", "quality", "speed" to direct optimization objective
- Out of scope:
  - Automatic prompt application (operator must manually apply suggestions)
  - A/B testing framework (manual via `prompt_context` variation)
  - Multi-objective optimization
  - Prompt versioning UI
- Dependencies: OPTIM-3 + OPTIM-4 (needs scores and comparison queries).
- Acceptance criteria:
  - `orch_suggest_prompt(agent_alias="dev")` returns coherent suggestion based on execution history
  - Suggestion includes rationale explaining what patterns it observed
  - Focus parameter changes the optimization direction (cost-focused vs quality-focused suggestions differ)
  - Tool returns error with guidance if insufficient execution history (<5 scored executions)
  - Optimizer config disabled by default; requires explicit `optimizer.enabled: true`
- Verification:
  - `make verify` passes
  - Integration test: mock execution history, verify meta-prompt construction
  - Manual: run optimizer after 10+ scored executions, verify suggestion quality
- Status: Todo

## Ticket EVO-8 — Automatic prompt modification on retry

- Goal: Inject retry-specific guidance into agent prompt when retrying after failure.
- In scope:
  - On retry, auto-generate `prompt_context` with error context: attempt number, error category, error detail summary
  - Store retry `prompt_context` on the retry execution's linked dispatch message
  - Format: "RETRY CONTEXT: Attempt #N failed. Error: {category} - {detail}. Guidance: Avoid the same failure pattern."
  - Truncate error detail to 500 chars to avoid prompt bloat
- Out of scope:
  - LLM-generated retry guidance (future, could use optimizer)
  - Retry strategy changes (backoff, max_retries unchanged)
  - Per-error-category custom guidance templates
- Dependencies: OPTIM-1 (requires prompt_context field on messages).
- Acceptance criteria:
  - Retry execution receives a `prompt_context` with error context from the failed attempt
  - Agent's `--system-prompt` includes the retry guidance on retry attempts
  - First attempt (non-retry) has no automatic `prompt_context` injection
  - `prompt_hash` differs between first attempt and retry (since prompt_context changes the composed prompt)
- Verification:
  - `make verify` passes
  - Integration test: simulate transient failure, verify retry execution has prompt_context with error info
  - Integration test: verify prompt_hash differs between attempt 1 and retry
- Status: Todo

## Execution Order

1. OPTIM-1 (foundation, all other tickets depend on this)
2. OPTIM-3 + EVO-8 (parallel — scoring touches scoring.rs+store, retry touches loop_runner retry path)
3. OPTIM-4 + OPTIM-5 (parallel — MCP tools+dashboard vs optimizer.rs+config)

## Tracking Notes

- Backlog-first governance applies.
- Implementation commits should reference ticket IDs.
- Phase 1 (OPTIM-1) is independently shippable and valuable.
- Phase 2 (OPTIM-3/4) and Phase 3 (OPTIM-5) each add value incrementally.
- EVO-8 is unblocked by OPTIM-1 and can run in parallel with OPTIM-3.

## Execution Metrics

- Ticket: OPTIM-1
- Owner:
- Complexity: L
- Risk: Medium
- Start: 2026-03-29 13:27 UTC
- Duration:
- Notes:

- Ticket: OPTIM-3
- Owner:
- Complexity: M
- Risk: Low
- Start:
- End:
- Duration:
- Notes:

- Ticket: EVO-8
- Owner:
- Complexity: S
- Risk: Low
- Start:
- End:
- Duration:
- Notes:

- Ticket: OPTIM-4
- Owner:
- Complexity: M
- Risk: Low
- Start:
- End:
- Duration:
- Notes:

- Ticket: OPTIM-5
- Owner:
- Complexity: M
- Risk: Medium
- Start:
- End:
- Duration:
- Notes:
