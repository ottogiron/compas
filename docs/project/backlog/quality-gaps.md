# Orchestration Quality Gaps & UX Improvements

Status: Active
Owner: otto
Created: 2026-03-22

## Scope Summary

- Track gaps identified from internal review of orchestration quality, developer ergonomics, and interface evolution
- Prioritize improvements across orchestration quality, developer ergonomics, and interface evolution
- Coordinate with existing backlogs: `multi-frontend.md` (MFE-1/2/3) covers the service layer and HTTP API; this backlog covers everything else

## Context

Gaps identified from internal review of compas's orchestration quality, developer ergonomics, and production readiness (2026-03-22). Key areas for improvement: failure resilience, session management, cost controls, governance/auditability, and installation friction.

### Existing strengths (no action needed)

- MCP-native 22-tool surface with operator-dispatched orchestration
- AX design principles (state-aware inference, diagnostic errors)
- Auto-handoff chains with TOCTOU-safe fan-out and depth checks
- Merge queue with per-target-branch serialization (ADR-019)
- Config-driven custom backend definitions
- Session resume after crash (ADR-017)

### Relationship to other backlogs

- `multi-frontend.md` (MFE-1/2/3): service layer, HTTP API, dashboard migration, deferred Tauri/web UI
- `multi-project.md` (MPR-1/2/3/4): project-based config overlays, per-project handoff overrides, dashboard grouping
- `orch-evolution.md`: EVO-6 (quick dispatch), EVO-10 (webhooks), EVO-11 (periodic summaries), EVO-14 (thread dependencies), EVO-16 (session resume after crash)
- `orch-observability.md`: OBS-02 (tool metrics aggregation), OBS-04 (dashboard cost visibility — in progress)
- `lifecycle-hooks.md`: HOOKS-4 (declarative hook filters)
- `recurring-schedules.md`: CRON-3 — done (archived)
- `delayed-dispatch.md`: SCHED-3 (dashboard visibility for scheduled tasks)
- `orch-wait-ax.md`: WAIT-AX-1 (fan-out settlement metadata)
- `orch-team.md`: deferred team-scale features (TEAM-1 through TEAM-7), some partially superseded by OBS-01 and this backlog
- This backlog: quality gaps NOT covered by existing backlogs — circuit breaker, stale session recovery, cost budgets, governance audit trail, mouse support, shared context, distribution

---

## Ticket GAP-1 — Circuit Breaker for Backend Failures

- Goal: Stop dispatching to backends that are consistently failing, preventing cascading token waste and queue buildup.
- In scope:
  - Per-backend failure counter with configurable threshold (default: 3 consecutive failures)
  - Cooldown period (default: 60s) before retrying the backend
  - Circuit states: Closed (normal) → Open (failing, skip dispatch) → Half-Open (try one, reset or re-open)
  - `orch_health` reports circuit state per backend
  - Dashboard Agents tab shows circuit state (green/yellow/red)
  - Config field: `orchestration.circuit_breaker` with `failure_threshold`, `cooldown_secs`, `enabled`
  - Hot-reloadable config
- Out of scope:
  - Per-agent circuit breakers (backend-level is sufficient for v1)
  - Automatic model fallback (e.g., Opus → Sonnet when Opus circuit opens)
- Dependencies: None
- Acceptance criteria:
  - After `failure_threshold` consecutive failures on a backend, new dispatches to agents using that backend are skipped with a diagnostic message
  - After `cooldown_secs`, one execution is attempted (half-open); success resets, failure re-opens
  - `orch_health` includes `circuit_state: "closed" | "open" | "half_open"` per backend
  - `orch_diagnose` suggests circuit breaker as a cause when relevant
  - `make verify` passes
- Verification:
  - Unit test: simulate N failures, verify circuit opens, verify cooldown resets
  - Integration test: configure a backend with a failing command, verify circuit behavior
  - `make verify` (fmt-check + clippy + test + lint-md)
- Status: In Progress
- Complexity: S-M
- Risk: Low
- Notes: The deprecated `aster-orchestrator` had a circuit breaker (3 failures, 60s cooldown). Restore with the new architecture.

## Ticket GAP-2 — Session Resume Fallback (Stale Session Recovery)

- Goal: Automatically recover from stale/expired backend session IDs instead of hard-failing executions.
- In scope:
  - Per-backend `detect_stale_session(stderr: &str) -> bool` function matching known error patterns:
    - Claude: "session not found", "invalid session"
    - Codex: "thread not found", "expired thread"
    - OpenCode: "session expired", "unknown session"
  - When stale session detected: clear `backend_session_id` for the thread, retry execution as fresh session (counts as a retry attempt if `max_retries > 0`)
  - If `max_retries == 0`: fail with diagnostic message suggesting re-dispatch
  - Log warning: "Stale session detected for thread {id}, retrying as fresh session"
- Out of scope:
  - Proactive session validation before dispatch (would add latency)
  - Session TTL tracking (backends don't expose expiry consistently)
- Dependencies: Soft dependency on ORCH-EVO-16 (session resume after crash, in `orch-evolution.md`). EVO-16 persists session IDs mid-stream; GAP-2 handles recovery when a persisted ID is stale. EVO-16 should land first but GAP-2 can be implemented independently.
- Acceptance criteria:
  - Execution with expired session ID retries with fresh session instead of failing
  - `orch_diagnose` reports "stale session recovered" when this happens
  - Known issues "Stale backend session IDs cause hard execution failures" can be closed
  - `make verify` passes
- Verification:
  - Unit test: mock backend stderr with stale session patterns, verify retry logic
  - `make verify` (fmt-check + clippy + test + lint-md)
- Status: In Progress
- Complexity: S
- Risk: Low
- Notes: Fixes two known issues from `known-issues.md`. Probe (2026-03-22) showed Claude sessions expire under 27 hours. Codex self-heals. OpenCode hangs (timeout catches it). Fix is Claude-specific: match "No conversation found with session ID" pattern.

## Ticket GAP-3 — Cost Budget and Model Routing Controls

- Goal: Give the operator per-agent cost limits and cost-aware model routing, turning the existing cost telemetry (OBS-01) into an active control mechanism.
- In scope:
  - Agent config field: `cost_limit_usd: Option<f64>` — when cumulative cost for the agent exceeds this, new dispatches are rejected with a diagnostic message
  - Agent config field: `cost_limit_window: "session" | "daily" | "total"` (default: "daily") — reset window for cost tracking
  - `orch_metrics` includes `cost_by_agent` breakdown with limit/remaining
  - Dashboard Agents tab shows cost bar relative to limit (when configured)
  - `orch_dispatch` response includes `cost_remaining_usd` when a limit is set
  - Hot-reloadable config (cost limits can be adjusted without restart)
- Out of scope:
  - Automatic model downgrade (e.g., Opus → Sonnet when approaching limit) — deferred
  - Cross-agent budget pooling
  - Token-based limits (cost in USD is more intuitive)
- Dependencies: OBS-01 (cost telemetry, already shipped in v0.3.0)
- Acceptance criteria:
  - Agent with `cost_limit_usd: 5.0` rejects dispatch after $5 cumulative spend with actionable error
  - Cost resets at midnight UTC when `cost_limit_window: "daily"`
  - `orch_metrics` shows per-agent cost/limit/remaining
  - `make verify` passes
- Verification:
  - Unit test: simulate executions with known costs, verify limit enforcement
  - `make verify` (fmt-check + clippy + test + lint-md)
- Status: Todo
- Complexity: M
- Risk: Low
- Notes: Token costs are already captured per-execution (OBS-01). This ticket adds the solo-developer control layer. The team-scale version with per-project budgets and alert thresholds is ORCH-TEAM-7 in `orch-team.md` (deferred). GAP-3 is the pragmatic subset that's useful now. OBS-02 (tool metrics aggregation, in `orch-observability.md`) should ideally land first to provide the `cost_by_agent()` query, but GAP-3 can implement its own simpler aggregation.

## Ticket GAP-4 — Governance Audit Trail

- Goal: Create an append-only audit ledger linking every agent action to git state, enabling post-hoc reconstruction of "which agent did what, when, and based on what context."
- In scope:
  - New `audit_log` table: `id` (ULID), `timestamp`, `event_type`, `thread_id`, `execution_id`, `agent_alias`, `action` (JSON), `git_sha` (nullable — HEAD of worktree at event time), `cost_usd` (nullable)
  - Events logged: dispatch_received, execution_started, execution_completed, execution_failed, handoff_inserted, merge_queued, merge_completed, thread_closed, thread_abandoned
  - `orch_audit` MCP tool: query audit log by thread, agent, time range, or event type
  - `compas audit` CLI subcommand: dump audit log as JSON or CSV for external analysis
  - Audit entries are never deleted (append-only, no retention policy)
  - Git SHA captured via `git rev-parse HEAD` in worktree at execution start/complete
- Out of scope:
  - Real-time audit streaming (use hooks for that)
  - Audit log in the dashboard (deferred — query via CLI or MCP)
  - Cryptographic signing of audit entries
- Dependencies: None (can run in parallel with other tickets)
- Acceptance criteria:
  - Every dispatch, execution lifecycle event, handoff, merge, and thread close creates an audit entry
  - `orch_audit(thread_id="...")` returns chronological audit trail for a thread
  - `compas audit --since "2026-03-22" --agent dev --format json` dumps filtered entries
  - Audit entries include git SHA when the execution ran in a git repo
  - `make verify` passes
- Verification:
  - Integration test: dispatch → execute → close → verify audit trail completeness
  - `make verify` (fmt-check + clippy + test + lint-md)
- Status: Todo
- Complexity: M
- Risk: Low
- Notes: Motivated by HN discussion where a developer built a governance layer specifically because there was no trace of agent actions. The `execution_events` table captures tool calls within a single execution; this captures the cross-execution lifecycle. The team-scale version is ORCH-TEAM-4 in `orch-team.md` (deferred), which adds operator identity attribution and multi-user filtering. GAP-4 is the solo-developer subset — no operator identity needed, just the append-only ledger with git SHA linkage.

## Ticket GAP-5 — TUI Mouse Support

- Goal: Add mouse support to the TUI dashboard for clicking list items, selecting tabs, and drilling into threads.
- In scope:
  - Enable crossterm mouse capture in the terminal setup
  - Mouse click on Ops list items selects the row
  - Mouse click on tab bar switches tabs
  - Mouse click on execution row opens log viewer (equivalent to Enter)
  - Mouse click on thread opens conversation view (equivalent to `c`)
  - Mouse scroll in list views, log viewer, and conversation view
  - Right-click does nothing (no context menus in v1)
- Out of scope:
  - Drag-and-drop (not applicable to current TUI layout)
  - Mouse hover highlighting (would require continuous redraw)
  - Resizable panels via mouse drag
- Dependencies: None
- Acceptance criteria:
  - Clicking an Ops row selects it and highlights it
  - Clicking a tab switches to that tab
  - Scrolling with mouse wheel navigates lists and log content
  - All existing keyboard shortcuts continue to work unchanged
  - `make verify` passes
- Verification:
  - Manual: test click selection, tab switching, scroll, and Enter-equivalent on all tabs
  - Existing integration tests pass unchanged
  - `make verify` (fmt-check + clippy + test + lint-md)
- Status: Done
- Complexity: S
- Risk: Low
- Notes: Already tracked in `known-issues.md`. Ratatui supports mouse events via crossterm. Low effort, high ergonomic payoff.

## Ticket GAP-6 — Shared Context Store (Inter-Agent Knowledge Base)

- Goal: Provide a read-write shared knowledge base that agents can query mid-execution via MCP tool, enabling coordination without direct inter-agent messaging.
- In scope:
  - New `shared_context` table: `key` (TEXT, primary key), `value` (TEXT/JSON), `set_by` (agent alias or "operator"), `updated_at` (INTEGER), `thread_id` (nullable — context of the write)
  - `orch_context_set(key, value)` MCP tool — write a key-value pair (agent or operator)
  - `orch_context_get(key)` MCP tool — read a single key
  - `orch_context_list(prefix?)` MCP tool — list keys with optional prefix filter
  - `orch_context_delete(key)` MCP tool — remove a key (operator only)
  - Use cases: API contracts shared between frontend/backend agents, architectural decisions recorded by architect agent, shared configuration or environment details
  - Agent prompts can reference: "Check shared context for API contracts before implementing"
- Out of scope:
  - Real-time notification when context changes (use handoff chains for that)
  - Versioning / history of context values
  - Access control per-agent (any agent can read/write any key in v1)
  - Large binary data (text/JSON only, max 64KB per value)
- Dependencies: None
- Acceptance criteria:
  - Agent A calls `orch_context_set(key="api/users", value="{...}")` during execution
  - Agent B calls `orch_context_get(key="api/users")` in a subsequent execution and receives the value
  - `orch_context_list(prefix="api/")` returns all keys starting with "api/"
  - Context survives worker restarts (persisted in SQLite)
  - `make verify` passes
- Verification:
  - Integration test: two sequential executions, first writes context, second reads it
  - Unit test: CRUD operations on `shared_context` table
  - `make verify` (fmt-check + clippy + test + lint-md)
- Status: Todo
- Complexity: M
- Risk: Medium
- Notes: This is a lightweight alternative to full inter-agent messaging. A key-value store is simpler and covers the primary use case: sharing decisions and contracts between agents working on related parts of a system. Can evolve into a richer model later.

## Ticket GAP-7 — Distribution and Installation Improvements

- Goal: Reduce installation friction from `cargo install --git` to standard package managers.
- In scope:
  - GitHub Actions release workflow: build binaries for `x86_64-linux`, `aarch64-linux`, `x86_64-darwin`, `aarch64-darwin` on tag push
  - Attach binaries to GitHub Releases as assets
  - `cargo-binstall` support via `[package.metadata.binstall]` in `Cargo.toml`
  - Homebrew formula (tap: `ottogiron/tap`) for macOS/Linux
  - README install section updated with `brew install ottogiron/tap/compas` and `cargo binstall compas`
- Out of scope:
  - AUR package (too niche for now)
  - Docker image (compas needs access to local git repos and backend CLIs)
  - Windows support
- Dependencies: None
- Acceptance criteria:
  - `brew install ottogiron/tap/compas` installs the latest release binary
  - `cargo binstall compas` downloads a pre-built binary instead of compiling from source
  - GitHub Release page has downloadable binaries for all 4 targets
  - `compas doctor` works correctly after brew/binstall installation
  - `make verify` passes
- Verification:
  - CI: release workflow succeeds on tag push, artifacts attached
  - Manual: test `brew install` on macOS, verify `compas --version` and `compas doctor`
  - `make verify` (fmt-check + clippy + test + lint-md)
- Status: Done
- Complexity: M
- Risk: Low
- Notes: Standard package manager installation is expected for CLI tools targeting broad adoption. `cargo install --git` with a Rust toolchain requirement limits trial adoption.

## Ticket GAP-8 — Release Policy Exception for Ticket Tracking

- Goal: Allow release operations to proceed without `ticket` session tracking while keeping tickets required for feature/bug work.
- In scope:
  - Update `AGENTS.md` session tracking policy to exempt release operations
  - Clarify that feature/bug work still requires tickets
- Out of scope:
  - Changes to other governance rules (review policy, quality gates)
  - Changes to backlog workflow
- Dependencies: None
- Acceptance criteria:
  - `AGENTS.md` explicitly states releases are exempt from ticket tracking
  - Ticket requirement remains for feature/bug work
- Verification:
  - Manual: review updated `AGENTS.md` language for clarity
- Status: Done
- Complexity: XS
- Risk: None

---

## Deferred: Visual Orchestration Canvas

**Batch:** To be created when MFE-2 (HTTP API) is stable.

**Summary:** Node-based editor for agent workflow visualization. Nodes = agents, edges = handoff chains, visual state = execution status.

**Key considerations:**

Node-based visual editors have been validated at scale for complex workflow authoring. Compas handoff chains already form a directed graph — the config YAML is the graph definition, the canvas would visualize it.

Could be a view in the Tauri desktop app (depends on MFE-2).

**Estimated effort:** XL (10+ days)

## Deferred: Spatial Computing / AR Integration

**Batch:** Speculative. Depends on hardware maturity (Apple Vision Pro v2, Quest 4).

**Summary:** Spatial workspace concept where each agent's context exists as a physical region. No shipped implementations yet. 2-3 hardware generations from daily-driver status.

**Key insight:** The data model for spatial agent management can be designed now. Compas's SQLite schema already supports it — threads as spatial regions, executions as timeline events, agents as entities with health/cost state. The HTTP API (MFE-2) would serve as the data source for any spatial client.

**Estimated effort:** Unknown. 2-3 hardware generations from daily-driver status.

## Deferred: Automatic Model Fallback / Cost-Aware Routing

**Batch:** To be created after GAP-3 (cost budgets) proves the cost tracking is reliable.

**Summary:** When an agent approaches its cost limit or a backend's circuit breaker opens, automatically route to a cheaper model. E.g., `model_fallback: claude-sonnet-4-6` on an Opus agent means Sonnet takes over when Opus budget is exhausted.

**Estimated effort:** M

---

## Execution Order

1. **GAP-2** (session resume fallback) — smallest, fixes two known issues. Ideally after EVO-16 but can be independent.
2. **GAP-1** (circuit breaker) — small, restores a feature from aster-orchestrator
3. **GAP-5** (mouse support) — small, pure ergonomic win
4. **GAP-7** (distribution) — medium, unblocks adoption
5. **GAP-8** (release policy exception) — small, governance tweak
6. **GAP-3** (cost budgets) — medium, builds on OBS-01 telemetry. Ideally after OBS-02 lands.
7. **GAP-4** (audit trail) — medium, new table + MCP tool + CLI
8. **GAP-6** (shared context) — medium, new coordination primitive

GAP-1 through GAP-5 and GAP-8 can run in parallel with the MFE and MPR backlogs. GAP-6 and GAP-7 are independent of all other backlogs.

### Cross-backlog priority recommendation (all open tickets across all backlogs)

For context, here's a suggested global priority ordering considering all open work:

**Quick wins (S complexity, high payoff):**
GAP-2, GAP-1, GAP-5, WAIT-AX-1, HOOKS-4

**Medium efforts with immediate value:**
OBS-02 → OBS-04 (finish observability), GAP-7 (distribution), EVO-16 (crash resume), EVO-6 (quick dispatch)

**Foundational for next phase:**
MFE-1 → MFE-2 → MFE-3 (service layer + HTTP API — unlocks Tauri, web UI, visual canvas)
MPR-1 → MPR-2 → MPR-3/4 (multi-project)

**Higher effort, differentiation:**
GAP-3 (cost budgets), GAP-4 (audit trail), GAP-6 (shared context), EVO-14 (thread dependencies)

**Deferred (team-scale or speculative):**
ORCH-TEAM-* (all), visual canvas, spatial computing, automatic model fallback

## Tracking Notes

- Backlog-first governance applies.
- Implementation commits should reference ticket IDs (GAP-N).
- Record scope changes/deferrals here.
- Gap analysis: completed 2026-03-22.

- [GAP-7] Session opened to add GAP-8; switching to GAP-8.
## Execution Metrics
- Ticket: GAP-8
- Owner: (pending)
- Complexity: (pending)
- Risk: (pending)
- Start: 2026-03-29 02:45 UTC
- End: 2026-03-29 02:51 UTC
- Duration: 00:06:33
- Notes: (pending)

- Ticket: GAP-1
- Owner: otto
- Complexity: S-M
- Risk: Low
- Start:
- End:
- Duration:
- Notes:

- Ticket: GAP-2
- Owner: otto
- Complexity: S
- Risk: Low
- Start:
- End:
- Duration:
- Notes:

- Ticket: GAP-3
- Owner: otto
- Complexity: M
- Risk: Low
- Start:
- End:
- Duration:
- Notes:

- Ticket: GAP-4
- Owner: otto
- Complexity: M
- Risk: Low
- Start:
- End:
- Duration:
- Notes:

- Ticket: GAP-5
- Owner: otto
- Complexity: S
- Risk: Low
- Start: 2026-03-28 18:50 UTC
- End: 2026-03-28 19:12 UTC
- Duration: 00:21:46
- Notes: 2 review rounds. Round 1: 3 blocking (terminal restore, changelog, tests) + 3 minor. All resolved in round 2. Residual: agents scroll offset (low risk).

- Ticket: GAP-6
- Owner: otto
- Complexity: M
- Risk: Medium
- Start:
- End:
- Duration:
- Notes:

- Ticket: GAP-7
- Owner: otto
- Complexity: M
- Risk: Low
- Start:
- End:
- Duration:
- Notes:


- Start: 2026-03-29 02:35 UTC


- End: 2026-03-29 02:45 UTC


- Duration: 00:09:04

## Closure Evidence

- <ticket completion summary>
- <behavior delivered>
- <docs/ADR/changelog parity summary>
- Verification:
  - `<command>`: <result>
  - `<command>`: <result>
- Deferred:
  - Visual orchestration canvas (separate batch, depends on MFE-2)
  - Spatial computing integration (speculative, depends on hardware)
  - Automatic model fallback (depends on GAP-3)
