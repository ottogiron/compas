# Architecture — Aster Orchestrator

## Overview

```text
Operator (MCP client)
    │
    ▼
┌─────────────────────────────────────┐
│  MCP Server  (stdio transport)      │
│  17 tools: dispatch, status, poll,  │
│  close, abandon, reopen, ...        │
│                                     │
│  ┌───────────┐   ┌───────────────┐  │
│  │ messages   │   │ executions    │  │
│  │  table     │   │  table        │  │
│  └─────┬─────┘   └───────┬───────┘  │
│        │    SQLite (WAL)  │          │
└────────┼──────────────────┼──────────┘
         │                  │
         │    ┌─────────────┘
         ▼    ▼
┌─────────────────────────────────────┐
│  Worker  (custom poll-loop)         │
│                                     │
│  claim_next_execution()             │
│  → resolve agent/backend            │
│  → spawn_blocking(trigger CLI)      │
│  → update execution status          │
│  → insert reply message             │
│  → periodic heartbeat               │
└─────────────────────────────────────┘
         │
         ▼
┌─────────────────┐
│  Backends       │
│  claude, codex, │
│  gemini,opencode│
└─────────────────┘
```

## Two-Process Model

The MCP server and worker are separate processes sharing the same SQLite database (`{state_dir}/jobs.sqlite`). WAL mode enables concurrent read/write without SQLITE_BUSY errors.

- **MCP server** — started by the MCP client (Claude Code, OpenCode, etc.) via stdio transport. Reads/writes `messages` and `threads` tables. Inserts queued rows into the `executions` table when dispatching to worker agents.
- **Worker** — long-running background process. Polls the `executions` table for `status='queued'` rows, claims work atomically with per-agent concurrency enforcement, runs backend triggers via `tokio::task::spawn_blocking`, and writes reply messages back.

## Project/State Paths

`aster-orch` uses two distinct filesystem roots:

- `target_repo_root`: the target repository where backend CLIs run commands/tasks.
- `state_dir`: orchestrator-owned runtime state (DB/logs/heartbeats).

This separation allows one shared orchestrator binary/config model to work across unrelated repositories.

## Database Schema

Five tables in a single SQLite file with WAL mode:

| Table | Purpose |
|-------|---------|
| `threads` | Thread lifecycle (Active, Completed, Failed, Abandoned) |
| `messages` | Conversation ledger between operator and agents |
| `executions` | Job queue AND execution lifecycle tracker (queued → picked_up → executing → completed/failed/timed_out/crashed/cancelled) |
| `worker_heartbeats` | Worker liveness tracking |
| `execution_events` | Structured telemetry events extracted from backend output |

The `executions` table is the single source of truth for both queuing and execution state — no separate job queue system.

## Dispatch Flow

When `orch_dispatch` is called:

1. Message inserted into `messages` table (thread auto-created if needed)
2. If intent matches `trigger_intents` config AND target agent role is `Worker`:
   - Execution row inserted into `executions` table with `status='queued'`
   - Worker picks it up on next poll cycle via `claim_next_execution()`
   - Worker resolves agent config → backend → starts session
   - Backend CLI process spawned inside `tokio::task::spawn_blocking`
   - All agent replies get `response` intent (agents reply naturally, no intent parsing)
   - Execution status updated (completed/failed/timed_out)
   - Reply message inserted into `messages` table
   - If agent has `on_response` handoff config, a new `handoff` message is auto-inserted targeting the next agent, triggering a new execution on the next poll cycle. When `on_response` is a list (fan-out), a separate batch-linked thread is created per target agent. Chain depth is tracked by counting `handoff` messages on the thread; when `max_chain_depth` is reached, a review-request to the operator is inserted instead.

## Worker Lifecycle

On startup:

1. **Crash recovery** — marks orphaned executions (`picked_up`/`executing`) as `crashed`
2. **Initial heartbeat** — writes to `worker_heartbeats` table

Main loop (concurrent via `tokio::select!`):

- **Poll interval** — claims queued executions, enforces per-agent concurrency via SQL, spawns execution tasks with global semaphore
- **Heartbeat interval** (10s) — writes liveness record for `orch_health` to check

## Execution Status Lifecycle

```text
queued → picked_up → executing → completed
                               → failed (transient) → re-queued (retry backoff)
                               → failed (non-retryable / retries exhausted)
                               → timed_out
                               → crashed (worker died mid-execution)
                    → cancelled (thread abandoned)
```

## Agent Roles

- **Worker** (default) — triggered when dispatch intent matches `trigger_intents`. The worker spawns CLI processes via `spawn_blocking` to execute work.
- **Operator** — MCP-only, never triggered. The operator is whoever calls the MCP tools.

## Key Design Decisions

- **Custom `executions` table as job queue** — single source of truth for both queuing and execution lifecycle. No external job queue dependency.
- **`spawn_blocking` for CLI execution** — backend trigger calls run inside `tokio::task::spawn_blocking` to avoid starving the async runtime with blocking subprocess I/O.
- **Per-agent concurrency enforcement** — `claim_next_execution()` uses a SQL subquery to check active execution count per agent before claiming work.
- **Crash recovery on startup** — worker marks orphaned `picked_up`/`executing` rows as `crashed` on startup, preventing lost work from going unnoticed.
- **WAL mode mandatory** — SQLite WAL mode enables the two-process model (MCP server + worker) to read/write concurrently without SQLITE_BUSY errors.
- **200ms DB polling for wait** — `wait_for_message()` polls at 200ms intervals. Exposed via `aster_orch wait` CLI subcommand. Removed from MCP surface (stdio transport timeouts made it unreliable).
- **Five tables** — `threads` (lifecycle), `messages` (conversation ledger), `executions` (job queue + execution tracker), `worker_heartbeats` (liveness), `execution_events` (telemetry).
- **Retry via store re-enqueue** — failed executions with transient errors are retried by inserting a new queued execution with a `retry_after` timestamp. The poll loop claims retries only when the backoff expires. No synchronous sleep.
- **Execution telemetry via line-level channel** — backend stdout lines flow through a `sync_channel(128)` from the reader thread to a tokio consumer that parses JSONL and batch-inserts events.
- **Auto-handoff chains** — config-driven agent-to-agent routing via `on_response`. All agent replies get `response` intent automatically (no agent-side intent parsing). Chain depth is tracked by counting `handoff`-intent messages on the thread. Depth check and handoff insert run in a single SQL transaction to prevent TOCTOU races. Forced operator escalation at `max_chain_depth` (default: 3).
- **Fan-out via batch-linked threads** — when `on_response` is a list, each target agent gets its own thread, all sharing a batch ID. Operator is the join point; `orch_batch_status` aggregates results. Parallel execution, not same-thread concurrency (ADR-014).

## Module Structure

```text
src/
├── bin/aster_orch.rs    # CLI binary (worker, mcp-server)
├── lib.rs               # Module declarations
├── error.rs             # Error types
├── backend/             # Backend trait + implementations
│   ├── mod.rs           #   Backend trait, PingResult
│   ├── claude.rs        #   Claude CLI backend
│   ├── codex.rs         #   Codex CLI backend
│   ├── gemini.rs        #   Gemini CLI backend
│   ├── opencode.rs      #   OpenCode CLI backend
│   ├── process.rs       #   CLI process spawning, output extraction
│   └── registry.rs      #   BackendRegistry lookup
├── config/              # Configuration
│   ├── mod.rs           #   Config loading + normalization
│   ├── types.rs         #   Config structs, AgentRole, OrchestrationConfig
│   └── validation.rs    #   Config validation
├── mcp/                 # MCP server (17 tools)
│   ├── mod.rs           #   Module declarations
│   ├── server.rs        #   OrchestratorMcpServer, #[tool] stubs, ServerHandler
│   ├── params.rs        #   All parameter structs
│   ├── dispatch.rs      #   orch_dispatch (message + execution insert)
│   ├── query.rs         #   orch_status, transcript, read, metrics, poll, batch_status, tasks
│   ├── lifecycle.rs     #   orch_close, abandon, reopen
│   ├── wait.rs          #   wait logic (200ms DB poll, used by CLI wait)
│   ├── session.rs       #   orch_session_info, orch_list_agents
│   └── health.rs        #   orch_health, orch_diagnose
├── store/               # SQLite store (threads + messages + executions + heartbeats)
│   └── mod.rs           #   Store with WAL setup, typed enums, all CRUD, claim logic
├── worktree.rs          # Git worktree creation, cleanup, and path resolution
├── events.rs            # EventBus and execution telemetry pipeline
├── worker/              # Custom poll-loop worker
│   ├── mod.rs           #   Re-exports
│   ├── loop_runner.rs   #   WorkerRunner: poll loop, heartbeat, crash recovery
│   └── executor.rs      #   execute_trigger: spawn_blocking wrapper, output parsing
└── model/               # Domain types
    ├── agent.rs         #   Agent struct
    └── session.rs       #   Session, SessionStatus, TriggerResult
```

## Differences from `aster-orchestrator` (deprecated)

| Old (`aster-orchestrator`) | New (`aster-orch`) |
|----------------------------|---------------------|
| Custom daemon poll loop | Custom poll-loop worker with `spawn_blocking` |
| rusqlite | sqlx (async, WAL mode) |
| Single `messages.status` field (never updated) | Typed `ExecutionStatus` enum with full lifecycle |
| No crash recovery | Orphan detection on startup |
| No heartbeat | Worker heartbeats every 10s |
| Blocking I/O on async runtime | `spawn_blocking` for all CLI execution |
| Circuit breaker (3 failures, 60s cooldown) | Not implemented (planned) |
| AgentRuntime state machine | Stateless worker (execution-per-trigger) |
| Session namespace scoping | Scoping by DB file |
| 24 MCP tools | 17 MCP tools (wait moved to CLI-only) |
| `daemon run` subcommand | `worker` + `mcp-server` subcommands only |
