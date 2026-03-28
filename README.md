# compas

![Status: Pre-v1](https://img.shields.io/badge/status-pre--v1-orange)

Dispatch tasks to AI coding agents from your terminal. They work in isolated git worktrees, hand off to reviewers automatically, and merge back when you approve. You stay in control — agents do the work.

Works with Claude Code, Codex, Gemini CLI, and OpenCode. Point it at any repository.

![Compas dashboard showing active threads and executions](docs/images/dashboard-ops.png)

```bash
brew install ottogiron/tap/compas
```

Pre-built binaries available for macOS (Apple Silicon, Intel) and Linux (x86_64, ARM64).

<details>
<summary><b>Other install methods</b></summary>

Always install from a tagged release — `main` may contain incomplete features.

**Pre-built binary:**

Download the archive for your platform from the [latest release](https://github.com/ottogiron/compas/releases/latest), extract it, and place `compas` on your PATH. Available for `aarch64-darwin`, `x86_64-darwin`, `aarch64-linux`, and `x86_64-linux`.

**cargo-binstall:**

```bash
cargo binstall --git-url https://github.com/ottogiron/compas compas
```

> Note: `--git-url` is required because compas is not published on crates.io.

**From source:**

```bash
cargo install --git https://github.com/ottogiron/compas --tag v0.4.3
```

**Upgrading:** `brew upgrade compas` (or `cargo install --force` with the new tag).
Run `compas doctor` after upgrading. Check the [CHANGELOG](CHANGELOG.md) for breaking changes.

</details>

## Quick Start

> Requires at least one AI coding CLI installed and authenticated. [Details →](#prerequisites)

### 1. Create a config

```bash
compas init
```

Interactively creates `~/.compas/config.yaml` — detects installed backends, prompts for your repo path and agent settings.

### 2. Connect your coding CLI

```bash
compas setup-mcp
```

Registers compas as an MCP server in all detected coding tools (Claude Code, Codex, OpenCode, Gemini CLI).

### 3. Verify setup

```bash
compas doctor
```

Validates config, backends, worker status, and MCP registration. Use `--fix` to auto-remediate issues.

### 4. Start the dashboard

```bash
compas dashboard
```

The dashboard includes an embedded worker by default. Use `--standalone` for monitoring only.

### 5. Dispatch your first task

![Dispatching a task from Claude Code](docs/images/dispatch-1.png)

From your coding CLI, ask it to dispatch work:

> "Dispatch to dev: Add a health check endpoint that returns the current version"

The agent works in your repo while the dashboard shows progress in real time.

![Waiting for results and closing the thread](docs/images/dispatch-2.png)

Review the result in the dashboard or ask your CLI:

> "Check the status of my dispatch to dev"

Close and merge when satisfied:

> "Close that thread as completed and merge into main"

Group related tasks with a batch:

> "Dispatch to dev with batch API-CLEANUP: rename all endpoint handlers"
> "Show me the batch status for API-CLEANUP"

For a structured dispatch-review-complete workflow, see the [orchestration skill example](examples/skills/orch-dispatch/SKILL.md).

## How It Works

```text
Your CLI ──MCP──▶ compas ──▶ Worker ──▶ Agent ──execute──▶ reply
                                                             │
                                   merge ◀──review ◀──handoff─┘
```

You talk to your coding CLI (Claude Code, Codex, etc.) as usual. When you say "dispatch this to dev," the CLI calls compas through MCP — compas queues the work, the background worker picks it up, and launches the agent in an isolated worktree. You never leave your editor.

1. **You ask your CLI** — "dispatch to dev: add a health check endpoint"
2. **CLI calls compas via MCP** — `orch_dispatch` queues an execution for the `dev` agent
3. **Worker claims and executes** — launches the backend CLI in an isolated git worktree
4. **Auto-handoff** — the system routes the response to `reviewer` automatically
5. **Review loop** — `reviewer` reads the diff, replies with feedback, and the chain bounces until `max_chain_depth` is reached
6. **You close and merge** — review the result, close the thread, changes merge back to your branch

Wait for the full chain to settle from a script or CLI:

```bash
compas wait --thread-id <id> --await-chain --timeout 900
```

## Configuration

A minimal config — one agent, one repo:

```yaml
default_workdir: /path/to/repo
state_dir: ~/.compas/state

agents:
  - alias: dev
    backend: claude
    prompt: "You implement changes. Follow the project's AGENTS.md."
```

Add a reviewer with auto-handoff and worktree isolation:

```yaml
agents:
  # Implements changes in an isolated worktree
  - alias: dev
    backend: claude
    model: claude-sonnet-4-6
    workspace: worktree
    handoff:
      on_response: reviewer
      max_chain_depth: 3
    prompt: "You implement changes. Follow the project's AGENTS.md."

  # Reviews work and bounces back to dev
  - alias: reviewer
    backend: claude
    model: claude-sonnet-4-6
    handoff:
      on_response: dev
    prompt: "Review for correctness and test coverage. Do not implement."
```

Key configuration areas: **agents** (backend, model, prompt, workspace isolation, retry, auto-handoff chains) and **schedules** (cron-based recurring dispatches). The worker hot-reloads configuration without restart.

Handoff chains support fan-out to multiple reviewers — see the [Cookbook](docs/guides/cookbook.md).

See the [Configuration Reference](docs/guides/configuration.md) for the full schema and the [Cookbook](docs/guides/cookbook.md) for real-world patterns.

## MCP Tools

After dispatching work via `orch_dispatch`, use `orch_wait` to block for the response. It sends progress notifications every 10s to prevent transport timeouts. If `orch_wait` returns `found=false`, re-issue with the same parameters. Use `await_chain=true` to wait for the entire handoff chain to settle.

### Quick reference

#### Dispatch

| Tool | What it does |
| --- | --- |
| `orch_dispatch` | Send a task to an agent |
| `orch_wait` | Block until response (progress notifications every 10s) |

#### Monitor

| Tool | What it does |
| --- | --- |
| `orch_status` | Thread and execution status by agent or thread |
| `orch_poll` | Quick non-blocking check for new messages |
| `orch_transcript` | Full conversation history for a thread |

#### Act

| Tool | What it does |
| --- | --- |
| `orch_close` | Close a thread as completed or failed |
| `orch_merge` | Queue a merge of a completed thread's branch |
| `orch_abandon` | Cancel a thread and its active executions |

#### Debug

| Tool | What it does |
| --- | --- |
| `orch_health` | Worker heartbeat, backend health, circuit breaker state |
| `orch_diagnose` | Thread diagnostics with suggested next actions |

<details>
<summary><b>All 23 tools — complete reference</b></summary>

#### Core (lifecycle)

| Tool | What it does |
| --- | --- |
| `orch_dispatch` | Send a task to an agent (creates a thread, queues execution). Accepts optional `summary`, `scheduled_for` (ISO 8601) for delayed execution, and `skip_handoff` to suppress auto-handoff on the response |
| `orch_wait` | Block until a matching message arrives or timeout; sends progress notifications every 10s |
| `orch_close` | Close a thread as `completed` or `failed`. Completed worktree threads require a completed `orch_merge` first |
| `orch_abandon` | Cancel a thread and its active executions |
| `orch_reopen` | Reopen a closed/failed/abandoned thread |

#### Monitor

| Tool | What it does |
| --- | --- |
| `orch_status` | Thread and execution status (filter by agent or thread); includes `scheduled_count` |
| `orch_poll` | Quick non-blocking check for new messages |
| `orch_transcript` | Full conversation history for a thread |
| `orch_read` | Read a single message by reference |
| `orch_batch_status` | Status breakdown for all threads in a batch |
| `orch_tasks` | Execution history with timing and results; `filter="scheduled"` lists pending scheduled executions |
| `orch_metrics` | Aggregate stats (thread counts, queue depth) |
| `orch_diagnose` | Thread diagnostics with suggested next actions |
| `orch_execution_events` | Structured events from a running/completed execution |
| `orch_read_log` | Paginated access to execution log files |
| `orch_tool_stats` | Per-tool call counts, error rates, and cost breakdown |

#### Merge

| Tool | What it does |
| --- | --- |
| `orch_merge` | Queue a merge of a completed thread's branch into a target branch |
| `orch_merge_status` | Query merge operation detail or aggregate overview |
| `orch_merge_cancel` | Cancel a queued merge operation |

#### System

| Tool | What it does |
| --- | --- |
| `orch_health` | Worker heartbeat, backend health pings, circuit breaker state |
| `orch_list_agents` | List configured agents with backend/model info |
| `orch_session_info` | Current MCP session metadata |
| `orch_worktrees` | List active git worktrees for agent isolation |

</details>

### CLI Equivalents

For non-MCP or scripted-shell workflows, use the CLI commands directly.

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

**`compas wait-merge` flags:**

Use `compas wait-merge --op-id <id>` to block until a merge operation reaches a terminal status. The op ID is returned by `orch_merge`.

| Flag | Description |
| --- | --- |
| `--op-id <id>` | Merge operation ID (ULID) to wait on (required) |
| `--timeout <secs>` | Timeout in seconds (default: 120) |
| `--config <path>` | Config file path (default: `~/.compas/config.yaml`) |

**Exit codes:** `0` = completed, `1` = failed/cancelled/timeout, `2` = error (including unknown op ID).

## Dashboard

The TUI dashboard shows real-time orchestrator state across four tabs: **Ops** (active threads, executions, merge queue), **Agents** (health and execution history per agent), **History** (recent executions with batch grouping), and **Settings** (config overview and schedules). Navigate with `Tab`/`1-4`, drill into executions with `Enter`, view thread conversations with `c`, and press `?` for keyboard help. The dashboard runs an embedded worker by default — use `--standalone` for monitoring only.

See the [Dashboard Guide](docs/guides/dashboard.md) for full keyboard shortcuts and tips.

## More Information

- [Configuration Reference](docs/guides/configuration.md) — full schema, agent fields, handoff chains, schedules, hooks, custom backends
- [Cookbook](docs/guides/cookbook.md) — multi-project teams, dev-review-merge loops, scheduled automation, lifecycle hooks, custom backends
- [Dashboard Guide](docs/guides/dashboard.md) — tabs, keyboard shortcuts, tips
- [Architecture](docs/project/architecture.md) — system design and internals
- [Design Decisions](docs/project/DECISIONS.md) — architectural decision records
- [Development](AGENTS.md) — development workflow and contribution guide

### Troubleshooting

**Agent not responding?** Ask your CLI:

> "Run orch_health to check the worker"
> "Diagnose that thread"

**Stale state / corrupted DB:**

```bash
# Stop all processes, remove state, restart
kill $(pgrep compas)
rm ~/.compas/state/jobs.sqlite*
compas dashboard
```

**Worker not picking up work:**

- Check `orch_health` for a recent heartbeat
- Check `orch_metrics` for `queue_depth > 0`
- Verify the agent's backend CLI is installed and authenticated (see [Prerequisites](#prerequisites))

## Prerequisites

At least one backend CLI installed and authenticated:

| Backend | Install | Authenticate |
| --- | --- | --- |
| Claude Code | `npm install -g @anthropic-ai/claude-code` | `claude login` |
| Codex | `npm install -g @openai/codex` | `codex login` |
| Gemini CLI | `npm install -g @google/gemini-cli` | `gemini auth` |
| OpenCode | See [opencode.ai](https://opencode.ai) | varies by provider |

> Note: The Gemini backend is stateless — it does not support session resume on follow-up dispatches to the same thread.

## Development

```bash
make setup-hooks       # Install pre-commit hook
make verify            # fmt-check + clippy + tests + lint-md
make dashboard-dev     # Dashboard + worker on isolated dev DB
```

See [AGENTS.md](AGENTS.md) for the full development workflow and [CONTRIBUTING.md](CONTRIBUTING.md) for contribution guidelines.

## License

Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE) at your option.
