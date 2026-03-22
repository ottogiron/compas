# Configuration Reference

Complete reference for all compas configuration fields.

For usage patterns, see the [Cookbook](cookbook.md). For architecture details, see [architecture.md](../project/architecture.md).

## Overview

Compas reads its configuration from `~/.compas/config.yaml` by default. Override the location with `--config <path>` on any subcommand.

```bash
compas init                        # Interactive config creation
compas init --non-interactive \
  --repo /path/to/project \
  --backend claude                 # Scripted setup
```

See [`examples/config-generic.yaml`](../../examples/config-generic.yaml) for a fully commented starter config.

## Full Schema

```yaml
default_workdir: /path/to/repo           # Default working directory for agents (required)
state_dir: ~/.compas/state               # Runtime state: DB, logs (required)
poll_interval_secs: 1                    # Worker poll frequency (default: 1, range: 1..=3600)
worktree_dir: /custom/worktrees          # Override worktree parent dir (default: {repo_root}/.compas-worktrees/)

models:                                  # Optional model catalog (informational only)
  - claude-sonnet-4-6                    # Plain string form
  - id: gpt-5.4                          # Object form with metadata
    backend: codex
    description: "Codex default model"
    timeout_secs: 900                    # Optional per-model timeout hint

orchestration:
  trigger_intents:                       # Intents that trigger execution
    - dispatch                           #   (default: [dispatch, handoff, changes-requested])
    - handoff
    - changes-requested
  execution_timeout_secs: 600            # Per-task timeout (default: 600)
  max_concurrent_triggers: 4             # Global concurrency limit (default: number of worker agents)
  max_triggers_per_agent: 2              # Per-agent concurrency limit (default: 1)
  stale_active_secs: 3600               # Staleness threshold for idle threads (default: 3600, range: 60..=604800)
  ping_timeout_secs: 15                  # Backend health check timeout (default: 15)
  ping_cache_ttl_secs: 60               # TTL for cached ping results (default: 60)
  log_retention_count: 100               # Max execution log files to retain (default: 100)
  merge_timeout_secs: 30                 # Timeout for merge operations (default: 30)
  default_merge_strategy: merge          # Merge strategy: "merge", "rebase", or "squash" (default: "merge")
  default_merge_target: main             # Target branch for auto-merge on close (default: "main")

database:                                # SQLite connection pool (requires restart to change)
  max_connections: 32                    # Pool max connections (default: 32, min: 1)
  min_connections: 4                     # Pool min idle connections (default: 4, min: 1, <= max_connections)
  acquire_timeout_ms: 30000              # Pool acquire timeout in milliseconds (default: 30000, min: 100)

notifications:
  desktop: false                         # macOS desktop notifications (default: false, requires worker restart)

agents:
  - alias: dev                           # Unique name for dispatching (required)
    backend: claude                      # claude | codex | gemini | opencode | <custom> (required)
    model: claude-sonnet-4-6             # Model to use
    prompt: "..."                        # System prompt for the agent
    prompt_file: prompts/dev.md          # Load prompt from file (takes precedence over prompt)
    role: worker                         # worker (default, triggered by dispatches) | operator (never triggered)
    timeout_secs: 900                    # Per-agent timeout override (default: execution_timeout_secs)
    env:                                 # Per-agent environment variables
      SOME_VAR: value
    backend_args: ["--flag"]             # Extra CLI args for the backend
    workdir: /path/to/other/repo         # Per-agent workdir override (default: default_workdir)
    workspace: worktree                  # "worktree" for git worktree isolation, "shared" (default)
    max_retries: 0                       # Auto-retry on transient failure (default: 0 = disabled)
    retry_backoff_secs: 30               # Base delay between retries, doubles each attempt (default: 30)
    handoff:                             # Auto-handoff chain routing
      on_response: other-agent           # String or list: agent alias, "operator" (chain-stop), or [agent-a, agent-b] (fan-out)
      handoff_prompt: |                  # Custom prompt prepended to auto-generated handoff context
        Review for correctness, test coverage, and AGENTS.md compliance.
      max_chain_depth: 3                 # Max auto-handoffs before forcing operator review (default: 3, range: 1..=20)

# schedules, hooks, and backend_definitions shown in their own sections below
```

All hook-point keys (`on_execution_started`, `on_execution_completed`, `on_thread_closed`, `on_thread_failed`) are optional — omit any you don't use. See the [Lifecycle Hooks cookbook recipe](cookbook.md#lifecycle-hooks) for examples.

Custom backends are defined via `backend_definitions`. See the [Custom Backends cookbook recipe](cookbook.md#custom-backends) for integration walkthroughs, or the field reference below.

## Agents

### Agent Fields

| Field | Required | Default | Description |
| --- | --- | --- | --- |
| `alias` | yes | -- | Unique name used for dispatching |
| `backend` | yes | -- | Backend: `claude`, `codex`, `gemini`, `opencode`, or a custom name from `backend_definitions` |
| `model` | no | -- | Model identifier passed to the backend CLI |
| `prompt` | no | -- | Inline system prompt |
| `prompt_file` | no | -- | Path to prompt file (takes precedence over `prompt` if both set) |
| `role` | no | `worker` | `worker` (triggered by dispatches) or `operator` (never triggered) |
| `timeout_secs` | no | `execution_timeout_secs` | Per-agent timeout override |
| `env` | no | -- | Environment variables injected into the agent's process |
| `backend_args` | no | -- | Extra CLI flags appended before instruction text |
| `workdir` | no | `default_workdir` | Per-agent working directory override |
| `workspace` | no | `shared` | `"worktree"` for git worktree isolation, `"shared"` for direct access |
| `max_retries` | no | `0` | Retry attempts for transient failures (0 = disabled) |
| `retry_backoff_secs` | no | `30` | Base delay between retries (doubles each attempt) |
| `handoff` | no | -- | Auto-handoff routing (see [Auto-Handoff Chains](#auto-handoff-chains)) |

### Per-Agent Working Directory

By default, all agents work in `default_workdir`. Set `workdir` to scope an agent to a different repo:

```yaml
agents:
  - alias: dev
    backend: claude
    workdir: /path/to/other/repo
    workspace: worktree
```

### Workspace Isolation

`workspace: worktree` gives each thread its own git worktree, preventing concurrent agents from stepping on each other's files:

```yaml
agents:
  - alias: agent-a
    workspace: worktree    # Isolated worktree per thread
  - alias: agent-b
    workspace: worktree    # Independent, no file conflicts with agent-a
  - alias: reviewer
    workspace: shared      # Default — reads files directly
```

Worktrees are created at `{repo_root}/.compas-worktrees/{thread_id}/` on branch `compas/{thread_id}`. Override the parent directory with the top-level `worktree_dir` field. Worktrees are cleaned up when threads are completed or abandoned; failed threads retain theirs for inspection. Falls back to shared mode for non-git directories.

### Retry on Transient Failure

`max_retries` auto-retries transient failures (network blips, rate limits) with exponential backoff. Non-retryable failures (quota exhaustion, auth errors) fail immediately. Each retry creates a new execution entry — check `orch_tasks` for `attempt_number`. The thread stays Active during retries and only fails when all retries are exhausted.

## Auto-Handoff Chains

Configure `handoff` on an agent to auto-route its output to another agent. Agents reply naturally — the system assigns `response` intent and handles routing via config.

```yaml
agents:
  - alias: dev
    handoff:
      on_response: reviewer          # Dev's output goes to reviewer
      handoff_prompt: |
        Review for correctness and test coverage.

  - alias: reviewer
    handoff:
      on_response: dev               # Reviewer's feedback goes back to dev
```

See the [Dev-Review-Merge Loop cookbook recipe](cookbook.md#dev-review-merge-loop) for complete examples.

**Fan-out:** `on_response` accepts a list to route to multiple agents in parallel:

```yaml
    handoff:
      on_response: [reviewer-a, reviewer-b]
```

Fan-out creates batch-linked threads. Use `orch_batch_status` to check aggregate progress.

**Chain-stop:** Set `on_response: operator` to explicitly stop a chain and force operator review, without waiting for depth exhaustion.

**Custom prompt:** `handoff_prompt` is prepended to auto-generated handoff context (which includes the originating thread's transcript).

**Depth limit:** `max_chain_depth` (default: 3, range: 1..=20) caps consecutive auto-handoffs. At the limit, a review-request is inserted for the operator.

**Waiting:** `compas wait --thread-id <id> --await-chain` blocks until the entire chain settles.

**Viewing:** Press `c` on a thread in the dashboard Ops tab, or use `orch_transcript` from your CLI.

## Recurring Schedules

Cron-based recurring dispatches evaluated by the worker. See the [Scheduled Automation cookbook recipe](cookbook.md#scheduled-automation) for examples.

| Field | Required | Default | Description |
| --- | --- | --- | --- |
| `name` | yes | -- | Unique schedule identifier |
| `agent` | yes | -- | Target agent alias (must exist in `agents`) |
| `cron` | yes | -- | Standard cron expression (5 fields: minute, hour, day, month, weekday) |
| `body` | yes | -- | Dispatch message body |
| `batch` | no | -- | Batch/ticket ID attached to each dispatch |
| `max_runs` | no | `100` | Safety cap — stops firing after this many runs |
| `enabled` | no | `true` | Set to `false` to pause without removing |

The worker evaluates schedules every 60 seconds. Run counts and last-fire times are persisted in SQLite to prevent double-fires on restart. The Settings tab shows schedule status. `compas doctor` validates agent aliases and cron expressions.

## Lifecycle Hooks

Shell scripts fired on execution lifecycle events. See the [Lifecycle Hooks cookbook recipe](cookbook.md#lifecycle-hooks) for integration patterns and ready-to-use scripts.

| Hook | Fired when |
|---|---|
| `on_execution_started` | Agent process is spawned |
| `on_execution_completed` | Execution reaches a terminal state |
| `on_thread_closed` | Thread transitions to Completed |
| `on_thread_failed` | Thread transitions to Failed |

Each hook entry takes: `command` (required), `args` (optional), `timeout_secs` (default: 10, effective ceiling: timeout + 5s grace before SIGKILL), and `env` (optional). All hook-point keys are optional — omit any you don't use.

Hooks receive JSON event data on stdin. `thread_summary` may be null. Multiple hooks per point run sequentially. Failures are logged as warnings only. Hooks are hot-reloaded.

## Custom Backends

Define CLI-based backends in YAML via `backend_definitions`. See the [Custom Backends cookbook recipe](cookbook.md#custom-backends) for integration walkthroughs.

| Field | Required | Description |
| --- | --- | --- |
| `name` | yes | Backend identifier (cannot use built-in names: claude, codex, gemini, opencode) |
| `command` | yes | CLI command to invoke |
| `args` | no | CLI args with template variables: `{{instruction}}`, `{{model}}`, `{{session_id}}` |
| `resume` | no | Session resume: `flag` (e.g. `--resume`) + `session_id_arg` |
| `output` | no | Output parsing: `format` (plaintext/json/jsonl), `result_field`, `session_id_field` |
| `ping` | no | Custom liveness check: `command` + `args` |
| `env_remove` | no | Environment variables to strip before spawning |

## Live Reload

The worker hot-reloads these fields without restart: `agents`, `schedules`, `hooks`, `trigger_intents`, `max_triggers_per_agent`, `ping_timeout_secs`, `ping_cache_ttl_secs`, `log_retention_count`, `notifications`.

These fields require a worker restart: `default_workdir`, `state_dir`, `database`, `max_concurrent_triggers`, `default_merge_target`, `worktree_dir`.

## Path Resolution

- **Absolute paths** are used as-is.
- **`~/`** expands to `$HOME`.
- **Relative paths** resolve against the config file's parent directory.

Applies to `default_workdir`, `state_dir`, `worktree_dir`, per-agent `workdir`, and `prompt_file`.

---

Back to the [README](../../README.md).
