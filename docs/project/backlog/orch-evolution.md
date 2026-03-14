# Aster-Orch Evolution — Visibility, Ergonomics & Remote Access

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
- Status: Todo

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
- Status: Todo

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
- Status: Todo

## Ticket ORCH-EVO-4 — Desktop Notifications

- Goal: Send macOS desktop notifications on execution completion, failure, and batch completion.
- In scope:
  - macOS notification via `osascript` or `terminal-notifier` (with fallback)
  - Configurable in `aster-orch.yaml`: `notifications.desktop = true/false`
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
- Status: Todo

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
  - Intent badges are shown (dispatch, status-update, completion, etc.)
  - Timestamps are displayed relative or absolute
- Verification:
  - Manual: dispatch to an agent, wait for reply, verify conversation view shows both messages
  - Manual: close a thread, verify close message appears in conversation
- Status: Todo

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
- Dependencies: None
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
- Status: Todo

## Ticket ORCH-EVO-8 — HTTP API Layer

- Goal: Expose aster-orch operations as HTTP REST endpoints, enabling web dashboard, remote clients, and webhook integrations.
- In scope:
  - New CLI subcommand: `aster_orch serve` (or `--web` flag on dashboard)
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
- Status: Todo

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
- Status: Todo

## Ticket ORCH-EVO-10 — Webhook Notifications

- Goal: Send notifications to external services (Slack, Discord, generic HTTP) on orchestrator events.
- In scope:
  - Webhook configuration in `aster-orch.yaml`:
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

## Execution Order

1. ORCH-EVO-2 (Event Broadcast — foundation for most other tickets)
2. ORCH-EVO-1 (Execution Telemetry — feeds into dashboard display)
3. ORCH-EVO-4 (Desktop Notifications — quick win, high daily-use value)
4. ORCH-EVO-3 (Dashboard "Currently Working On" — uses telemetry + events)
5. ORCH-EVO-5 (Conversation View — independent, high ergonomic value)
6. ORCH-EVO-6 (Quick Dispatch — independent, high ergonomic value)
7. ORCH-EVO-7 (Git Worktrees — independent, enables parallel agents)
8. ORCH-EVO-11 (Periodic Summaries — builds on telemetry + dashboard)
9. ORCH-EVO-8 (HTTP API — larger effort, enables remote access)
10. ORCH-EVO-10 (Webhook Notifications — uses event broadcast + HTTP)
11. ORCH-EVO-9 (Web Dashboard — largest effort, caps remote access story)

## Tracking Notes

- Backlog-first governance applies.
- Implementation commits should reference ticket IDs.
- Tickets 1-6 are focused on local TUI ergonomics and can be done independently.
- Tickets 8-10 form the "remote access" track and should be done in order.
- Ticket 7 (worktrees) is independent and can be scheduled based on need.
- All work happens in the `crates/aster-orch` submodule — follow submodule git workflow.

## Execution Metrics

- Ticket: ORCH-EVO-1
- Owner: TBD
- Complexity: L
- Risk: Medium
- Start:
- End:
- Duration:
- Notes:

- Ticket: ORCH-EVO-2
- Owner: TBD
- Complexity: M
- Risk: Low
- Start:
- End:
- Duration:
- Notes:

- Ticket: ORCH-EVO-3
- Owner: TBD
- Complexity: M
- Risk: Low
- Start:
- End:
- Duration:
- Notes:

- Ticket: ORCH-EVO-4
- Owner: TBD
- Complexity: S
- Risk: Low
- Start:
- End:
- Duration:
- Notes:

- Ticket: ORCH-EVO-5
- Owner: TBD
- Complexity: M
- Risk: Low
- Start:
- End:
- Duration:
- Notes:

- Ticket: ORCH-EVO-6
- Owner: TBD
- Complexity: M
- Risk: Medium
- Start:
- End:
- Duration:
- Notes:

- Ticket: ORCH-EVO-7
- Owner: TBD
- Complexity: L
- Risk: Medium
- Start:
- End:
- Duration:
- Notes:

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

## Closure Evidence

- (To be filled on batch completion)
