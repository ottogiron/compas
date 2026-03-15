# aster-orch

Multi-agent orchestrator for AI-assisted software development. Dispatch tasks to AI coding agents, monitor execution in a TUI dashboard, and manage the full lifecycle from your terminal.

Works with Claude Code, Codex, Gemini CLI, and OpenCode. Project-agnostic — point it at any repository.

## Prerequisites

- **Rust** toolchain (`cargo`)
- At least one backend CLI installed and authenticated:

| Backend | Install | Authenticate |
|---------|---------|-------------|
| Claude Code | `npm install -g @anthropic-ai/claude-code` | `claude login` |
| Codex | `npm install -g @openai/codex` | `codex login` |
| Gemini CLI | `npm install -g @google/gemini-cli` | `gemini auth` |
| OpenCode | See [opencode.ai](https://opencode.ai) | varies by provider |

## Install

```bash
cargo install --git https://github.com/ottogiron/aster-orch
```

This puts `aster_orch` on your PATH. Or build from source:

```bash
git clone git@github.com:ottogiron/aster-orch.git
cd aster-orch
cargo build --release
# Binary at target/release/aster_orch — add to PATH or use the full path below
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
  max_concurrent_triggers: 4       # adjust based on your API budget
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

Add the MCP server to your preferred tool. If you used `cargo install`, you can use `aster_orch` directly. For source builds, use the full path to `target/release/aster_orch`.

**Claude Code:**
```bash
claude mcp add --scope user --transport stdio aster-orch -- \
  aster_orch mcp-server --config /path/to/.aster-orch/config.yaml
```

**Codex:**
```bash
codex mcp add aster-orch -- \
  aster_orch mcp-server --config /path/to/.aster-orch/config.yaml
```

**OpenCode** — add to `opencode.json` (project root) or `~/.config/opencode/opencode.json` (global):
```json
{
  "mcp": {
    "aster-orch": {
      "type": "local",
      "command": ["aster_orch", "mcp-server", "--config", "/path/to/.aster-orch/config.yaml"]
    }
  }
}
```

**Gemini CLI** — add to `.gemini/settings.json`:
```json
{
  "mcpServers": {
    "aster-orch": {
      "command": "aster_orch",
      "args": ["mcp-server", "--config", "/path/to/.aster-orch/config.yaml"]
    }
  }
}
```

### 3. Start the worker

The worker is the background process that picks up dispatched tasks and runs your agents. The dashboard is a TUI for monitoring — optional but recommended.

```bash
# Dashboard + worker together (recommended for getting started)
aster_orch dashboard --with-worker --config .aster-orch/config.yaml

# Or run them separately
aster_orch worker --config .aster-orch/config.yaml &
aster_orch dashboard --config .aster-orch/config.yaml
```

The worker continues running after the dashboard exits. Without a running worker, dispatched tasks will queue but not execute.

### 4. Dispatch your first task

From your coding CLI (Claude Code, Codex, Gemini CLI, or OpenCode), just ask it to dispatch work:

> "Dispatch to dev: Add a health check endpoint that returns the current version"

Your CLI uses `orch_dispatch` behind the scenes. You can let it infer the dispatch intent, or name the orchestrator explicitly — both work:

> "Use orch to dispatch to the dev agent: refactor the error handling in src/api.rs to use proper error types"

The agent works in your repo while the dashboard shows progress in real time. When the agent finishes, it sends a review request. Review the work in the dashboard log viewer (`Enter` on the execution) or ask your CLI:

> "Check the status of my dispatch to dev"

> "Show me the transcript for that thread"

Once you're satisfied with the result:

> "Close that thread as completed"

For multi-step work, continue a conversation on the same thread:

> "Dispatch a follow-up to dev on that thread: now add tests for the health check endpoint"

Group related tasks with batches:

> "Dispatch to dev with batch API-CLEANUP: rename all endpoint handlers to follow the new naming convention"

Check batch progress:

> "Show me the batch status for API-CLEANUP"

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

For blocking waits, use the CLI: `aster_orch wait --thread-id <id> --timeout 300`. The MCP transport is unsuitable for long-blocking calls.

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

notifications:
  desktop: false                       # macOS desktop notifications (requires worker restart)

agents:
  - alias: dev                         # Unique name for dispatching
    backend: claude                    # claude | codex | gemini | opencode
    model: claude-sonnet-4-6           # Model to use
    prompt: "..."                      # System prompt for the agent
    # prompt_file: prompts/dev.md      # Or load prompt from file
    # backend_args: ["--flag"]         # Extra CLI args for the backend
    # workdir: /path/to/other/repo     # Per-agent repo override (default: target_repo_root)
    # workspace: shared                # "worktree" for git worktree isolation, "shared" (default)
```

**Path resolution:** Absolute paths are used as-is. `~/` expands to `$HOME`. Relative paths resolve against the config file's directory.

**Multiple agents:** Define as many agents as needed with different backends, models, and prompts. Each agent gets its own concurrency slot.

### Per-Agent Working Directory

By default, all agents work in `target_repo_root`. To have an agent work in a different repository, set `workdir`:

```yaml
agents:
  - alias: orch-dev
    backend: claude
    workdir: /path/to/aster-orch       # Works in a different repo
    workspace: worktree                # Optional: isolated worktree per thread
```

### Workspace Isolation

When `workspace: worktree` is set, each thread dispatched to that agent gets its own git worktree. This prevents concurrent agents from stepping on each other's files:

```yaml
agents:
  - alias: agent-a
    workspace: worktree    # Each thread gets its own worktree
  - alias: agent-b
    workspace: worktree    # Independent worktree, no file conflicts with agent-a
  - alias: reviewer
    workspace: shared      # Default — reads files directly, no isolation needed
```

Worktrees are created at `{state_dir}/worktrees/{thread_id}/` on a branch named `aster-orch/{thread_id}`. They're automatically cleaned up when the thread is closed or abandoned. Requires `workdir` (or `target_repo_root`) to be a git repository — falls back to shared mode for non-git directories.

## How It Works

1. **You dispatch** — ask your CLI to send a task to an agent
2. **Worker claims it** — the background worker picks up the queued execution
3. **Agent executes** — the backend CLI (Claude Code, Codex, Gemini, OpenCode) runs in your repo
4. **Agent replies** — sends a structured response (review-request, status-update, etc.)
5. **You review** — read the output in the dashboard or via `orch_transcript`
6. **You close** — mark the thread as completed, or dispatch follow-up work

The dashboard shows all of this in real time. For the full architecture, see [docs/project/architecture.md](docs/project/architecture.md).

## Troubleshooting

**Agent not responding?** Ask your CLI:

> "Run orch_health to check the worker"

> "Diagnose that thread"

> "Show me recent tasks and their status"

**Stale state / corrupted DB:**
```bash
# Stop all processes, remove state, restart
kill $(pgrep aster_orch)
rm ~/.aster/orch/jobs.sqlite*
aster_orch dashboard --with-worker --config .aster-orch/config.yaml
```

**Worker not picking up work:**
- Ask *"Run orch_health"* — is there a recent heartbeat?
- Ask *"Check orch_metrics"* — is `queue_depth > 0`?
- Verify the agent's backend CLI is installed and authenticated (see Prerequisites)

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
