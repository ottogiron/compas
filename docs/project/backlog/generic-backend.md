# Config-Driven Generic Backend

Status: Active
Owner: operator
Created: 2026-03-21

## Scope Summary

- Allow backends to be defined entirely in config.yaml without Rust code
- New `GenericBackend` implements `Backend` trait, reading behavior from a config definition
- Template substitution for `{{instruction}}`, `{{model}}`, `{{session_id}}` in CLI args
- Handles the 80% case: CLI tool that takes a prompt, returns text

## Ticket GBE-1 — Config schema for backend definitions

- Goal: Add `backend_definitions` section to `OrchestratorConfig`
- In scope:
  - `BackendDefinition` struct in `src/config/types.rs`: `name`, `command`, `args` (with `{{instruction}}`, `{{model}}`, `{{session_id}}` template vars), `resume` (optional: flag + session_id_arg), `output` (format: plaintext/json/jsonl, result_field, session_id_field), `ping` (optional: command + args), `env_remove`
  - No `model_flag` field — template substitution in `args` covers model selection
  - `backend_definitions: Option<Vec<BackendDefinition>>` on `OrchestratorConfig`
  - Validation: name not empty, no duplicate names, no conflict with built-in names (claude, codex, gemini, opencode), command not empty, template vars must be valid
  - Hot-reload: restart-required (consistent with `default_workdir`)
  - `env_remove` composes with per-agent `env` — agent `env` adds, backend `env_remove` strips
- Out of scope:
  - GenericBackend implementation (GBE-2)
  - Registration in BackendRegistry (GBE-3)
- Dependencies: none
- Acceptance criteria:
  - Config with `backend_definitions` section parses correctly
  - Validation rejects: empty name, duplicate names, conflict with built-in names, empty command
  - Config without `backend_definitions` still works (backward compat)
  - `make verify` passes
- Verification:
  - Unit tests for deserialization and validation
  - `make verify`
- Status: Todo

## Ticket GBE-2 — GenericBackend implementation

- Goal: Implement `Backend` trait for config-defined backends
- In scope:
  - New `src/backend/generic.rs` implementing all 6 Backend trait methods:
    - `name()` → return configured name
    - `start_session()` → create UUID, load resume session ID if `resume` config present
    - `session_status()` → PID liveness via ProcessTracker (`src/backend/process.rs`)
    - `kill_session()` → SIGTERM → grace period → SIGKILL (reuse `wait_with_timeout`)
    - `trigger()` → template args, `spawn_cli`, parse output
    - `ping()` → configurable ping command or default (`command --version`)
  - Template substitution engine for `{{instruction}}`, `{{model}}`, `{{session_id}}` in args
  - Output parsing: plaintext (raw stdout = result), json (extract from configurable field), jsonl (last line, extract from field)
  - Session ID extraction from configurable JSON field (or None for stateless backends)
  - Error classification: reuse `classify_error()` from `src/backend/mod.rs`
  - Reuse `spawn_cli` from `src/backend/process.rs` for subprocess execution
- Out of scope:
  - Complex streaming/telemetry parsing (built-in backends handle that)
  - Custom error classification patterns per backend
- Dependencies: GBE-1
- Acceptance criteria:
  - All 6 Backend trait methods implemented
  - Plaintext output mode: raw stdout becomes result text
  - JSON/JSONL output mode: extracts result from configured field
  - Session resume works when `resume` config is provided
  - Stateless backends (no resume config) work correctly
  - `session_status()` returns Running for active subprocess, None for terminated
  - `kill_session()` terminates running subprocess
  - Template substitution handles missing optional vars gracefully
  - `make verify` passes
- Verification:
  - Unit tests with stub commands (`echo`, `cat`)
  - `make verify`
- Status: Todo

## Ticket GBE-3 — Registry integration and documentation

- Goal: Wire GenericBackend into BackendRegistry and document for users
- In scope:
  - In `build_backend_registry()` (`src/bin/compas.rs`): iterate `config.backend_definitions`, create GenericBackend instances, register them
  - `compas doctor` integration: validate generic backend commands exist on PATH
  - `compas init` awareness: generic backends don't appear in init's backend detection
  - README: document `backend_definitions` section with examples (aider, custom script)
  - CHANGELOG entry, DECISIONS.md (new ADR for generic backends)
  - Example in `examples/config-generic.yaml`
  - Verify hooks fire for generic backend executions (cross-backlog with HOOKS)
- Out of scope:
  - Hot-reload of backend definitions (require restart)
  - Generic backends overriding built-in names (rejected by validation)
- Dependencies: GBE-1, GBE-2
- Acceptance criteria:
  - Agent with `backend: aider` works when `backend_definitions` includes an `aider` definition
  - `compas doctor` reports missing generic backend commands
  - README has working examples
  - `make verify` passes
- Verification:
  - Integration test: config with generic backend + stub command
  - Manual: define a generic backend, dispatch work to it
  - `make verify`
- Status: Todo

## Execution Order

1. GBE-1
2. GBE-2
3. GBE-3

## Tracking Notes

- Backlog-first governance applies.
- Implementation commits should reference ticket IDs.
- Architect consultation: thread `01KM8CW0R6CKD5J4T7JWXSSSG1` (design), thread `01KM8DQ5X0BW0AB3F1Q79D6QMQ` (backlog review).
- Built-in backends (claude, codex, gemini, opencode) remain as-is — they have nuanced parsing that a generic schema can't express cleanly.
- GBE and HOOKS backlogs are fully independent — can be developed in parallel.
