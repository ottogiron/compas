# Architecture Decision Records — Aster Orchestrator

## ADR-001: SQLite as sole persistence backend

**Date:** 2024-12
**Status:** Active

SQLite in WAL mode provides concurrent read/write from worker + MCP server processes without external dependencies. Scales to hundreds of threads/executions. No need for a database server.

## ADR-002: Two-process model (worker + MCP server)

**Date:** 2024-12
**Status:** Active

MCP server handles operator-facing tools (dispatch, close, status). Worker handles background execution (polling, triggering backends, writing results). Both share SQLite. Dashboard optionally embeds the worker (`--with-worker`).

This separation keeps MCP responses fast and worker execution unblocked.

## ADR-003: Backend CLI abstraction

**Date:** 2025-01
**Status:** Active

All AI backends (Claude, Codex, Gemini, OpenCode) are invoked as CLI subprocesses. The `Backend` trait normalizes args, session management, and output parsing. Adding a new backend means implementing one trait.

This avoids SDK dependencies and works with any tool that has a CLI.

## ADR-004: Parallel ticket sessions

**Date:** 2026-03
**Status:** Active

Moved from single `.session` file to `.sessions/` directory with per-key YAML files. Multiple batches (e.g., compiler + orchestrator) can run concurrently. Pre-commit hook validates any active session (permissive policy).

## ADR-005: Standalone ticket-tracker repo

**Date:** 2026-03
**Status:** Active

Extracted ticket-tracker to its own repo (`ottogiron/ticket-tracker`). Installed globally via `cargo install`. Generic tool usable across any project — not coupled to aster or aster-orch.

## ADR-006: Standalone repo with independent dev infrastructure

**Date:** 2026-03
**Status:** Active

Extracted aster-orch from aster as a fully independent repository with its own development infrastructure: ticket system, backlogs, pre-commit hooks, skills, governance docs, and MCP server configs.

**Why:** Submodule git workflow (two-step commits, detached HEAD) added friction. Parallel development on aster (compiler) and aster-orch (orchestrator) was blocked by the single-session ticket system. Independent repos enable independent development cadences.

**How it works:**
- Production orch (`aster-orch` MCP server) dispatches agents to work on any repo, including aster-orch itself.
- Dev orch (`aster-orch-dev` MCP server, via `cargo run`) uses a local state directory (`.aster-orch/state/`) for testing MCP changes.
- Both MCP servers are configured globally (user scope) in Claude Code, Codex, and OpenCode — available from any project.
- `make dashboard-dev` runs the dashboard with an embedded worker on the dev DB.

**Trade-off:** Loses the convenience of `cargo test -p aster-orch` from the aster workspace. Gained: independent git history, parallel ticket sessions, no submodule friction, self-contained dev infrastructure.

## ADR-007: Graceful worker shutdown via SIGTERM + semaphore drain

**Date:** 2026-03
**Status:** Active

`--with-worker` previously spawned the worker as a fully independent OS process (`process_group(0)`, `kill_on_drop(false)`) that survived dashboard exit indefinitely. This caused orphaned workers running stale code after rebuilds, and heartbeat guards preventing new workers from spawning.

**Decision:** Dashboard sends SIGTERM to the worker on exit. Worker handles SIGTERM (and SIGINT) by breaking its poll loop and draining in-flight executions via semaphore permit acquisition with a timeout of `execution_timeout_secs`.

**Alternatives considered:**
- Kill worker immediately on dashboard exit — rejected because it kills running agent executions mid-task.
- Embed worker in-process (same tokio runtime) — rejected because dashboard exit always kills the worker, even during long executions.

**Accepted residual risk:** If the dashboard crashes (SIGKILL, panic) before the cleanup block runs, the worker remains orphaned. Crash recovery on next startup (`mark_orphaned_executions_crashed`) handles the execution state; the stale process must be killed manually. This is the same behavior as before — the fix only covers the clean exit path.

## ADR-008: Claude backend uses `stream-json` output format

**Date:** 2026-03
**Status:** Active

Switched Claude Code CLI from `--output-format json` (single JSON blob after completion) to `--output-format stream-json` (JSONL events during execution). This is a prerequisite for real-time execution telemetry (EVO-1).

**Why:** With `json`, the orchestrator gets no output until the agent finishes — which can be 10+ minutes. With `stream-json`, Claude emits JSONL events (tool calls, content, result) as they happen, enabling mid-execution progress visibility.

**Parsing contract:** The final line of the JSONL stream is `{"type":"result","result":"...","session_id":"..."}`. The `extract_claude_stream_output()` function scans lines from the end for this result line. If no result line is found, raw stdout is used as fallback.

**Success detection:** An execution is considered successful if the exit code is zero OR a result line was found in the output. This matches the previous behavior where success was declared when a valid JSON result object was present, regardless of exit code.

## ADR-009: Desktop notifications via osascript

**Date:** 2026-03
**Status:** Active

macOS desktop notifications on execution completion/failure via `osascript -e 'display notification ...'`. Consumer subscribes to EventBus in the worker process.

**Key decisions:**
- `osascript` over `notify-rust` crate or `terminal-notifier` — zero dependencies, built into macOS
- Consumer in worker process (always running) rather than dashboard (interactive, may be closed)
- Only notifies on `ExecutionCompleted` — other events are too noisy
- Fire-and-forget via nested `tokio::spawn` — hung `osascript` can't block the consumer loop
- Simple `notifications.desktop: bool` config — no granular per-event toggles
- `#[cfg(target_os = "macos")]` compile-time conditional with no-op on other platforms
- Notification toggle requires worker restart (not live-reloadable)

## ADR-010: Per-agent workdir as interim multi-repo solution

**Date:** 2026-03
**Status:** Active

Added `workdir: Option<PathBuf>` to agent config, allowing agents to work in different repositories without changing the global `target_repo_root`. Combined with `workspace: worktree | shared` for git worktree isolation per-thread.

**Why:** The `target_repo_root` is global — all agents share it. When orchestrating work across multiple repos (e.g., aster compiler + aster-orch), agents need different working directories. The original workaround was prompt-based `cd` instructions, which was fragile.

**Design:** Per-agent `workdir` is the low-level primitive. It sets the `current_dir` for the backend CLI process. `workspace: worktree` creates git worktrees from the agent's base workdir. Both are optional — omitting them preserves the existing shared-workspace behavior.

**Deferred alternative:** Project-based config (Option B from the design session — `projects:` section with per-project agents and repo roots) was deferred to ORCH-TEAM-6. Per-agent `workdir` is the interim solution that solves the immediate need. When project-based config is implemented, it would set `workdir` on its agents, making `workdir` the underlying primitive either way.

**Config lives in the aster repo:** The production orch config is at `~/workspace/github.com/ottogiron/aster/.aster-orch/config.yaml`. It defines agents for both the aster repo (via `target_repo_root: ..`) and the aster-orch repo (via per-agent `workdir`). The aster-orch repo has its own dev config at `.aster-orch/config.yaml` for testing MCP changes.

## ADR-011: Retry with error classification

**Date:** 2026-03
**Status:** Active

Failed executions with transient errors (network blips, temporary rate limits) are retried automatically when `max_retries > 0` on the agent config. Non-retryable failures (quota exhaustion, auth errors, agent-reported errors) fail immediately regardless of `max_retries`.

**Error classification:** Deny-list strategy — a curated set of error patterns is matched against exit code and stderr output. Anything not on the deny-list is treated as transient and eligible for retry.

**Backoff:** Exponential via store re-enqueue. A new queued execution row is inserted with a `retry_after` timestamp computed as `now + retry_backoff_secs * 2^attempt`. The `claim_next_execution()` SQL query gates on `retry_after IS NULL OR retry_after <= now`, so the poll loop ignores retries until their backoff expires. No synchronous sleep in the worker loop.

**Thread lifecycle:** The thread remains Active during retries. It only transitions to Failed when all retries are exhausted. Each retry creates a new execution row; `attempt_number` tracks the sequence.

**Defaults:** `max_retries: 0` (disabled), `retry_backoff_secs: 30`.

## ADR-012: Execution telemetry pipeline

**Date:** 2026-03
**Status:** Active

Backend stdout lines are streamed through a `sync_channel(128)` from the reader thread (inside `spawn_blocking`) to a tokio consumer task. The consumer parses JSONL events, batch-inserts them into the `execution_events` table, and emits `ExecutionProgress` events on the EventBus for live dashboard updates.

**Architecture:** `sync_channel` bridges the blocking reader thread and the async consumer without blocking the tokio runtime. Channel bound of 128 provides backpressure — if the consumer falls behind, the reader blocks until space is available.

**Backend-specific parsers:** Claude, Codex, and OpenCode backends each have JSONL parsers that extract typed events (tool calls, file edits, content blocks, result lines). Gemini has no parser (single JSON output, no streaming). The consumer silently returns for unsupported backends.

**Storage:** Batch SQLite inserts reduce write amplification. Events are queryable via `orch_execution_events` MCP tool, enabling mid-execution progress inspection without waiting for the agent to finish.

**EventBus emission:** `ExecutionProgress` events are broadcast on the shared EventBus so the dashboard can update the active execution view in real time without polling.
