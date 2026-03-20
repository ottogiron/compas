# Compas Multi-Project Support

Status: Active
Owner: otto
Created: 2026-03-20

## Scope Summary

- Add optional `projects` config section using an overlays design: agents defined once globally, projects provide repo context and per-agent handoff/workspace overrides
- Add `project` and `repo` optional params to `orch_dispatch` for project-scoped dispatches
- Resolve effective workdir and handoff chains from project context at execution time
- Show project context in TUI dashboard with project filtering

## Context

Architecture evaluation completed by `as-architect` agent (thread `01KM6PWMC2DNQ4TT94CDZ2D2QR`, 2026-03-20). Extracted from ORCH-TEAM-6 in `orch-team.md` (team-scale batch deferred).

**Core principle:** Agents are defined once in the flat `agents` list (core identity: backend, model, prompt). Projects are an optional grouping layer providing `repo_root` context and per-agent overrides for handoff and workspace only. Cross-cutting agents (pir-reviewer, architect) have no project affiliation and work globally.

### Design: Projects as Overlays

**Config example:**

```yaml
target_repo_root: ~/workspace/github  # global fallback, still required
state_dir: ~/.compas/state

agents:
  - alias: implementer
    backend: claude
    model: eu.anthropic.claude-opus-4-6-v1
    workspace: worktree
    timeout_secs: 900
    handoff:
      on_response: [reviewer-opus, reviewer-codex]  # default chain
    prompt: |
      You implement changes in the target repository. ...

  - alias: analyst
    backend: codex
    prompt: ...

  - alias: reviewer-opus
    backend: claude
    prompt: ...

  - alias: reviewer-codex
    backend: codex
    prompt: ...

  - alias: architect
    backend: claude
    prompt: ...

  - alias: pir-reviewer        # cross-cutting, no project
    backend: claude
    prompt: ...

  - alias: orch-reviewer       # has its own workdir, no project needed
    backend: claude
    workdir: ~/workspace/github/ottogiron/compas
    prompt: ...

projects:
  - id: acme-platform
    repo_root: ~/workspace/github/acme-platform
    repos:                       # explicit list for validation + worktree
      - api-gateway
      - user-service
      - billing-service
      - notification-service
      - search-service
      - data-pipeline
    # No agent_overrides — default handoff applies

  - id: compas
    repo_root: ~/workspace/github/ottogiron/compas
    agent_overrides:
      implementer:
        handoff:
          on_response: orch-reviewer
          max_chain_depth: 3

  - id: pit
    repo_root: ~/workspace/github/ottogiron/pit
    agent_overrides:
      implementer:
        handoff:
          on_response: pit-reviewer
```

**Dispatch examples:**

```text
# Multi-repo project: agent + project + repo
orch_dispatch(to="implementer", project="acme-platform",
              repo="api-gateway", body="...")
# → workdir: ~/workspace/github/acme-platform/api-gateway
# → handoff: [reviewer-opus, reviewer-codex] (agent default)

# Single-repo project: agent + project
orch_dispatch(to="implementer", project="compas", body="...")
# → workdir: ~/workspace/github/ottogiron/compas
# → handoff: orch-reviewer (project override)

# Cross-cutting: agent only, no project
orch_dispatch(to="pir-reviewer", body="Review the PIR at ...")
# → workdir: ~/workspace/github (global fallback)

# Agent borrowing: any agent in an ad-hoc repo
orch_dispatch(to="implementer", body="Fix script in ~/scripts/...")
# → workdir: ~/workspace/github (global fallback)
# → handoff: [reviewer-opus, reviewer-codex] (agent default)
```

**Workdir resolution order:**

1. `project.repo_root/repo` (if project + repo provided)
2. `project.repo_root` (if project provided, no repo)
3. `agent.workdir` (if set on agent)
4. `target_repo_root` (global fallback)

**Handoff resolution:** If dispatch has project context, check `project.agent_overrides[alias].handoff`. If present, use it (full replacement, no merge). Otherwise, use agent's own handoff. No project context → agent's own handoff.

**Thread inheritance:** Once a thread has project/repo set from the first dispatch, follow-up dispatches to the same thread inherit project/repo if not explicitly provided.

---

## Ticket MPR-1 — Config Schema

- Goal: Add `ProjectConfig` and `AgentProjectOverride` types, `projects` field on `OrchestratorConfig`, validation, and path resolution.
- In scope:
  - New types in `src/config/types.rs`:
    - `ProjectConfig { id, repo_root, repos: Option<Vec<String>>, agent_overrides: HashMap<String, AgentProjectOverride> }`
    - `AgentProjectOverride { handoff: Option<HandoffConfig>, workspace: Option<String>, env: Option<HashMap<String, String>> }`
  - `OrchestratorConfig.projects: Option<Vec<ProjectConfig>>` with `#[serde(default)]`
  - Config validation: unique project IDs, repo_root exists, agent_override keys reference valid aliases, handoff targets valid
  - Path resolution for `project.repo_root` in `config/mod.rs`
- Out of scope: Runtime behavior changes, dispatch params, DB schema
- Dependencies: None
- Acceptance criteria:
  - Config with `projects` section parses and validates correctly
  - Config without `projects` section continues to work (backward compatible)
  - Invalid project config (bad alias ref, duplicate ID) produces clear errors
  - `make verify` passes
- Verification:
  - Unit tests for config parsing with and without projects
  - Unit tests for validation errors
  - `make verify`
- Status: Todo

## Ticket MPR-2 — Dispatch + Thread Resolution

- Goal: Add `project` and `repo` optional params to `DispatchParams`, store project context on threads, resolve effective workdir from project/repo context.
- In scope:
  - Add `project: Option<String>` and `repo: Option<String>` to `DispatchParams`
  - Add `project_id TEXT` and `repo TEXT` columns to `threads` table (nullable, migration)
  - Validate project/repo at dispatch time (project exists, repo in project's repo list)
  - Store project/repo on thread creation
  - Thread inheritance: follow-up dispatches inherit project/repo from existing thread if not specified
  - `resolve_execution_workdir()` function in executor: project.repo_root/repo > project.repo_root > agent.workdir > target_repo_root
  - Update `orch_dispatch` MCP tool description to list available projects
  - Update `orch_status` / `orch_poll` to include project context in output
  - Index on `threads.project_id` for dashboard queries
- Out of scope: Handoff override resolution (MPR-3), dashboard grouping UI (MPR-4)
- Dependencies: MPR-1
- Acceptance criteria:
  - `orch_dispatch(to="implementer", project="acme-platform", repo="api-gateway")` creates thread with project/repo context
  - Agent runs in `{project.repo_root}/{repo}` directory
  - Worktree created from the correct git repo root (not a parent directory)
  - Invalid project/repo rejected with actionable error
  - Follow-up dispatch to same thread inherits project/repo
  - Dispatch without project works as before (backward compatible)
  - `make verify` passes
- Verification:
  - Integration test: dispatch with project+repo, verify workdir resolution
  - Integration test: dispatch without project, verify fallback to target_repo_root
  - Integration test: follow-up dispatch inherits project context
  - `make verify`
- Status: Todo

## Ticket MPR-3 — Handoff Override Resolution

- Goal: Resolve handoff chains from project context, allowing per-project handoff overrides.
- In scope:
  - `resolve_handoff()` function: check project.agent_overrides[alias].handoff, fall back to agent.handoff
  - Update `maybe_auto_handoff` in `loop_runner.rs` to look up project context from thread before resolving handoff
  - Project override replaces the entire handoff block (no partial merge)
- Out of scope: Per-project prompt overrides, workspace override resolution
- Dependencies: MPR-1, MPR-2 (thread carries project context)
- Acceptance criteria:
  - Dispatch with `project="compas"` routes implementer→orch-reviewer (project override)
  - Dispatch with `project="acme-platform"` routes implementer→[reviewer-opus, reviewer-codex] (agent default)
  - Dispatch without project uses agent's own handoff
  - `make verify` passes
- Verification:
  - Integration test: dispatch to two projects, verify different handoff chains fire
  - Integration test: dispatch without project, verify agent default handoff
  - `make verify`
- Status: Todo

## Ticket MPR-4 — Dashboard Project Grouping

- Goal: Show project context in the TUI dashboard and support filtering by project.
- In scope:
  - Ops tab: show project label on thread rows (when project_id is set)
  - History tab: show project label on execution rows
  - Agents tab: show project breakdown per agent
  - Optional project filter toggle (keyboard shortcut) to show only one project's threads
  - `orch_status` and `orch_batch_status` include project context in output
- Out of scope: Web dashboard (deferred), project management UI
- Dependencies: MPR-2 (threads carry project context)
- Acceptance criteria:
  - Threads with project context show project label in Ops and History tabs
  - Project filter narrows the view to one project's threads
  - Threads without project context display normally (no label)
  - `make verify` passes
- Verification:
  - Manual: dispatch to two projects, verify dashboard shows project labels
  - Manual: toggle project filter, verify correct filtering
  - `make verify`
- Status: Todo

## Execution Order

1. MPR-1 (Config Schema — non-breaking config addition)
2. MPR-2 (Dispatch + Thread Resolution — builds on MPR-1)
3. MPR-3 (Handoff Override — builds on MPR-1 + MPR-2)
4. MPR-4 (Dashboard Project Grouping — builds on MPR-2)

Note: MPR-3 and MPR-4 can run in parallel once MPR-2 lands.

## Tracking Notes

- Backlog-first governance applies.
- Implementation commits should reference ticket IDs.
- Extracted from ORCH-TEAM-6 in `orch-team.md` (2026-03-20). Team-scale batch deferred.
- Architecture evaluation: as-architect thread `01KM6PWMC2DNQ4TT94CDZ2D2QR` (2026-03-20).
- Original ORCH-TEAM-6 design (project-scoped-agents) replaced with projects-as-overlays.
- No dependency on HTTP API — works with existing MCP dispatch.

## Execution Metrics

- Ticket: MPR-1
- Owner: TBD
- Complexity: S
- Risk: Low
- Start:
- End:
- Duration:
- Notes: Config schema only, no runtime changes

- Ticket: MPR-2
- Owner: TBD
- Complexity: M
- Risk: Medium
- Start:
- End:
- Duration:
- Notes: Dispatch params + thread project context + workdir resolution

- Ticket: MPR-3
- Owner: TBD
- Complexity: S
- Risk: Low
- Start:
- End:
- Duration:
- Notes: Handoff override resolution from project context

- Ticket: MPR-4
- Owner: TBD
- Complexity: M
- Risk: Low
- Start:
- End:
- Duration:
- Notes: Dashboard project labels + filter

## Closure Evidence

- <ticket completion summary>
- <behavior delivered>
- <docs/ADR/changelog parity summary>
- Verification:
  - `<command>`: <result>
  - `<command>`: <result>
- Deferred:
  - <deferred item and why>
