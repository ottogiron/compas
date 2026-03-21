# Architecture Decision Records — Compas

## ADR-001: SQLite as sole persistence backend

**Date:** 2024-12
**Status:** Active

SQLite in WAL mode provides concurrent read/write from worker + MCP server processes without external dependencies. Scales to hundreds of threads/executions. No need for a database server.

## ADR-002: Two-process model (worker + MCP server)

**Date:** 2024-12
**Status:** Active

MCP server handles operator-facing tools (dispatch, close, status). Worker handles background execution (polling, triggering backends, writing results). Both share SQLite. Dashboard embeds the worker by default (`--standalone` to opt out).

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

Extracted ticket-tracker to its own repo (`ottogiron/ticket-tracker`). Installed globally via `cargo install`. Generic tool usable across any project — not coupled to aster or compas.

## ADR-006: Standalone repo with independent dev infrastructure

**Date:** 2026-03
**Status:** Active

Extracted compas from aster as a fully independent repository with its own development infrastructure: ticket system, backlogs, pre-commit hooks, skills, governance docs, and MCP server configs.

**Why:** Submodule git workflow (two-step commits, detached HEAD) added friction. Parallel development on aster (compiler) and compas (orchestrator) was blocked by the single-session ticket system. Independent repos enable independent development cadences.

**How it works:**

- Production orch (`compas` MCP server) dispatches agents to work on any repo, including compas itself.
- Dev orch (`compas-dev` MCP server, via `cargo run`) uses a local state directory (`.compas/state/`) for testing MCP changes.
- Both MCP servers are configured globally (user scope) in Claude Code, Codex, and OpenCode — available from any project.
- `make dashboard-dev` runs the dashboard with an embedded worker on the dev DB.

**Trade-off:** Loses the convenience of `cargo test -p compas` from the aster workspace. Gained: independent git history, parallel ticket sessions, no submodule friction, self-contained dev infrastructure.

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

**Why:** The `target_repo_root` is global — all agents share it. When orchestrating work across multiple repos (e.g., aster compiler + compas), agents need different working directories. The original workaround was prompt-based `cd` instructions, which was fragile.

**Design:** Per-agent `workdir` is the low-level primitive. It sets the `current_dir` for the backend CLI process. `workspace: worktree` creates git worktrees from the agent's base workdir. Both are optional — omitting them preserves the existing shared-workspace behavior.

**Deferred alternative:** Project-based config (Option B from the design session — `projects:` section with per-project agents and repo roots) is now tracked in `docs/project/backlog/multi-project.md` (batch MPR). Per-agent `workdir` is the interim solution that solves the immediate need. The multi-project design uses an overlays approach where projects provide repo_root context and per-agent handoff overrides, with `workdir` remaining the underlying primitive.

**Config location:** The production orch config has migrated to `~/.compas/config.yaml` (the new default). See ADR-013. The compas repo retains `.compas/config.yaml` as the **dev** config for testing MCP changes — this is distinct from the production default.

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

## ADR-013: Production Config at ~/.compas/

**Date:** 2026-03
**Status:** Active

The default config location for the production `compas` binary is now `~/.compas/config.yaml`. All subcommands (`worker`, `mcp-server`, `dashboard`, `wait`) fall back to this path when `--config` is not provided.

**Context:** Previously the config was coupled to a specific repository checkout path. This created a discovery problem: every MCP server registration, every `compas` CLI invocation, and every doc example had to hardcode that path. Moving to a machine-installed binary (via `cargo install`) made the old path a portability liability.

**Decision:** Default to `~/.compas/config.yaml`. The `--config` flag remains available to override for non-default setups (e.g., the repo-level dev config at `.compas/config.yaml`).

**Rationale:**

- Neutral, user-scoped location — no dependency on a specific repo being checked out.
- Simplifies MCP server registration: `compas mcp-server` with no flags just works.
- Prepares for multi-project config support (`docs/project/backlog/multi-project.md`) where a single user-level config defines agents across multiple repos via per-agent `workdir`.
- Consistent with Unix conventions for user-scoped tool config (`~/.tool/`).

**Dev config distinction:** The repo-relative `.compas/config.yaml` remains the dev config for testing MCP changes. It is loaded via `make dashboard-dev` or `cargo run` with an explicit `--config` flag, keeping it fully isolated from the production default.

## ADR-014: Config-driven auto-handoff chains

**Date:** 2026-03
**Status:** Active

Operator-mediated dispatch is a bottleneck for multi-step workflows (e.g., implement → review → fix → re-review). Auto-handoff chains let agents route their output to the next agent automatically based on reply intent.

**Decision:** Added a `handoff` config section to agent definitions with `on_response` routing and a `max_chain_depth` safety limit (default: 3).

**Amendment (2026-03):** Simplified from 5 routing fields to 3: `on_response` (now `HandoffTarget` — string or list via serde untagged enum), `handoff_prompt` (custom text prepended to handoff context), `max_chain_depth` (safety limit). Removed agent-side intent management (see ADR-015). Added fan-out: when `on_response` is a list, each target gets its own batch-linked thread. Added `--await-chain` CLI wait flag for blocking until chain and direct fan-out child threads settle (single-depth; grandchildren not tracked).

**Key choices:**

- **Config declares routes, not agents** — the `handoff` section is config on the producing agent, not a property of the consuming agent. This keeps agent definitions self-contained and makes chains visible from the config.
- **Atomic depth check + insert transaction** — `insert_handoff_if_under_depth()` counts existing `handoff`-intent messages and inserts the new one in a single SQL transaction. This prevents TOCTOU races where concurrent executions on the same thread could both pass the depth check before either inserts.
- **Chain depth via message count** — depth is the count of `handoff`-intent messages on the thread, not a counter on the execution. This is durable (survives crashes) and visible in the transcript.
- **Forced operator escalation at limit** — when `max_chain_depth` is reached, a review-request message is inserted for the operator instead of the handoff. The chain stops cleanly and the operator can decide next steps.
- **"operator" as target alias** — setting `on_response` to `"operator"` explicitly stops the chain. Omitting `on_response` has the same effect.
- **Fan-out via batch-linked threads** — when `on_response` is a list, each target gets its own independent thread sharing a batch ID. Parallel execution runs across threads, not within a single thread. The operator is the join point; `orch_batch_status` provides aggregate results. This avoids same-thread parallel execution complexity and keeps the single-active-execution-per-thread invariant.
- **`handoff_prompt` composition** — custom prompt text is prepended first, then the auto-generated context (originating thread ID, agent alias, transcript). This lets the receiving agent read task-specific instructions before the context dump.
- **`HandoffTarget` untagged enum** — YAML `on_response: reviewer` (string) and `on_response: [reviewer, reviewer-2]` (list) both deserialize correctly without a type tag. This preserves backward compatibility for existing configs using the string form.
- **`--await-chain` CLI wait flag** — `compas wait --thread-id <id> --await-chain` polls until the thread's chain AND direct fan-out child threads (linked via `source_thread_id`) have settled. Only direct children are tracked — if a fan-out child itself triggers further fan-out, those grandchildren are not counted (`max_chain_depth` prevents this in practice). Reply message and fan-out thread creation are atomic (single transaction) to prevent the wait loop from seeing the reply without the fan-out.

## ADR-015: Intent simplification — agents don't manage intents

**Date:** 2026-03
**Status:** Active

Agent intent annotation (parsing JSON `{"intent":"review-request",...}` from agent output) was unreliable and created cognitive overhead. Agents had to follow a REPLY PROTOCOL, and `parse_intent_from_text()` attempted to extract structured intents from free-form text — a fragile heuristic.

**Decision:** Removed `parse_intent_from_text()`. All successful agent replies automatically get `response` intent. Routing is exclusively via the `on_response` handoff config field. `HandoffConfig` simplified from 5 intent-based fields to 3 (`on_response` + `handoff_prompt` + `max_chain_depth`). `changes-requested` added to the default `trigger_intents` list so operator change-request dispatches trigger execution without extra config.

**What was removed:**

- `parse_intent_from_text()` function and all its tests
- `on_review_request`, `on_changes_requested`, `on_escalation` handoff fields
- Agent REPLY PROTOCOL requirement — agents reply naturally

**Note:** `HandoffTarget` enum was re-introduced in ORCH-HANDOFF-2 with a different shape (`Single(String)` / `FanOut(Vec<String>)`) for fan-out support. It is not the same as the original `Gated` variant that was removed.

**Rationale:** Agents are pure workers. Intent management and routing are config/operator concerns, not agent concerns. This eliminates a class of bugs where agents produced malformed intent JSON, and simplifies agent prompts by removing protocol overhead.

## ADR-016: Worker singleton guard + dashboard default flip

**Date:** 2026-03
**Status:** Active

**Problem:** Multiple concurrent worker processes cause an orphan-crash hazard. `mark_orphaned_executions_crashed` blanket-marks all in-flight work as crashed when a worker starts, which means a second worker kills the first worker's active executions. The standalone `compas worker` command had no guard — only `dashboard --with-worker` had a lockfile check during spawn.

**Decision:** Fail-fast singleton guard via exclusive lockfile + heartbeat/PID liveness check, enforced in `run_worker()` itself (not just the dashboard spawn path). The guard:

1. Acquires `flock(LOCK_EX | LOCK_NB)` on `{state_dir}/worker.lock`
2. Checks heartbeat freshness + PID liveness via `kill(pid, 0)`
3. Returns a RAII guard struct that holds the file descriptor (lock persists for process lifetime)
4. On failure, returns an actionable error with worker PID and heartbeat age

**Dashboard default flip:** `compas dashboard` now spawns a worker by default (previously required `--with-worker`). A new `--standalone` flag opts out. `--with-worker` is retained as a hidden no-op for backward compatibility.

**Rationale:**

- The dashboard is the primary entry point for most users. Requiring `--with-worker` was a papercut that led to "dispatched work not executing" confusion.
- The singleton guard makes the default safe — if a worker is already running, the dashboard's embedded worker spawn detects it and skips.
- Standalone mode (`--standalone`) is available for monitoring-only dashboards that connect to a separately managed worker.

**Key choices:**

- **Guard in `run_worker()`**, not just `spawn_worker_process()` — covers both `compas worker` and `dashboard --with-worker` paths.
- **`spawn_worker_process()` keeps its pre-flight check** — avoids spawning a child process that would immediately exit due to the guard.
- **`DaemonLockHeld` error enriched** with `worker_id`, PID, and heartbeat age for actionable diagnostics.

## ADR-017: Session resume after crash (early session ID persistence)

**Date:** 2026-03
**Status:** Active

Backend session IDs were only persisted on successful completion (`executor.rs`, inside `if result.success`). When an execution crashed, the session ID was lost, forcing the agent to start a fresh CLI session on re-dispatch instead of resuming conversation context.

**Decision:** Persist the backend session ID mid-stream, within milliseconds of the first backend output line, via the telemetry consumer. Additionally, move the executor's `set_backend_session_id` call out of the success guard as an unconditional safety net. Update `get_last_backend_session_id` to return session IDs from any execution status (not just completed).

**Implementation:**

- Per-backend `extract_session_id_from_line(line)` functions parse the session ID from the first JSONL stdout line (Claude `system/init`, Codex `thread.started`, OpenCode any line with `sessionID`).
- The telemetry consumer (`consume_telemetry`) calls the appropriate extractor on each received line. On first match, it calls `store.set_backend_session_id()` and sets a `session_id_persisted` flag (one-shot per execution).
- The executor persists the session ID unconditionally from `BackendOutput` as a fallback (idempotent write).
- `get_last_backend_session_id` no longer filters on `status = 'completed'`; it uses `COALESCE(finished_at, started_at, queued_at) DESC` ordering and logs when the returned session comes from a non-completed execution.

**Key choices:**

- **Mid-stream over post-execution** because crashes lose post-execution writes. The telemetry channel already receives stdout lines in real time, making it the natural insertion point.
- **One-shot flag** (`session_id_persisted`) avoids redundant DB writes on every stdout line.
- **Safety net in executor** (unconditional write) ensures session ID is captured even if the telemetry consumer missed it (e.g., channel backpressure, backend emits session ID after the init line).
- **No status filter in query** because a crashed execution's session ID is just as valid for resume as a completed one. The backend CLI maintains the same session state regardless of how the orchestrator exited.
- **Gemini excluded** because it is stateless (no session ID concept).
