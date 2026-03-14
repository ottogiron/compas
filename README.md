# aster-orch

Multi-agent orchestrator for AI-assisted software development. Dispatch tasks to AI coding agents, monitor execution in a TUI dashboard, and manage the full lifecycle from your terminal.

Works with Claude Code, Codex, Gemini CLI, and OpenCode. Project-agnostic — point it at any repository.

## Install

```bash
cargo install --git https://github.com/ottogiron/aster-orch
```

Or build from source:

```bash
git clone git@github.com:ottogiron/aster-orch.git
cd aster-orch
cargo build --release
# Binary at target/release/aster_orch
```

## Quick Start

### 1. Create a config

Create `.aster-orch/config.yaml` in your project (or anywhere):

```yaml
target_repo_root: /path/to/your/project
state_dir: ~/.aster/orch
poll_interval_secs: 1

orchestration:
  trigger_intents: [dispatch, handoff]
  execution_timeout_secs: 600
  max_concurrent_triggers: 4
  max_triggers_per_agent: 1

agents:
  - alias: dev
    backend: claude
    model: claude-sonnet-4-6
    prompt: >
      You are a development agent. Follow the project's AGENTS.md.
      When done, end your response with:
      {"intent":"review-request","to":"operator"}
```

Supported backends: `claude`, `codex`, `gemini`, `opencode`.

### 2. Connect your coding CLI

Add the MCP server to your preferred tool:

**Claude Code:**
```bash
claude mcp add --scope user --transport stdio aster-orch -- \
  /path/to/aster_orch mcp-server --config /path/to/.aster-orch/config.yaml
```

**Codex:**
```bash
codex mcp add aster-orch -- \
  /path/to/aster_orch mcp-server --config /path/to/.aster-orch/config.yaml
```

**OpenCode** — add to `opencode.json` (project root) or `~/.config/opencode/opencode.json` (global):
```json
{
  "mcp": {
    "aster-orch": {
      "type": "local",
      "command": ["/path/to/aster_orch", "mcp-server", "--config", "/path/to/.aster-orch/config.yaml"]
    }
  }
}
```

### 3. Start the dashboard

```bash
# Dashboard + worker (recommended for getting started)
aster_orch dashboard --with-worker --config .aster-orch/config.yaml

# Or run worker and dashboard separately
aster_orch worker --config .aster-orch/config.yaml &
aster_orch dashboard --config .aster-orch/config.yaml
```

### 4. Dispatch your first task

From your coding CLI (Claude Code, Codex, or OpenCode), use the MCP tools:

```
orch_dispatch(from="operator", to="dev", body="Add a hello world endpoint to the API", intent="dispatch")
```

Watch the agent work in the dashboard. When it responds, review and close:

```
orch_close(thread_id="<id>", from="operator", status="completed")
```

## Dashboard

The TUI dashboard shows real-time orchestrator state across four tabs:

- **Ops** — active threads, running executions, batch progress
- **Agents** — configured agents with health status
- **History** — completed executions with duration and status
- **Settings** — current configuration

| Key | Action |
|-----|--------|
| `Tab` / `1-4` | Switch tabs |
| `↑/↓` or `j/k` | Navigate |
| `Enter` | Open log viewer / drill into batch |
| `a` | Action menu (close, abandon, reopen) |
| `s` | Abandon stale threads |
| `?` | Keyboard help |
| `q` | Quit |

**Log viewer** (`Enter` on an execution):

| Key | Action |
|-----|--------|
| `Tab` | Switch Input/Output sections |
| `←/→` | Collapse/expand section |
| `f` | Toggle follow mode |
| `J` | Pretty-print JSON |
| `Esc` | Back to dashboard |

## MCP Tools

### Core

| Tool | What it does |
|------|-------------|
| `orch_dispatch` | Send a task to an agent (creates a thread, queues execution) |
| `orch_close` | Close a thread as `completed` or `failed` |
| `orch_abandon` | Cancel a thread and its active executions |
| `orch_reopen` | Reopen a closed/failed/abandoned thread |

### Monitor

| Tool | What it does |
|------|-------------|
| `orch_status` | Thread and execution status (filter by agent or thread) |
| `orch_poll` | Quick non-blocking check for new messages |
| `orch_wait` | Block until a message arrives (with timeout) |
| `orch_transcript` | Full conversation history for a thread |
| `orch_read` | Read a single message by reference |
| `orch_batch_status` | Status breakdown for all threads in a batch |
| `orch_tasks` | Execution history with timing and results |
| `orch_metrics` | Aggregate stats (thread counts, queue depth) |
| `orch_diagnose` | Thread diagnostics with suggested next actions |

### System

| Tool | What it does |
|------|-------------|
| `orch_health` | Worker heartbeat + backend health pings |
| `orch_list_agents` | List configured agents with backend/model info |
| `orch_session_info` | Current MCP session metadata |

## Configuration Reference

```yaml
target_repo_root: /path/to/repo        # Where agents work (required)
state_dir: ~/.aster/orch               # Runtime state: DB, logs (required)
poll_interval_secs: 1                  # Worker poll frequency

orchestration:
  trigger_intents: [dispatch, handoff]  # Intents that trigger agent execution
  execution_timeout_secs: 600           # Per-task timeout
  max_concurrent_triggers: 10           # Global concurrency limit
  max_triggers_per_agent: 2             # Per-agent concurrency limit
  stale_active_secs: 3600              # Staleness threshold for idle threads
  ping_timeout_secs: 15                # Backend health check timeout

agents:
  - alias: dev                         # Unique name for dispatching
    backend: claude                    # claude | codex | gemini | opencode
    model: claude-sonnet-4-6           # Model to use
    prompt: "..."                      # System prompt for the agent
    # prompt_file: prompts/dev.md      # Or load prompt from file
    # backend_args: ["--flag"]         # Extra CLI args for the backend
```

**Path resolution:** Absolute paths are used as-is. `~/` expands to `$HOME`. Relative paths resolve against the config file's directory.

**Multiple agents:** Define as many agents as needed with different backends, models, and prompts. Each agent gets its own concurrency slot.

## Typical Workflow

```
1. Operator dispatches task    →  orch_dispatch(to="dev", body="...", intent="dispatch")
2. Worker picks up execution   →  visible in dashboard as "executing"
3. Agent works in the repo     →  backend CLI runs with the prompt
4. Agent responds              →  reply message with intent (e.g., review-request)
5. Operator reviews            →  orch_transcript or dashboard log viewer
6. Operator closes thread      →  orch_close(status="completed")
```

For multi-step work, dispatch follow-ups to the same thread by passing `thread_id`. Use `batch` to group related threads.

## Troubleshooting

**Agent not responding:**
```
orch_health()              # Check worker heartbeat and backend pings
orch_diagnose(thread_id=…) # Thread-level diagnostics with suggestions
orch_tasks()               # Check execution status and errors
```

**Stale state / corrupted DB:**
```bash
# Stop all processes, remove state, restart
kill $(pgrep aster_orch)
rm ~/.aster/orch/jobs.sqlite*
aster_orch dashboard --with-worker --config .aster-orch/config.yaml
```

**Worker not picking up work:**
- Check `orch_health()` — is there a recent heartbeat?
- Check `orch_metrics()` — is `queue_depth > 0`?
- Verify the agent's backend CLI is installed and authenticated

## More Information

- [Architecture & internals](docs/project/architecture.md)
- [Development workflow](AGENTS.md)
- [Design decisions](docs/project/DECISIONS.md)

## Development

```bash
make setup-hooks       # Install pre-commit hook
make verify            # fmt-check + clippy + tests
make dashboard-dev     # Dashboard + worker on isolated dev DB
```

See [AGENTS.md](AGENTS.md) for the full development workflow including dual MCP server setup, testing MCP changes, and ticket tracking.
