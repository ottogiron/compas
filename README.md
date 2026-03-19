# compas

Multi-agent orchestrator for AI-assisted software development. Dispatch tasks to AI coding agents, monitor execution in a TUI dashboard, and manage the full lifecycle from your terminal.

Works with Claude Code, Codex, Gemini CLI, and OpenCode. Project-agnostic — point it at any repository.

![Compas dashboard showing active threads and executions](docs/images/dashboard-ops.png)

## Prerequisites

- **Rust** toolchain (`cargo`)
- At least one backend CLI installed and authenticated:

| Backend | Install | Authenticate |
| --- | --- | --- |
| Claude Code | `npm install -g @anthropic-ai/claude-code` | `claude login` |
| Codex | `npm install -g @openai/codex` | `codex login` |
| Gemini CLI | `npm install -g @google/gemini-cli` | `gemini auth` |
| OpenCode | See [opencode.ai](https://opencode.ai) | varies by provider |

## Install

Add this to your ~/.cargo/config.toml:

```toml
[net]
git-fetch-with-cli = true
```

Then run

```bash
# you will be asked for github authentication in this step
cargo install --git https://github.com/ottogiron/compas
```

This puts `compas` on your PATH. Or build from source:

```bash
git clone git@github.com:ottogiron/compas.git
cd compas
cargo build --release
# Binary at target/release/compas — add to PATH or use the full path below
```

## Quick Start

### 1. Create a config

Create `~/.compas/config.yaml` (the default location):

```yaml
target_repo_root: /path/to/your/project
state_dir: ~/.compas/state

agents:
  - alias: dev
    backend: claude
    model: claude-sonnet-4-6
    prompt: >
      You are a development agent. Follow the project's AGENTS.md.
```

Supported backends: `claude`, `codex`, `gemini`, `opencode`. See the [Configuration Reference](#configuration-reference) for all available fields (concurrency, trigger intents, timeouts, handoff chains, etc.).

> **Note:** The Gemini backend is stateless — it does not support session resume on follow-up dispatches to the same thread. Each execution starts a fresh session.

### 2. Connect your coding CLI

Add the MCP server to your preferred tool. If you used `cargo install`, you can use `compas` directly. For source builds, use the full path to `target/release/compas`.

**Claude Code:**

```bash
# --config is optional if using the default location (~/.compas/config.yaml)
claude mcp add --scope user --transport stdio compas -- \
  compas mcp-server
```

**Codex:**

```bash
# --config is optional if using the default location (~/.compas/config.yaml)
codex mcp add compas -- \
  compas mcp-server
```

**OpenCode** — add to `opencode.json` (project root) or `~/.config/opencode/opencode.json` (global):

```json
{
  "mcp": {
    "compas": {
      "type": "local",
      "command": ["compas", "mcp-server"]
    }
  }
}
```

`--config <path>` is optional if using the default location (`~/.compas/config.yaml`).

**Gemini CLI** — add to `.gemini/settings.json`:

```json
{
  "mcpServers": {
    "compas": {
      "command": "compas",
      "args": ["mcp-server"]
    }
  }
}
```

`--config <path>` is optional if using the default location (`~/.compas/config.yaml`).

### 3. Start the worker

The worker is the background process that picks up dispatched tasks and runs your agents. The dashboard includes an embedded worker by default.

```bash
# Dashboard + embedded worker (recommended)
compas dashboard

# Dashboard only (monitoring, no execution) — use when running worker separately
compas dashboard --standalone

# Or run the worker as a standalone process
compas worker
```

`--config <path>` is optional if using the default location (`~/.compas/config.yaml`).

Only one worker can run at a time. If a worker is already running, the dashboard detects it and skips spawning a second one. Running `compas worker` when another worker is alive fails with an actionable error showing the existing worker's PID.

When the dashboard exits, it sends SIGTERM to the embedded worker, which drains in-flight executions and shuts down. A standalone `compas worker` process is independent and must be stopped separately. Without a running worker, dispatched tasks will queue but not execute.

### 4. Dispatch your first task

![Dispatching a task from Claude Code](docs/images/dispatch-1.png)

From your coding CLI (Claude Code, Codex, Gemini CLI, or OpenCode), just ask it to dispatch work:

> "Dispatch to dev: Add a health check endpoint that returns the current version"

Your CLI uses `orch_dispatch` behind the scenes. You can let it infer the dispatch intent, or name the orchestrator explicitly — both work:

> "Use orch to dispatch to the dev agent: refactor the error handling in src/api.rs to use proper error types"

The agent works in your repo while the dashboard shows progress in real time.

![Waiting for results and closing the thread](docs/images/dispatch-2.png)

Review the work in the dashboard log viewer (`Enter` on the execution) or ask your CLI:

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

### 5. Install the orchestration skill (recommended)

A skill teaches your coding CLI the full dispatch-review-complete workflow. Copy the example skill into your project and install it following your tool's instructions:

```bash
cp -r /path/to/compas/examples/skills/orch-dispatch your-project/skills/
```

The skill covers: worker delegation, reviewer routing, session continuity, worktree isolation, retry behavior, and failure handling. See [examples/skills/orch-dispatch/SKILL.md](examples/skills/orch-dispatch/SKILL.md) for the full reference.

## Dashboard

The TUI dashboard shows real-time orchestrator state across four tabs:

- **Ops** — active threads, running executions, batch progress
- **Agents** — configured agents with health status
- **History** — completed executions with duration and status
- **Settings** — current configuration

| Key | Action |
| --- | --- |
| `Tab` / `Shift+Tab` | Next / previous tab |
| `1-4` | Jump to tab |
| `↑/↓` or `j/k` | Navigate rows |
| `g` / `G` | Jump to first / last row |
| `Enter` | Open log viewer / drill into batch |
| `c` | Open conversation view (Ops tab, see below) |
| `x` / `Esc` | Clear batch drill-down |
| `r` | Refresh current tab |
| `?` | Keyboard help |
| `q` / `Ctrl+C` | Quit |

**Log viewer** (`Enter` on an execution):

| Key | Action |
| --- | --- |
| `↑/↓` or `j/k` | Navigate sections |
| `Enter` | Expand / collapse section |
| `g` / `G` | Jump to top / bottom |
| `PgUp` / `PgDn` | Page scroll |
| `f` | Toggle follow mode |
| `J` | Pretty-print JSON |
| `Esc` | Back to dashboard |

**Conversation view** (`c` on a thread in Ops tab):

![Conversation view showing dispatch and agent replies](docs/images/dashboard-conversation.png)

| Key | Action |
| --- | --- |
| `↑/↓` or `j/k` | Scroll line by line |
| `g` / `G` | Jump to top / bottom |
| `PgUp` / `PgDn` | Page scroll |
| `f` | Toggle follow mode (auto-scroll to new messages) |
| `Esc` | Back to dashboard |

## MCP Tools

For blocking waits, use the CLI: `compas wait --thread-id <id> --since db:<msg-id> --timeout 300`. The `--since` cursor ensures you only match replies after your dispatch message. Add `--await-chain` to wait for all threads in the chain to settle (useful after fan-out handoffs). The MCP transport is unsuitable for long-blocking calls. The `orch_dispatch` response includes a `next_step` field with a ready-to-use wait command.

**`compas wait` flags:**

| Flag | Description |
| --- | --- |
| `--thread-id <id>` | Thread to wait on (required) |
| `--since <cursor>` | Only match messages newer than this (`db:<msg-id>` or numeric) |
| `--intent <intent>` | Wait for a specific intent (e.g. `response`, `review-request`) |
| `--strict-new` | Only match messages strictly newer than the `--since` cursor |
| `--timeout <secs>` | Timeout in seconds (default: 120) |
| `--await-chain` | Keep waiting until the entire handoff chain settles |

**Exit codes:** `0` = matching message found, `1` = timeout (no match within deadline), `2` = error.

### Core

| Tool | What it does |
| --- | --- |
| `orch_dispatch` | Send a task to an agent (creates a thread, queues execution) |
| `orch_close` | Close a thread as `completed` or `failed` |
| `orch_abandon` | Cancel a thread and its active executions |
| `orch_reopen` | Reopen a closed/failed/abandoned thread |

### Monitor

| Tool | What it does |
| --- | --- |
| `orch_status` | Thread and execution status (filter by agent or thread) |
| `orch_poll` | Quick non-blocking check for new messages |
| `orch_transcript` | Full conversation history for a thread |
| `orch_read` | Read a single message by reference |
| `orch_batch_status` | Status breakdown for all threads in a batch |
| `orch_tasks` | Execution history with timing and results |
| `orch_metrics` | Aggregate stats (thread counts, queue depth) |
| `orch_diagnose` | Thread diagnostics with suggested next actions |
| `orch_execution_events` | Structured events from a running/completed execution (tool calls, file edits) |
| `orch_read_log` | Paginated access to execution log files with offset/limit/tail support |

### System

| Tool | What it does |
| --- | --- |
| `orch_health` | Worker heartbeat + backend health pings |
| `orch_list_agents` | List configured agents with backend/model info |
| `orch_session_info` | Current MCP session metadata |
| `orch_worktrees` | List active git worktrees for agent isolation |

## Configuration Reference

The default config location is `~/.compas/config.yaml`. Use `--config <path>` to override it for any subcommand (`worker`, `mcp-server`, `dashboard`, `wait`).

```yaml
target_repo_root: /path/to/repo        # Where agents work (required)
state_dir: ~/.compas/state               # Runtime state: DB, logs (required)
poll_interval_secs: 1                  # Worker poll frequency
# worktree_dir: /custom/worktrees     # Override worktree parent dir (default: {repo_root}/../.compas-worktrees/)

orchestration:
  trigger_intents: [dispatch, handoff, changes-requested]  # Intents that trigger execution
  execution_timeout_secs: 600           # Per-task timeout
  max_concurrent_triggers: 4            # Global concurrency limit (default: number of worker agents)
  max_triggers_per_agent: 2             # Per-agent concurrency limit
  stale_active_secs: 3600              # Staleness threshold for idle threads
  ping_timeout_secs: 15                # Backend health check timeout
  # log_retention_count: 100      # Max execution log files to retain (default: 100)

database:                              # SQLite connection pool (requires restart to change)
  max_connections: 32
  min_connections: 4
  acquire_timeout_ms: 30000

notifications:
  desktop: false                       # macOS desktop notifications (requires worker restart)

agents:
  - alias: dev                         # Unique name for dispatching
    backend: claude                    # claude | codex | gemini | opencode
    model: claude-sonnet-4-6           # Model to use
    prompt: "..."                      # System prompt for the agent
    # prompt_file: prompts/dev.md      # Or load prompt from file (takes precedence over prompt)
    # timeout_secs: 900                # Per-agent timeout override (default: execution_timeout_secs)
    # env:                             # Per-agent environment variables
    #   SOME_VAR: value
    # backend_args: ["--flag"]         # Extra CLI args for the backend
    # workdir: /path/to/other/repo     # Per-agent repo override (default: target_repo_root)
    # workspace: shared                # "worktree" for git worktree isolation, "shared" (default)
    # max_retries: 0              # Auto-retry on transient failure (0 = disabled)
    # retry_backoff_secs: 30      # Base delay between retries (doubles each attempt)
    # handoff:                          # Auto-handoff chain routing
    #   on_response: other-agent        # String (single target) or list (fan-out): [agent-a, agent-b]
    #   handoff_prompt: |               # Custom prompt prepended to auto-generated handoff context
    #     Review for correctness, test coverage, and AGENTS.md compliance.
    #   max_chain_depth: 3              # Max auto-handoffs before forcing operator review (default: 3)
```

**Path resolution:** Absolute paths are used as-is. `~/` expands to `$HOME`. Relative paths resolve against the config file's directory.

**Multiple agents:** Define as many agents as needed with different backends, models, and prompts. Each agent gets its own concurrency slot.

**Live config reload:** The worker hot-reloads the following fields without restart: `agents`, `trigger_intents`, `max_triggers_per_agent`, `ping_timeout_secs`, `log_retention_count`, `notifications`. Changes to `target_repo_root`, `state_dir`, `database`, and `max_concurrent_triggers` require a restart.

### Per-Agent Working Directory

By default, all agents work in `target_repo_root`. To have an agent work in a different repository, set `workdir`:

```yaml
agents:
  - alias: orch-dev
    backend: claude
    workdir: /path/to/compas       # Works in a different repo
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

Worktrees are created at `{repo_root}/../.compas-worktrees/{thread_id}/` on a branch named `compas/{thread_id}`. The parent directory can be overridden via the top-level `worktree_dir` config field. They're automatically cleaned up when the thread is completed or abandoned. Failed threads retain their worktrees for inspection. Requires `workdir` (or `target_repo_root`) to be a git repository — falls back to shared mode for non-git directories.

### Retry on Transient Failure

When `max_retries` is set on an agent, transient failures (network blips, temporary rate limits) are automatically retried with exponential backoff. Non-retryable failures (quota exhaustion, auth errors, agent errors) fail immediately.

Each retry creates a new execution entry. Check `orch_tasks` for `attempt_number` to see retry history. The thread stays Active during retries — it only fails when all retries are exhausted.

### Auto-Handoff Chains

Agents can automatically chain to other agents based on `on_response` routing. Configure `handoff` on an agent to dispatch its output to the next agent without operator intervention — for example, a dev agent that auto-routes to a reviewer, which routes back to dev.

Agents reply naturally with no protocol overhead — the system assigns `response` intent to all agent replies and handles routing via config.

```yaml
agents:
  - alias: dev
    backend: claude
    model: claude-sonnet-4-6
    prompt: "You implement changes. Follow the project's AGENTS.md."
    handoff:
      on_response: reviewer            # Dev's replies go to reviewer
      handoff_prompt: |                # Custom instructions prepended to handoff context
        Review for correctness, test coverage, and AGENTS.md compliance.

  - alias: reviewer
    backend: claude
    model: claude-sonnet-4-6
    prompt: "You review code changes for correctness and quality."
    handoff:
      on_response: dev                 # Reviewer's replies go back to dev
```

**Fan-out:** `on_response` accepts a list to route one agent's output to multiple agents simultaneously:

```yaml
    handoff:
      on_response: [reviewer, reviewer-2]   # Fan-out to two reviewers in parallel
```

Fan-out creates one new batch-linked thread per target agent. All fan-out threads share the same batch ID, so you can track aggregate results with `orch_batch_status`. The operator is the join point — use `orch_batch_status` to see when all reviewers have finished, then decide next steps.

**Custom prompt:** `handoff_prompt` text is prepended to the auto-generated handoff context (which includes the originating thread's transcript). Use it to give the receiving agent specific instructions for that handoff.

**Chain depth limit:** `max_chain_depth` (default: 3) caps the number of consecutive auto-handoffs on a thread. When the limit is reached, the chain stops and a review-request is inserted for the operator. This prevents runaway loops.

**Waiting for chain settlement:** Use `compas wait --thread-id <id> --await-chain` to block until all threads in the chain (including fan-out threads) have settled.

**Viewing chains:** In the dashboard, open a thread's conversation (`c` on a thread in the Ops tab) to see the full chain of dispatch → reply → handoff → reply messages. Use `orch_transcript` from your CLI to see the same history. Handoff messages appear with intent `handoff` in the transcript.

## How It Works

1. **You dispatch** — ask your CLI to send a task to an agent
2. **Worker claims it** — the background worker picks up the queued execution
3. **Agent executes** — the backend CLI (Claude Code, Codex, Gemini, OpenCode) runs in your repo
4. **Agent replies** — the system assigns `response` intent to all agent replies (agents reply naturally, no protocol overhead)
5. **Auto-handoff** (optional) — if the agent has `on_response` configured, a new handoff message is auto-inserted, triggering the target agent. The chain runs autonomously up to `max_chain_depth`, then forces operator review.
6. **You review** — read the output in the dashboard or via `orch_transcript`
7. **You close** — mark the thread as completed, or dispatch follow-up work

The dashboard shows all of this in real time. For the full architecture, see [docs/project/architecture.md](docs/project/architecture.md).

## Troubleshooting

**Agent not responding?** Ask your CLI:

> "Run orch_health to check the worker"
> "Diagnose that thread"
> "Show me recent tasks and their status"

**Stale state / corrupted DB:**

```bash
# Stop all processes, remove state, restart
kill $(pgrep compas)
rm ~/.compas/state/jobs.sqlite*
compas dashboard
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

See [AGENTS.md](AGENTS.md) for the full development workflow and [CONTRIBUTING.md](CONTRIBUTING.md) for contribution guidelines.

## License

Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE) at your option.
