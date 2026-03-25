# Changelog

All notable changes to Compas are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [v0.4.3] - 2026-03-26

### Added

- Add fanout_children_awaited and settled_at metadata to --await-chain output

### Changed

- Derive orch_wait timeout ceiling from execution_timeout_secs; remove mcp_wait_max_timeout_secs config

## [v0.4.2] - 2026-03-22

### Fixed

- orch_dispatch next_step now directs agents to orch_wait MCP tool instead of compas wait CLI

## [v0.4.1] - 2026-03-22

### Fixed

- Fix execution stuck in 'executing': set busy_timeout on every pool connection and retry finalization with backoff

## [v0.4.0] - 2026-03-22

### Added

- Re-add orch_wait MCP tool with await_chain support and configurable timeout ceiling
- Release workflow with Homebrew tap auto-update, cargo-binstall metadata, and pre-built binary install paths
- Per-backend circuit breaker: stops dispatching to consistently failing backends (3 failures, 60s cooldown, configurable)

### Changed

- Document backlog structure and priority queue governance in AGENTS.md Ticket Workflow section

### Fixed

- Prevent internal session UUID from being persisted as backend session ID
- Detect stale Claude session IDs and automatically retry with fresh session

## [v0.3.0] - 2026-03-22

### Added

- ADR-019: Merge queue for worktree branch integration — FIFO queue model, temporary worktree isolation, per-target-branch serialization
- Config-declared recurring schedules (`schedules` section): cron-triggered dispatches in config with agent targeting, cron expression validation via `croner`, batch IDs, max runs safety cap, and enable/disable toggle (CRON-1)
- Config-driven generic backend definitions (`backend_definitions`): define CLI-based backends entirely in YAML with template variables, output format parsing, session resume, and env var stripping (GBE-1)
- `GenericBackend` implementing the `Backend` trait for config-defined backends — template substitution, output parsing, session resume, env_remove composition (GBE-2)
- Generic backend registry wiring and documentation (GBE-3): `build_backend_registry()` registers config-defined `GenericBackend` instances; `compas doctor` validates commands on PATH
- Lifecycle hooks config schema and execution engine: `hooks` section in config for shell commands at lifecycle events, JSON on stdin, fire-and-forget semantics (HOOKS-1)
- Hook EventBus integration: hooks fire in the worker process on execution and thread lifecycle events with hot-reload support (HOOKS-2)
- Lifecycle hooks documentation, example scripts, and `compas doctor` hook-command existence check (HOOKS-3)
- Store: `merge_operations` table and store methods for merge queue (MERGE-1)
- MergeExecutor: temporary-worktree-based merge execution with conflict detection (MERGE-2)
- Worker: merge queue polling, crash recovery, and stale merge detection (MERGE-3)
- MCP tools: `orch_merge`, `orch_merge_status`, `orch_merge_cancel` for merge queue operations (MERGE-4)
- CLI: `compas wait-merge --op-id <id>` blocking wait for merge completion (MERGE-6)
- Execution cost/token telemetry (OBS-01): `cost_usd`, `tokens_in`, `tokens_out`, `num_turns` columns; backend-specific extraction for Claude, Codex, Gemini
- Tool and cost aggregation queries (OBS-02): `tool_call_counts`, `tool_error_rates`, `cost_summary`, `cost_by_agent` store methods
- Dashboard cost/token visibility (OBS-04): Ops footer shows cost and token data; Agents tab cards show per-agent cost/token row
- Delayed dispatch via `scheduled_for` parameter on `orch_dispatch` (SCHED-2): schedule executions for a future time using ISO 8601 timestamps, deferred via `eligible_at` / `eligible_reason` columns
- Dashboard and MCP visibility for scheduled tasks (SCHED-3): `orch_tasks` supports `filter="scheduled"`, `orch_status` includes `scheduled_count`, Ops tab displays a "Scheduled" section with human-readable due times
- `orch_close` accepts optional `merge` field to atomically queue a merge with the close
- Improved config error messages: `load_config` reports missing file path and suggests `compas init`
- CONTRIBUTING.md, CODE_OF_CONDUCT.md, SECURITY.md, and GitHub issue/PR templates
- `compas doctor` command: pre-flight validation with ordered checklist, actionable fix suggestions, `--fix` for auto-registering MCP servers
- `orch_health` parallel pings with TTL cache: concurrent pings via `JoinSet`, cached results within `ping_cache_ttl_secs` window
- `compas init` command: interactive and non-interactive config scaffolding with backend detection and overwrite protection
- Config: `merge_timeout_secs` (default 30) and `default_merge_strategy` (default "merge") in orchestration config
- Orphan backend CLI detection: persist PIDs in executions table, kill orphan processes on worker startup
- `orch_read_log` MCP tool: paginated access to execution log files with offset/limit/tail support
- Session resume after crash (ADR-017): backend session IDs persisted mid-stream, enabling resume after crashed executions
- `compas setup-mcp` command: auto-register compas as an MCP server in coding tools with `--tool` filter, `--remove`, `--dry-run`, and idempotent behavior
- Worker singleton guard: fail-fast lockfile + heartbeat/PID check prevents multiple concurrent workers (ADR-016)
- `--standalone` flag for `dashboard` to opt out of the embedded worker
- `summary` field on threads — optional short one-liner set via `orch_dispatch`, visible across all MCP tools and dashboard
- Embedded wait guidance in `orch_dispatch` and `orch_poll` tool descriptions with `next_step` CLI command
- Worktree uncommitted change detection: executor appends `## Worktree Status` to agent output when worktree has uncommitted changes
- EventBus: MergeStarted and MergeCompleted events for merge lifecycle observability, desktop notifications on merge completion, on_merge_completed lifecycle hook (MERGE-5A)
- Worker cron schedule evaluation loop: evaluates config-declared schedules every 60s, dispatches messages when cron expressions are due, tracks fires in durable schedule_runs table (CRON-2)
- Dashboard: merge queue section in Activity tab with row display, conflict details, footer counts, and keyboard navigation (MERGE-5B)
- Desktop notifications and hook payloads now include thread summary for work context
- Dashboard schedule visibility, README documentation, and compas doctor validation for recurring schedules (CRON-3)

### Changed

- README: add Config Patterns section (multi-repo agent teams, cross-cutting agents), add `role` and `models` fields to config reference
- README: rewrite Quick Start to use `compas init` + `compas setup-mcp` + `compas doctor` (4-step flow)
- Reply message and fan-out thread creation are atomic (single transaction) to prevent race conditions
- `--await-chain` now waits for fan-out child threads to settle before returning (ADR-014 Phase 2)
- Batch stats use readable labels with semantic colors
- Conversation view: code blocks now have 2-space indent and subtle card background for visual distinction
- Added column separators between agent, summary, and batch columns
- `[c]` conversation shortcut now works from History tab and batch drill views, not just Ops
- `dashboard` now spawns an embedded worker by default; `--standalone` disables it (ADR-016)
- Fan-out child threads linked via `source_thread_id` column instead of batch_id heuristics
- Conversation view: heading levels have distinct visual weight (H1 accent, H2 bright, H3+ normal)
- Licensed under MIT OR Apache-2.0
- Ops view: agent column widened (18/14 chars) with truncation; summary column is now elastic (fills remaining width, clamped 10-60 chars)
- README audit: fix dashboard keybindings, correct worktree path, document `compas wait` flags/exit codes, add undocumented config fields
- Default worktree location changed from `{repo_root}/../.compas-worktrees/` to `{repo_root}/.compas-worktrees/` (inside repo, gitignored)
- Renamed config field `target_repo_root` to `default_workdir` — old key still works via serde alias
- Rebranded from aster-orch to compas — new package name, binary, config paths (`~/.compas/`), and documentation
- Renamed `orch-reviewer` agent to `compas-reviewer` in dispatch skill and config examples
- Shortened thread IDs in Ops view (show last 8 chars wide / 6 narrow instead of first 16)
- Progress summary now shows tool names instead of raw API IDs
- orch_close(status: completed) now auto-merges worktree threads to default_merge_target (default: main). Use Failed or Abandoned for non-merge intents.
- Restructure documentation: slim README, add configuration reference, dashboard guide, and cookbook (multi-project setup, dev-review-merge loop, scheduled automation, lifecycle hooks, custom backends) under docs/guides/

### Fixed

- `--await-chain` TOCTOU fix: re-fetch messages once when chain settles to capture responses inserted between queries
- Conversation view: code block closing brace no longer merges with the next content element
- Merge queue now resolves `repo_root` from the thread's `worktree_repo_root` for agents with per-agent `workdir` overrides
- Conversation view: ordered (numbered) lists now render as `1. 2. 3.` instead of bullets
- Worktree cleanup safety guard (ADR-018): worker checks `worktree_status` before removal; dirty worktrees and git-failure cases are skipped with a warning
- Worktree handoff bug: non-worktree agents on the same thread now inherit the thread's worktree path when targeting the same repo. `workspace: shared` explicitly opts out of inheritance.
- orch_tasks filter=scheduled now returns thread summary instead of null
- Dashboard schedule view now shows correct run count instead of Unix timestamp
- Merge executor now syncs the operator working tree via git reset --keep when the target branch is checked out, eliminating phantom diffs after merge
- Worktree path persistence now verifies row count and retries on transient DB errors, fixing silent auto-merge failures

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
