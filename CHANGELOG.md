# Changelog

All notable changes to Compas are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Fixed

- Worktree handoff bug: non-worktree agents on the same thread now inherit the thread's worktree path when targeting the same repo, so reviewers see the dev agent's changes instead of the unchanged main repo. `workspace: shared` explicitly opts out of inheritance.
- Conversation view: code block closing brace no longer merges with the next content element
- Conversation view: ordered (numbered) lists now render as `1. 2. 3.` instead of bullets

### Changed

- Default worktree location changed from `{repo_root}/../.compas-worktrees/` to `{repo_root}/.compas-worktrees/` (inside repo, gitignored). Operators with existing worktrees at the old location should run `git worktree prune`.
- Conversation view: code blocks now have 2-space indent and subtle card background for visual distinction
- Conversation view: heading levels have distinct visual weight (H1 accent, H2 bright, H3+ normal)
- `[c]` conversation shortcut now works from History tab and batch drill views, not just Ops
- Shortened thread IDs in Ops view (show last 8 chars wide / 6 narrow instead of first 16)
- Added column separators between agent, summary, and batch columns
- Progress summary now shows tool names instead of raw API IDs
- Batch stats use readable labels with semantic colors
- Ops view: agent column widened (18/14 chars) with truncation; summary column is now elastic (fills remaining width, clamped 10–60 chars)

### Added

- Delayed dispatch via `scheduled_for` parameter on `orch_dispatch` (SCHED-2): operators can schedule executions for a future time using an ISO 8601 timestamp. The worker defers pickup until the scheduled time via the new `eligible_at` / `eligible_reason` columns on the executions table.
- `orch_close` accepts optional `merge` field to atomically queue a merge with the close, eliminating the race between worktree cleanup and `orch_merge`
- MCP tools: `orch_merge`, `orch_merge_status`, `orch_merge_cancel` for merge queue operations (MERGE-4). Includes `count_merge_ops_by_status` and `count_queued_merge_ops` store methods for accurate aggregate counts
- Worker: merge queue polling, crash recovery, and stale merge detection (MERGE-3)
- Dashboard cost/token visibility (OBS-04): Ops footer appends `│ Cost: $X.XX  Tok: NK/NK` when cost or token data exists; Agents tab cards show a per-agent cost/token row between the active count and recent executions. Format helpers `format_tokens` and `format_cost_usd` added to `dashboard::views`.
- CLI: `compas wait-merge --op-id <id>` blocking wait for merge completion (MERGE-6)
- Generic backend registry wiring and documentation (GBE-3): `build_backend_registry()` now iterates `config.backend_definitions` and registers each as a `GenericBackend`; `compas doctor` validates generic backend commands on PATH; README documents `backend_definitions` with examples; ADR-022 records the config-driven generic backend design
- ADR-019: Merge queue for worktree branch integration — documents FIFO queue model, temporary worktree isolation, per-target-branch serialization, merge timeout (30s), eligible thread statuses, no-push-in-v1 decision, and worktree cleanup guard
- Config: Added `merge_timeout_secs` (default 30) and `default_merge_strategy` (default "merge") to orchestration config
- Store: Added `merge_operations` table and store methods for merge queue (MERGE-1)
- MergeExecutor: temporary-worktree-based merge execution with conflict detection (MERGE-2)
- `summary` field on threads — optional short one-liner (~80 chars) set via `orch_dispatch`, visible in `orch_status`, `orch_tasks`, `orch_diagnose`, `orch_transcript`, `orch_poll`, `orch_batch_status`, and the TUI dashboard
- Tool and cost aggregation queries (OBS-02): `tool_call_counts`, `tool_error_rates`, `tool_usage_by_agent`, `cost_summary`, and `cost_by_agent` store methods aggregate `execution_events` and `executions` into per-tool and per-agent observability metrics. New `ToolCallStat`, `CostSummary`, and `AgentCostSummary` structs carry the results. All methods return empty/zero results on empty tables.
- Execution cost/token telemetry (OBS-01): `cost_usd`, `tokens_in`, `tokens_out`, `num_turns` columns on `executions` table; `tool_name` column on `execution_events`. Claude backend extracts cost and token data from the result event. Codex extracts token counts from `turn.completed` usage (independently tracked per direction). Gemini extracts token counts from `result.stats`. Tool call/result events now carry `tool_name`. Claude `user` events with `tool_result` content blocks emit `tool_result` execution events. Codex `item.started`/`item.completed` command_execution events emit tool_call/tool_result pairs. Gemini stream parser (`parse_gemini_stream_line`) added and wired into telemetry consumer. `truncate_detail` utility consolidated in `backend/mod.rs`. All new columns nullable — old data unbroken.
- Config-driven generic backend definitions (`backend_definitions`) — define CLI-based backends entirely in YAML with template variables (`{{instruction}}`, `{{model}}`, `{{session_id}}`), output format parsing (plaintext/json/jsonl), optional session resume, custom ping commands, and env var stripping (GBE-1)
- `GenericBackend` implementing the `Backend` trait for config-defined backends — template substitution in CLI args, plaintext/json/jsonl output parsing with configurable result and session ID field extraction, session resume via `ResumeConfig`, `env_remove` composition with per-agent env, and reuse of `spawn_cli`/`kill_process`/`ProcessTracker` from process infrastructure (GBE-2)
- Hook EventBus integration (`spawn_hook_consumer`): hooks now fire in the worker process on `ExecutionStarted`, `ExecutionCompleted`, `ThreadStatusChanged("Completed")`, and `ThreadStatusChanged("Failed")` events. Each hook group runs in a separate `spawn_blocking` task to avoid blocking the subscriber loop. Hook config is re-read on every event for hot-reload support — add or remove hooks without restarting the worker (HOOKS-2)
- HOOKS-3: Lifecycle hooks documentation, example scripts (`examples/hooks/notify-slack.sh`, `examples/hooks/log-to-file.sh`), and `compas doctor` hook-command existence check
- Lifecycle hooks config schema and execution engine (`HooksConfig`, `HookEntry`, `HookRunner`): add a `hooks` section to `config.yaml` to fire shell commands at `on_execution_started`, `on_execution_completed`, `on_thread_closed`, and `on_thread_failed` events. Each hook receives event JSON on stdin; failures are logged as warnings and never affect the execution path (HOOKS-1)
- `compas doctor` command: pre-flight validation with ordered checklist (config, target repo, state dir, backend CLIs, backend pings, worker heartbeat, MCP registration), actionable fix suggestions, `--fix` for auto-registering MCP servers, and exit code 0/1 based on pass/fail
- `compas setup-mcp` command: auto-register compas as an MCP server in coding tools (Claude Code, Codex, OpenCode, Gemini) with `--tool` filter, `--remove` for unregistration, `--dry-run` preview, and idempotent behavior. Includes shared `detection` module for tool discovery reusable by future CLI commands.
- `compas init` command: interactive and non-interactive config scaffolding with backend detection, overwrite protection, and commented/minimal YAML output
- Improved config error messages: `load_config` now reports the missing file path and suggests `compas init` instead of a bare OS error
- Session resume after crash (ADR-017): backend session IDs are now persisted mid-stream within milliseconds of first output line, enabling agents to resume their CLI session after a crashed execution instead of starting fresh. `get_last_backend_session_id` returns session IDs from any execution status, not just completed.
- `orch_health` parallel pings with TTL cache: backend pings now run concurrently via `JoinSet` (total time drops from N*latency to max(latency)), cached results are returned within the configurable `ping_cache_ttl_secs` window (default 60s), and a `cached` field in per-agent output indicates whether the result is fresh
- Worktree uncommitted change detection — executor appends `## Worktree Status` (porcelain status + diff stat) to agent output when workspace is `worktree` and the worktree has uncommitted changes, giving downstream reviewers visibility into filesystem modifications
- Worker singleton guard — fail-fast lockfile + heartbeat/PID check prevents multiple workers from running simultaneously, avoiding orphan-crash hazard (ADR-016)
- `--standalone` flag for `dashboard` — opt out of the embedded worker when monitoring only
- `orch_read_log` MCP tool — paginated access to execution log files with offset/limit/tail support, falls back to output_preview when log file is unavailable
- Orphan backend CLI detection — persist PIDs in executions table, kill still-alive orphan processes on worker startup before marking crashed
- Embedded wait guidance in `orch_dispatch` and `orch_poll` tool descriptions; dispatch response now includes `next_step` CLI command with templated thread/message IDs
- CONTRIBUTING.md, CODE_OF_CONDUCT.md, SECURITY.md, and GitHub issue/PR templates

### Fixed

- Worktree cleanup safety guard (ADR-018) — worker loop checks `worktree_status` (tri-state: clean / dirty / git-error) before removing a worktree; dirty worktrees and git-failure cases are both skipped with a warning (thread ID, path, branch) and retried next cycle, preventing data loss when an operator closes a thread before merging the branch
- `--await-chain` TOCTOU fix: re-fetch messages once when chain settles to capture responses inserted between the messages query and the pending-work check

### Changed

- README: rewrite Quick Start to use `compas init` + `compas setup-mcp` + `compas doctor` (4-step flow), move manual config/MCP setup to collapsible details section, reference `examples/config-generic.yaml`
- README: add Config Patterns section (multi-repo agent teams, cross-cutting agents), add `role` and `models` fields to config reference, fix stale `orch-dev` alias in examples
- Renamed `orch-reviewer` agent to `compas-reviewer` in dispatch skill and config examples
- **Renamed config field `target_repo_root` → `default_workdir`** — the field is the default working directory fallback for agents without a per-agent `workdir`; old key `target_repo_root` still works via serde alias for backward compatibility
- README audit: fix dashboard keybinding tables (log viewer, add conversation view), correct worktree path, add `changes-requested` to trigger\_intents examples, document `compas wait` flags/exit codes, add undocumented config fields (worktree\_dir, database, timeout\_secs, env), note Gemini stateless limitation, note prompt\_file precedence, document live config reload, simplify Quick Start config
- **Rebranded from aster-orch to compas** — new package name, binary, config paths (`~/.compas/`), and documentation
- Licensed under MIT OR Apache-2.0
- `dashboard` now spawns an embedded worker by default; `--with-worker` is a hidden no-op, `--standalone` disables it (ADR-016)
- `--await-chain` now waits for fan-out child threads to settle before returning (ADR-014 Phase 2)
- Fan-out child threads linked via `source_thread_id` column instead of batch_id heuristics
- Reply message and fan-out thread creation are atomic (single transaction) to prevent race conditions

## [0.2.0]

### Added

- Config-driven auto-handoff chains with depth limit and forced operator escalation (ADR-014)
- Execution telemetry pipeline — real-time JSONL streaming from backends with `execution_events` table and `orch_execution_events` MCP tool (ADR-012)
- Retry with error classification — transient errors retried with exponential backoff, non-retryable failures fail immediately (ADR-011)
- Per-agent workdir and git worktree isolation for multi-repo orchestration (ADR-010)
- Desktop notifications on execution completion via osascript on macOS (ADR-009)
- Backend session continuity — Claude `-r`, Codex `exec resume`, OpenCode `-s`, session IDs persisted in SQLite
- Unified `BackendOutput` struct — normalized output and intent parsing across all 4 backends
- Event broadcast channel — `tokio::broadcast` EventBus for push-based dashboard and notification updates (ADR-012)
- Dashboard "Currently Working On" — live per-execution progress summary in Ops tab
- Thread conversation view in dashboard — full message transcript with chat-like rendering
- Prompt version hashing — SHA-256 of resolved prompt stored per execution for correlation
- `handoff_prompt` field for custom handoff message injection
- `--await-chain` CLI wait flag — polls until chain settles with no pending work

### Changed

- Intent simplification — agents don't manage intents, all replies default to `response`; `parse_intent_from_text()` removed (ADR-015)
- Default config path moved to `~/.compas/config.yaml` (ADR-013)
- Claude backend uses `stream-json` output format for real-time telemetry (ADR-008)
- Graceful worker shutdown via SIGTERM + semaphore drain on dashboard exit (ADR-007)
- HandoffConfig simplified from 5 routing fields to `on_response` + `max_chain_depth`, later extended with `handoff_prompt`
- Dashboard source-based message coloring — operator (accent), agent (green), system (dim) replaces intent-based coloring
- Ops tab redesign — context panel removed, inline detail rows, responsive columns at narrow widths, empty section collapse
- Conversation view polish — markdown rendering via pulldown-cmark, scroll fix for wrapped lines, left-side indicator border

### Tooling

- Standalone repo with independent dev infrastructure — dual MCP servers (`compas` production, `compas-dev` for testing), skills, pre-commit hooks, repo-level `.mcp.json` (ADR-006)
- Parallel ticket sessions via `.sessions/` directory with per-key YAML files (ADR-004)
- Standalone ticket-tracker extracted to `ottogiron/ticket-tracker` (ADR-005)
- Markdown linting (`markdownlint-cli2`) formalized in `make verify` gate and CI

## [0.1.0]

### Added

- SQLite persistence backend with WAL mode for concurrent MCP server + worker access (ADR-001)
- Two-process model: MCP server for operator tools, worker for background execution (ADR-002)
- Backend CLI abstraction for Claude, Codex, Gemini, OpenCode via the `Backend` trait (ADR-003)
- MCP tools: dispatch, close, status, poll, diagnose, transcript, health, metrics, batch\_status, tasks, worktrees
- TUI dashboard (ratatui) for monitoring threads, executions, and agent health
- CLI `wait` subcommand for blocking on thread replies with timeout
