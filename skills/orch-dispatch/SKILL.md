---
name: orch-dispatch
description: Operator dispatch-review-complete loop for delegating work to worker agents via the orchestrator.
---

# orch-dispatch

## Description

Full lifecycle for dispatching implementation work, getting it reviewed, and closing the thread. Covers two modes:

- **Mode A — Worker delegation** (default): operator dispatches work to a worker agent, then sends the result to a reviewer agent for code review.
- **Mode B — Operator self-review**: operator implements a change inline, then sends their own diff to a reviewer agent before committing.

Do not use for trivial fixes (typo, single-line config) where dispatch overhead exceeds value.

Ticket lifecycle (`ticket start`, `ticket done`, branch, merge) is owned by `/dev-workflow`.

## Inputs

- Active ticket or batch context from `/dev-workflow`
- Target worker alias (routing: `orch-dev` for aster-orch development work)
- Reviewer alias: `orch-reviewer` (configured in the production orch config). Verify with `orch_list_agents()`.
- Task description with acceptance criteria

---

## Session Continuity

Follow-up dispatches to the **same thread + same agent** automatically resume the agent's prior CLI session. The agent retains full conversational context — what it did, what files it changed, what the reviewer said. This happens transparently: no extra flags or parameters needed.

This means the `changes-requested` → rework loop (Step 8 → Step 3) is significantly more effective: the agent can make targeted fixes instead of re-reading the entire codebase. Always prefer continuing a thread over creating a new one for related work.

**New thread = fresh session.** Only close a thread and open a new one when you want the agent to start with a clean slate.

---

## Worktree Isolation

Agents with `workspace: worktree` run in isolated git worktrees. Each thread gets its own copy of the repo on branch `aster-orch/{thread_id}`. This prevents concurrent agents from conflicting on files.

When reviewing work from a worktree agent, the changes are on the worktree branch — not on main. The operator merges or cherry-picks the work after approval.

Worktrees are automatically cleaned up when threads are completed or abandoned. Failed threads retain their worktrees for inspection.

## Automatic Retry

Agents with `max_retries > 0` automatically retry on transient failures (network errors, temporary rate limits). Quota exhaustion and auth failures are never retried.

The operator does NOT need to do anything differently — retries are transparent. If all retries are exhausted, the thread fails normally and the operator can re-dispatch.

Check `orch_tasks` for `attempt_number` to see if an execution was a retry.

---

## Mode A — Worker Delegation

### Step 1 — Health check

```text
orch_health(alias="<worker>")
```

Use the **production** orch (`aster-orch` MCP server) for dispatching work. The dev orch (`aster-orch-dev`) is for testing MCP changes only.

### Step 2 — Dispatch to worker

```text
orch_dispatch(
  from="operator",
  to="<worker>",
  intent="dispatch",
  body="<task + acceptance criteria>",
  batch="<ticket-or-batch-id>"
)
```

Save `thread_id` and dispatch message `reference` (e.g. `db:42`).

### Step 3 — Wait for worker response

Use the CLI wait (not MCP `orch_wait` — removed due to stdio transport timeout issues).

```bash
aster_orch wait \
  --thread-id <thread-id> \
  --since db:<dispatch-message-id> \
  --timeout 900
```

`--config <path>` is optional if using the default location (`~/.aster-orch/config.yaml`).

> **No `--intent` filter by default.** When omitted, the wait matches any non-trigger reply. This is the safest approach. Agents don't manage intents (ADR-015) — all replies get `response` automatically.
>
> Add `--await-chain` to block until the entire handoff chain settles (including auto-forwarded reviewers).

### Step 4 — Contract check

Verify the worker's `review-request` message contains the 4 required fields (see `references/review-request-contract.md`):

1. TL;DR present
2. File paths changed listed
3. Verification command + result included
4. Next action requested

If any field is missing, reject immediately with feedback specifying what's missing.

### Step 5 — Run verification

Execute the verification command the worker claims to have run:

```bash
make verify   # fmt-check + clippy + test
```

If verification fails, reject with the failure output.

### Step 6 — Dispatch to reviewer

The operator does NOT review code. A reviewer agent does.

**Trivial exception:** For trivial worker output (config tweak, typo fix, mechanical rename), the operator may skip this step and approve directly. If in doubt, send to reviewer.

For non-trivial work:

```text
orch_health(alias="orch-reviewer")

orch_dispatch(
  from="operator",
  to="orch-reviewer",
  intent="dispatch",
  body="Review the following changes. Report findings ordered by severity (blocking, major, minor, nit) with file:line references.\n\n## Scope\n<git diff --stat summary + key file diffs>\n\n## Context\n<ticket ID, batch, what the worker was asked to do>\n\n## Focus\nCorrectness, test coverage, doc alignment, scope creep, no unrelated changes.\n\nIf no issues, state residual risks and verification gaps.",
  batch="<ticket-or-batch-id>"
)
```

Save the reviewer `thread_id` and `reference`.

### Step 7 — Wait for reviewer findings

```bash
aster_orch wait \
  --thread-id <reviewer-thread-id> \
  --since db:<reviewer-dispatch-message-id> \
  --timeout 300
```

`--config <path>` is optional if using the default location (`~/.aster-orch/config.yaml`).

> **Why no `--intent` flag here?** Reviewers reply with `response` (the default intent). No filter is needed — any reply from the reviewer thread is the findings.

### Step 8 — Act on findings

Based on reviewer response:

- **No blocking findings → close both threads:**

  ```text
  orch_close(from="operator", thread_id="<reviewer-thread-id>", status="completed", note="Review passed")
  orch_close(from="operator", thread_id="<worker-thread-id>", status="completed", note="Approved after review")
  ```

  Then commit the worker's changes.

- **Blocking findings → request changes from worker:**

  ```text
  orch_dispatch(
    from="operator",
    to="<worker>",
    thread_id="<worker-thread-id>",
    intent="changes-requested",
    body="<reviewer findings, verbatim or summarized>"
  )
  ```

  Close the reviewer thread:

  ```text
  orch_close(from="operator", thread_id="<reviewer-thread-id>", status="completed", note="Changes requested from worker based on findings")
  ```

  Then loop back to Step 3. Use the `reference` from the `changes-requested` dispatch as the new `--since` value.

- **Blocking findings → operator fixes directly:**

  If the fixes are straightforward (the reviewer gave clear instructions), the operator may apply them directly instead of sending back to the worker. After applying fixes:

  1. Run verification: `make verify`
  2. Send the updated diff to the reviewer for a second pass (Mode B Step 3)
  3. Do NOT skip review just because the operator made the fix — reviewer must confirm the findings are addressed

- **Unclear findings → ask reviewer for clarification:**

  ```text
  orch_dispatch(
    from="operator",
    to="orch-reviewer",
    thread_id="<reviewer-thread-id>",
    intent="dispatch",
    body="Clarify finding #N: <question>"
  )
  ```

  Then wait again on the reviewer thread with `--since db:<clarification-message-id>`.

---

## Mode B — Operator Self-Review

When the operator implements a change inline (trivial fix, config cleanup, etc.) and the change is non-trivial enough to warrant review:

### Step 1 — Implement the change

Operator makes the code changes directly.

### Step 2 — Run verification

```bash
make verify
```

### Step 3 — Dispatch own diff to reviewer

```bash
# Gather the diff
git diff --stat
git diff
```

```text
orch_health(alias="orch-reviewer")

orch_dispatch(
  from="operator",
  to="orch-reviewer",
  intent="dispatch",
  body="Review my changes before commit. Report findings ordered by severity with file:line references.\n\n## Scope\n<git diff --stat + key diffs>\n\n## Context\n<what was changed and why>\n\n## Focus\nCorrectness, test coverage, no regressions.",
  batch="<ticket-or-batch-id>"
)
```

### Step 4 — Wait and act on findings

Same as Mode A Steps 7-8. Fix issues if any, then commit.

---

## When to Skip Reviewer

The reviewer dispatch (Step 6 in Mode A, Step 3 in Mode B) may be skipped for:

- Single-line config/typo fixes
- Mechanical field deletion (e.g., removing dead code already confirmed unused)
- Documentation-only changes with no behavioral impact

When skipping, state the reason out loud so the decision is auditable:
> "Skipping reviewer — trivial config cleanup, no behavioral change."

---

## Required Checks

- Review-request contract compliance (all 4 required fields)
- Verification command actually passes (not just claimed by worker)
- `orch_health` and `orch_tasks` checked for timeout or unexpected behavior
- Reviewer findings acted on — do not ignore blocking findings

## Output Format

```text
Thread ID: <worker-thread-id>
Worker: <alias>
Review Thread: <reviewer-thread-id> (or "skipped — <reason>")
Review Decision: approved / changes-requested / operator-takeover
Verification Result: pass / fail
Completion Status: completed / rejected / abandoned
```

## Failure Handling

- **CLI wait timeout:** `aster_orch wait` exits `1`. Run `orch_poll(thread_id=<thread-id>)`, `orch_tasks(alias="<worker>")`, and `orch_diagnose(thread_id="<thread-id>")` before deciding to continue waiting, abandon, or re-dispatch.
- **CLI wait error:** `aster_orch wait` exits `2`. Verify worker process + config path, then retry.
- **Backend unhealthy:** Check `orch_health(alias="<worker>")` for backend ping status and worker heartbeat.
- **Stale thread:** Use `orch_abandon(thread_id="<thread-id>")` and re-dispatch.
- **Change-request loop:** After 2 `changes-requested` dispatches on the same worker thread, consider operator takeover.
- **Reviewer unresponsive:** Check `orch_health(alias="orch-reviewer")`. If unhealthy, operator may do a manual code review as fallback (read the full diff) and document that reviewer was bypassed.
- **Debugging slow executions:** Use `orch_execution_events(execution_id=...)` to see what tool calls the agent has made so far.
