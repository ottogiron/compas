# Cookbook -- Configuration Patterns

Practical patterns for common compas setups. Each recipe is self-contained -- copy the YAML, adapt the paths and prompts, and you're set. See the [Configuration Reference](configuration.md) for field documentation.

---

## Multi-Project Agent Teams

Manage agents across multiple repositories from a single compas instance. Each project gets its own agent team with project-prefixed aliases while sharing one worker and dashboard.

```yaml
default_workdir: /home/user/workspace
state_dir: ~/.compas/state

agents:
  # -- Frontend project agents --
  - alias: frontend-dev
    backend: claude
    model: claude-sonnet-4-6
    workdir: /home/user/workspace/frontend
    workspace: worktree
    handoff:
      on_response: frontend-reviewer
      max_chain_depth: 2
    prompt: >
      You are the frontend development agent. Implement changes in the
      frontend repository. Follow AGENTS.md. Run tests before submitting.

  - alias: frontend-reviewer
    backend: claude
    model: claude-sonnet-4-6
    workdir: /home/user/workspace/frontend
    prompt: >
      You review frontend changes. Check for correctness, test coverage,
      accessibility, and coding standards. Do not implement -- review only.

  # -- Backend project agents --
  - alias: backend-dev
    backend: claude
    model: claude-opus-4-6
    workdir: /home/user/workspace/backend
    workspace: worktree
    handoff:
      on_response: backend-reviewer
      max_chain_depth: 2
    prompt: >
      You are the backend development agent. Implement changes in the
      backend service. Follow AGENTS.md. Run make verify before submitting.

  - alias: backend-reviewer
    backend: claude
    model: claude-sonnet-4-6
    workdir: /home/user/workspace/backend
    prompt: >
      You review backend changes. Prioritize correctness bugs, missing
      error handling, and test gaps. Do not implement -- review only.

  # -- Shared agent (works in whichever repo is specified per dispatch) --
  - alias: shared-docs
    backend: claude
    model: claude-sonnet-4-6
    prompt: >
      You review and improve documentation across any repository.
      Check for accuracy, completeness, and consistency with code.
```

**Notes:** All agents share one worker process and dashboard. The `default_workdir` can point to a workspace root -- agents with explicit `workdir` always work in their specified repo. Agents without `workdir` (like `shared-docs`) fall back to `default_workdir`, which lets them serve any project when the dispatch body specifies context.

---

## Role-Based Agent Design

Assign different roles to agents: implementers that write code, reviewers that check it, architects that advise on design, and UX advisors that evaluate interfaces. Match models to task complexity.

```yaml
agents:
  - alias: dev
    backend: claude
    model: claude-opus-4-6
    workspace: worktree
    handoff:
      on_response: reviewer
      max_chain_depth: 2
    prompt: >
      You implement changes in this repository. Follow AGENTS.md.
      Run the full test suite before submitting work.

  - alias: reviewer
    backend: claude
    model: claude-sonnet-4-6
    prompt: >
      You review code changes. Prioritize correctness bugs, behavior
      regressions, missing tests, and stale docs. Report findings
      ordered by severity. Do not implement -- review only.

  - alias: architect
    backend: claude
    model: claude-opus-4-6
    prompt: >
      You are the architecture advisor. Analyze design proposals and
      produce technical evaluations. Cover prior art, tradeoffs,
      coherence with existing patterns, and a clear recommendation.
      Reference actual modules and code paths, not abstract advice.
      You do NOT implement code -- advisory only.

  - alias: ux-advisor
    backend: claude
    model: claude-sonnet-4-6
    prompt: >
      You evaluate user-facing interfaces: CLI output, error messages,
      configuration ergonomics, and documentation clarity. Assess
      information hierarchy, consistency, and actionability. Provide
      specific improvement suggestions with rationale. Advisory only.
```

**Notes:** Advisory agents still receive dispatches but their prompts instruct them to analyze only -- they produce recommendations, not code changes. Use stronger models (opus) for advisory roles where depth matters more than speed. Implementers benefit from `workspace: worktree` for file isolation; reviewers and advisors typically work in `shared` mode since they only read.

---

## Review Chains

Automate the dev-to-reviewer loop. When the dev agent completes work, its output automatically routes to a reviewer. The reviewer's feedback routes back to the dev for fixes, up to a configurable depth.

```yaml
agents:
  - alias: dev
    backend: claude
    model: claude-opus-4-6
    workspace: worktree
    handoff:
      on_response: reviewer
      handoff_prompt: |
        Review the implementation for correctness, test coverage,
        and AGENTS.md compliance. List required fixes by severity.
      max_chain_depth: 3
    prompt: >
      You implement changes in this repository. When receiving reviewer
      feedback, address all required fixes before resubmitting.

  - alias: reviewer
    backend: claude
    model: claude-sonnet-4-6
    handoff:
      on_response: dev
      handoff_prompt: |
        Address the review findings listed above. Fix all blocking
        and major issues. Re-run verification before resubmitting.
      max_chain_depth: 3
    prompt: >
      You review code changes. Prioritize correctness bugs, behavior
      regressions, and missing tests. Report findings ordered by
      severity with file:line references and explicit required fixes.
      If no blocking issues remain, state "LGTM" with residual risks.
```

**Notes:** `max_chain_depth` prevents infinite loops -- when reached, the chain stops and the thread awaits operator review. A depth of 3 means: dev -> reviewer -> dev -> reviewer (3 handoffs), which gives one full fix-and-re-review cycle. Use `compas wait --thread-id <id>` to block until the chain settles. The `handoff_prompt` is prepended to the auto-generated handoff context, so the receiving agent sees both the routing instructions and the previous agent's output.

---

## Fan-Out Review

Route one agent's output to multiple reviewers in parallel for different perspectives -- design review, correctness review, security review.

```yaml
agents:
  - alias: dev
    backend: claude
    model: claude-opus-4-6
    workspace: worktree
    handoff:
      on_response: [design-reviewer, correctness-reviewer]
    prompt: >
      You implement changes in this repository. Follow AGENTS.md.
      Run make verify before submitting work.

  - alias: design-reviewer
    backend: claude
    model: claude-sonnet-4-6
    prompt: >
      Review for architecture quality, design patterns, and long-term
      maintainability. Flag coupling issues, abstraction leaks, and
      naming problems. Do not review for correctness -- focus on design.

  - alias: correctness-reviewer
    backend: codex
    model: gpt-5.3-codex
    prompt: >
      Review for bugs, edge cases, error handling gaps, and test
      coverage. Do not review for design -- focus on correctness.
      Provide file:line references for all findings.
```

**Notes:** Fan-out creates batch-linked threads -- one thread per reviewer, all linked to the original dispatch. Use `orch_batch_status` to check when all reviewers have finished. The operator is the join point: review all feedback, then decide next steps (dispatch fixes, close, or escalate). Fan-out reviewers do not automatically hand back to the dev; this is intentional to keep the operator in the loop for multi-perspective reviews.

---

## Cross-Project Agent Sharing

Reuse a reviewer (or any agent) across multiple projects. Project-specific dev agents hand off to a shared reviewer that works in whatever repo the dispatch originates from.

```yaml
default_workdir: /home/user/workspace

agents:
  - alias: project-a-dev
    backend: claude
    model: claude-opus-4-6
    workdir: /home/user/workspace/project-a
    workspace: worktree
    handoff:
      on_response: shared-reviewer
      max_chain_depth: 2
    prompt: >
      You implement changes in project-a. Follow AGENTS.md.

  - alias: project-b-dev
    backend: claude
    model: claude-opus-4-6
    workdir: /home/user/workspace/project-b
    workspace: worktree
    handoff:
      on_response: shared-reviewer
      max_chain_depth: 2
    prompt: >
      You implement changes in project-b. Follow AGENTS.md.

  - alias: shared-reviewer
    backend: claude
    model: claude-sonnet-4-6
    # No workdir -- uses default_workdir.
    # No workspace -- runs in shared mode.
    prompt: >
      You are a cross-project code reviewer. Read the project's AGENTS.md
      for conventions before reviewing. Prioritize correctness, test
      coverage, and coding standards. Report findings with file:line
      references. Do not implement -- review only.
```

**Notes:** The shared reviewer has no `workdir` set, so it falls back to `default_workdir`. When it receives a handoff from `project-a-dev`, the handoff context includes the originating thread's details and diff. If the reviewer needs to read files in the originating repo, the dispatch body should include enough context (file paths, diffs) for the reviewer to work without switching directories. For reviewers that need direct file access in the originating repo, set `workspace: shared` and point `default_workdir` to a common workspace root that contains both projects.

---

## Higher Concurrency

Run many agents in parallel for batch operations like refactoring across modules or running parallel reviews.

```yaml
orchestration:
  max_concurrent_triggers: 10     # Up from default (= number of worker agents)
  max_triggers_per_agent: 3       # Allow 3 parallel threads per agent
  trigger_intents:
    - dispatch
    - handoff
  execution_timeout_secs: 600

agents:
  - alias: refactor-dev
    backend: claude
    model: claude-sonnet-4-6
    workspace: worktree
    prompt: >
      You refactor code modules. Each dispatch targets a specific module.
      Make minimal, focused changes. Run tests for the affected module.

  - alias: review-dev
    backend: claude
    model: claude-sonnet-4-6
    prompt: >
      You review code changes for correctness and style.

  - alias: test-dev
    backend: codex
    model: gpt-5.3-codex
    workspace: worktree
    prompt: >
      You write and update tests. Each dispatch targets a specific module.
```

**Notes:** Each trigger spawns a CLI subprocess. Tune based on machine resources (CPU, memory, API rate limits). The default `max_concurrent_triggers` equals the number of worker agents, and `max_triggers_per_agent` defaults to 1 -- meaning each agent processes one thread at a time. Raising `max_triggers_per_agent` to 3 lets the same agent handle 3 concurrent threads, but each thread needs its own worktree (set `workspace: worktree`) to avoid file conflicts. With 3 agents at `max_triggers_per_agent: 3`, the global cap of 10 means at most 9 concurrent triggers will actually run.

---

## Execution Timeout Tuning

Long-running tasks (architecture analysis, large refactors, opus-powered deep work) need more than the default 600s timeout. Set per-agent overrides for agents that routinely run longer.

```yaml
orchestration:
  execution_timeout_secs: 600       # Global default: 10 minutes

agents:
  - alias: deep-dev
    backend: claude
    model: claude-opus-4-6
    workspace: worktree
    timeout_secs: 1800               # 30 minutes for complex implementation
    prompt: >
      You handle complex implementation tasks: large refactors, new
      subsystems, cross-cutting changes. Take time to understand the
      codebase before making changes.

  - alias: architect
    backend: claude
    model: claude-opus-4-6
    timeout_secs: 1200               # 20 minutes for deep analysis
    prompt: >
      You produce architecture analysis documents. Read broadly across
      the codebase before forming recommendations. Advisory only.

  - alias: quick-reviewer
    backend: claude
    model: claude-sonnet-4-6
    timeout_secs: 300                # 5 minutes -- fail fast on stuck reviews
    prompt: >
      You do focused code reviews. Keep reviews concise and actionable.
      If the diff is too large to review in one pass, say so.

  - alias: fast-fixer
    backend: claude
    model: claude-sonnet-4-6
    # No timeout_secs -- inherits global 600s default
    prompt: >
      You handle small, well-scoped fixes: typos, config changes,
      single-function bug fixes. Fast turnaround.
```

**Notes:** Per-agent `timeout_secs` overrides the global `execution_timeout_secs`. Opus tasks on large codebases routinely need 1200-1800s. Fast review agents can use shorter timeouts (300s) to fail fast on stuck executions -- a review that takes longer than 5 minutes is likely stuck or working on too large a scope. When a timeout fires, the execution is marked failed and the thread can be retried or reassigned.

---

## Model Catalog

Document available models in config for team visibility. The `models` section is informational only -- it does not affect behavior, but serves as a reference for which models are available per backend.

```yaml
models:
  - id: claude-opus-4-6
    backend: claude
    description: "Deepest reasoning. Best for complex implementation and architecture analysis."

  - id: claude-sonnet-4-6
    backend: claude
    description: "Fast and capable. Good for reviews, routine implementation, and iterative fixes."

  - id: gpt-5.3-codex
    backend: codex
    description: "Strong correctness focus. Good for review, subtle bug detection, and test writing."

  - id: gemini-3-flash-preview
    backend: gemini
    description: "Fast reasoning with large context. Good for broad codebase analysis."

  - id: openai/gpt-5.3-codex
    backend: opencode
    description: "OpenCode-routed GPT-5.3. Use when codex backend is unavailable."

agents:
  - alias: dev
    backend: claude
    model: claude-opus-4-6           # References catalog entry
    prompt: "..."

  - alias: reviewer
    backend: codex
    model: gpt-5.3-codex             # References catalog entry
    prompt: "..."
```

**Notes:** Agents reference models by `id` in their `model` field. The catalog helps operators choose the right model when dispatching or reconfiguring agents. Keep descriptions short -- they should answer "when would I use this model?" The `backend` field in the catalog is informational; what matters is that the agent's `backend` field matches a working backend (built-in or custom-defined in `backend_definitions`). Models can also be listed as bare strings (`- claude-sonnet-4-6`) when no description or backend annotation is needed.
