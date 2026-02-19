# aster-orch

Agent orchestrator for the Aster project. Coordinates multiple AI coding agents
(Claude, Codex, Gemini, OpenCode) through an MCP server interface with an
apalis-based background worker for trigger execution.

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
│  │ messages   │   │ Jobs (apalis) │  │
│  │  table     │   │  table        │  │
│  └─────┬─────┘   └───────┬───────┘  │
│        │    SQLite (WAL)  │          │
└────────┼──────────────────┼──────────┘
         │                  │
         │    ┌─────────────┘
         ▼    ▼
┌─────────────────────────────────────┐
│  Worker  (apalis background jobs)   │
│                                     │
│  TriggerJob → resolve agent/backend │
│            → start/reuse session    │
│            → execute CLI process    │
│            → parse JSON reply       │
│            → insert reply message   │
│            → update thread status   │
└─────────────────────────────────────┘
         │
         ▼
┌─────────────────┐
│  Backends       │
│  claude, codex, │
│  gemini,opencode│
└─────────────────┘
```

The MCP server and worker are two separate processes sharing the same SQLite
database resolved from `db_path` in the provided config file (this repo's
`.aster-orch/config.yaml` uses `~/.aster/orch/jobs.sqlite`). The MCP server handles operator commands,
the worker handles agent execution.

- **MCP server** — started by the MCP client (Claude Code, opencode, etc.) via
  stdio transport. Reads/writes the `messages` table and pushes trigger jobs to
  the apalis `Jobs` table.
- **Worker** — long-running background process (the "daemon" equivalent). Polls
  the `Jobs` table via apalis, executes triggers against real backends, and
  writes reply messages back to the `messages` table.

Both can also run in the same process via the `run` subcommand.

## CLI

```bash
# Worker only (run in background terminal / RustRover)
aster_orch worker --config .aster-orch/config.yaml

# MCP server only (started by MCP client config)
aster_orch mcp-server --config .aster-orch/config.yaml

# Unified: worker + MCP server in one process
aster_orch run --config .aster-orch/config.yaml
```

### Options

| Flag | Default | Description |
|------|---------|-------------|
| `--config` | *(required)* | Path to config YAML |
| `--concurrency` | `2` | Max concurrent trigger jobs (worker/run only) |

## MCP Tools (18)

### Core Workflow
| Tool | Description |
|------|-------------|
| `orch_dispatch` | Send a message to an agent. Creates or continues a thread. |
| `orch_approve` | Approve a review, issuing a review token. |
| `orch_reject` | Reject a review with feedback. |
| `orch_complete` | Complete a thread using the review token. |

### Query & Observability
| Tool | Description |
|------|-------------|
| `orch_status` | Query message status by agent and/or thread. |
| `orch_transcript` | Get full conversation transcript for a thread. |
| `orch_read` | Read a single message by reference (`db:<id>`). |
| `orch_metrics` | Aggregate metrics (active/blocked/completed threads). |
| `orch_batch_status` | Batch-level status with per-thread breakdown. |
| `orch_tasks` | List trigger execution records from the job queue. |

### Blocking & Polling
| Tool | Description |
|------|-------------|
| `orch_wait` | Block until a message appears on a thread (with timeout). |
| `orch_poll` | Non-blocking check of thread state. |

### Lifecycle
| Tool | Description |
|------|-------------|
| `orch_abandon` | Abandon a stuck thread. |
| `orch_reopen` | Reopen a terminal thread. |
| `orch_diagnose` | Thread diagnostics: status, blockers, suggested actions. |

### Configuration
| Tool | Description |
|------|-------------|
| `orch_session_info` | Current session metadata. |
| `orch_list_agents` | List all configured agents. |
| `orch_health` | Backend health pings for all or specific agents. |

## Dispatch Flow

When `orch_dispatch` is called:

1. Message inserted into `messages` table
2. Waiters notified (for any blocking `orch_wait` calls)
3. If intent matches `trigger_intents` config AND target agent is a `Worker`:
   - `TriggerJob` pushed to apalis `Jobs` table (ULID job ID)
   - Worker picks it up, resolves agent config → backend → session
   - Backend CLI process spawned with instruction prompt
   - Output parsed for JSON auto-reply
   - Reply inserted as a message, thread status updated

### Worker Log Phases

Worker logs now use explicit phase labels so dispatch-to-worker flow is easier
to read in real time:

- `phase=enqueue` — MCP server enqueued a trigger job (`job_id`, `thread_id`,
  `agent_alias`, `intent`).
- `phase=picked` — worker picked the queued job for execution.
- `phase=backend_session` — worker reused/opened backend session for the agent.
- `phase=backend_execute` — backend trigger execution started/finished.
- `phase=parse` — worker parsed backend output and classified reply intent.
- `phase=persist` — worker persisted reply/status updates to `messages`/`threads`.

### Worker Stability Notes

- Worker queue execution uses a shared SQLx pool (`apalis.db_max_connections`,
  `apalis.db_min_connections`, `apalis.db_acquire_timeout_ms`) to avoid
  connection starvation under concurrent triggers.
- If `apalis.listener_enabled=true` is configured, runtime may still use
  shared-pool polling backend for worker stability.

## Configuration

Config file: `.aster-orch/config.yaml`

```yaml
state_dir: ~/.aster/orch
db_path: ~/.aster/orch/jobs.sqlite
orchestration:
  auto_trigger_enabled: true
  trigger_intents: [dispatch, handoff, changes-requested]
  execution_timeout_secs: 300
  max_concurrent_triggers: 10
  max_triggers_per_agent: 2
  ping_timeout_secs: 15

models:
  - id: claude-opus-4-6
    backend: claude
  - id: claude-sonnet-4-5
    backend: claude

agents:
  - alias: focused
    identity: Claude
    backend: opencode
    model: anthropic/claude-opus-4-6
    prompt: "You are Focused, Compiler Core Engineer..."
  - alias: chill
    identity: Claude
    backend: claude
    model: claude-sonnet-4-5
    prompt: "You are Chill, Tooling Engineer..."
```

### Agent Roles

- **Worker** (default) — can be triggered by dispatch. The worker spawns CLI
  processes to execute work.
- **Operator** — MCP-only, never triggered. Auto-registered at runtime.

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

- **apalis for job queuing** — SQLite-backed, async, handles polling/acking/retry.
  Single worker handles all agents with configurable concurrency.
- **apalis TaskSink for job push** — `Store::push_trigger_job()` enqueues through
  `SqliteStorage` TaskSink API (`push_task`) with explicit task ID/attempt/priority,
  instead of writing to the `Jobs` table manually.
- **Two tables, not one** — `messages` table is the conversation ledger (permanent,
  MCP tools read/write). `Jobs` table is the worker queue (transient trigger
  execution). `orch_dispatch` inserts a message AND pushes a TriggerJob.
- **Pipeline returns result, handler routes** — the pipeline (execute -> parse ->
  dispatch) returns `ParsedReply`. Job routing (follow-up triggers) is done by
  the handler, not the pipeline.
- **sqlx instead of rusqlite** — required by apalis-sqlite. Both share the same
  SQLite file via WAL mode.

## Differences from `aster-orchestrator` (deprecated)

| Old | New |
|-----|-----|
| Custom daemon poll loop | apalis background worker |
| rusqlite | sqlx |
| Circuit breaker (3 failures, 60s cooldown) | Not implemented (planned) |
| Retry with backoff | apalis built-in retry (max_attempts) |
| Watchdog thread | Not needed (apalis handles orphan recovery) |
| AgentRuntime state machine | Stateless worker (session cache only) |
| ModelPoolState / hot-swap | Not implemented (planned) |
| Session namespace scoping | Scoping by DB file |
| 24 MCP tools | 18 MCP tools |
| `daemon run` subcommand | `worker` subcommand |

## Module Structure

```
src/
├── bin/aster_orch.rs    # CLI binary (worker, mcp-server, run)
├── lib.rs               # Module declarations
├── error.rs             # Error types (sqlx-based)
├── observability.rs     # Tracing setup
├── testing.rs           # StubBackend, StubNotifier
├── backend/             # Backend trait + implementations
│   ├── mod.rs           #   Backend trait, PingResult
│   ├── claude.rs        #   Claude CLI backend
│   ├── codex.rs         #   Codex CLI backend
│   ├── gemini.rs        #   Gemini CLI backend
│   ├── opencode.rs      #   OpenCode CLI backend
│   ├── process.rs       #   Shared CLI process spawning
│   └── registry.rs      #   BackendRegistry lookup
├── config/              # Configuration
│   ├── types.rs         #   Config structs, AgentRole, OrchestrationConfig
│   ├── validation.rs    #   Config validation
│   └── loader.rs        #   YAML loading
├── mcp/                 # MCP server (18 tools)
│   ├── server.rs        #   OrchestratorMcpServer, tool stubs
│   ├── params.rs        #   All parameter structs
│   ├── dispatch.rs      #   orch_dispatch + trigger push
│   ├── query.rs         #   orch_status, transcript, read, metrics, diagnose, batch_status
│   ├── lifecycle.rs     #   orch_approve, reject, complete, reopen, abandon
│   ├── wait.rs          #   WaitRegistry, orch_wait, orch_poll
│   ├── session.rs       #   orch_session_info, orch_list_agents
│   └── health.rs        #   orch_health, orch_tasks
├── store/               # SQLite store (messages + threads)
│   └── mod.rs           #   Store with full CRUD + push_trigger_job
├── worker/              # apalis worker
│   ├── mod.rs           #   Re-exports
│   ├── trigger.rs       #   TriggerJob, ParsedReply, JSON extraction
│   ├── context.rs       #   TriggerContext, build_backend_registry
│   └── pipeline.rs      #   execute_trigger, parse_reply, dispatch_result
├── model/               # Domain types
│   ├── agent.rs         #   Agent model
│   ├── message.rs       #   Intent, ThreadStatus enums
│   ├── review.rs        #   ReviewToken
│   └── session.rs       #   Session, TriggerResult
├── workflow/            # Workflow logic
│   ├── alias.rs         #   Alias resolution
│   ├── validator.rs     #   Intent state machine
│   ├── review_ledger.rs #   Review token ledger
│   └── session_ledger.rs#   Session persistence
├── audit/               # Audit logging
├── health/              # Health report types
└── notification/        # Notification (Telegram)
```
