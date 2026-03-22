# Interactive Agent Sessions

Status: Active
Owner: otto
Created: 2026-03-22

## Scope Summary

- Add optional tmux-based execution mode for interactive access to running agents
- Enable operator to attach to running agent terminals, approve permission prompts, and steer mid-execution
- Preserve existing subprocess execution as default — tmux is opt-in
- Phased delivery: validation spike → tmux batch mode → full interactive mode

## Context

### Problem

Agents are fire-and-forget black boxes. The operator dispatches a task, the worker spawns a CLI subprocess via `spawn_blocking`, and the only interaction channel is the post-execution reply message. Mid-execution, the operator can *observe* (via `orch_execution_events`, dashboard progress line, log viewer) but cannot *interact* — cannot type into a running agent, approve a permission prompt, steer direction, or inject context.

The most common developer pain point with autonomous agents is **agents silently hanging on permission prompts**. Compas's dashboard partially addresses visibility (progress lines, notifications), but when an agent is stuck on `[Y/n]`, the operator has no way to respond.

### Architecture evaluation

Completed 2026-03-22 by `compas-architect` (thread `01KMAYECE633A73AR9XG6MJQCB`). Four options evaluated; modified Option D (tmux hybrid) selected.

### Key architectural decisions

- `TmuxDriver` module in `src/backend/tmux.rs` — all tmux CLI calls isolated in one file (create session, pipe-pane, wait-for, kill, send-keys, attach, list, has-session, version check)
- FIFO (named pipe) for telemetry transport — `tmux pipe-pane` writes to FIFO, reader thread feeds existing `sync_channel(128)` pipeline unchanged
- `subprocess` remains default `execution_mode` — tmux is opt-in via config
- Phase 1 keeps `--print` mode inside tmux (structured JSONL telemetry preserved)
- Phase 2 drops `--print` for full interactive mode (degraded telemetry: no cost/token tracking, best-effort event parsing)
- Kill tmux sessions immediately on completion (no configurable retention in v1)
- `ProcessHandle` is a module boundary for code organization, NOT a swappability abstraction
- tmux coupling is intentional — document in a future ADR

## Ticket ISESS-SPIKE — Interactive Sessions Validation Spike

- Goal: Validate the tmux-based execution approach before committing to full implementation scope.
- In scope:
  - Prototype `TmuxDriver`: `tmux new-session -d -s test -x 200 -y 50`, `tmux send-keys`, `tmux pipe-pane`, `tmux wait-for`, `tmux kill-session`
  - Create FIFO, connect to `BufReader`, verify JSONL output from `--print` mode passes through `pipe-pane` cleanly
  - Verify `tmux wait-for -S` correctly signals on session exit
  - Verify dashboard suspend/resume (`terminal.restore()` → `tmux attach` → `terminal.init()`)
  - Test concurrent tmux sessions (10+) and measure PTY allocation on macOS
  - Document findings in a spike report
- Out of scope:
  - Production implementation
  - Config changes
  - MCP tools
- Dependencies: None (soft: EVO-16 for session ID concepts)
- Acceptance criteria:
  - FIFO reader successfully parses JSONL from `pipe-pane` output
  - `tmux wait-for` reliably detects session exit
  - Dashboard suspend/resume cycle works without terminal corruption
  - 10+ concurrent tmux sessions work within macOS PTY limits
  - Spike report documents findings and go/no-go recommendation
- Verification:
  - Spike report reviewed by operator
- Status: Todo
- Complexity: S-M
- Risk: Low

## Ticket ISESS-1 — TmuxDriver + `execution_mode: tmux` Config

- Goal: Add tmux session management as an alternative execution backend, gated behind config.
- In scope:
  - `src/backend/tmux.rs` — `TmuxDriver` struct with all tmux CLI interactions
  - Config field: `execution_mode: "subprocess" | "tmux"` (default: `subprocess`)
  - `TmuxDriver::create_session(name, cmd, workdir)` — `tmux new-session -d`
  - `TmuxDriver::wait_for_exit(session, timeout)` — `tmux wait-for -S`
  - `TmuxDriver::kill_session(session)` — `tmux kill-session -t`
  - `TmuxDriver::has_session(session)` — existence check
  - `TmuxDriver::send_keys(session, text)` — for initial command injection
  - Integration with executor lifecycle in `src/worker/executor.rs` — dispatch on `execution_mode`
  - Session naming convention: `compas-{execution_id}`
- Out of scope:
  - Telemetry pipeline changes (ISESS-2)
  - Operator attachment (ISESS-3)
  - Interactive mode (ISESS-5)
- Dependencies: ISESS-SPIKE (spike must validate approach)
- Acceptance criteria:
  - `execution_mode: tmux` creates tmux sessions for executions
  - `execution_mode: subprocess` behavior is unchanged (default)
  - Executor lifecycle (mark executing → wait → mark complete/failed) works identically for both modes
  - `make verify` passes
- Verification:
  - Integration test with tmux mode enabled
  - `make verify`
- Status: Todo (gated on ISESS-SPIKE)
- Complexity: M
- Risk: Medium

## Ticket ISESS-2 — FIFO Telemetry Pipeline for tmux Mode

- Goal: Connect tmux output to the existing telemetry pipeline via a FIFO (named pipe), preserving the `sync_channel(128)` consumer unchanged.
- In scope:
  - Create FIFO before session start: `mkfifo /tmp/compas-{exec_id}.pipe`
  - `TmuxDriver::pipe_to_fifo(session, fifo_path)` — `tmux pipe-pane -o "cat > {fifo}"`
  - Reader thread opens FIFO via `BufReader`, feeds lines to `sync_channel(128)`
  - Reader thread must open FIFO for reading BEFORE `pipe_to_fifo()` is called to prevent deadlock
  - Downstream `consume_telemetry` in `loop_runner.rs` is unchanged
  - FIFO cleanup after reader exits
  - Session ID extraction, cost tracking, tool events all work through FIFO path
- Out of scope:
  - Interactive mode output parsing (ISESS-7)
- Dependencies: ISESS-SPIKE, ISESS-1
- Acceptance criteria:
  - Telemetry events (session ID, cost, tool calls) are captured identically in tmux mode vs subprocess mode
  - `execution_events` table populated correctly for tmux-mode executions
  - Dashboard progress line updates in real-time for tmux-mode executions
  - Reader thread is established before `pipe_to_fifo()` is invoked; no deadlock on session start
  - `make verify` passes
- Verification:
  - Integration test comparing tmux vs subprocess telemetry output
  - `make verify`
- Status: Todo (gated on ISESS-SPIKE)
- Complexity: M
- Risk: Medium

## Ticket ISESS-3 — `compas attach` CLI + Dashboard `a` Key

- Goal: Allow operator to attach to a running agent's tmux session for live terminal access.
- In scope:
  - `compas attach --execution-id <id>` CLI command — resolves tmux session name, calls `TmuxDriver::attach()`
  - `compas attach --thread-id <id>` — finds active execution for the thread
  - Dashboard: `a` key on running execution → `terminal.restore()` → `tmux attach` → `terminal.init()` on detach
  - Pre-attach validation: check session exists, show error if execution already completed
  - Print guidance message before attach: "Attached to compas-{exec_id}. Detach with Ctrl+B D to return."
  - Scopeguard for terminal restore on attach failure
- Out of scope:
  - Quick-switching between sessions (tmux native switching is sufficient)
  - Mouse-click attach (GAP-5 integration)
- Dependencies: ISESS-1
- Acceptance criteria:
  - `compas attach --thread-id <id>` opens live terminal of running agent
  - Dashboard `a` key suspends TUI, attaches to tmux, restores TUI on detach
  - Attaching to completed/nonexistent execution shows clear error
  - Terminal state is clean after attach/detach cycle
  - `make verify` passes
- Verification:
  - Manual test of attach/detach cycle from both CLI and dashboard
  - `make verify`
- Status: Todo (gated on ISESS-1)
- Complexity: S
- Risk: Low

## Ticket ISESS-4 — `compas doctor` tmux Validation

- Goal: Detect tmux availability and version when `execution_mode: tmux` is configured.
- In scope:
  - `TmuxDriver::version()` — parse `tmux -V` output
  - `compas doctor` check: when `execution_mode: tmux`, verify tmux is installed and version >= 1.8 (`wait-for` support)
  - Actionable error message if tmux missing: "tmux is required for execution_mode: tmux. Install with: brew install tmux (macOS) or apt install tmux (Linux)"
  - Skip check when `execution_mode: subprocess`
- Out of scope:
  - tmux configuration validation
- Dependencies: ISESS-1 (config field must exist)
- Acceptance criteria:
  - `compas doctor` reports tmux status when tmux mode configured
  - Missing tmux shows actionable install instructions
  - Old tmux version shows minimum version requirement
  - `make verify` passes
- Verification:
  - Test with tmux installed, uninstalled, and old version
  - `make verify`
- Status: Todo (gated on ISESS-1)
- Complexity: S
- Risk: Low

## Ticket ISESS-5 — Interactive Execution Mode

- Goal: Support full interactive agent execution (no `--print` flag) inside tmux sessions, enabling real terminal interaction.
- In scope:
  - Config value: `execution_mode: "interactive"` (or per-agent `interactive: true`)
  - Backend invocation without `--print` — full conversational mode
  - Agent sees a real PTY (tmux provides this) — permission prompts, progress bars, colors all work
  - Degraded telemetry: no JSONL parsing. Pipe-pane output is raw terminal text.
  - Exit detection via `tmux wait-for` (no result JSON to parse)
  - Document explicitly: cost/token tracking unavailable in interactive mode
- Out of scope:
  - Restoring structured telemetry in interactive mode
- Dependencies: ISESS-1, ISESS-2 (tmux batch mode must work first)
- Acceptance criteria:
  - Agent runs in full interactive mode inside tmux
  - Operator can attach and interact (type, approve prompts, steer)
  - Execution lifecycle (start → wait → complete/fail) works correctly
  - `orch_health` and `orch_status` correctly report interactive-mode executions
  - Documentation states telemetry limitations
  - `make verify` passes
- Verification:
  - Manual test of interactive dispatch + attach + interact + complete
  - `make verify`
- Status: Todo (gated on ISESS-1, ISESS-2)
- Complexity: L
- Risk: High

## Ticket ISESS-6 — `orch_inject` + `orch_approve` MCP Tools

- Goal: Allow the operator to inject text into a running agent's terminal via MCP, without manually attaching.
- In scope:
  - `orch_inject(execution_id, text)` MCP tool — wraps `TmuxDriver::send_keys(session, text)`
  - `orch_approve(execution_id)` MCP tool — sugar for `orch_inject(execution_id, "y")`
  - Only works for tmux/interactive mode executions — clear error for subprocess mode
  - Validation: execution must be in `Executing` state
- Out of scope:
  - Multi-character approval sequences
  - Automated approval
- Dependencies: ISESS-5 (interactive mode must exist for inject to be useful)
- Acceptance criteria:
  - `orch_inject` sends text to running agent's terminal
  - `orch_approve` sends "y" to approve permission prompts
  - Clear error when used against subprocess-mode execution
  - `make verify` passes
- Verification:
  - Integration test: dispatch interactive → inject text → verify in pipe-pane output
  - `make verify`
- Status: Todo (gated on ISESS-5)
- Complexity: S
- Risk: Low

## Ticket ISESS-7 — Permission Prompt Detection + `ExecutionAwaitingInput` Event

- Goal: Detect when an agent is waiting for operator input and surface it as a structured event.
- In scope:
  - Regex patterns on pipe-pane output for common prompts: `[Y/n]`, `[y/N]`, `Allow?`, `(yes/no)`, `Do you want to`
  - New `ExecutionAwaitingInput { execution_id, prompt_text }` event on EventBus
  - Dashboard: running execution shows "Awaiting input" indicator
  - Desktop notification: "Agent {alias} is waiting for approval"
  - `compas wait` exits with distinct exit code (2) when timeout expires while agent is awaiting input
- Out of scope:
  - Auto-approval
  - Custom prompt patterns
- Dependencies: ISESS-5 (interactive mode + pipe-pane infrastructure)
- Acceptance criteria:
  - Permission prompts detected from pipe-pane output
  - `ExecutionAwaitingInput` event fires on EventBus
  - Dashboard shows "Awaiting input" status
  - `compas wait` distinguishes timeout-while-awaiting from timeout-while-stuck
  - `make verify` passes
- Verification:
  - Integration test with mock permission prompt
  - `make verify`
- Status: Todo (gated on ISESS-5)
- Complexity: M
- Risk: Medium

## Ticket ISESS-8 — tmux Session Crash Recovery

- Goal: On worker restart, detect orphaned tmux sessions from crashed executions and reattach telemetry.
- In scope:
  - `TmuxDriver::list_compas_sessions()` — scan for `compas-*` sessions on startup
  - Match orphaned sessions to execution rows by naming convention (`compas-{exec_id}`)
  - Reconnect FIFO reader for still-running sessions to capture new output (output produced during downtime window is lost — FIFO does not buffer)
  - Resume `consume_telemetry` consumer for still-running sessions; finalize completed-during-downtime sessions using DB state + tmux session exit status only (no FIFO replay)
- Out of scope:
  - Recovering conversational context (that's EVO-16's domain)
- Dependencies: ISESS-1 (tmux sessions must exist)
- Acceptance criteria:
  - Worker restart detects orphaned tmux sessions
  - Telemetry capture resumes for still-running sessions
  - Completed-during-downtime sessions are finalized using DB state and session exit status (telemetry during downtime window is best-effort — data may be lost)
  - `make verify` passes
- Verification:
  - Integration test: start tmux execution → kill worker → restart worker → verify recovery
  - `make verify`
- Status: Todo (gated on ISESS-1)
- Complexity: M
- Risk: Medium

## Deferred

### WebSocket Terminal Proxy

- Batch: To be created when MFE-2 (HTTP API) is stable.
- Summary: WebSocket endpoint `/api/executions/{id}/terminal` for web-based terminal access. Uses xterm.js on the client side. For Tauri desktop app and future web UI.
- Estimated effort: M-L

### Native PTY Backend

- Batch: To be created if web/desktop frontends need direct PTY access without tmux.
- Summary: `portable-pty` crate for native PTY allocation. Worker holds master FD, feeds WebSocket bridge. Relevant when MFE-2 + Tauri materializes. tmux mode continues for CLI operators.
- Estimated effort: L

## Execution Order

1. ISESS-SPIKE (validation spike — gates everything)
2. ISESS-1 (TmuxDriver + config)
3. ISESS-2 (FIFO telemetry — can parallel with ISESS-3/4)
4. ISESS-3 (compas attach + dashboard)
5. ISESS-4 (doctor check)
6. ISESS-5 (interactive mode — starts Phase 2)
7. ISESS-6 (inject/approve tools)
8. ISESS-7 (prompt detection)
9. ISESS-8 (crash recovery — can parallel with ISESS-6/7)

## Cross-Backlog Dependencies

- EVO-16 (soft prerequisite — session ID persistence concepts)
- GAP-1 (circuit breaker must distinguish `awaiting_input` from backend failure)
- GAP-5 (mouse click on running execution could trigger attach)
- EVO-6 (dashboard `d` dispatch and `a` attach are complementary hotkeys)
- MFE-2 (WebSocket terminal proxy deferred until HTTP API exists)
- ADR-008, ADR-012, ADR-017 (related architectural decisions)

## Tracking Notes

- Backlog-first governance applies.
- Implementation commits should reference ticket IDs.
- Architecture evaluation: thread `01KMAYECE633A73AR9XG6MJQCB` (compas-architect).
- Phased delivery: ISESS-SPIKE gates Phase 1 (ISESS-1/2/3/4), Phase 1 gates Phase 2 (ISESS-5/6/7/8).
- Phase 2 deferred items: WebSocket terminal proxy, native PTY backend.
- Record scope changes/deferrals here.

## Execution Metrics

- Ticket: ISESS-SPIKE
- Owner: (pending)
- Complexity: S-M
- Risk: Low
- Start: (pending)
- End: (pending)
- Duration: (pending)
- Notes: (pending)

- Ticket: ISESS-1
- Owner: (pending)
- Complexity: M
- Risk: Medium
- Start: (pending)
- End: (pending)
- Duration: (pending)
- Notes: (pending)

- Ticket: ISESS-2
- Owner: (pending)
- Complexity: M
- Risk: Medium
- Start: (pending)
- End: (pending)
- Duration: (pending)
- Notes: (pending)

- Ticket: ISESS-3
- Owner: (pending)
- Complexity: S
- Risk: Low
- Start: (pending)
- End: (pending)
- Duration: (pending)
- Notes: (pending)

- Ticket: ISESS-4
- Owner: (pending)
- Complexity: S
- Risk: Low
- Start: (pending)
- End: (pending)
- Duration: (pending)
- Notes: (pending)

- Ticket: ISESS-5
- Owner: (pending)
- Complexity: L
- Risk: High
- Start: (pending)
- End: (pending)
- Duration: (pending)
- Notes: (pending)

- Ticket: ISESS-6
- Owner: (pending)
- Complexity: S
- Risk: Low
- Start: (pending)
- End: (pending)
- Duration: (pending)
- Notes: (pending)

- Ticket: ISESS-7
- Owner: (pending)
- Complexity: M
- Risk: Medium
- Start: (pending)
- End: (pending)
- Duration: (pending)
- Notes: (pending)

- Ticket: ISESS-8
- Owner: (pending)
- Complexity: M
- Risk: Medium
- Start: (pending)
- End: (pending)
- Duration: (pending)
- Notes: (pending)

## Closure Evidence

- (pending — update as tickets complete)
- Verification:
  - `make verify`: (pending)
- Deferred:
  - WebSocket terminal proxy — deferred until MFE-2 (HTTP API) is stable
  - Native PTY backend — deferred until web/desktop frontends need direct PTY access
