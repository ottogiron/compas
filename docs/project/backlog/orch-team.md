# Compas Team — Multi-Operator, Cost Tracking & Company Scale

Status: Deferred
Owner: otto
Created: 2026-03-14

## Scope Summary

- Add cost tracking per execution (token counts, USD estimates, budget visibility)
- Introduce operator identity on dispatches and messages for multi-user coordination
- Extend HTTP API with per-user authentication for concurrent operator access
- Build audit log for operator accountability and team coordination
- Add operator-scoped dashboard views (my work vs all work)
- Support multiple project contexts from a single orchestrator instance
- Implement budget controls with alerts and auto-pause

## Ticket ORCH-TEAM-1 — Cost Tracking per Execution

- Goal: Parse token counts and cost from backend output, store per-execution, and surface aggregated cost data in the dashboard and via MCP tools.
- In scope:
  - Parse `input_tokens` / `output_tokens` from Claude JSON output
  - Parse token counts from Codex JSONL and Gemini JSON where available
  - Add `input_tokens`, `output_tokens`, `cost_usd` columns to `executions` table
  - Cost estimation using configurable per-model token pricing table
  - New MCP tool `orch_cost` for querying cost by batch, agent, time range
  - Dashboard: show cost per execution in History tab, aggregate cost in Agents tab
  - Dashboard: cost summary widget in Ops tab (session total, daily, monthly)
- Out of scope:
  - Budget enforcement (separate ticket: ORCH-TEAM-7)
  - Real-time cost streaming during execution
  - Third-party billing integration
- Dependencies: None
- Acceptance criteria:
  - Claude executions have token counts and estimated cost stored after completion
  - `orch_cost` MCP tool returns cost breakdown by batch, agent, or time range
  - Dashboard History tab shows cost per execution row
  - Dashboard Agents tab shows aggregate cost per agent
  - Cost estimation is configurable via pricing table in config.yaml
- Verification:
  - Integration test: stub backend returns JSON with token fields, verify stored in DB
  - Manual: run Claude execution, verify cost appears in dashboard and MCP tool
- Status: Todo

## Ticket ORCH-TEAM-2 — Operator Identity on Messages

- Goal: Associate every dispatch and lifecycle action with an operator identity so multi-user attribution is possible.
- In scope:
  - Add `operator_id` column to `messages` table
  - MCP session binds to operator identity via config (`operator.id` in config.yaml) or environment variable (`COMPAS_OPERATOR_ID`)
  - All dispatches, close, abandon, and reopen actions carry operator identity
  - Dashboard shows operator identity on messages in conversation view
  - `orch_session_info` MCP tool returns current operator identity
  - Default operator identity when not configured (e.g., `default` or hostname-based)
- Out of scope:
  - Authentication/authorization (separate ticket: ORCH-TEAM-3)
  - Operator registration or management UI
  - Per-operator permissions
- Dependencies: None
- Acceptance criteria:
  - Messages in the database carry `operator_id` from the dispatching session
  - Different MCP sessions with different `operator.id` config produce distinct operator IDs
  - Dashboard conversation view shows who dispatched each message
  - Backward compatible: existing messages without operator_id display as `unknown`
- Verification:
  - Integration test: dispatch from two sessions with different operator IDs, verify messages are attributed correctly
  - Manual: configure operator ID, dispatch, verify in transcript and dashboard
- Status: Todo

## Ticket ORCH-TEAM-3 — Multi-Operator HTTP Access

- Goal: Extend the HTTP API layer (from ORCH-EVO-8) with per-user API keys so multiple operators can access the same orchestrator instance concurrently.
- In scope:
  - API key management: generate, list, revoke keys per operator
  - Each API key maps to an operator identity (from ORCH-TEAM-2)
  - HTTP requests authenticated via `Authorization: Bearer <api-key>` header
  - API keys stored in SQLite (hashed) with creation timestamp and last-used tracking
  - CLI commands: `compas api-key create <operator-id>`, `api-key list`, `api-key revoke`
  - Rate limiting per API key (configurable)
- Out of scope:
  - Role-based access control (e.g., read-only vs admin)
  - OAuth/SSO integration
  - Session management (stateless API keys only)
- Dependencies: MFE-2 (HTTP API layer, multi-frontend.md), ORCH-TEAM-2 (operator identity)
- Acceptance criteria:
  - Multiple operators can concurrently dispatch and monitor via HTTP API
  - Each operator's actions are attributed to their identity
  - Invalid/revoked API keys are rejected with 401
  - Rate limiting prevents abuse from any single key
- Verification:
  - Integration test: two API keys dispatch concurrently, verify both succeed with correct attribution
  - Manual: generate key, use from curl, verify dispatch works and identity is recorded
- Status: Todo

## Ticket ORCH-TEAM-4 — Activity Feed / Audit Log

- Goal: Log all operator actions with identity, timestamp, and context into a queryable audit trail for team coordination and accountability.
- In scope:
  - `audit_log` table: timestamp, operator_id, action (dispatch, close, abandon, reopen, config_change), target (thread_id, batch_id), details (JSON)
  - Automatic recording from MCP handlers and lifecycle service
  - New MCP tool `orch_audit_log` for querying by operator, action type, time range
  - Dashboard: activity feed view showing recent operator actions across all sessions
  - Retention policy: configurable max age or max entries
- Out of scope:
  - Real-time streaming of audit events (use event broadcast from EVO-2)
  - Compliance-grade audit (immutable log, tamper detection)
  - Export to external audit systems
- Dependencies: ORCH-TEAM-2 (operator identity)
- Acceptance criteria:
  - All dispatch, close, abandon, and reopen actions are logged with operator identity
  - `orch_audit_log` returns filtered audit entries
  - Dashboard shows recent activity feed
  - Audit entries survive process restart (persisted in SQLite)
- Verification:
  - Integration test: perform various actions, query audit log, verify all are recorded
  - Manual: dispatch and close from dashboard, verify audit log shows actions with identity
- Status: Todo

## Ticket ORCH-TEAM-5 — Operator-Scoped Dashboard Views

- Goal: Allow operators to filter dashboard views to show only their own work or all work, improving focus in multi-operator environments.
- In scope:
  - Dashboard operator identity (configured via CLI flag `--operator-id` or config)
  - Toggle in Ops tab: "My dispatches" vs "All" (keyboard shortcut)
  - Filter History tab by operator
  - Filter batch views by operator
  - Persist filter preference across dashboard restarts (local config)
  - Show operator badge/indicator in status bar
- Out of scope:
  - Per-operator customization of dashboard layout or theme
  - Hiding other operators' data (this is a filter, not an access control)
- Dependencies: ORCH-TEAM-2 (operator identity on messages)
- Acceptance criteria:
  - "My dispatches" filter shows only threads/batches initiated by the current operator
  - Toggle between filtered and unfiltered views is instant
  - Filter state persists across dashboard restarts
  - Unfiltered view shows all work with operator attribution
- Verification:
  - Manual: configure operator ID, dispatch some work, toggle filter, verify correct filtering
  - Manual: two operators dispatch work, each sees only their own in filtered mode
- Status: Todo

## Ticket ORCH-TEAM-6 — Multi-Project Support

- Status: Extracted to `docs/project/backlog/multi-project.md` (batch MPR, 2026-03-20)
- See: MPR-1 through MPR-4 for the revised overlay-based design and sub-tickets.

## Ticket ORCH-TEAM-7 — Budget Controls

- Goal: Implement per-project and per-batch spending limits with alerts and automatic dispatch pausing when budgets are exceeded.
- In scope:
  - Budget configuration per project and per batch:

    ```yaml
    projects:
      - id: aster
        budget:
          daily_limit_usd: 50.0
          monthly_limit_usd: 500.0
          alert_threshold_pct: 80
    ```

  - Budget checks before dispatch: reject new dispatches when budget exceeded
  - Alert events (via event broadcast from EVO-2) when approaching threshold
  - Webhook/notification integration for budget alerts (via EVO-10)
  - Dashboard: budget usage bar in Ops tab per project
  - MCP tool `orch_budget` for querying current spend vs limits
  - Override option: `--force` flag to bypass budget for critical work
- Out of scope:
  - Billing/invoicing
  - Per-operator budget limits
  - Historical budget reporting (use cost tracking from TEAM-1)
- Dependencies: ORCH-TEAM-1 (cost tracking), ORCH-EVO-2 (event broadcast for alerts)
- Acceptance criteria:
  - Dispatches are rejected with clear error when budget is exceeded
  - Alert events fire at configurable threshold percentage
  - Budget override with `--force` works for operators
  - Dashboard shows budget usage with visual indicator
  - Budget resets daily/monthly according to config
- Verification:
  - Integration test: set low budget, run executions until exceeded, verify rejection
  - Manual: configure budget, run work, verify alerts and dashboard display
- Status: Todo

## Execution Order

All tickets deferred. Multi-project support (formerly ORCH-TEAM-6) extracted to `multi-project.md` (batch MPR).

If this batch is revisited:
1. ORCH-TEAM-1 (Cost Tracking)
2. ORCH-TEAM-2 (Operator Identity)
3. ORCH-TEAM-4 (Audit Log — builds on identity)
4. ORCH-TEAM-5 (Scoped Views — builds on identity)
5. ORCH-TEAM-3 (Multi-Operator HTTP — builds on MFE-2 + identity)
6. ORCH-TEAM-7 (Budget Controls — builds on cost tracking)

## Tracking Notes

- Batch deferred (2026-03-20): team-scale features have no value for a solo developer. ORCH-TEAM-6 (multi-project) extracted to its own batch (`multi-project.md`) as it has immediate solo-developer value.
- Backlog-first governance applies.
- Implementation commits should reference ticket IDs.
- All work happens in the `ottogiron/compas` standalone repo.
- This batch targets the "small AI lab with 2-5 orch devs" scale. Revisit when that becomes reality.

## Execution Metrics

- Ticket: ORCH-TEAM-1
- Owner: TBD
- Complexity: M
- Risk: Low
- Start:
- End:
- Duration:
- Notes:

- Ticket: ORCH-TEAM-2
- Owner: TBD
- Complexity: M
- Risk: Low
- Start:
- End:
- Duration:
- Notes:

- Ticket: ORCH-TEAM-3
- Owner: TBD
- Complexity: L
- Risk: Medium
- Start:
- End:
- Duration:
- Notes:

- Ticket: ORCH-TEAM-4
- Owner: TBD
- Complexity: M
- Risk: Low
- Start:
- End:
- Duration:
- Notes:

- Ticket: ORCH-TEAM-5
- Owner: TBD
- Complexity: M
- Risk: Low
- Start:
- End:
- Duration:
- Notes:

- Ticket: ORCH-TEAM-6
- Owner: N/A
- Complexity: N/A
- Risk: N/A
- Start:
- End:
- Duration:
- Notes: Extracted to multi-project.md (MPR-1 through MPR-4)

- Ticket: ORCH-TEAM-7
- Owner: TBD
- Complexity: M
- Risk: Medium
- Start:
- End:
- Duration:
- Notes:

## Closure Evidence

- (To be filled on batch completion)
