# Execution Observability

Status: Active
Owner: operator
Created: 2026-03-21

## Scope Summary

- Enrich all backend telemetry parsers with cost/token extraction and tool event capture
- Add store aggregation queries for tool-level metrics and execution cost
- Surface tool stats via new MCP tool and cost overview in orch_metrics

## Spike Results (2026-03-21)

Captured raw stream-json from all four backends. Key findings:

**Claude** (`--output-format stream-json --verbose`):

- Tool results are `"type":"user"` events with `content[].type: "tool_result"` — not a separate event type
- `result` event has: `total_cost_usd`, `usage.input_tokens`, `output_tokens`, `cache_read_input_tokens`, `cache_creation_input_tokens`, `num_turns`, `duration_ms`, `modelUsage`

**Codex** (`codex exec --json`):

- Events: `thread.started`, `turn.started`, `item.started`, `item.completed`, `turn.completed`
- Tool results: `item.completed` with `type: "command_execution"`, `exit_code`, `aggregated_output`
- `turn.completed` has: `usage.input_tokens`, `output_tokens`, `cached_input_tokens`. No cost_usd.

**Gemini** (`--output-format stream-json`):

- Events: `init`, `message` (user/assistant), `result`
- `result.stats` has: `total_tokens`, `input_tokens`, `output_tokens`, `cached`, `duration_ms`, `tool_calls`. No cost_usd.

**OpenCode** (`--format json`):

- Events: `{ type, timestamp, sessionID, ... }` format. Rate-limited during spike, incomplete capture.
- Has `opencode stats` for usage data but unclear if inline token counts are emitted per-turn.

## Ticket OBS-01 — Enrich backend telemetry

- Goal: Extract cost/token data and tool events from all backends that provide them; add tool_name column to execution_events
- In scope:
  - **Schema migrations:**
    - `ALTER TABLE executions ADD COLUMN cost_usd REAL`
    - `ALTER TABLE executions ADD COLUMN tokens_in INTEGER`
    - `ALTER TABLE executions ADD COLUMN tokens_out INTEGER`
    - `ALTER TABLE executions ADD COLUMN num_turns INTEGER`
    - `ALTER TABLE execution_events ADD COLUMN tool_name TEXT`
  - **BackendOutput** (`backend/mod.rs`): add optional fields `cost_usd: Option<f64>`, `tokens_in: Option<i64>`, `tokens_out: Option<i64>`, `num_turns: Option<i32>`
  - **Store** (`store/mod.rs`): extend `complete_execution()` and `fail_execution()` to persist new fields
  - **Executor** (`executor.rs`): pass cost data from BackendOutput through to store calls
  - **Claude** (`claude.rs`):
    - Extract `total_cost_usd`, `usage.input_tokens`, `usage.output_tokens`, `num_turns` from `result` event in `extract_claude_stream_output()`
    - Parse `"type":"user"` events with `content[].type: "tool_result"` as tool_result execution events
    - Populate `tool_name` on tool_call events from `tool_use.name` field
  - **Codex** (`codex.rs`):
    - Implement `parse_codex_stream_line()` — parse `item.started`/`item.completed` as tool_call/tool_result events with `tool_name`
    - Extract `usage.input_tokens`, `output_tokens` from `turn.completed` event
    - No cost_usd available (populate as None)
  - **Gemini** (`gemini.rs`):
    - Implement `parse_gemini_stream_line()` — parse `message` events
    - Extract `stats.input_tokens`, `output_tokens`, `duration_ms`, `tool_calls` from `result` event
    - No cost_usd available (populate as None)
  - **OpenCode** (`opencode.rs`):
    - Implement `parse_opencode_stream_line()` — best-effort based on available format documentation
    - Extract token counts if available in output events
    - If format is insufficiently documented, implement skeleton parser that captures what it can and logs warnings for unknown events
- Out of scope:
  - Dashboard changes
  - Aggregation queries (OBS-02)
  - Changes to `consume_telemetry` batching logic (existing pipeline handles all backends)
- Dependencies: None.
- Acceptance criteria:
  - After a Claude execution, executions row has `cost_usd`, `tokens_in`, `tokens_out`, `num_turns` populated
  - After a Codex execution, executions row has `tokens_in`, `tokens_out` populated (cost_usd NULL)
  - After a Gemini execution, executions row has `tokens_in`, `tokens_out` populated (cost_usd NULL)
  - `execution_events` rows for tool_call events have `tool_name` populated across all backends
  - Claude tool_result events appear in `orch_execution_events`
  - Codex `item.completed` (command_execution) events appear as tool_result events with exit_code in summary
  - All new columns are nullable — old executions unbroken
  - Existing events unaffected
  - `make verify` passes
- Verification:
  - `make verify`
  - Dispatch to Claude agent → verify cost/token columns populated
  - Dispatch to Codex agent → verify token columns populated, tool events captured
  - `orch_execution_events` shows tool_name on tool_call events for each backend
- Status: Done

## Ticket OBS-02 — Tool metrics aggregation queries

- Goal: Add store methods that aggregate execution_events into per-tool and per-execution-cost metrics
- In scope:
  - `tool_call_counts(agent_alias: Option<&str>) -> Vec<ToolCallStat>` — per-tool call count using `tool_name` column, optional agent filter
  - `tool_error_rates(agent_alias: Option<&str>) -> Vec<ToolCallStat>` — per-tool error rate from tool_result events; degrade gracefully if tool_result events don't exist for a backend
  - `tool_usage_by_agent() -> Vec<(agent_alias, tool_name, count)>` — join execution_events with executions
  - `cost_summary(agent_alias: Option<&str>) -> CostSummary` — total and avg cost_usd, total tokens_in/tokens_out, execution count, optional agent filter
  - `cost_by_agent() -> Vec<AgentCostSummary>` — cost and token breakdown per agent
  - Struct definitions: `ToolCallStat { tool_name, call_count, error_count, error_rate }`, `CostSummary { total_cost_usd, avg_cost_usd, total_tokens_in, total_tokens_out, execution_count }`, `AgentCostSummary { agent_alias, total_cost_usd, total_tokens_in, total_tokens_out, execution_count }`
  - All methods return empty/zero results when no data exists
- Out of scope:
  - MCP tool surface (OBS-03)
  - Time-range filtering
  - Dashboard visualization
- Dependencies: OBS-01.
- Acceptance criteria:
  - `tool_call_counts(None)` returns correct counts across all executions
  - `tool_call_counts(Some("agent"))` filters correctly
  - `cost_summary(None)` returns correct totals including token sums
  - `cost_by_agent()` groups correctly, handles agents with no cost data (cost_usd NULL)
  - Integration tests cover each method with seeded test data
  - `make verify` passes
- Verification:
  - `make verify`
  - Integration tests with seeded execution_events and executions data
- Status: Todo

## Ticket OBS-03 — Surface metrics via MCP

- Goal: Expose tool and cost aggregations via a new `orch_tool_stats` MCP tool and add cost overview to `orch_metrics`
- In scope:
  - New `orch_tool_stats` MCP tool with input `{ agent_alias?: string }`, returning `{ tool_stats: [ToolCallStat], cost_by_agent: [AgentCostSummary] }`
  - Add `cost` section to `orch_metrics` response: `{ total_cost_usd, total_tokens_in, total_tokens_out, executions_with_cost }`
  - Tool registration in `server.rs`, handler in `query.rs`, params in `params.rs`
  - Tool description follows AX principles (clear input/output documentation)
- Out of scope:
  - Dashboard visualization
  - Time-range filtering
  - CSV/export formats
- Dependencies: OBS-02.
- Acceptance criteria:
  - `orch_tool_stats` returns correct tool counts and error rates
  - `orch_tool_stats(agent_alias="worker")` filters correctly
  - `orch_metrics` includes a `cost` section with totals
  - Both tools return valid JSON with empty/zero values when no data exists
  - `make verify` passes
- Verification:
  - `make verify`
  - Call `orch_tool_stats` after executions across multiple backends and verify tool counts
  - Call `orch_metrics` and verify cost section present
  - Test with no execution data — returns zeros, not errors
- Status: Todo

## Execution Order

1. OBS-01
2. OBS-02
3. OBS-03

## Tracking Notes

- Backlog-first governance applies.
- Implementation commits should reference ticket IDs.
- Record scope changes/deferrals here.
- Origin: claude-cookbooks review (2026-03-21) — architect recommended tool metrics + cost tracking as Phase 1.
- Spike completed 2026-03-21: all four backends have parseable telemetry. Claude has cost_usd; Codex/Gemini have tokens only; OpenCode needs further investigation.
- OpenCode parser is best-effort due to incomplete format documentation during spike (rate-limited).
- Known limitation (OBS-01): Claude `tool_result` events have `tool_name: None` because the stream format only contains `tool_use_id`, not the tool name. Backfilling would require a stateful `tool_use_id → tool_name` map in `consume_telemetry`. Deferred — tool_name is populated on `tool_call` events which is the primary use case for tool metrics.
- Known limitation (OBS-01): Gemini uses `--output-format json` (not stream-json), so per-turn tool events are not captured. Token counts are extracted from the final result stats block.

## Execution Metrics

- Ticket: OBS-01
- Owner: TBD
- Complexity: L
- Risk: Medium
- Start: 2026-03-21 14:28 UTC
- End: TBD
- Duration: TBD
- Notes: Four backend parsers to implement/extend. Claude is richest (cost + tokens + tool events). Codex/Gemini have tokens. OpenCode is best-effort. Stream-json formats are undocumented and may change.

- Ticket: OBS-02
- Owner: TBD
- Complexity: S
- Risk: Low
- Start: TBD
- End: TBD
- Duration: TBD
- Notes: SQL aggregation using tool_name column from OBS-01; handles backends with partial data (NULL cost_usd)

- Ticket: OBS-03
- Owner: TBD
- Complexity: S
- Risk: Low
- Start: TBD
- End: TBD
- Duration: TBD
- Notes: New orch_tool_stats tool + cost section in orch_metrics

## Closure Evidence

- TBD
