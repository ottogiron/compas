# Configuration Reference

Complete reference for all compas configuration fields. This covers the YAML schema, agent settings, orchestration tuning, scheduling, lifecycle hooks, and custom backends.

For usage patterns and recipes, see the [README](../../README.md). For architecture details, see [architecture.md](../project/architecture.md).

## Overview

Compas reads its configuration from `~/.compas/config.yaml` by default. Override the location with `--config <path>` on any subcommand.

To create a config interactively:

```bash
compas init
```

Use `--non-interactive` for scripted setups:

```bash
compas init --non-interactive --repo /path/to/project --backend claude
```

See [`examples/config-generic.yaml`](../../examples/config-generic.yaml) for a fully commented example.

## Full Schema

```yaml
default_workdir: /path/to/repo           # Default working directory for agents (required)
state_dir: ~/.compas/state               # Runtime state: DB, logs (required)
poll_interval_secs: 1                    # Worker poll frequency (default: 1)
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
  stale_active_secs: 3600               # Staleness threshold for idle threads (default: 3600)
  ping_timeout_secs: 15                  # Backend health check timeout (default: 15)
  ping_cache_ttl_secs: 60               # TTL for cached ping results (default: 60)
  log_retention_count: 100               # Max execution log files to retain (default: 100)
  merge_timeout_secs: 30                 # Timeout for merge operations (default: 30)
  default_merge_strategy: merge          # Merge strategy: "merge", "rebase", or "squash" (default: "merge")

database:                                # SQLite connection pool (requires restart to change)
  max_connections: 32                    # Pool max connections (default: 32)
  min_connections: 4                     # Pool min idle connections (default: 4)
  acquire_timeout_ms: 30000              # Pool acquire timeout in milliseconds (default: 30000)

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
      on_response: other-agent           # String (single target) or list (fan-out): [agent-a, agent-b]
      handoff_prompt: |                  # Custom prompt prepended to auto-generated handoff context
        Review for correctness, test coverage, and AGENTS.md compliance.
      max_chain_depth: 3                 # Max auto-handoffs before forcing operator review (default: 3)

schedules:
  - name: ci-monitor                     # Unique schedule name (required)
    agent: reviewer                      # Target agent alias (required, must exist in agents)
    cron: "*/5 * * * *"                  # Standard cron expression (required)
    body: "Check CI status."             # Dispatch message body (required)
    batch: ci-monitoring                 # Optional batch ID for grouping dispatches
    max_runs: 50                         # Safety cap (default: 100)
    enabled: true                        # Active flag (default: true)

hooks:
  on_execution_started:                  # Fired when agent process is spawned
    - command: /path/to/script.sh        # Command on PATH or absolute path (required)
      args: ["--flag", "value"]          # Optional positional args
      timeout_secs: 10                   # Kill timeout in seconds (default: 10)
      env:                               # Optional extra env vars for this hook
        SLACK_WEBHOOK_URL: https://...
  on_execution_completed: []             # Fired when execution reaches terminal state
  on_thread_closed: []                   # Fired when thread transitions to Completed
  on_thread_failed: []                   # Fired when thread transitions to Failed

backend_definitions:
  - name: aider                          # Backend name, referenced by agent backend: field (required)
    command: aider                       # CLI command to invoke (required)
    args: ["--message", "{{instruction}}"]  # Args with template variables
    resume:                              # Optional session resume configuration
      flag: "--resume"
      session_id_arg: "{{session_id}}"
    output:                              # Output format and extraction
      format: json                       # plaintext (default) | json | jsonl
      result_field: data.text            # Dot-path into JSON response
      session_id_field: sid              # Field to extract session ID for resume
    ping:                                # Optional custom ping/liveness check
      command: aider
      args: ["--health"]
    env_remove:                          # Environment variables to strip before spawning
      - ANTHROPIC_API_KEY
```

## Agents

### Agent Fields

| Field | Required | Default | Description |
| --- | --- | --- | --- |
| `alias` | yes | -- | Unique name used for dispatching |
| `backend` | yes | -- | Backend identifier: `claude`, `codex`, `gemini`, `opencode`, or a custom backend name from `backend_definitions` |
| `model` | no | -- | Model identifier passed to the backend CLI |
| `prompt` | no | -- | Inline system prompt for the agent |
| `prompt_file` | no | -- | Path to a file containing the system prompt (takes precedence over `prompt` if both are set) |
| `role` | no | `worker` | `worker` (triggered by dispatches) or `operator` (coordinator, never triggered) |
| `timeout_secs` | no | `execution_timeout_secs` | Per-agent timeout override |
| `env` | no | -- | Map of environment variables injected into the agent's process |
| `backend_args` | no | -- | Extra CLI flags/args appended before instruction text |
| `workdir` | no | `default_workdir` | Per-agent working directory override |
| `workspace` | no | `shared` | Workspace isolation mode: `"worktree"` or `"shared"` |
| `max_retries` | no | `0` | Maximum retry attempts for transient failures (0 = disabled) |
| `retry_backoff_secs` | no | `30` | Base delay in seconds between retries (doubles each attempt) |
| `handoff` | no | -- | Auto-handoff chain routing configuration (see [Auto-Handoff Chains](#auto-handoff-chains)) |

**Multiple agents:** Define as many agents as needed with different backends, models, and prompts. Each agent gets its own concurrency slot.

### Per-Agent Working Directory

By default, all agents work in `default_workdir`. To have an agent work in a different repository, set `workdir`:

```yaml
agents:
  - alias: dev
    backend: claude
    workdir: /path/to/other/repo       # Works in a different repo
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

Worktrees are created at `{repo_root}/.compas-worktrees/{thread_id}/` on a branch named `compas/{thread_id}`. The parent directory can be overridden via the top-level `worktree_dir` config field. They're automatically cleaned up when the thread is completed or abandoned. Failed threads retain their worktrees for inspection. Requires `workdir` (or `default_workdir`) to be a git repository -- falls back to shared mode for non-git directories.

### Retry on Transient Failure

When `max_retries` is set on an agent, transient failures (network blips, temporary rate limits) are automatically retried with exponential backoff. Non-retryable failures (quota exhaustion, auth errors, agent errors) fail immediately.

Each retry creates a new execution entry. Check `orch_tasks` for `attempt_number` to see retry history. The thread stays Active during retries -- it only fails when all retries are exhausted.

## Auto-Handoff Chains

Agents can automatically chain to other agents based on `on_response` routing. Configure `handoff` on an agent to dispatch its output to the next agent without operator intervention -- for example, a dev agent that auto-routes to a reviewer, which routes back to dev.

Agents reply naturally with no protocol overhead -- the system assigns `response` intent to all agent replies and handles routing via config.

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

Fan-out creates one new batch-linked thread per target agent. All fan-out threads share the same batch ID, so you can track aggregate results with `orch_batch_status`. The operator is the join point -- use `orch_batch_status` to see when all reviewers have finished, then decide next steps.

**Custom prompt:** `handoff_prompt` text is prepended to the auto-generated handoff context (which includes the originating thread's transcript). Use it to give the receiving agent specific instructions for that handoff.

**Chain depth limit:** `max_chain_depth` (default: 3) caps the number of consecutive auto-handoffs on a thread. When the limit is reached, the chain stops and a review-request is inserted for the operator. This prevents runaway loops.

**Waiting for chain settlement:** Use `compas wait --thread-id <id> --await-chain` to block until all threads in the chain (including fan-out threads) have settled.

**Viewing chains:** In the dashboard, open a thread's conversation (`c` on a thread in the Ops tab) to see the full chain of dispatch -> reply -> handoff -> reply messages. Use `orch_transcript` from your CLI to see the same history. Handoff messages appear with intent `handoff` in the transcript.

## Recurring Schedules

Define cron-based recurring dispatches that the worker evaluates automatically. Useful for CI monitoring, periodic health checks, or any recurring task.

```yaml
schedules:
  - name: ci-monitor              # Unique schedule name
    agent: reviewer                # Target agent alias (must exist in agents)
    cron: "*/5 * * * *"            # Standard cron expression (every 5 minutes)
    body: "Check CI status for open PRs and report any failures."
    batch: ci-monitoring           # Optional batch ID for grouping dispatches
    max_runs: 50                   # Safety cap (default: 100) — stops after this many fires
    enabled: true                  # Active flag (default: true)

  - name: nightly-health
    agent: dev
    cron: "0 2 * * *"              # Daily at 2:00 AM UTC
    body: "Run full health check: verify all services are responsive, check disk usage, review error logs from the past 24 hours."
    max_runs: 365
```

**Schedule fields:**

| Field | Required | Default | Description |
| --- | --- | --- | --- |
| `name` | yes | -- | Unique identifier for the schedule |
| `agent` | yes | -- | Target agent alias (must exist in `agents`) |
| `cron` | yes | -- | Standard cron expression (5 fields: minute, hour, day, month, weekday) |
| `body` | yes | -- | Dispatch message body sent to the agent |
| `batch` | no | `null` | Batch/ticket ID attached to each dispatch |
| `max_runs` | no | `100` | Safety cap -- schedule stops firing after this many runs |
| `enabled` | no | `true` | Set to `false` to pause without removing the config |

**How it works:**

1. The worker evaluates all enabled schedules every 60 seconds
2. When a cron expression is due, the worker creates a dispatch message targeting the configured agent
3. Run counts are persisted in SQLite -- the worker tracks last-fire time per schedule to prevent double-fires on restart
4. When `max_runs` is reached, the schedule stops firing (bump the value or reset the schedule to continue)

**Dashboard visibility:** The Settings tab shows all configured schedules with their agent, cron expression, next fire time, run count, and enabled status.

**Doctor validation:** `compas doctor` validates that schedule agent aliases exist and cron expressions parse correctly, reporting issues as warnings.

## Lifecycle Hooks

Compas fires shell scripts at named execution lifecycle events. Scripts run as subprocesses, receive event data as JSON on stdin, and are subject to a configurable timeout. All failures are logged as warnings and never affect execution -- hooks are purely observational.

**Hook points:**

| Hook point | Fired when |
|---|---|
| `on_execution_started` | Agent process is spawned for a new execution |
| `on_execution_completed` | Execution reaches a terminal state (success or failure) |
| `on_thread_closed` | Thread transitions to `Completed` status |
| `on_thread_failed` | Thread transitions to `Failed` status |

> Note: `Abandoned` and `ExecutionRetrying` events are not hooked (Phase 2).

**Config example:**

```yaml
hooks:
  on_execution_completed:
    - command: ./scripts/notify-slack.sh
      timeout_secs: 10
    - command: ./scripts/log-audit.sh
  on_thread_failed:
    - command: ./scripts/alert-pagerduty.sh
      timeout_secs: 5
```

**Full `HookEntry` fields:**

```yaml
hooks:
  on_execution_started:
    - command: /path/to/script.sh   # Required: path or command name on PATH
      args: ["--flag", "value"]     # Optional: positional args
      timeout_secs: 10              # Optional: kill timeout (default: 10)
      env:                          # Optional: extra env vars for this hook
        SLACK_WEBHOOK_URL: https://hooks.slack.com/...
```

**JSON payload examples** (delivered to hook's stdin):

`on_execution_started`:

```json
{
  "event": "execution_started",
  "thread_id": "01ABC...",
  "execution_id": "01XYZ...",
  "agent_alias": "dev",
  "timestamp": "2026-03-21T17:00:00Z"
}
```

`on_execution_completed`:

```json
{
  "event": "execution_completed",
  "thread_id": "01ABC...",
  "execution_id": "01XYZ...",
  "agent_alias": "dev",
  "success": true,
  "duration_ms": 12345,
  "thread_summary": "Fix login timeout bug",
  "timestamp": "2026-03-21T17:00:00Z"
}
```

`on_thread_closed`:

```json
{
  "event": "thread_closed",
  "thread_id": "01ABC...",
  "new_status": "Completed",
  "timestamp": "2026-03-21T17:00:00Z"
}
```

`on_thread_failed`:

```json
{
  "event": "thread_failed",
  "thread_id": "01ABC...",
  "new_status": "Failed",
  "timestamp": "2026-03-21T17:00:00Z"
}
```

**Behavior notes:**

- Hooks run in `default_workdir` (no per-hook `workdir` override in Phase 1).
- Multiple hooks per point run **sequentially** in declaration order.
- A failing hook is logged as a warning and does not prevent subsequent hooks.
- **Hot-reload:** Add or remove hooks in config without restarting the worker.
- Webhooks: write `curl` in your hook script -- no built-in HTTP support needed.

See [`examples/hooks/`](../../examples/hooks/) for ready-to-use scripts.

## Custom Backends

Define CLI-based backends entirely in YAML using `backend_definitions`. Any CLI tool that accepts a prompt and returns text can be wired in without writing Rust code.

**Minimal example** (aider):

```yaml
backend_definitions:
  - name: aider
    command: aider
    args: ["--message", "{{instruction}}"]

agents:
  - alias: aider-dev
    backend: aider
    prompt: "You implement changes using aider."
```

**Full example** (with resume, JSON output, custom ping, env stripping):

```yaml
backend_definitions:
  - name: my-tool
    command: /usr/local/bin/my-tool
    args: ["--prompt", "{{instruction}}", "--model", "{{model}}"]
    resume:
      flag: "--resume"
      session_id_arg: "{{session_id}}"
    output:
      format: json              # plaintext (default) | json | jsonl
      result_field: data.text   # dot-path into JSON response
      session_id_field: sid     # field to extract session ID for resume
    ping:
      command: my-tool
      args: ["--health"]
    env_remove:
      - ANTHROPIC_API_KEY       # strip keys the tool shouldn't see
```

**Template variables** available in `args`:

| Variable | Description |
| --- | --- |
| `{{instruction}}` | The dispatch message / task text |
| `{{model}}` | Agent's configured `model` (omitted if absent) |
| `{{session_id}}` | Previous session ID for resume (omitted if absent) |

**Output formats:**

- `plaintext` (default) -- raw stdout is the result text
- `json` -- parse stdout as JSON, extract `result_field` for result text and `session_id_field` for session resume
- `jsonl` -- parse the last JSON line, extract fields as above

**Doctor integration:** `compas doctor` checks that each generic backend's `command` exists on PATH and reports missing commands as warnings.

See [`examples/config-generic.yaml`](../../examples/config-generic.yaml) for a complete example.

## Live Reload

The worker hot-reloads the following fields without restart: `agents`, `schedules`, `trigger_intents`, `max_triggers_per_agent`, `ping_timeout_secs`, `ping_cache_ttl_secs`, `log_retention_count`, `notifications`.

Changes to the following fields require a worker restart: `default_workdir`, `state_dir`, `database`, `max_concurrent_triggers`.

## Path Resolution

- **Absolute paths** are used as-is.
- **`~/`** expands to `$HOME`.
- **Relative paths** resolve against the config file's parent directory.

This applies to `default_workdir`, `state_dir`, `worktree_dir`, per-agent `workdir`, and `prompt_file`.

---

Back to the [README](../../README.md).
