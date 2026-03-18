# Changelog

All notable changes to Aster Orchestrator are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added

- `orch_read_log` MCP tool — paginated access to execution log files with offset/limit/tail support, falls back to output_preview when log file is unavailable
- Orphan backend CLI detection — persist PIDs in executions table, kill still-alive orphan processes on worker startup before marking crashed

### Changed

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
- Backend session continuity — Claude `-r`, Codex `exec resume`, OpenCode `-s`, session IDs persisted in SQLite (ORCH-FOUND-1)
- Unified `BackendOutput` struct — normalized output and intent parsing across all 4 backends (ORCH-FOUND-2)
- Event broadcast channel — `tokio::broadcast` EventBus for push-based dashboard and notification updates (ORCH-EVO-2)
- Dashboard "Currently Working On" — live per-execution progress summary in Ops tab (ORCH-EVO-3)
- Thread conversation view in dashboard — full message transcript with chat-like rendering (ORCH-EVO-5)
- Prompt version hashing — SHA-256 of resolved prompt stored per execution for correlation (ORCH-EVO-13)
- `handoff_prompt` field for custom handoff message injection (ORCH-HANDOFF-1)
- `--await-chain` CLI wait flag — polls until chain settles with no pending work (ORCH-HANDOFF-3)

### Changed

- Intent simplification — agents don't manage intents, all replies default to `response`; `parse_intent_from_text()` removed (ADR-015)
- Default config path moved to `~/.aster-orch/config.yaml` (ADR-013)
- Claude backend uses `stream-json` output format for real-time telemetry (ADR-008)
- Graceful worker shutdown via SIGTERM + semaphore drain on dashboard exit (ADR-007)
- HandoffConfig simplified from 5 routing fields to `on_response` + `max_chain_depth`, later extended with `handoff_prompt`
- Dashboard source-based message coloring — operator (accent), agent (green), system (dim) replaces intent-based coloring (ORCH-INTENT-3)
- Ops tab redesign — context panel removed, inline detail rows, responsive columns at narrow widths, empty section collapse (ORCH-OPS-1/2/3)
- Conversation view polish — markdown rendering via pulldown-cmark, scroll fix for wrapped lines, left-side indicator border (ORCH-CONV-1/2/3/4)

### Tooling

- Standalone repo with independent dev infrastructure — dual MCP servers (`aster-orch` production, `aster-orch-dev` for testing), skills, pre-commit hooks, repo-level `.mcp.json` (ADR-006)
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
