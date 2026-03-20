# Compas Team — Multi-Operator, Cost Tracking & Company Scale

Status: Active
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

- Goal: Allow a single compas instance to manage agents and work across multiple project repositories using a projects-as-overlays design. Agents are defined once globally; projects provide repo context and per-agent overrides.
- In scope:
  - ORCH-TEAM-6a: Config schema
  - ORCH-TEAM-6b: Dispatch + thread resolution
  - ORCH-TEAM-6c: Handoff override resolution
  - ORCH-TEAM-6d: Dashboard project grouping
- Out of scope:
  - Separate databases per project
  - Cross-project agent coordination (agent works on one project at a time)
  - Project creation/deletion from dashboard (config-only)
  - Per-project prompt overrides (only handoff, workspace, and env are overridable)
- Dependencies: None (HTTP API dependency removed — works with MCP dispatch)
- Acceptance criteria:
  - Multiple projects can be defined in config with per-project repo roots and repo lists
  - Dispatches with `project` and `repo` params run agents in the correct repo root
  - Worktree isolation works correctly for multi-repo projects (worktree created from the specific repo, not a parent dir)
  - Handoff chains can be overridden per project
  - Dashboard shows project context on threads
  - Existing single-project configs work without changes (`projects` is optional)
  - Agents can be dispatched without project context (cross-cutting, ad-hoc use)
- Verification:
  - Integration test: dispatch to two different projects, verify agents run in correct directories
  - Integration test: dispatch with project + repo, verify worktree created from correct git repo
  - Integration test: verify handoff override applies for project dispatch but agent default applies without project
  - Manual: configure two projects, dispatch to each, verify dashboard grouping
  - `make verify`
- Status: Todo

### Design: Projects as Overlays

Architecture evaluation completed by `as-architect` agent (thread `01KM6PWMC2DNQ4TT94CDZ2D2QR`, 2026-03-20).

**Core principle:** Agents are defined once in the flat `agents` list (core identity: backend, model, prompt). Projects are an optional grouping layer providing `repo_root` context and per-agent overrides for handoff and workspace only. Cross-cutting agents (pir-reviewer, architect) have no project affiliation and work globally.

**Config example:**

```yaml
target_repo_root: ~/workspace/github  # global fallback, still required
state_dir: ~/.compas/state

agents:
  - alias: implementer
    backend: claude
    model: eu.anthropic.claude-opus-4-6-v1
    workspace: worktree
    timeout_secs: 900
    handoff:
      on_response: [reviewer-opus, reviewer-codex]  # default chain
    prompt: |
      You implement changes in the target repository. ...

  - alias: analyst
    backend: codex
    prompt: ...

  - alias: reviewer-opus
    backend: claude
    prompt: ...

  - alias: reviewer-codex
    backend: codex
    prompt: ...

  - alias: architect
    backend: claude
    prompt: ...

  - alias: pir-reviewer        # cross-cutting, no project
    backend: claude
    prompt: ...

  - alias: orch-reviewer       # has its own workdir, no project needed
    backend: claude
    workdir: ~/workspace/github/ottogiron/compas
    prompt: ...

projects:
  - id: acme-platform
    repo_root: ~/workspace/github/acme-platform
    repos:                       # explicit list for validation + worktree
      - api-gateway
      - user-service
      - billing-service
      - notification-service
      - search-service
      - data-pipeline
    # No agent_overrides — default handoff applies

  - id: compas
    repo_root: ~/workspace/github/ottogiron/compas
    agent_overrides:
      implementer:
        handoff:
          on_response: orch-reviewer
          max_chain_depth: 3

  - id: pit
    repo_root: ~/workspace/github/ottogiron/pit
    agent_overrides:
      implementer:
        handoff:
          on_response: pit-reviewer
```

**Dispatch examples:**

```
# Multi-repo project: agent + project + repo
orch_dispatch(to="implementer", project="acme-platform",
              repo="api-gateway", body="...")
# → workdir: ~/workspace/github/acme-platform/api-gateway
# → handoff: [reviewer-opus, reviewer-codex] (agent default)

# Single-repo project: agent + project
orch_dispatch(to="implementer", project="compas", body="...")
# → workdir: ~/workspace/github/ottogiron/compas
# → handoff: orch-reviewer (project override)

# Cross-cutting: agent only, no project
orch_dispatch(to="pir-reviewer", body="Review the PIR at ...")
# → workdir: ~/workspace/github (global fallback)

# Agent borrowing: any agent in an ad-hoc repo
orch_dispatch(to="implementer", body="Fix script in ~/scripts/...")
# → workdir: ~/workspace/github (global fallback)
# → handoff: [reviewer-opus, reviewer-codex] (agent default)
```

**Workdir resolution order:**
1. `project.repo_root/repo` (if project + repo provided)
2. `project.repo_root` (if project provided, no repo)
3. `agent.workdir` (if set on agent)
4. `target_repo_root` (global fallback)

**Handoff resolution:** If dispatch has project context, check `project.agent_overrides[alias].handoff`. If present, use it (full replacement, no merge). Otherwise, use agent's own handoff. No project context → agent's own handoff.

**Thread inheritance:** Once a thread has project/repo set from the first dispatch, follow-up dispatches to the same thread inherit project/repo if not explicitly provided.

### Sub-ticket ORCH-TEAM-6a — Config Schema

- Goal: Add `ProjectConfig` and `AgentProjectOverride` types, `projects` field on `OrchestratorConfig`, validation, and path resolution.
- In scope:
  - New types in `src/config/types.rs`:
    - `ProjectConfig { id, repo_root, repos: Option<Vec<String>>, agent_overrides: HashMap<String, AgentProjectOverride> }`
    - `AgentProjectOverride { handoff: Option<HandoffConfig>, workspace: Option<String>, env: Option<HashMap<String, String>> }`
  - `OrchestratorConfig.projects: Option<Vec<ProjectConfig>>` with `#[serde(default)]`
  - Config validation: unique project IDs, repo_root exists, agent_override keys reference valid aliases, handoff targets valid
  - Path resolution for `project.repo_root` in `config/mod.rs`
- Out of scope: Runtime behavior changes, dispatch params, DB schema
- Dependencies: None
- Acceptance criteria:
  - Config with `projects` section parses and validates correctly
  - Config without `projects` section continues to work (backward compatible)
  - Invalid project config (bad alias ref, duplicate ID) produces clear errors
  - `make verify` passes
- Verification:
  - Unit tests for config parsing with and without projects
  - Unit tests for validation errors
  - `make verify`
- Status: Todo

### Sub-ticket ORCH-TEAM-6b — Dispatch + Thread Resolution

- Goal: Add `project` and `repo` optional params to `DispatchParams`, store project context on threads, resolve effective workdir from project/repo context.
- In scope:
  - Add `project: Option<String>` and `repo: Option<String>` to `DispatchParams`
  - Add `project_id TEXT` and `repo TEXT` columns to `threads` table (nullable, migration)
  - Validate project/repo at dispatch time (project exists, repo in project's repo list)
  - Store project/repo on thread creation
  - Thread inheritance: follow-up dispatches inherit project/repo from existing thread if not specified
  - `resolve_execution_workdir()` function in executor: project.repo_root/repo > project.repo_root > agent.workdir > target_repo_root
  - Update `orch_dispatch` MCP tool description to list available projects
  - Update `orch_status` / `orch_poll` to include project context in output
  - Index on `threads.project_id` for dashboard queries
- Out of scope: Handoff override resolution (6c), dashboard grouping UI (6d)
- Dependencies: ORCH-TEAM-6a
- Acceptance criteria:
  - `orch_dispatch(to="implementer", project="acme-platform", repo="api-gateway")` creates thread with project/repo context
  - Agent runs in `{project.repo_root}/{repo}` directory
  - Worktree created from the correct git repo root (not a parent directory)
  - Invalid project/repo rejected with actionable error
  - Follow-up dispatch to same thread inherits project/repo
  - Dispatch without project works as before (backward compatible)
  - `make verify` passes
- Verification:
  - Integration test: dispatch with project+repo, verify workdir resolution
  - Integration test: dispatch without project, verify fallback to target_repo_root
  - Integration test: follow-up dispatch inherits project context
  - `make verify`
- Status: Todo

### Sub-ticket ORCH-TEAM-6c — Handoff Override Resolution

- Goal: Resolve handoff chains from project context, allowing per-project handoff overrides.
- In scope:
  - `resolve_handoff()` function: check project.agent_overrides[alias].handoff, fall back to agent.handoff
  - Update `maybe_auto_handoff` in `loop_runner.rs` to look up project context from thread before resolving handoff
  - Project override replaces the entire handoff block (no partial merge)
- Out of scope: Per-project prompt overrides, workspace override resolution
- Dependencies: ORCH-TEAM-6a, ORCH-TEAM-6b (thread carries project context)
- Acceptance criteria:
  - Dispatch with `project="compas"` routes implementer→orch-reviewer (project override)
  - Dispatch with `project="acme-platform"` routes implementer→[reviewer-opus, reviewer-codex] (agent default)
  - Dispatch without project uses agent's own handoff
  - `make verify` passes
- Verification:
  - Integration test: dispatch to two projects, verify different handoff chains fire
  - Integration test: dispatch without project, verify agent default handoff
  - `make verify`
- Status: Todo

### Sub-ticket ORCH-TEAM-6d — Dashboard Project Grouping

- Goal: Show project context in the TUI dashboard and support filtering by project.
- In scope:
  - Ops tab: show project label on thread rows (when project_id is set)
  - History tab: show project label on execution rows
  - Agents tab: show project breakdown per agent
  - Optional project filter toggle (keyboard shortcut) to show only one project's threads
  - `orch_status` and `orch_batch_status` include project context in output
- Out of scope: Web dashboard (deferred), project management UI
- Dependencies: ORCH-TEAM-6b (threads carry project context)
- Acceptance criteria:
  - Threads with project context show project label in Ops and History tabs
  - Project filter narrows the view to one project's threads
  - Threads without project context display normally (no label)
  - `make verify` passes
- Verification:
  - Manual: dispatch to two projects, verify dashboard shows project labels
  - Manual: toggle project filter, verify correct filtering
  - `make verify`
- Status: Todo

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

1. ORCH-TEAM-1 (Cost Tracking — independent, immediately valuable)
2. ORCH-TEAM-2 (Operator Identity — foundation for multi-user)
3. ORCH-TEAM-6a (Multi-Project Config Schema — independent, non-breaking config addition)
4. ORCH-TEAM-6b (Multi-Project Dispatch + Thread Resolution — builds on 6a)
5. ORCH-TEAM-6c (Multi-Project Handoff Override — builds on 6a + 6b)
6. ORCH-TEAM-6d (Multi-Project Dashboard Grouping — builds on 6b)
7. ORCH-TEAM-4 (Audit Log — builds on identity)
8. ORCH-TEAM-5 (Scoped Views — builds on identity)
9. ORCH-TEAM-3 (Multi-Operator HTTP — builds on MFE-2 + identity)
10. ORCH-TEAM-7 (Budget Controls — builds on cost tracking)

## Tracking Notes

- Backlog-first governance applies.
- Implementation commits should reference ticket IDs.
- ORCH-TEAM depends on ORCH-EVO infrastructure (event broadcast) and MFE batch (HTTP API for TEAM-3).
- TEAM-1 (cost tracking) and TEAM-2 (operator identity) can start independently.
- TEAM-6 (multi-project) no longer depends on HTTP API — works with MCP dispatch. Moved earlier in execution order.
- TEAM-6 revised (2026-03-20): overlay-based design per as-architect evaluation (thread `01KM6PWMC2DNQ4TT94CDZ2D2QR`). Split into 4 sub-tickets (6a-6d). Original rigid project-scoped-agents design replaced with projects-as-overlays.
- All work happens in the `ottogiron/compas` standalone repo.
- This batch targets the "small AI lab with 2-5 orch devs" scale.

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

- Ticket: ORCH-TEAM-6a
- Owner: TBD
- Complexity: S
- Risk: Low
- Start:
- End:
- Duration:
- Notes: Config schema only, no runtime changes

- Ticket: ORCH-TEAM-6b
- Owner: TBD
- Complexity: M
- Risk: Medium
- Start:
- End:
- Duration:
- Notes: Dispatch params + thread project context + workdir resolution

- Ticket: ORCH-TEAM-6c
- Owner: TBD
- Complexity: S
- Risk: Low
- Start:
- End:
- Duration:
- Notes: Handoff override resolution from project context

- Ticket: ORCH-TEAM-6d
- Owner: TBD
- Complexity: M
- Risk: Low
- Start:
- End:
- Duration:
- Notes: Dashboard project labels + filter

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
