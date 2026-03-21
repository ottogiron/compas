# Compas CLI UX — Init, Doctor, Setup MCP

Status: Active
Owner: otto
Created: 2026-03-21

## Scope Summary

- Add `compas init` for interactive config scaffolding
- Add `compas setup mcp` for auto-registering MCP servers in coding tools
- Add `compas doctor` for pre-flight validation with actionable fix suggestions
- Improve error messages when config is missing
- Update README to reflect the new 4-step onboarding flow

## Context

Architecture evaluation completed by `compas-architect` agent (thread `01KM6ZR2CDTN5JCSN4PJ03D6J1`, 2026-03-21).

**Key decisions:**

- Config location: keep `~/.compas/` (ADR-013, matches ecosystem conventions)
- Interactive prompt crate: `dialoguer 0.11` (mature, minimal deps, 1.3M downloads/month)
- New module: `src/cli/` with `init.rs`, `doctor.rs`, `setup_mcp.rs`, `detection.rs`
- Phased delivery: error messages + init first, then setup mcp, then doctor

**Current onboarding (7 manual steps) → Target (4 commands):**

1. `cargo install --git ...`
2. `compas init`
3. `compas setup mcp`
4. `compas dashboard`

---

## Ticket CLI-1 — Error Messages + `compas init`

- Goal: Eliminate the biggest friction point — manually writing YAML config from scratch — by adding an interactive `compas init` command, and improve error messages when config is missing.
- In scope:
  - Better error in `src/config/mod.rs`: detect file-not-found specifically, print path and suggest `compas init`
  - Add `dialoguer = "0.11"` dependency to `Cargo.toml`
  - New module `src/cli/mod.rs` with `src/cli/init.rs`
  - `compas init` interactive flow:
    - Detect installed backends via `command_exists()` for `[claude, codex, gemini, opencode]`
    - Prompt for: default_workdir (default: CWD), backend (default: first detected), agent alias (default: `dev`), model (optional)
    - Generate commented YAML config and write to `~/.compas/config.yaml`
    - Overwrite protection: refuse if config exists (require `--force`)
  - `--non-interactive` mode with `--repo`, `--backend`, `--alias`, `--model` flags
  - `--minimal` flag for bare config vs full commented config
  - `--config <path>` to write to a non-default location
  - `config_template()` function in `src/config/mod.rs` or `src/cli/init.rs` for YAML generation
  - Wire up `Init` subcommand in `src/bin/compas.rs`
  - Print next steps after config creation: "Run `compas setup mcp` to register the MCP server"
- Out of scope:
  - MCP registration (CLI-2)
  - Doctor checks (CLI-3)
  - Generating multi-agent configs (one implementer is enough for init; users add more manually)
- Dependencies: None
- Acceptance criteria:
  - `compas init` in a repo directory creates a valid `~/.compas/config.yaml` with the repo's path as `default_workdir`
  - Generated config passes `load_config` + `validate_config` without errors
  - `compas init` when config exists prints error with `--force` hint
  - `compas init --force` overwrites existing config
  - `compas init --non-interactive --repo /path --backend claude` works without prompts
  - `compas dashboard` (or any command) without config prints: "Config file not found at {path}. Run `compas init` to create one."
  - `make verify` passes
- Verification:
  - Unit tests: config template generation, overwrite protection, non-interactive defaults
  - Integration test: init → load_config round-trip
  - Manual: fresh `~/.compas/` → `compas init` → `compas dashboard` works end-to-end
  - `make verify`
- Status: Todo

## Ticket CLI-2 — `compas setup mcp`

- Goal: Automate MCP server registration in coding tools, eliminating per-tool manual setup.
- In scope:
  - New module `src/cli/setup_mcp.rs` and shared `src/cli/detection.rs`
  - `compas setup mcp` auto-detects installed tools and registers compas in all of them
  - Per-tool registration:
    - Claude Code: `claude mcp add --scope user --transport stdio compas -- compas mcp-server`
    - Codex: `codex mcp add compas -- compas mcp-server`
    - OpenCode: edit `~/.config/opencode/opencode.json`, add `mcp.compas` entry
    - Gemini: edit `~/.gemini/settings.json`, add `mcpServers.compas` entry
  - `--tool <claude|codex|opencode|gemini|all>` flag to target specific tools
  - `--remove` flag for unregistration
  - `--dry-run` flag to show what would be done
  - `--config <path>` to append `--config <path>` to the registered mcp-server command
  - Idempotent: skip if already registered, print "already registered"
  - JSON file creation: if `opencode.json` or `settings.json` doesn't exist, create with compas entry only
  - Wire up `SetupMcp` subcommand in `src/bin/compas.rs`
- Out of scope:
  - Doctor checks (CLI-3 reuses detection.rs)
  - Tool authentication (if `claude mcp add` fails due to login, report the error)
- Dependencies: None (can be built independently of CLI-1)
- Acceptance criteria:
  - `compas setup mcp` detects installed tools and registers compas in all of them
  - `compas setup mcp --tool claude` registers only in Claude Code
  - Running `compas setup mcp` twice is idempotent (second run prints "already registered")
  - `compas setup mcp --remove` unregisters from all tools
  - `compas setup mcp --dry-run` prints actions without executing
  - JSON editing for OpenCode/Gemini produces valid JSON
  - `make verify` passes
- Verification:
  - Unit tests: JSON editing (add entry, idempotent add, remove entry, create new file)
  - Unit tests: CLI command construction for Claude Code and Codex
  - Manual: `compas setup mcp` → verify registration in each installed tool
  - `make verify`
- Status: Todo

## Ticket CLI-3 — `compas doctor`

- Goal: Pre-flight validation that catches setup issues before the user tries to dispatch, with actionable fix suggestions.
- In scope:
  - New module `src/cli/doctor.rs`
  - Reuse `src/cli/detection.rs` from CLI-2 for backend detection and MCP checks
  - Ordered checks:
    1. Config file exists and parses
    2. Config validates (default_workdir, agents, etc.)
    3. State directory writable
    4. Backend CLIs installed (per agent's backend)
    5. Backend CLIs authenticated (ping — reuse `backend.ping()`)
    6. Worker running (heartbeat check via SQLite)
    7. MCP server registered (check each installed tool)
  - Output: checklist with pass/fail/warn symbols per check
  - Every failure includes a fix suggestion (e.g., "Run `compas setup mcp --tool opencode`")
  - `--fix` flag: auto-fix what can be fixed (currently: MCP registration only)
  - `--config <path>` override
  - Exit code: 0 = all pass, 1 = any fail
  - Wire up `Doctor` subcommand in `src/bin/compas.rs`
- Out of scope:
  - `--json` output format (add later if CI needs it)
  - Auto-fix for missing backend CLIs (just suggest install command)
  - Auto-fix for authentication failures
- Dependencies: CLI-2 (reuses detection.rs for MCP checks; can stub if built in parallel)
- Acceptance criteria:
  - `compas doctor` with valid setup prints all-pass checklist, exits 0
  - `compas doctor` with missing backend prints fail + install suggestion, exits 1
  - `compas doctor` with unregistered MCP prints fail + `compas setup mcp` suggestion, exits 1
  - `compas doctor --fix` auto-registers missing MCP servers
  - `compas doctor` without config prints missing config + `compas init` suggestion, exits 1
  - `make verify` passes
- Verification:
  - Unit tests: check ordering, failure formatting, exit codes
  - Manual: run `compas doctor` with intentionally broken setups (missing backend, missing MCP, no worker)
  - `make verify`
- Status: Todo

## Ticket CLI-4 — Revised Onboarding Documentation

- Goal: Update README and docs to reflect the new 4-step onboarding flow.
- In scope:
  - Rewrite README "Quick Start" to lead with `compas init` and `compas setup mcp`
  - Keep manual setup instructions as a "Manual Configuration" section for advanced users
  - Reference `examples/config-generic.yaml` from README (currently hidden)
  - Add `compas init`, `compas doctor`, `compas setup mcp` to the CLI reference section
  - Update CHANGELOG with all CLI-UX entries
  - Add ADR-018 documenting CLI UX decisions (config location, crate selection, command design)
- Out of scope:
  - Changing the config schema
  - Changing existing command behavior
- Dependencies: CLI-1, CLI-2, CLI-3 (docs reflect implemented commands)
- Acceptance criteria:
  - README Quick Start shows the 4-step flow: install → init → setup mcp → dashboard
  - All three new commands are documented with flags and examples
  - `examples/config-generic.yaml` is referenced from README
  - ADR-018 exists in `docs/project/DECISIONS.md`
  - `make verify` passes (markdown lint)
- Verification:
  - Manual: follow the new Quick Start from scratch on a clean system
  - `make verify`
- Status: Todo

---

## Execution Order

1. CLI-1 (Error messages + `compas init` — highest impact, unlocks first-run)
2. CLI-2 (`compas setup mcp` — second highest impact, can overlap with CLI-1)
3. CLI-3 (`compas doctor` — depends on CLI-2's detection.rs)
4. CLI-4 (Documentation — depends on all commands being implemented)

Note: CLI-1 and CLI-2 can be built in parallel.

## Tracking Notes

- Backlog-first governance applies.
- Implementation commits should reference ticket IDs.
- Architecture evaluation: compas-architect thread `01KM6ZR2CDTN5JCSN4PJ03D6J1` (2026-03-21).
- New dependency: `dialoguer = "0.11"` (only used in `compas init`).
- New module: `src/cli/` with init.rs, doctor.rs, setup_mcp.rs, detection.rs.

## Execution Metrics

- Ticket: CLI-1
- Owner: TBD
- Complexity: M
- Risk: Low
- Start:
- End:
- Duration:
- Notes: Error messages + interactive init

- Ticket: CLI-2
- Owner: TBD
- Complexity: M
- Risk: Low
- Start:
- End:
- Duration:
- Notes: MCP server registration automation

- Ticket: CLI-3
- Owner: TBD
- Complexity: M
- Risk: Low
- Start:
- End:
- Duration:
- Notes: Pre-flight doctor with fix suggestions

- Ticket: CLI-4
- Owner: TBD
- Complexity: S
- Risk: Low
- Start:
- End:
- Duration:
- Notes: README rewrite + ADR-018

## Closure Evidence

- <ticket completion summary>
- <behavior delivered>
- <docs/ADR/changelog parity summary>
- Verification:
  - `<command>`: <result>
  - `<command>`: <result>
- Deferred:
  - <deferred item and why>
