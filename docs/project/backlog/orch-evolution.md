# Compas Evolution — Visibility, Ergonomics & Remote Access

Status: Active
Owner: otto
Created: 2026-03-14

## Scope Summary

- Add real-time execution telemetry and "what is the agent doing now" visibility to the dashboard
- Improve dashboard ergonomics: conversation view, quick dispatch, notifications
- Build event broadcast infrastructure that enables notifications, webhooks, and future web dashboard
- Git worktree management for parallel agent workspace isolation
- HTTP API layer and web dashboard for remote administration
- Webhook/notification integrations for mobile awareness

## Ticket ORCH-EVO-1 — Execution Telemetry & Structured Progress Events

- Goal: Parse backend output streams during execution to extract structured events (tool calls, file edits, test runs) and store them for real-time display.
- In scope:
  - `execution_events` table in SQLite (exec_id, event_type, summary, timestamp)
  - Parse Claude `stream-json` output incrementally during execution for tool-use events
  - Parse JSONL output from Codex/Gemini/OpenCode for structured events where possible
  - Store progress events as they happen (not just at execution end)
  - Expose events via new MCP tool `orch_execution_events` for querying
- Out of scope:
  - Dashboard display (separate ticket)
  - LLM-powered summarization of progress
  - Non-Claude backends beyond best-effort JSONL parsing
- Dependencies: None
- Acceptance criteria:
  - During a Claude execution, tool-call events are written to `execution_events` table in real time
  - Events include at minimum: event_type (tool_call, file_edit, test_run, etc.), summary text, timestamp
  - `orch_execution_events` MCP tool returns events for a given execution
  - Existing execution flow (log files, completion parsing) is not disrupted
- Verification:
  - Integration test: dispatch to stub backend that emits stream-json, verify events are stored
  - Manual: run a real Claude execution, query events via MCP tool during and after execution
- Status: Done

## Ticket ORCH-EVO-2 — Event Broadcast Channel

- Goal: Add an internal `tokio::broadcast` event stream that the worker emits to on state changes, enabling push-based consumers (dashboard, notifications, future web/SSE).
- In scope:
  - Define `OrchestratorEvent` enum (ExecutionStarted, ExecutionProgress, ExecutionCompleted, ThreadStatusChanged, BatchProgress, AgentHealthChanged, MessageReceived)
  - Worker emits events on all state transitions
  - Dashboard subscribes to broadcast channel (replaces or supplements polling)
  - Event channel is optional — system works without subscribers
- Out of scope:
  - Persistence of events (they're ephemeral broadcast)
  - Web/SSE delivery (separate ticket)
  - Webhook delivery (separate ticket)
- Dependencies: None
- Acceptance criteria:
  - `OrchestratorEvent` enum covers all major state transitions
  - Worker emits events during normal operation
  - Dashboard receives events via broadcast channel
  - No performance regression: broadcast with no subscribers is effectively free
- Verification:
  - Unit test: emit events, verify subscriber receives them
  - Integration test: full dispatch cycle produces expected event sequence
  - Dashboard shows updates without manual refresh when events fire
- Status: Done

## Ticket ORCH-EVO-3 — Dashboard "Currently Working On" Display

- Goal: Show a real-time 1-line summary of what each running execution is doing, visible in the Ops tab next to running executions.
- In scope:
  - Ops tab running section: add a truncated summary line per running execution
  - Source summary from execution_events (most recent event) or log file tail
  - Update on event broadcast (from ORCH-EVO-2) or fallback to periodic log tail
  - Log viewer: add a structured timeline/outline panel showing execution events as a TOC
- Out of scope:
  - LLM-powered summarization (just use raw event data)
  - Web dashboard display
- Dependencies: ORCH-EVO-1 (execution telemetry), ORCH-EVO-2 (event broadcast) — can partially work with log tail fallback if those aren't done yet
- Acceptance criteria:
  - Running executions in Ops tab show a summary like "editing src/parser.rs" or "running cargo test"
  - Summary updates within 1-2 seconds of new activity
  - Log viewer has an outline/timeline sidebar showing structured events
  - No visual regression in Ops tab when no events are available (graceful fallback)
- Verification:
  - Manual: run an execution, observe live summary updates in Ops tab
  - Manual: open log viewer during execution, verify timeline panel shows events
- Status: Done

## Ticket ORCH-EVO-4 — Desktop Notifications

- Goal: Send macOS desktop notifications on execution completion, failure, and batch completion.
- In scope:
  - macOS notification via `osascript` or `terminal-notifier` (with fallback)
  - Configurable in config.yaml: `notifications.desktop = true/false`
  - Notification on: execution completed, execution failed, execution timed out, batch completed
  - Notification content: agent alias, thread/batch context, duration, status
  - Subscribe to event broadcast channel (from ORCH-EVO-2) for triggers
- Out of scope:
  - Linux/Windows notification support (macOS only for now)
  - Sound customization
  - Webhook/Slack/Discord notifications (separate ticket)
- Dependencies: ORCH-EVO-2 (event broadcast) — or can poll as fallback
- Acceptance criteria:
  - When `notifications.desktop = true`, macOS notifications appear on execution complete/fail
  - Notifications show meaningful context (agent name, status, duration)
  - Notifications are non-blocking and don't slow down the worker
  - When `notifications.desktop = false` (default), no notifications are sent
- Verification:
  - Manual: enable desktop notifications, run an execution, verify notification appears
  - Unit test: notification formatting produces expected title/body
- Status: Done

## Ticket ORCH-EVO-5 — Thread Conversation View in Dashboard

- Goal: Add a conversation panel to the dashboard that renders thread messages as a readable chat transcript.
- In scope:
  - New view accessible from Ops tab context panel (Enter on a thread, or a tab/toggle)
  - Renders messages from `messages` table for the selected thread
  - Chat-like layout: from/to, intent badge, timestamp, message body
  - Markdown rendering of message body (basic formatting)
  - Auto-scroll to latest message
  - Distinction between operator messages, agent replies, and system messages (close/abandon)
- Out of scope:
  - Editing or replying from conversation view (separate ticket: quick dispatch)
  - Web dashboard version
- Dependencies: None
- Acceptance criteria:
  - Selecting a thread shows its full message history in a readable format
  - Messages are visually distinguished by sender (operator vs agent vs system)
  - Intent badges are shown (dispatch, response, review-request, etc.)
  - Timestamps are displayed relative or absolute
- Verification:
  - Manual: dispatch to an agent, wait for reply, verify conversation view shows both messages
  - Manual: close a thread, verify close message appears in conversation
- Status: Done

## Ticket ORCH-EVO-6 — Quick Dispatch from Dashboard

- Goal: Allow dispatching instructions to agents directly from the TUI dashboard without switching to the MCP client.
- In scope:
  - `d` key in Ops tab opens a dispatch prompt
  - Agent selection (list of available worker agents)
  - Text input for instruction body
  - Optional batch association
  - Dispatch creates a new thread or continues the selected thread
  - Confirmation before dispatch
- Out of scope:
  - Multi-turn conversation from dashboard (just single dispatches)
  - File attachment or complex message formatting
- Dependencies: None (soft: should use `DispatchService` from MFE-1 once available)
- Acceptance criteria:
  - `d` key opens dispatch prompt with agent selection
  - Typing an instruction and confirming creates a dispatch message and thread
  - The dispatched execution appears in the Ops tab running section
  - Can dispatch to an existing thread (continue conversation) or create new
- Verification:
  - Manual: press `d`, select agent, type instruction, verify execution starts
  - Manual: select a thread, press `d`, verify dispatch continues that thread
- Status: Todo

## Ticket ORCH-EVO-7 — Git Worktree Management for Agent Isolation

- Goal: Automatically create and manage git worktrees so multiple agents can work on different tasks in parallel without file conflicts.
- In scope:
  - New agent config option: `workspace: worktree | shared` (default: shared)
  - Worker creates `git worktree add` before trigger when `workspace: worktree`
  - Worktree path: `{state_dir}/worktrees/{batch_id or thread_id}/`
  - Pass worktree path as `--directory` (Claude) or `-C` (Codex) to backend CLI
  - Cleanup: remove worktree on batch close or configurable retention
  - Listing: expose active worktrees via MCP tool or dashboard
- Out of scope:
  - Branch management (auto-branch per worktree)
  - PR creation from worktrees
  - Merge conflict resolution
- Dependencies: None
- Acceptance criteria:
  - Agent with `workspace: worktree` gets a dedicated worktree for each batch/thread
  - Backend CLI runs in the worktree directory
  - Worktrees are cleaned up when batch/thread is closed
  - Shared workspace (default) behavior is unchanged
  - Multiple agents can work in parallel without file conflicts
- Verification:
  - Integration test: dispatch to two agents with worktree mode, verify separate directories
  - Manual: run parallel executions, verify no file conflicts
  - Manual: close batch, verify worktree is removed
- Status: Done

## Ticket ORCH-EVO-8 — HTTP API Layer

- Goal: Expose compas operations as HTTP REST endpoints, enabling web dashboard, remote clients, and webhook integrations.
- In scope:
  - New CLI subcommand: `compas serve`
  - Axum HTTP server with REST endpoints mirroring MCP tools
  - Endpoints: dispatch, close, status, transcript, metrics, batch_status, tasks, health, agents, poll
  - SSE endpoint `/api/events` for live event streaming (requires ORCH-EVO-2)
  - Pairing-based auth (bearer token, similar to ZeroClaw pattern)
  - JSON request/response format
  - CORS configuration for web dashboard
- Out of scope:
  - Web dashboard frontend (separate ticket)
  - Multi-user auth / role-based access
  - TLS termination (use reverse proxy)
- Dependencies: ORCH-EVO-2 (event broadcast for SSE)
- Acceptance criteria:
  - All major MCP tools are accessible via HTTP endpoints
  - SSE endpoint streams orchestrator events in real time
  - Auth prevents unauthorized access
  - API is usable from curl, browser, or any HTTP client
- Verification:
  - Integration test: dispatch via HTTP, poll for result, verify completion
  - Manual: curl endpoints, verify correct responses
  - Manual: connect to SSE endpoint, verify events stream during execution
- Status: Superseded by MFE-2 (multi-frontend.md)

## Ticket ORCH-EVO-9 — Web Dashboard (Read-Only)

- Goal: Build a web dashboard that provides remote read-only visibility into orchestrator state, served by the HTTP API layer.
- In scope:
  - Vite + React + TypeScript SPA (similar to ZeroClaw's web/ structure)
  - Served as static assets by the HTTP server (ORCH-EVO-8)
  - Pages: Ops (running/active/completed), Agents (health/status), History (executions)
  - SSE-powered live updates
  - Thread detail view with conversation transcript
  - Execution log viewer
  - Mobile-responsive layout
- Out of scope:
  - Write operations (dispatch, abandon, etc.) — add later
  - Desktop TUI feature parity (TUI stays primary)
  - Custom theming
- Dependencies: ORCH-EVO-8 (HTTP API), ORCH-EVO-2 (event broadcast)
- Acceptance criteria:
  - Web dashboard loads in a browser and shows current orchestrator state
  - Live updates via SSE without manual refresh
  - Thread conversations and execution logs are viewable
  - Works on mobile browsers
- Verification:
  - Manual: open web dashboard, dispatch via TUI/MCP, verify web updates in real time
  - Manual: open on mobile device, verify layout is usable
- Status: Superseded (deferred in multi-frontend.md)

## Ticket ORCH-EVO-10 — Webhook Notifications

- Goal: Send notifications to external services (Slack, Discord, generic HTTP) on orchestrator events.
- In scope:
  - Webhook configuration in config.yaml:

    ```yaml
    webhooks:
      - url: https://hooks.slack.com/...
        events: [execution_completed, execution_failed, batch_completed]
        format: slack
    ```

  - Supported formats: slack, discord, generic (raw JSON)
  - Async delivery with retry (1 retry, no queue persistence)
  - Subscribe to event broadcast channel (ORCH-EVO-2) for triggers
  - Rate limiting to prevent webhook spam
- Out of scope:
  - Incoming webhooks (receiving events from external services)
  - Persistent delivery queue / guaranteed delivery
  - Custom webhook templates
- Dependencies: ORCH-EVO-2 (event broadcast)
- Acceptance criteria:
  - Configured webhooks fire on matching events
  - Slack/Discord messages are well-formatted with context
  - Failed webhook delivery is logged but doesn't block the worker
  - Rate limiting prevents more than N notifications per minute
- Verification:
  - Integration test: configure webhook to local HTTP server, trigger event, verify delivery
  - Manual: configure Slack webhook, run execution, verify Slack message
- Status: Todo

## Ticket ORCH-EVO-11 — Periodic Execution Summary Updates

- Goal: During long-running executions, periodically generate and display a brief summary of progress in the dashboard execution line.
- In scope:
  - Configurable summary interval (e.g., every 30 seconds or every 50 log lines)
  - Summary derived from execution events (ORCH-EVO-1) or log file content
  - Displayed inline in Ops tab running section, replacing/updating the "currently working on" line
  - Stored in `execution_events` table as a `summary` event type
  - Optional: use a fast/cheap LLM call to summarize recent activity (configurable, off by default)
- Out of scope:
  - Full transcript summarization
  - Summary persistence beyond execution lifetime
- Dependencies: ORCH-EVO-1 (execution telemetry), ORCH-EVO-3 (dashboard display)
- Acceptance criteria:
  - Long-running executions show periodic summary updates in the dashboard
  - Summaries are concise (1 line, ~60 chars) and reflect recent activity
  - Summary interval is configurable
  - No noticeable performance impact from summary generation
- Verification:
  - Manual: run a long execution (>60s), verify summary updates appear periodically
  - Unit test: summary generation from event sequence produces reasonable output
- Status: Todo

## Ticket ORCH-EVO-12 — Retry with Error Classification

- Goal: Automatically retry failed executions for transient errors (network blips, temporary rate limits), while not retrying terminal failures (usage exhausted, genuine agent errors).
- In scope:
  - Configurable per-agent `max_retries` (default: 0 — no retry, preserving current behavior)
  - Error classification per backend: parse exit code + stderr/output for known transient patterns
    - Claude: rate limit (429), temporary API errors vs usage exhaustion
    - Codex: similar classification
    - OpenCode: similar classification
  - New execution row per retry attempt (linked to original via `retry_of` column)
  - Exponential backoff between retries (configurable base delay)
  - Dashboard shows retry count and history for an execution
  - `orch_diagnose` includes retry history in diagnostics
- Out of scope:
  - Fallback to a different backend on failure (cross-backend retry)
  - Automatic prompt modification on retry
  - Circuit breaker (global failure rate tracking)
- Dependencies: ORCH-FOUND-2 (backend output contract helps with error classification)
- Acceptance criteria:
  - Transient failures are retried up to `max_retries` with backoff
  - Terminal failures (usage exhausted, timeout) are NOT retried
  - Each retry attempt is a separate execution row, traceable to the original
  - `max_retries: 0` (default) preserves current fail-fast behavior
  - Dashboard and `orch_tasks` show retry lineage
- Verification:
  - Integration test: stub backend fails with transient error, verify retry occurs
  - Integration test: stub backend fails with terminal error, verify no retry
  - `make verify` passes
- Status: Done

## Ticket ORCH-EVO-13 — Prompt Version Hashing

- Goal: Store a hash of the agent prompt at dispatch time in the executions table, enabling prompt-to-outcome correlation without building a full prompt management system.
- In scope:
  - Compute SHA-256 hash of the resolved agent prompt (system prompt + any prompt_file content) at execution creation time
  - Add `prompt_hash TEXT` column to `executions` table
  - `orch_tasks` includes `prompt_hash` in output
  - Enables querying: "all executions that ran with this prompt version"
  - Store full prompt text in execution log (already happens via backend args) — hash is for correlation only
- Out of scope:
  - Prompt versioning UI or management
  - A/B testing framework
  - Prompt rollback
  - Prompt storage/registry (just the hash)
- Dependencies: None
- Acceptance criteria:
  - Every execution has a `prompt_hash` stored
  - Same prompt produces same hash (deterministic)
  - Different prompts produce different hashes
  - `orch_tasks` output includes the hash
  - Config hot-reload that changes a prompt produces a different hash on next execution
- Verification:
  - Unit test: same prompt → same hash, different prompt → different hash
  - Integration test: dispatch, verify prompt_hash is stored in executions table
  - `make verify` passes
- Status: Done

## Ticket ORCH-EVO-14 — Thread Dependency Primitive

- Goal: Allow threads to declare dependencies on other threads, enabling basic coordination for complex multi-step orchestration without a full DAG engine.
- In scope:
  - Add `depends_on TEXT` column to `threads` table (JSON array of thread IDs, nullable)
  - `orch_dispatch` accepts optional `depends_on: ["thread-id-1", "thread-id-2"]` parameter
  - Worker skips execution for threads whose dependencies are not all in `Completed` status
  - `orch_status` and `orch_batch_status` show dependency state (waiting, ready, blocked)
  - `orch_diagnose` reports unmet dependencies as blockers
  - Dashboard Ops tab shows dependency indicators on threads
- Out of scope:
  - Full DAG execution engine (topological sort, parallel branch execution)
  - Circular dependency detection (enforce at dispatch time with simple check)
  - Automatic dispatch when dependencies complete (operator must dispatch explicitly)
  - Cross-batch dependencies
- Dependencies: None
- Acceptance criteria:
  - Thread with `depends_on` is not executed until all dependencies are `Completed`
  - Thread without `depends_on` behaves as today (no change)
  - `orch_diagnose` shows "waiting on thread X (Active)" as a blocker
  - Circular dependency is rejected at dispatch time with clear error
  - Dashboard shows dependency status visually
- Verification:
  - Integration test: create thread A, create thread B depending on A, verify B is not executed until A completes
  - Integration test: circular dependency rejected with error
  - `make verify` passes
- Status: Todo

## Ticket ORCH-EVO-15 — Health Check Performance (Parallel Pings + Cache)

- Goal: Fix `orch_health` performance by parallelizing backend pings and adding a TTL-based cache. Currently pings run sequentially (4-6s per agent, ~60s for all 13 agents).
- In scope:
  - Parallelize backend pings in `src/mcp/health.rs:68-98` using `tokio::task::JoinSet` or `futures::future::join_all` — reduce from N×latency to max(latency)
  - Add `PingCache` with configurable TTL (default 60s) — repeated `orch_health` calls within the TTL return cached results instantly
  - Cache keyed by agent alias, stores `PingResult` + timestamp
  - Cache is shared across MCP sessions (lives on the `McpServer` struct or a shared `Arc`)
  - `orch_health` response includes a `cached: bool` field so the caller knows if the result is fresh or cached
  - Optional: remove `command_exists()` (`which` subprocess) from the ping hot path in `src/backend/process.rs:91` — check CLI existence once at registry construction, not per-ping
  - Optional: fix OpenCode synchronous `Command::output()` session cleanup in `src/backend/opencode.rs:272` — should use async or fire-and-forget spawn
- Out of scope:
  - Changing the ping probe from a real API call to `--version` (loses API connectivity verification)
  - Background periodic health polling (pings only happen on `orch_health` calls)
- Dependencies: None
- Acceptance criteria:
  - `orch_health()` (all agents) completes in ~5-6s instead of ~60s
  - `orch_health(alias="compas-implementer")` completes in ~5-6s (single ping, no cache)
  - Second `orch_health()` call within 60s returns cached results in <100ms
  - Cache TTL is configurable via `orchestration.ping_cache_ttl_secs` (default 60)
  - `orch_health` response includes `cached` indicator per agent
  - No behavioral regression: same JSON structure, same ping semantics
  - `make verify` passes
- Verification:
  - Unit test: `PingCache` TTL expiry and cache hit/miss
  - Integration test: two sequential `orch_health` calls, second returns cached
  - Manual: `orch_health()` timing before/after
  - `make verify`
- Status: Todo

## Execution Order

1. ~~ORCH-EVO-2 (Event Broadcast — done)~~
2. ~~ORCH-EVO-1 (Execution Telemetry — done)~~
3. ~~ORCH-EVO-7 (Git Worktrees — done)~~
4. ~~ORCH-EVO-4 (Desktop Notifications — done)~~
5. ~~ORCH-EVO-3 (Dashboard "Currently Working On" — done)~~
6. ~~ORCH-EVO-5 (Conversation View — done)~~
7. ~~ORCH-EVO-12 (Retry with Error Classification — done)~~
8. ~~ORCH-EVO-13 (Prompt Version Hashing — done)~~
9. ORCH-EVO-15 (Health Check Performance — quick win, parallel pings + cache)
10. ORCH-EVO-10 (Webhook Notifications — simpler than HTTP API, high value for Slack/Discord alerts)
11. ORCH-EVO-6 (Quick Dispatch — independent, high ergonomic value)
11. ORCH-EVO-11 (Periodic Summaries — builds on telemetry + dashboard)
12. ORCH-EVO-14 (Thread Dependency Primitive — sequenced multi-step orchestration)
13. ~~ORCH-EVO-8 (HTTP API — superseded by MFE-2 in multi-frontend.md)~~
14. ~~ORCH-EVO-9 (Web Dashboard — superseded, deferred in multi-frontend.md)~~

## Tracking Notes

- Backlog-first governance applies.
- Implementation commits should reference ticket IDs.
- EVO-2 completed 2026-03-14 (event broadcast channel).
- EVO-7 (worktrees) moved up per orch-architect review — concurrent agents without isolation is a data corruption risk.
- EVO-10 (webhooks) moved before EVO-8 (HTTP API) — outbound webhooks are 10x simpler and more immediately useful than a full API layer.
- EVO-9 (web dashboard) deferred per orch-architect review — HTTP API (EVO-8) enables web UIs without committing to maintaining a React app. Build the API, let the UI emerge.
- All work happens in the compas standalone repo.
- **EVO-8 and EVO-9 superseded (2026-03-20).** The multi-frontend batch (`docs/project/backlog/multi-frontend.md`) replaces EVO-8 with MFE-2 (adds a service layer prerequisite and detailed route design) and defers EVO-9's web UI scope. Cross-backlog dependencies (ORCH-TEAM-3) updated to reference MFE-2.
- **Sessions concept deferred.** Orch-architect recommended promoting batches to first-class entities (with description, lifecycle, tags) instead of adding a session layer above them. See architect review thread from 2026-03-15. When batches need cross-batch grouping, add tags. Revisit full sessions at TEAM-scale if multi-batch campaigns become a real pattern.
- **Batch promotion (future ticket):** Create a `batches` table with description, status (active/paused/completed), created_at/completed_at, and tags. Auto-created on first dispatch. `orch_batch_create`/`orch_batch_close` for explicit lifecycle. Not yet scheduled — defer until cost tracking (TEAM-1) or multi-project (TEAM-6) work begins.

## Execution Metrics

- Ticket: ORCH-EVO-1
- Owner: orch-dev
- Complexity: L
- Risk: Medium
- Start: 2026-03-14 23:19 UTC
- End: 2026-03-14 23:50 UTC
- Duration: ~00:30:00
- Notes: Split into EVO-1a (stream-json format) and EVO-1b (telemetry plumbing). Both completed same session.

- Ticket: ORCH-EVO-2
- Owner: TBD
- Complexity: M
- Risk: Low
- Start:
- End:
- Duration:
- Notes:

- Ticket: ORCH-EVO-3
- Owner: orch-dev
- Complexity: M
- Risk: Low
- Start: 2026-03-15 12:48 UTC
- End: 2026-03-15 12:48 UTC
- Duration: ~00:30:00
- Notes: Dashboard "currently working on" display + log viewer timeline.

- Ticket: ORCH-EVO-4
- Owner: orch-dev
- Complexity: S
- Risk: Low
- Start: 2026-03-15 11:35 UTC
- End: 2026-03-15 21:23 UTC
- Duration: 09:59:16
- Notes: macOS osascript notifications. Notifications lack task context (known issue).

- Ticket: ORCH-EVO-5
- Owner: orch-dev
- Complexity: M
- Risk: Low
- Start: 2026-03-15 20:00 UTC
- End: 2026-03-15 20:52 UTC
- Duration: ~00:52:00
- Notes: Full-screen overlay, message + execution marker interleaving, live polling. Multiple fix rounds.

- Ticket: ORCH-EVO-6
- Owner: TBD
- Complexity: M
- Risk: Medium
- Start:
- End:
- Duration:
- Notes:

- Ticket: ORCH-EVO-7
- Owner: orch-dev
- Complexity: L
- Risk: Medium
- Start: 2026-03-15 10:35 UTC
- End: 2026-03-15 17:20 UTC
- Duration: ~06:45:00
- Notes: Multiple iterations — initial impl, repo-sibling relocation, worktree cleanup fix, stored DB path fix. Per-agent workdir (ADR-010).

- Ticket: ORCH-EVO-8
- Owner: TBD
- Complexity: XL
- Risk: Medium
- Start:
- End:
- Duration:
- Notes:

- Ticket: ORCH-EVO-9
- Owner: TBD
- Complexity: XL
- Risk: Medium
- Start:
- End:
- Duration:
- Notes:

- Ticket: ORCH-EVO-10
- Owner: TBD
- Complexity: M
- Risk: Low
- Start:
- End:
- Duration:
- Notes:

- Ticket: ORCH-EVO-11
- Owner: TBD
- Complexity: M
- Risk: Medium
- Start:
- End:
- Duration:
- Notes:

- Ticket: ORCH-EVO-12
- Owner: orch-dev
- Complexity: M
- Risk: High
- Start: 2026-03-15 15:22 UTC
- End: 2026-03-15 16:00 UTC
- Duration: ~00:38:00
- Notes: ErrorCategory enum, deny-list, exponential backoff, multiple fix rounds for rate-limit classification and overflow.

- Ticket: ORCH-EVO-13
- Owner: orch-dev
- Complexity: S
- Risk: Low
- Start: 2026-03-15 15:00 UTC
- End: 2026-03-15 15:02 UTC
- Duration: ~00:05:00
- Notes: SHA-256 hash of resolved prompt. Quick implementation.

- Ticket: ORCH-EVO-14
- Owner: TBD
- Complexity: M
- Risk: Medium
- Start:
- End:
- Duration:
- Notes:

- Ticket: ORCH-EVO-15
- Owner: TBD
- Complexity: S
- Risk: Low
- Start:
- End:
- Duration:
- Notes: Parallel pings + TTL cache for orch_health

## Closure Evidence

- 8 of 14 tickets shipped and merged on main
- EVO-1: Execution telemetry — execution_events table, real-time tool-call parsing, orch_execution_events MCP tool
- EVO-2: Event broadcast channel — OrchestratorEvent enum, tokio::broadcast, dashboard subscribes
- EVO-3: Dashboard "currently working on" — real-time summary per running execution, log viewer timeline
- EVO-4: Desktop notifications — macOS osascript notifications on execution complete/fail/batch complete
- EVO-5: Conversation view — full-screen overlay, message + execution marker interleaving, live polling
- EVO-7: Git worktrees — workspace: worktree|shared config, git worktree add/remove, per-agent workdir (ADR-010)
- EVO-12: Retry with error classification — ErrorCategory enum, transient vs terminal failures, exponential backoff
- EVO-13: Prompt version hashing — SHA-256 hash of resolved prompt stored per execution
- Still Todo (7): EVO-6 (Quick Dispatch), ~~EVO-8 (HTTP API — superseded)~~, ~~EVO-9 (Web Dashboard — superseded)~~, EVO-10 (Webhooks), EVO-11 (Periodic Summaries), EVO-14 (Thread Dependencies), EVO-15 (Health Check Performance)
- Verification:
  - `make verify`: fmt-check + clippy + 362 unit + 22 bin + 93 integration = 477 tests pass
  - Visual verification: dashboard shows live updates, conversation view renders correctly, notifications fire
