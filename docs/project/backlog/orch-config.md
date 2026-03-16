# Aster-Orch Config Location Migration

Status: Active
Owner: otto
Created: 2026-03-16

## Scope Summary

- Move production config from aster repo to `~/.aster-orch/config.yaml`
- Update default config path resolution in the binary
- Update MCP server configs, docs, and ADR-010
- State directory defaults to `~/.aster-orch/state/`

## Ticket ORCH-CONFIG-1 — Update Default Config Path in Binary

- Goal: Change the default config path from CWD-relative `.aster-orch/config.yaml` to home-relative `~/.aster-orch/config.yaml`. Ensure tilde expansion works in the config resolution path.
- In scope:
  - Update `DEFAULT_CONFIG_PATH` constant in `src/bin/aster_orch.rs` to `~/.aster-orch/config.yaml`
  - Ensure `effective_config_path()` performs tilde expansion before returning
  - Update `state_dir` default documentation to recommend `~/.aster-orch/state/`
  - All subcommands (`worker`, `mcp-server`, `dashboard`, `wait`) use the new default
  - Update existing tests for the new default path
  - Add test: `effective_config_path` returns home-expanded path when no `--config` flag
- Out of scope:
  - Env var support (`ASTER_ORCH_CONFIG`) — rejected by architect
  - Cascading/merging config files
  - Changing the dev config (`.aster-orch/config.yaml` in repo stays)
- Dependencies: None
- Acceptance criteria:
  - `aster_orch mcp-server` with no `--config` flag looks for `~/.aster-orch/config.yaml`
  - `--config <path>` still overrides the default
  - Tilde in the default path is expanded to the actual home directory
  - `make verify` passes
- Verification:
  - `make verify` passes
  - Unit test: `effective_config_path` returns expanded home path
  - Manual: run `aster_orch mcp-server` with no flags, verify it looks for `~/.aster-orch/config.yaml`
- Status: Todo

## Ticket ORCH-CONFIG-2 — Update Docs, ADR, and MCP Server Configs

- Goal: Update all documentation, decision records, and configuration references to reflect the new config location.
- In scope:
  - Update `AGENTS.md` — config location references, MCP server setup instructions
  - Update `README.md` — quick start, config file location
  - Update `docs/project/DECISIONS.md` — amend ADR-010, add new ADR for config migration
  - Update `examples/config-generic.yaml` — update `state_dir` example to `~/.aster-orch/state/`
  - Update `.mcp.json` and `opencode.json` if they reference the production config path
  - Update skill files if they reference config paths
  - Document migration steps for existing users
- Out of scope:
  - Actually performing the migration on the running instance (operator does this manually)
  - Changing the dev config or dev MCP server setup
- Dependencies: ORCH-CONFIG-1 (needs to know the exact new path)
- Acceptance criteria:
  - No references to `.aster-orch/config.yaml` as production config location in docs
  - ADR documents the rationale for the move
  - Migration guide is clear and actionable
  - `make verify` passes (no code changes, docs only)
- Verification:
  - `make verify` passes
  - Grep for old production config path — no stale references
- Status: Todo

## Execution Order

1. ORCH-CONFIG-1 (binary default path change — the foundation)
2. ORCH-CONFIG-2 (docs update — references the new path)

## Tracking Notes

- Architect recommended Option A (`~/.aster-orch/`) over XDG (`~/.config/aster-orch/`)
- Rejected: env vars, cascading configs
- Makes TEAM-6 (multi-project) easier — config in neutral location
- Dev config unchanged — `.aster-orch/config.yaml` in repo stays for aster-orch-dev
- Migration is zero-downtime: copy config, update MCP configs, restart

## Execution Metrics

- Ticket: ORCH-CONFIG-1
- Owner: TBD
- Complexity: S
- Risk: Low
- Start:
- End:
- Duration:
- Notes:

- Ticket: ORCH-CONFIG-2
- Owner: TBD
- Complexity: S
- Risk: Low
- Start:
- End:
- Duration:
- Notes:

## Closure Evidence

- (To be filled on batch completion)
