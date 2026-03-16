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

**Config location:** The production orch config has migrated to `~/.aster-orch/config.yaml` (the new default). See ADR-013. The aster-orch repo retains `.aster-orch/config.yaml` as the **dev** config for testing MCP changes — this is distinct from the production default.

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

## ADR-013: Production Config at ~/.aster-orch/

**Date:** 2026-03
**Status:** Active

The default config location for the production `aster_orch` binary is now `~/.aster-orch/config.yaml`. All subcommands (`worker`, `mcp-server`, `dashboard`, `wait`) fall back to this path when `--config` is not provided.

**Context:** Previously the config was coupled to the aster repo at `~/workspace/github.com/ottogiron/aster/.aster-orch/config.yaml`. This created a discovery problem: every MCP server registration, every `aster_orch` CLI invocation, and every doc example had to hardcode that path. Moving to a machine-installed binary (via `cargo install`) made the old path a portability liability.

**Decision:** Default to `~/.aster-orch/config.yaml`. The `--config` flag remains available to override for non-default setups (e.g., the repo-level dev config at `.aster-orch/config.yaml`).

**Rationale:**
- Neutral, user-scoped location — no dependency on a specific repo being checked out.
- Simplifies MCP server registration: `aster_orch mcp-server` with no flags just works.
- Prepares for multi-project config support (ORCH-TEAM-6) where a single user-level config defines agents across multiple repos via per-agent `workdir`.
- Consistent with Unix conventions for user-scoped tool config (`~/.tool/`).

**Dev config distinction:** The repo-relative `.aster-orch/config.yaml` remains the dev config for testing MCP changes. It is loaded via `make dashboard-dev` or `cargo run` with an explicit `--config` flag, keeping it fully isolated from the production default.

## ADR-014: Config-driven auto-handoff chains

**Date:** 2026-03
**Status:** Active

Operator-mediated dispatch is a bottleneck for multi-step workflows (e.g., implement → review → fix → re-review). Auto-handoff chains let agents route their output to the next agent automatically based on reply intent.

**Decision:** Added a `handoff` config section to agent definitions with intent-based routing fields (`on_response`, `on_review_request`, `on_changes_requested`, `on_escalation`) and a `max_chain_depth` safety limit (default: 3).

**Key choices:**

- **Config declares routes, not agents** — the `handoff` section is config on the producing agent, not a property of the consuming agent. This keeps agent definitions self-contained and makes chains visible from the config.
- **Untagged enum for `HandoffTarget`** — `HandoffTarget` is `#[serde(untagged)]` with `Simple(String)` and `Gated { target, gate, gate_timeout_secs }` variants. Simple targets work today; gated targets parse without error but are rejected at validation, providing forward-compatibility for Phase 2 gated handoffs.
- **Atomic depth check + insert transaction** — `insert_handoff_if_under_depth()` counts existing `handoff`-intent messages and inserts the new one in a single SQL transaction. This prevents TOCTOU races where concurrent executions on the same thread could both pass the depth check before either inserts.
- **Chain depth via message count** — depth is the count of `handoff`-intent messages on the thread, not a counter on the execution. This is durable (survives crashes) and visible in the transcript.
- **Forced operator escalation at limit** — when `max_chain_depth` is reached, a review-request message is inserted for the operator instead of the handoff. The chain stops cleanly and the operator can decide next steps.
- **"operator" as target alias** — setting a route to `"operator"` explicitly stops the chain for that intent. Omitting the route has the same effect.
