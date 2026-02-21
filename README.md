# aster-orch

Agent orchestrator for multi-agent engineering workflows. Coordinates AI coding
agents (Claude, Codex, Gemini, OpenCode) through an MCP server interface with a
custom poll-loop background worker for trigger execution.

`aster-orch` is project-agnostic: Aster uses it, but it can orchestrate agents
for any repository by pointing `project_root` at that repo.

Replaces the deprecated `aster-orchestrator` crate.

## Architecture

```
Operator (MCP client)
    │
    ▼
┌─────────────────────────────────────┐
│  MCP Server  (stdio transport)      │
│  18 tools: dispatch, status, wait,  │
│  approve, reject, complete, ...     │
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

**Two-process model.** The MCP server and worker are separate processes sharing
the same SQLite database (resolved from `db_path` in config). WAL mode enables
concurrent read/write without SQLITE_BUSY errors.

- **MCP server** — started by the MCP client (Claude Code, opencode, etc.) via
  stdio transport. Reads/writes `messages` and `threads` tables. Inserts queued
  rows into the `executions` table when dispatching to worker agents.
- **Worker** — long-running background process. Polls the `executions` table for
  `status='queued'` rows, claims work atomically with per-agent concurrency
  enforcement, runs backend triggers via `tokio::task::spawn_blocking`, and
  writes reply messages back.

## Project/State Paths

`aster-orch` uses two distinct filesystem roots:

- `project_root`: the target repository where backend CLIs run commands/tasks.
- `state_dir`: orchestrator-owned runtime state (DB/logs/heartbeats).

This separation allows one shared orchestrator binary/config model to work
across unrelated repositories.

## Database Schema

Four tables in a single SQLite file with WAL mode:

| Table | Purpose |
|-------|---------|
| `threads` | Thread lifecycle (Active, ReviewPending, Completed, Failed, Abandoned) |
| `messages` | Conversation ledger between operator and agents |
| `executions` | Job queue AND execution lifecycle tracker (queued → picked_up → executing → completed/failed/timed_out/crashed/cancelled) |
| `worker_heartbeats` | Worker liveness tracking |

The `executions` table is the single source of truth for both queuing and
execution state — no separate job queue system.

## CLI

```bash
# Worker only (run in background terminal / RustRover)
aster_orch worker

# MCP server only (started by MCP client config)
aster_orch mcp-server

# Dashboard only (reads SQLite directly)
aster_orch dashboard

# Dashboard + embedded worker (convenience mode)
aster_orch dashboard --with-worker

# Optional override when config is not at the standard location
aster_orch wait --config /path/to/config.yaml --thread-id <thread-id>
```

`--with-worker` is intended for local convenience. For long-running or production
operation, prefer the standard two-process setup (`worker` + `mcp-server`).

Dashboard controls:

- `Tab` / `Shift+Tab` / `1-4`: switch tabs
- `↑/↓` or `j/k`: move selection
- `g` / `G`: jump to first/last row
- `Enter`: open selected log, or drill into selected batch on Ops
- `Esc`: back out of active batch drill filter
- `a`: open guided action menu for selected thread
- `b` / `o`: quick aliases for abandon/reopen (still opens guided confirm)
- `J`: toggle payload rendering in Ops context pane (pretty JSON is default)
- `?`: open keyboard help overlay
- `q` or `Ctrl+C`: exit dashboard

Log viewer controls:

- `Esc`: back to dashboard
- `g` / `G`: top/bottom
- `f`: toggle follow mode
- `J`: pretty-print JSON log lines when possible

| Flag | Default | Description |
|------|---------|-------------|
| `--config` | `.aster-orch/config.yaml` | Optional config path override |

## MCP Tools (18)

### Core Workflow
| Tool | Description |
|------|-------------|
| `orch_dispatch` | Send a message to an agent. Creates or continues a thread. |
| `orch_approve` | Approve a review, issuing a review token. |
| `orch_reject` | Reject a review with feedback, re-triggers worker agents. |
| `orch_complete` | Complete a thread using the review token. |

### Query & Observability
| Tool | Description |
|------|-------------|
| `orch_status` | Query thread + latest execution status by agent and/or thread. |
| `orch_transcript` | Get full conversation transcript (messages + executions) for a thread. |
| `orch_read` | Read a single message by reference (`db:<id>`). |
| `orch_metrics` | Aggregate metrics (thread counts, queue depth, active executions). |
| `orch_batch_status` | Batch-level status with per-thread breakdown. |
| `orch_tasks` | List trigger execution records with timing and result status. |

### Blocking & Polling
| Tool | Description |
|------|-------------|
| `orch_wait` | Poll DB at 200ms intervals until a message appears (with timeout). |
| `orch_poll` | Non-blocking check of thread state and recent messages. |

### Lifecycle
| Tool | Description |
|------|-------------|
| `orch_abandon` | Abandon a thread, cancel active executions. |
| `orch_reopen` | Reopen a terminal thread (Completed/Failed/Abandoned) to Active. |
| `orch_diagnose` | Thread diagnostics: status, blockers, suggested next actions. |

### Configuration
| Tool | Description |
|------|-------------|
| `orch_session_info` | Current MCP session metadata. |
| `orch_list_agents` | List all configured agents with backend/model info. |
| `orch_health` | Worker heartbeat status + backend health pings. |

## Dispatch Flow

When `orch_dispatch` is called:

1. Message inserted into `messages` table (thread auto-created if needed)
2. If intent matches `trigger_intents` config AND target agent role is `Worker`:
   - Execution row inserted into `executions` table with `status='queued'`
   - Worker picks it up on next poll cycle via `claim_next_execution()`
   - Worker resolves agent config → backend → starts session
   - Backend CLI process spawned inside `tokio::task::spawn_blocking`
   - Output parsed for structured intent (JSON auto-reply)
   - Execution status updated (completed/failed/timed_out)
   - Reply message inserted into `messages` table

### Worker Lifecycle

On startup:
1. **Crash recovery** — marks orphaned executions (`picked_up`/`executing`) as `crashed`
2. **Initial heartbeat** — writes to `worker_heartbeats` table

Main loop (concurrent via `tokio::select!`):
- **Poll interval** — claims queued executions, enforces per-agent concurrency via SQL, spawns execution tasks with global semaphore
- **Heartbeat interval** (10s) — writes liveness record for `orch_health` to check

### Execution Status Lifecycle

```
queued → picked_up → executing → completed
                               → failed
                               → timed_out
                               → crashed (worker died mid-execution)
                    → cancelled (thread abandoned)
```

## Configuration

Config file: `.aster-orch/config.yaml`

Generic template: `crates/aster-orch/examples/config-generic.yaml`

```yaml
project_root: /path/to/target-repo
state_dir: ~/.aster/orch
db_path: ~/.aster/orch/jobs.sqlite
poll_interval_secs: 1
orchestration:
  trigger_intents: [dispatch, handoff, changes-requested]
  execution_timeout_secs: 300
  max_concurrent_triggers: 10
  max_triggers_per_agent: 2
  ping_timeout_secs: 15

models:
  - id: claude-sonnet-4-6
    backend: claude
  - id: claude-opus-4-6
    backend: claude

agents:
  - alias: focused
    identity: Claude
    backend: claude
    model: claude-sonnet-4-6
    prompt: "You are focused on backend implementation and tests."
  - alias: chill
    identity: Claude
    backend: claude
    model: claude-sonnet-4-6
    prompt: "You are focused on docs and release quality."
```

Path resolution rules:

- absolute paths are used as-is
- `~/...` expands to `$HOME/...`
- relative paths resolve against the directory containing the config file
  (`project_root`, `state_dir`, `db_path`, and agent `prompt_file`)

### Agent Roles

- **Worker** (default) — triggered when dispatch intent matches `trigger_intents`.
  The worker spawns CLI processes via `spawn_blocking` to execute work.
- **Operator** — MCP-only, never triggered. The operator is whoever calls the MCP tools.

### Key Config Fields

| Field | Default | Description |
|-------|---------|-------------|
| `project_root` | *(required)* | Target repository root where all backend CLIs execute |
| `state_dir` | *(required)* | Orchestrator runtime directory (logs/state files) |
| `poll_interval_secs` | 1 | Worker poll interval for queued executions |
| `orchestration.max_concurrent_triggers` | worker count | Global concurrent execution limit |
| `orchestration.max_triggers_per_agent` | 1 | Per-agent concurrent execution limit |
| `orchestration.execution_timeout_secs` | 30 | Per-trigger timeout |
| `orchestration.trigger_intents` | dispatch, handoff, changes-requested | Intents that trigger worker execution |
| `orchestration.ping_timeout_secs` | 15 | Backend ping liveness timeout |

## MCP Client Configuration

### Claude Code (`.mcp.json`)
```json
{
  "mcpServers": {
    "aster-orch": {
      "command": "/path/to/target/release/aster_orch",
      "args": ["mcp-server", "--config", "/path/to/.aster-orch/config.yaml"]
    }
  }
}
```

### opencode (`opencode.json`)
```json
{
  "mcp": {
    "aster-orch": {
      "type": "local",
      "command": ["/path/to/target/release/aster_orch", "mcp-server", "--config", "/path/to/.aster-orch/config.yaml"]
    }
  }
}
```

### RustRover (worker)
```
run --package aster-orch --bin aster_orch -- worker --config .aster-orch/config.yaml
```

## Building & Testing

```bash
# Build
cargo build --package aster-orch

# Release build
cargo build --package aster-orch --release

# Run tests
cargo test --package aster-orch

# Binary location (root workspace)
target/release/aster_orch
```

## Key Design Decisions

- **Custom `executions` table as job queue** — single source of truth for both
  queuing and execution lifecycle. No external job queue dependency.
- **`spawn_blocking` for CLI execution** — backend trigger calls run inside
  `tokio::task::spawn_blocking` to avoid starving the async runtime with blocking
  subprocess I/O.
- **Per-agent concurrency enforcement** — `claim_next_execution()` uses a SQL
  subquery to check active execution count per agent before claiming work.
- **Crash recovery on startup** — worker marks orphaned `picked_up`/`executing`
  rows as `crashed` on startup, preventing lost work from going unnoticed.
- **WAL mode mandatory** — SQLite WAL mode enables the two-process model (MCP
  server + worker) to read/write concurrently without SQLITE_BUSY errors.
- **200ms DB polling for `orch_wait`** — simple, reliable, works across process
  boundaries without in-memory notification channels.
- **Three core tables** — `threads` (lifecycle), `messages` (conversation
  ledger), `executions` (job queue + execution tracker). Plus `worker_heartbeats`
  for liveness.

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
| 24 MCP tools | 18 MCP tools |
| `daemon run` subcommand | `worker` + `mcp-server` subcommands only |

## Module Structure

```
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
├── mcp/                 # MCP server (18 tools)
│   ├── mod.rs           #   Module declarations
│   ├── server.rs        #   OrchestratorMcpServer, #[tool] stubs, ServerHandler
│   ├── params.rs        #   All parameter structs
│   ├── dispatch.rs      #   orch_dispatch (message + execution insert)
│   ├── query.rs         #   orch_status, transcript, read, metrics, poll, batch_status, tasks
│   ├── lifecycle.rs     #   orch_approve, reject, complete, abandon, reopen
│   ├── wait.rs          #   orch_wait (200ms DB poll loop)
│   ├── session.rs       #   orch_session_info, orch_list_agents
│   └── health.rs        #   orch_health, orch_diagnose
├── store/               # SQLite store (threads + messages + executions + heartbeats)
│   └── mod.rs           #   Store with WAL setup, typed enums, all CRUD, claim logic
├── worker/              # Custom poll-loop worker
│   ├── mod.rs           #   Re-exports
│   ├── loop_runner.rs   #   WorkerRunner: poll loop, heartbeat, crash recovery
│   └── executor.rs      #   execute_trigger: spawn_blocking wrapper, output parsing
└── model/               # Domain types
    ├── agent.rs         #   Agent struct
    └── session.rs       #   Session, SessionStatus, TriggerResult
```
