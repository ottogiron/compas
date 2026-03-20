# Multi-Frontend Architecture

Status: Active
Owner: otto
Created: 2026-03-20

## Scope Summary

- Extract a shared service/query layer between Store and all consumers (dashboard, MCP, future HTTP API)
- Add `compas serve` subcommand with Axum HTTP REST API + SSE for real-time events
- Migrate TUI dashboard from direct Store access to the service layer
- Document API surface for future desktop (Tauri) and web frontends

## Context

Architecture evaluation completed by `architect` agent (thread `01KM6N8S3WSBV2P3EZGVSMQRY4`, 2026-03-20). Key decisions:

- **Service layer**: Thin query services following `LifecycleService` pattern, `Serialize + Clone` domain types
- **HTTP API**: Axum REST + SSE in `compas serve` (worker + API in one process)
- **Real-time events**: Shared-process EventBus solves cross-process delivery; SSE bridges to clients
- **TUI migration**: Refactor to service layer directly (not HTTP), incremental per-refresh-method
- **Desktop app**: Tauri with sidecar model (deferred to separate batch)
- **Web UI**: Deferred (HTTP API enables it without committing to a React app)

---

## Ticket MFE-1 — Service Layer Extraction

- Goal: Eliminate duplicated query composition between dashboard and MCP server by introducing a shared service layer with serializable domain types.
- In scope:
  - `src/service/mod.rs` — module root, re-exports
  - `src/service/types.rs` — domain types: `ActivitySnapshot`, `AgentsSnapshot`, `ExecutionsSnapshot`, `Transcript`, `ExecutionDetail`, `MetricsSummary`, `PollResult`, `BatchStatus`, `Diagnosis`, `HealthReport`
  - All types derive `Serialize`, `Clone`, and `Debug`
  - `src/service/query.rs` — `QueryService` struct holding `Store` + `ConfigHandle`, consolidating composite query logic from `src/dashboard/app.rs` and `src/mcp/query.rs`
  - `src/service/dispatch.rs` — `DispatchService` extracting validation + message insertion from `src/mcp/dispatch.rs`
  - Move existing `src/lifecycle/mod.rs` under `src/service/lifecycle.rs` or keep it and re-export from `src/service/`
  - Worker-internal Store methods (~30) remain unwrapped — only consumer-facing composite queries (~15) get service wrappers
- Out of scope:
  - HTTP API (MFE-2)
  - Dashboard refactor to use service layer (MFE-3)
  - MCP server refactor to use service layer (MFE-3)
  - Changing Store method signatures or DB schema
- Dependencies: None
- Acceptance criteria:
  - `QueryService::activity_snapshot()` returns `ActivitySnapshot` matching the data composition currently in `refresh_activity()`
  - `QueryService::agents_snapshot()` returns `AgentsSnapshot` matching `refresh_agents()`
  - `QueryService::transcript()` returns `Transcript` matching both `open_conversation()` and `transcript_impl()`
  - `QueryService::execution_detail()` returns `ExecutionDetail` matching `open_log_viewer()` and `execution_events_impl()`
  - `QueryService::metrics()` returns `MetricsSummary` matching `metrics_impl()`
  - `QueryService::poll()`, `batch_status()`, `diagnose()`, `health()` return appropriate domain types
  - `DispatchService::dispatch()` encapsulates alias validation + message insertion
  - All domain types serialize to JSON cleanly (unit tests)
  - `make verify` passes
- Verification:
  - Unit tests for each service method confirming correct Store method composition
  - `make verify` (fmt-check + clippy + test + lint-md)
- Status: Todo

## Ticket MFE-2 — HTTP API + SSE (`compas serve`)

- Goal: Add an HTTP API layer enabling non-TUI frontends (desktop apps, web UIs) to consume orchestrator data with real-time event streaming.
- In scope:
  - Add `axum`, `tower`, `tower-http`, `tokio-stream` dependencies
  - `Commands::Serve` subcommand in `src/bin/compas.rs` with `--port` (default 19095) and `--bind` (default 127.0.0.1) flags
  - `src/api/mod.rs` — Axum router wiring, state injection, middleware
  - `src/api/routes.rs` — REST endpoints delegating to service layer:
    - `GET /api/status` → `QueryService::activity_snapshot()`
    - `GET /api/agents` → `QueryService::agents_snapshot()`
    - `GET /api/threads/:id/transcript` → `QueryService::transcript()`
    - `GET /api/executions/:id` → `QueryService::execution_detail()`
    - `GET /api/executions/:id/events` → `QueryService::execution_events()`
    - `GET /api/executions/:id/log` → log file reading with offset/limit
    - `GET /api/metrics` → `QueryService::metrics()`
    - `GET /api/poll/:thread_id` → `QueryService::poll()`
    - `GET /api/batch/:id` → `QueryService::batch_status()`
    - `GET /api/health` → `QueryService::health()`
    - `GET /api/diagnose/:thread_id` → `QueryService::diagnose()`
    - `GET /api/agents/list` → agent config listing
    - `POST /api/dispatch` → `DispatchService::dispatch()`
    - `POST /api/threads/:id/close` → `LifecycleService::close()`
    - `POST /api/threads/:id/abandon` → `LifecycleService::abandon()`
    - `POST /api/threads/:id/reopen` → `LifecycleService::reopen()`
  - `src/api/sse.rs` — SSE endpoint `GET /api/events` subscribing to `EventBus::subscribe()` and streaming `OrchestratorEvent` variants as JSON
  - `src/api/auth.rs` — Bearer token middleware; token generated at startup and printed to stderr (local-only model)
  - `compas serve` runs worker loop + Axum server in single Tokio runtime, sharing `EventBus` in-process
  - CORS middleware allowing `localhost` origins (for Tauri/web dev)
- Out of scope:
  - TLS termination (use a reverse proxy for network deployments)
  - Network-accessible auth (OAuth, API keys in config) — deferred
  - WebSocket transport (SSE is sufficient for read-only event streaming)
  - Web UI frontend (deferred)
  - Desktop app frontend (deferred)
- Dependencies: MFE-1 (service layer provides the query/dispatch types)
- Acceptance criteria:
  - `compas serve` starts worker + HTTP server, prints bearer token to stderr
  - `curl -H "Authorization: Bearer <token>" http://127.0.0.1:19095/api/status` returns JSON `ActivitySnapshot`
  - `curl -H "Authorization: Bearer <token>" http://127.0.0.1:19095/api/metrics` returns JSON `MetricsSummary`
  - `curl -H "Authorization: Bearer <token>" http://127.0.0.1:19095/api/threads/<id>/transcript` returns thread messages
  - `POST /api/dispatch` creates a message and returns thread_id + message_id
  - `GET /api/events` (SSE) streams execution events in real-time when dispatches execute
  - Requests without valid bearer token receive 401
  - Worker heartbeat is healthy throughout
  - `make verify` passes
- Verification:
  - Integration test: start `compas serve`, dispatch via HTTP, poll for response, verify SSE events received
  - Manual: `curl` against all endpoints, verify JSON matches service layer types
  - `make verify` (fmt-check + clippy + test + lint-md)
- Status: Todo

## Ticket MFE-3 — Dashboard + MCP Migration to Service Layer

- Goal: Refactor the TUI dashboard and MCP server to consume the shared service layer, eliminating duplicated query composition and validating the API surface.
- In scope:
  - Dashboard `App` struct holds `QueryService` (and optionally `DispatchService`, `LifecycleService`) instead of raw `Store`
  - Refactor `refresh_activity()` to call `QueryService::activity_snapshot()` + TUI-specific state (selection clamping, staleness tracking, `Instant::now()`)
  - Refactor `refresh_agents()` to call `QueryService::agents_snapshot()`
  - Refactor `refresh_executions()` to call `QueryService::executions_snapshot()`
  - Refactor `open_conversation()` to call `QueryService::transcript()`
  - Refactor `open_log_viewer()` to call `QueryService::execution_detail()`
  - Refactor MCP `query.rs` methods to delegate to `QueryService` and wrap results in `CallToolResult`
  - Refactor MCP `dispatch.rs` to delegate to `DispatchService`
  - Refactor MCP `health.rs` to delegate to `QueryService`
  - Remove duplicated Store call composition from both dashboard and MCP
  - TUI keeps direct in-process service layer access (no HTTP round-trip)
- Out of scope:
  - Changing dashboard UI/UX or adding features
  - Changing MCP tool signatures or behavior
  - HTTP API consumption from TUI (unnecessary overhead)
- Dependencies: MFE-1 (service layer must exist first)
- Acceptance criteria:
  - Dashboard produces identical visual output before and after migration (manual comparison)
  - MCP tools return identical JSON responses before and after migration (diff test)
  - Dashboard `App` struct no longer holds a `Store` directly — only service layer types
  - No raw `Store` calls remain in `src/dashboard/` or `src/mcp/` (except MCP session/wait which don't overlap)
  - `make verify` passes
- Verification:
  - Existing integration tests pass unchanged
  - Manual: launch dashboard, verify all tabs render correctly, open conversation and log viewer
  - Manual: dispatch via MCP, verify response format unchanged
  - `make verify` (fmt-check + clippy + test + lint-md)
- Status: Todo

---

## Deferred: Tauri Desktop App

**Batch:** To be created separately when HTTP API (MFE-2) is stable.

**Summary:** Tauri app with sidecar model running `compas serve`. React/TypeScript frontend. Types generated from Rust domain types via `specta` or `ts-rs`. SSE for real-time updates.

**Key decisions from architect evaluation:**
- Tauri over Electron: Rust type sharing, small bundle (~10-15 MB vs 200+), native webview, natural fit for solo Rust developer
- Sidecar model (Tauri manages `compas serve` process lifecycle) preferred over embedding worker as Rust thread
- `desktop/` directory with Tauri project scaffolding
- Views: Activity/Ops, Conversation/Transcript, Execution Detail, Agent Health

**Estimated effort:** L (5-8 days)

## Deferred: Web UI

**Batch:** To be created separately, only if network-accessible monitoring is needed.

**Summary:** Web SPA consuming the HTTP API from MFE-2. Requires TLS (reverse proxy), proper auth (OAuth or config-based API keys), and CORS configuration.

**Prior decision:** Deferred per architect review (ORCH-EVO-9). The HTTP API enables web UIs without committing to maintaining a React app.

**Estimated effort:** L-XL

---

## Execution Order

1. MFE-1 (service layer extraction)
2. MFE-2 (HTTP API + SSE) — can start in parallel with MFE-3 once MFE-1 lands
3. MFE-3 (dashboard + MCP migration)

## Tracking Notes

- Backlog-first governance applies.
- Implementation commits should reference ticket IDs.
- Record scope changes/deferrals here.
- Architect evaluation thread: `01KM6N8S3WSBV2P3EZGVSMQRY4`

## Execution Metrics

- Ticket: MFE-1
- Owner: otto
- Complexity: M
- Risk: Low
- Start:
- End:
- Duration:
- Notes:

- Ticket: MFE-2
- Owner: otto
- Complexity: M-L
- Risk: Medium
- Start:
- End:
- Duration:
- Notes:

- Ticket: MFE-3
- Owner: otto
- Complexity: M
- Risk: Low
- Start:
- End:
- Duration:
- Notes:

## Closure Evidence

- <ticket completion summary>
- <behavior delivered>
- <docs/ADR/changelog parity summary>
- Verification:
  - `<command>`: <result>
  - `<command>`: <result>
- Deferred:
  - Tauri desktop app (separate batch, depends on MFE-2 stability)
  - Web UI (separate batch, depends on MFE-2 + network auth requirements)
