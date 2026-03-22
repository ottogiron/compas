# Cookbook

Patterns for common compas setups. Each recipe shows a distinct operational pattern — copy the YAML, adapt paths and prompts, and you're set.

For field documentation, see the [Configuration Reference](configuration.md).

---

## Multi-Project Setup

Manage multiple repositories from a single compas instance with project-scoped agent teams.

**The core pattern:** `workdir` scopes an agent to a repo. Agents without `workdir` fall back to `default_workdir`.

```yaml
default_workdir: /home/user/workspace
state_dir: ~/.compas/state

agents:
  # Project A — dev + reviewer
  - alias: project-a-dev
    backend: claude
    model: claude-opus-4-6
    workdir: /home/user/workspace/project-a
    workspace: worktree
    handoff:
      on_response: project-a-reviewer
      max_chain_depth: 2
    prompt: >
      You implement changes in project-a. Follow AGENTS.md.

  - alias: project-a-reviewer
    backend: claude
    model: claude-sonnet-4-6
    workdir: /home/user/workspace/project-a
    prompt: >
      Review only. Prioritize correctness, test gaps, stale docs.
      Report findings by severity. Do not implement.

  # Project B — dev only, shares the reviewer below
  - alias: project-b-dev
    backend: claude
    model: claude-sonnet-4-6
    workdir: /home/user/workspace/project-b
    workspace: worktree
    handoff:
      on_response: shared-reviewer
      max_chain_depth: 2
    prompt: >
      You implement changes in project-b. Follow AGENTS.md.
```

**Adding roles.** Not every agent writes code. Advisory agents analyze but don't implement — use stronger models where depth matters more than speed:

```yaml
  # Shared reviewer — no workdir, works in default_workdir
  - alias: shared-reviewer
    backend: claude
    model: claude-sonnet-4-6
    prompt: >
      Cross-project code reviewer. Read the project's AGENTS.md
      before reviewing. Report findings with file:line references.
      Do not implement — review only.

  # Architect — advisory only, deep analysis
  - alias: architect
    backend: claude
    model: claude-opus-4-6
    timeout_secs: 1200
    prompt: >
      Architecture advisor. Analyze design proposals. Cover prior
      art, tradeoffs, coherence with existing patterns. Reference
      actual code paths. You do NOT implement — advisory only.
```

**Notes:** All agents share one worker process and dashboard. Project-prefixed aliases (`project-a-dev`, `project-b-dev`) prevent name collisions. Agents with `workspace: worktree` get file isolation for concurrent work; reviewers and advisors typically use `shared` mode since they only read. Use `timeout_secs` overrides for agents that routinely run longer — opus analysis tasks often need 1200-1800s vs the default 600s.

---

## Dev-Review-Merge Loop

Automate the implement-review-merge cycle. The dev agent's output auto-routes to a reviewer. The reviewer's feedback routes back to dev for fixes. When the chain settles, close with an atomic merge.

**Single reviewer:**

```yaml
agents:
  - alias: dev
    backend: claude
    model: claude-opus-4-6
    workspace: worktree
    handoff:
      on_response: reviewer
      handoff_prompt: |
        Review for correctness, test coverage, and AGENTS.md compliance.
        List required fixes by severity.
      max_chain_depth: 3
    prompt: >
      You implement changes. When receiving reviewer feedback,
      address all required fixes before resubmitting.

  - alias: reviewer
    backend: claude
    model: claude-sonnet-4-6
    handoff:
      on_response: dev
      handoff_prompt: |
        Address the review findings above. Fix all blocking and
        major issues. Re-run verification before resubmitting.
      max_chain_depth: 3
    prompt: >
      Review code changes. Report findings ordered by severity
      with file:line references. If no blocking issues remain,
      state "LGTM" with residual risks.
```

**Fan-out to multiple reviewers:**

```yaml
  - alias: dev
    handoff:
      on_response: [design-reviewer, correctness-reviewer]
    # ... rest of agent config

  - alias: design-reviewer
    backend: claude
    prompt: >
      Review for architecture quality and maintainability.
      Do not review for correctness — focus on design.

  - alias: correctness-reviewer
    backend: codex
    model: gpt-5.3-codex
    prompt: >
      Review for bugs, edge cases, and test coverage.
      Do not review for design — focus on correctness.
```

**Closing with merge.** When the chain settles and you're satisfied:

```text
"Close that thread as completed and merge into main"
```

Your CLI uses `orch_close` with a `merge` object, which atomically queues the merge before worktree cleanup runs. Use `compas wait-merge --op-id <id>` to block until the merge completes.

**Chain-stop.** To unconditionally stop a chain at a specific agent (without waiting for depth exhaustion), use `on_response: operator`:

```yaml
  - alias: final-reviewer
    handoff:
      on_response: operator    # Chain stops here, operator review required
```

**Notes:** `max_chain_depth: 3` means 3 handoffs total: dev -> reviewer -> dev -> reviewer. The chain stops and inserts a review-request for the operator. Fan-out creates batch-linked threads — use `orch_batch_status` to check when all reviewers have finished. Use `compas wait --thread-id <id> --await-chain` to block until the entire chain (including fan-out) settles.

---

## Scheduled Automation

Run recurring tasks unattended — CI monitoring, nightly health checks, periodic code quality scans.

```yaml
schedules:
  - name: ci-monitor
    agent: reviewer
    cron: "*/5 * * * *"          # Every 5 minutes
    body: "Check CI status for open PRs and report any failures."
    batch: ci-monitoring         # Group all dispatches under one batch
    max_runs: 50                 # Safety cap — stops after 50 fires
    enabled: true

  - name: nightly-health
    agent: dev
    cron: "0 2 * * *"            # Daily at 2:00 AM UTC
    body: >
      Run full health check: verify all services are responsive,
      check disk usage, review error logs from the past 24 hours.
    max_runs: 365

  - name: weekly-lint
    agent: dev
    cron: "0 9 * * 1"            # Mondays at 9:00 AM UTC
    body: >
      Run the full linter suite across the codebase. Fix any
      auto-fixable issues and report the rest.
    batch: weekly-maintenance
    max_runs: 52
    enabled: false               # Paused — re-enable when ready
```

**Notes:** The worker evaluates all enabled schedules every 60 seconds. Run counts are persisted in SQLite — the worker tracks last-fire time per schedule to prevent double-fires on restart. When `max_runs` is reached, the schedule stops firing (bump the value to continue). The Settings tab in the dashboard shows all schedules with their next fire time, run count, and enabled status. `compas doctor` validates that schedule agent aliases exist and cron expressions parse correctly.

---

## Lifecycle Hooks

Integrate compas with external systems by firing shell scripts on execution lifecycle events. Hooks receive JSON event data on stdin and are purely observational — failures are logged as warnings and never affect execution.

```yaml
hooks:
  on_execution_completed:
    - command: ./examples/hooks/notify-slack.sh
      timeout_secs: 10
      env:
        SLACK_WEBHOOK_URL: https://hooks.slack.com/services/T.../B.../xxx
    - command: ./examples/hooks/log-to-file.sh

  on_thread_failed:
    - command: ./scripts/alert-pagerduty.sh
      timeout_secs: 5
```

**Hook points:**

| Hook | Fired when |
|---|---|
| `on_execution_started` | Agent process is spawned |
| `on_execution_completed` | Execution reaches a terminal state (success or failure) |
| `on_thread_closed` | Thread transitions to Completed |
| `on_thread_failed` | Thread transitions to Failed |

**JSON payload** (delivered to stdin). Fields vary by event:

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

`thread_summary` is null when the thread has no summary set. `on_thread_closed` and `on_thread_failed` payloads include `event`, `thread_id`, `new_status`, and `timestamp`. `on_execution_started` includes `event`, `thread_id`, `execution_id`, `agent_alias`, and `timestamp`.

**Ready-to-use scripts** ship in [`examples/hooks/`](../../examples/hooks/):

- `notify-slack.sh` — posts formatted messages to a Slack incoming webhook (requires `SLACK_WEBHOOK_URL` env var)
- `log-to-file.sh` — appends timestamped JSON lines to a log file (default: `/tmp/compas-hooks.log`)

**Behavior:** Multiple hooks per point run sequentially in declaration order. `timeout_secs` (default: 10) sets the SIGTERM deadline; a 5-second grace period follows before SIGKILL (effective ceiling: `timeout_secs + 5s`). Hooks are hot-reloaded — add or remove them without restarting the worker. For webhooks beyond Slack, write `curl` in your hook script.

---

## Custom Backends

Wire in any CLI tool as a compas backend via YAML. If it accepts a prompt and returns text, it can be dispatched to.

**Step 1 — identify the tool's native invocation:**

```bash
aider --message "fix the login timeout bug"
```

**Step 2 — map it to a `backend_definitions` entry:**

```yaml
backend_definitions:
  - name: aider
    command: aider
    args: ["--message", "{{instruction}}"]
```

**Step 3 — create an agent that uses it:**

```yaml
agents:
  - alias: aider-dev
    backend: aider                 # References the backend_definitions name
    prompt: "You implement changes using aider."
```

That's the minimal setup. `{{instruction}}` is replaced with the dispatch message body at runtime.

**Full-featured example** (session resume, JSON output, custom ping, env stripping):

```yaml
backend_definitions:
  - name: my-tool
    command: /usr/local/bin/my-tool
    args: ["--prompt", "{{instruction}}", "--model", "{{model}}"]
    resume:
      flag: "--resume"
      session_id_arg: "{{session_id}}"
    output:
      format: json                 # plaintext (default) | json | jsonl
      result_field: data.text      # Dot-path into JSON response
      session_id_field: sid        # Extract session ID for resume
    ping:
      command: my-tool
      args: ["--health"]
    env_remove:
      - ANTHROPIC_API_KEY          # Strip keys the tool shouldn't see
```

**Template variables:** `{{instruction}}` (dispatch message), `{{model}}` (agent's configured model, omitted if absent), `{{session_id}}` (previous session ID for resume, omitted if absent).

**Output formats:** `plaintext` returns raw stdout as the result. `json` parses stdout as JSON and extracts `result_field` for the result text and `session_id_field` for session resume. `jsonl` parses the last JSON line.

**Notes:** `compas doctor` checks that each custom backend's `command` exists on PATH. Built-in backend names (`claude`, `codex`, `gemini`, `opencode`) cannot be overridden. See [`examples/config-generic.yaml`](../../examples/config-generic.yaml) for the full commented example.
