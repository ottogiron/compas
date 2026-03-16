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

## Inputs

- Target worker alias — check available agents with `orch_list_agents()`.
- Reviewer alias — the agent that reviews code. Set this to your designated reviewer agent.
- Task description with acceptance criteria.

---

## Session Continuity

Follow-up dispatches to the **same thread + same agent** automatically resume the agent's prior CLI session. The agent retains full conversational context — what it did, what files it changed, what the reviewer said. This happens transparently: no extra flags or parameters needed.

This means the `changes-requested` → rework loop (Step 8 → Step 3) is significantly more effective: the agent can make targeted fixes instead of re-reading the entire codebase. Always prefer continuing a thread over creating a new one for related work.

**New thread = fresh session.** Only close a thread and open a new one when you want the agent to start with a clean slate.

## Worktree Isolation

Agents with `workspace: worktree` run in isolated git worktrees. Each thread gets its own copy of the repo on a branch. This prevents concurrent agents from conflicting on files.

When reviewing work from a worktree agent, the changes are on the worktree branch — not on main. The operator merges or cherry-picks the work after approval.

Worktrees are automatically cleaned up when threads are completed or abandoned. Failed threads retain their worktrees for inspection.

## Automatic Retry

Agents with `max_retries > 0` automatically retry on transient failures (network errors, temporary rate limits). Quota exhaustion and auth failures are never retried.

The operator does NOT need to do anything differently — retries are transparent. If all retries are exhausted, the thread fails normally and the operator can re-dispatch.

Check `orch_tasks` for `attempt_number` to see if an execution was a retry.

---

## Mode A — Worker Delegation

### Step 1 — Health check

```
orch_health(alias="<worker>")
```

Verify the worker is available and the backend is responsive.

### Step 2 — Dispatch to worker

```
orch_dispatch(
  from="operator",
  to="<worker>",
  intent="dispatch",
  body="<task + acceptance criteria>",
  batch="<optional-batch-id>"
)
```

Save `thread_id` and dispatch message `reference` (e.g. `db:42`).

**Tips for effective dispatch prompts:**
- Be specific about what to implement and where
- Include acceptance criteria (what does "done" look like?)
- Reference relevant files or modules if you know them
- Include verification instructions (what commands to run)

### Step 3 — Wait for worker response

Use the CLI wait (not the MCP tool — it was removed due to transport timeout issues):

```bash
aster_orch wait \
  --config <path-to-config> \
  --thread-id <thread-id> \
  --intent review-request \
  --since db:<dispatch-message-id> \
  --timeout 900
```

> **Why `--intent review-request`?** Implementers explicitly signal `review-request` to gate on operator approval before merging. This is intentional — it distinguishes "I need a decision" from a plain status reply.

### Step 4 — Contract check

Verify the worker's `review-request` message contains:

1. **TL;DR** — summary of what was done
2. **Files Changed** — list of modified files
3. **Verification** — command run + result
4. **Next Action** — what the operator should do

If any field is missing, reject immediately with feedback specifying what's missing.

### Step 5 — Run verification

Execute the verification command the worker claims to have run. This is project-specific — for example:

```bash
# Examples — use YOUR project's verification command
npm test                    # Node.js
cargo test                  # Rust
pytest                      # Python
make verify                 # If you have a Makefile
```

If verification fails, reject with the failure output.

### Step 6 — Dispatch to reviewer

The operator does NOT review code. A reviewer agent does.

**Trivial exception:** For trivial worker output (config tweak, typo fix, mechanical rename), the operator may skip this step and approve directly. If in doubt, send to reviewer.

For non-trivial work:

```
orch_health(alias="<reviewer>")

orch_dispatch(
  from="operator",
  to="<reviewer>",
  intent="dispatch",
  body="Review the following changes. Report findings ordered by severity
(blocking, major, minor, nit) with file:line references.

## Scope
<git diff --stat summary + key file diffs>

## Context
<what the worker was asked to do>

## Focus
Correctness, test coverage, doc alignment, scope creep, no unrelated changes.

If no issues, state residual risks and verification gaps.",
  batch="<batch-id>"
)
```

Save the reviewer `thread_id` and `reference`.

### Step 7 — Wait for reviewer findings

```bash
aster_orch wait \
  --config <path-to-config> \
  --thread-id <reviewer-thread-id> \
  --since db:<reviewer-dispatch-message-id> \
  --timeout 300
```

> **Why no `--intent` flag here?** Reviewers reply with `response` (the default intent). No filter is needed — any reply from the reviewer thread is the findings.

### Step 8 — Act on findings

Based on reviewer response:

- **No blocking findings → close both threads:**

  ```
  orch_close(from="operator", thread_id="<reviewer-thread-id>", status="completed", note="Review passed")
  orch_close(from="operator", thread_id="<worker-thread-id>", status="completed", note="Approved after review")
  ```

  Then commit the worker's changes.

- **Blocking findings → request changes from worker:**

  ```
  orch_dispatch(
    from="operator",
    to="<worker>",
    thread_id="<worker-thread-id>",
    intent="changes-requested",
    body="<reviewer findings, verbatim or summarized>"
  )
  ```

  Close the reviewer thread:

  ```
  orch_close(from="operator", thread_id="<reviewer-thread-id>", status="completed", note="Changes requested from worker based on findings")
  ```

  Then loop back to Step 3. Use the `reference` from the `changes-requested` dispatch as the new `--since` value.

- **Blocking findings → operator fixes directly:**

  If the fixes are straightforward (the reviewer gave clear instructions), the operator may apply them directly instead of sending back to the worker. After applying fixes:

  1. Run verification
  2. Send the updated diff to the reviewer for a second pass (Mode B Step 3)
  3. Do NOT skip review just because the operator made the fix — reviewer must confirm the findings are addressed

- **Unclear findings → ask reviewer for clarification:**

  ```
  orch_dispatch(
    from="operator",
    to="<reviewer>",
    thread_id="<reviewer-thread-id>",
    intent="dispatch",
    body="Clarify finding #N: <question>"
  )
  ```

  Then wait again on the reviewer thread with `--since db:<clarification-message-id>`.

---

## Mode B — Operator Self-Review

When the operator implements a change inline and the change is non-trivial enough to warrant review:

### Step 1 — Implement the change

Operator makes the code changes directly.

### Step 2 — Run verification

```bash
# Use your project's verification command
npm test / cargo test / pytest / make verify
```

### Step 3 — Dispatch own diff to reviewer

```bash
# Gather the diff
git diff --stat
git diff
```

```
orch_health(alias="<reviewer>")

orch_dispatch(
  from="operator",
  to="<reviewer>",
  intent="dispatch",
  body="Review my changes before commit. Report findings ordered by severity
with file:line references.

## Scope
<git diff --stat + key diffs>

## Context
<what was changed and why>

## Focus
Correctness, test coverage, no regressions.",
  batch="<batch-id>"
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

```
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
- **Reviewer unresponsive:** Check `orch_health(alias="<reviewer>")`. If unhealthy, operator may do a manual code review as fallback (read the full diff) and document that reviewer was bypassed.
- **Debugging slow executions:** Use `orch_execution_events(execution_id=...)` to see what tool calls the agent has made so far.
