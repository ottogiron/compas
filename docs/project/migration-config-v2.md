# Config Migration: v1 (repo-relative) → v2 (home directory)

**Date:** 2026-03-16
**Applies to:** aster-orch v0.2+

## What Changed

The production config moved from inside the aster project repo to a standard home-directory location. The binary now defaults to `~/.aster-orch/config.yaml` without needing `--config`.

| | Before (v1) | After (v2) |
|---|---|---|
| Config location | `<project-repo>/.aster-orch/config.yaml` | `~/.aster-orch/config.yaml` |
| State directory | `~/.aster/orch/` | `~/.aster-orch/state/` |
| MCP server args | `aster_orch mcp-server --config <path>` | `aster_orch mcp-server` |
| Dashboard launch | `aster_orch dashboard --config <path> --with-worker` | `aster_orch dashboard --with-worker` |

**Also changed:**
- REPLY PROTOCOL removed from all agent prompts (agents reply naturally, no JSON intent lines)
- `HandoffConfig` simplified to 2 fields: `on_response` + `max_chain_depth`
- New agent: `orch-reviewer` (code reviewer for aster-orch work)
- Auto-handoff chains: `orch-dev → orch-reviewer → operator`

## Migration Steps

### 1. Install the latest binary

```bash
cd ~/workspace/github.com/ottogiron/aster-orch
git pull
make install
```

### 2. Create the new config directory

```bash
mkdir -p ~/.aster-orch/state
```

### 3. Copy your config

```bash
cp <old-config-path> ~/.aster-orch/config.yaml
```

Where `<old-config-path>` is wherever your production config currently lives (e.g., `~/workspace/.../aster/.aster-orch/config.yaml`).

### 4. Update the config file

Edit `~/.aster-orch/config.yaml`:

**a) Fix `state_dir`:**
```yaml
# Before
state_dir: /Users/<you>/.aster/orch

# After
state_dir: /Users/<you>/.aster-orch/state
```

**b) Fix `target_repo_root`** (if it was relative):
```yaml
# Before (relative — worked when config was inside the aster repo)
target_repo_root: ..

# After (absolute — config is no longer inside a project repo)
target_repo_root: /Users/<you>/workspace/github.com/<you>/aster
```

**c) Remove REPLY PROTOCOL from all agent prompts:**

Delete the entire `REPLY PROTOCOL:` block from every agent's `prompt:` field. This typically looks like:
```
REPLY PROTOCOL: When your task is complete, end your response with
a JSON line on its own line. Use review-request when submitting work:
{"intent":"review-request","to":"operator"}
Use status-update for progress, decision-needed when blocked.
Do NOT dispatch to other threads or use orchestrator tools.
```

Agents no longer need this — all routing is handled by config. See ADR-015.

**d) Update handoff config** (if you had one):

The `HandoffConfig` is now 2 fields only. Remove any `on_review_request`, `on_escalation`, `on_changes_requested` fields:
```yaml
# Before
handoff:
  on_review_request: some-reviewer
  on_response: operator
  max_chain_depth: 5

# After
handoff:
  on_response: orch-reviewer   # or "operator" or another agent alias
  max_chain_depth: 2
```

**e) Add orch-reviewer** (optional, recommended):

Add a dedicated reviewer agent for aster-orch work:
```yaml
  - alias: orch-reviewer
    backend: claude
    model: claude-sonnet-4-6
    workdir: /Users/<you>/workspace/github.com/<you>/aster-orch
    prompt: >
      You are orch-reviewer, Independent Code Quality Reviewer for aster-orch
      (the multi-agent orchestrator). This is a Rust project.
      Review only: do not implement unless explicitly instructed.
      Read AGENTS.md for project conventions and quality gates.
      Prioritize: correctness bugs, behavior regressions, missing tests,
      stale docs, scope creep, no unrelated changes. Report findings
      ordered by severity (blocking, major, minor, nit) with file:line
      references and explicit required fixes.
      If no blocking issues, state residual risks and verification gaps.
      Always verify: does `make verify` pass (fmt-check + clippy + tests)?
```

### 5. Copy the database (preserves history)

```bash
cp ~/.aster/orch/jobs.sqlite* ~/.aster-orch/state/
```

If you want a fresh start, skip this step.

### 6. Update MCP server configs

The binary now defaults to `~/.aster-orch/config.yaml`, so you can remove the `--config` argument.

**Claude Code** (`~/.claude.json`):
```json
"aster-orch": {
  "type": "stdio",
  "command": "aster_orch",
  "args": ["mcp-server"],
  "env": {}
}
```

**Codex** (`~/.codex/config.toml`):
```toml
[mcp_servers.aster-orch]
command = "aster_orch"
args = ["mcp-server"]
```

**OpenCode** (`~/.config/opencode/opencode.json`):
```json
"aster-orch": {
  "type": "local",
  "command": ["aster_orch", "mcp-server"],
  "enabled": true
}
```

### 7. Delete the old config

```bash
rm -r <project-repo>/.aster-orch/
```

### 8. Restart everything

```bash
# Restart dashboard with worker
aster_orch dashboard --with-worker
```

MCP servers restart automatically when Claude Code / Codex / OpenCode reconnect.

### 9. Smoke test

```bash
# Verify config is found
aster_orch mcp-server --help   # should not complain about missing config

# In your MCP client:
orch_health()                  # all agents healthy
orch_list_agents()             # should include orch-reviewer
orch_metrics()                 # should show historical data (if DB was copied)
```

## Rollback

If something breaks:

1. Recreate the old config: `mkdir -p <project-repo>/.aster-orch/ && cp ~/.aster-orch/config.yaml <project-repo>/.aster-orch/config.yaml`
2. Add `--config <old-path>` back to MCP server args
3. Fix `target_repo_root` to be relative (`..`) and `state_dir` to the old path
4. Restart

## Related ADRs

- ADR-010: Per-agent workdir for multi-repo support
- ADR-013: Production config at `~/.aster-orch/`
- ADR-014: Config-driven auto-handoff chains
- ADR-015: Intent simplification — agents don't manage intents
