# Security Hardening

Status: Active
Owner: operator
Created: 2026-03-29

## Scope Summary

- Make permission-bypass flags explicit via required `safety_mode` config field
- Add secret redaction at log/DB persistence boundaries
- Warn on dangerous `backend_args` and sensitive env var overrides
- Enforce restrictive file permissions on state directory

## Ticket SEC-1 — Required `safety_mode` field for built-in backends

- Goal: Users must explicitly acknowledge that built-in backends run with permission bypass flags. Add `safety_mode: auto_approve` as a required field on agents using built-in backends.
- In scope:
  - Add `safety_mode` enum field to `AgentConfig` in `src/config/types.rs` (initial variant: `auto_approve`)
  - Config validation rejects if `safety_mode` missing on agents with built-in backends (claude, codex, gemini, opencode)
  - Skip requirement for agents using `backend_definitions` (generic backends)
  - Emit `tracing::warn!` at worker startup listing effective safety flags per agent
  - Add `## Security Model` section to `docs/guides/configuration.md`
  - Add security callout + update config examples in `README.md`
  - Update all test fixtures and example configs to include `safety_mode`
- Out of scope:
  - `supervised` mode (future variant, no backend supports interactive approval today)
  - Changing the actual bypass behavior (flags still injected when `auto_approve`)
- Dependencies: none
- Acceptance criteria:
  - Config without `safety_mode` on a built-in backend agent produces a clear validation error
  - Config with `safety_mode: auto_approve` loads successfully
  - Generic backend agents load without `safety_mode`
  - Worker startup logs show effective safety flags per agent at WARN level
  - README and configuration guide document the security model
  - All existing tests pass with updated fixtures
- Verification:
  - `make verify`
  - Manual: remove `safety_mode` from a test config → validation error
  - Manual: worker startup logs show safety flags
- Status: In Progress

## Ticket SEC-2 — Secret redaction in execution logs and output previews

- Goal: Best-effort redaction of common secret patterns before persisting agent output to log files and SQLite.
- In scope:
  - New `redact_output` module with regex-based scrubber
  - Pattern list: `sk-`, `sk_live_`, `sk_test_`, `ghp_`, `gho_`, `AKIA`, `Bearer <token>`, `-----BEGIN.*KEY-----`, generic `key=`/`secret=`/`password=` followed by long strings
  - Apply at two persistence boundaries: log file writes (`src/backend/process.rs`) and `output_preview` storage (`src/worker/executor.rs`)
  - Config: `orchestration.redact_secrets: true` (default on), optional `redaction_patterns` list
  - Redacted text replaced with `[REDACTED:<pattern-name>]`
  - Do NOT redact in-memory `BackendOutput.raw_output` (needed for response parsing)
- Out of scope:
  - Redacting message bodies in the messages table
  - Encryption at rest
- Dependencies: none
- Acceptance criteria:
  - Common secret patterns (AWS keys, GitHub tokens, Stripe keys, Bearer tokens, PEM keys) redacted in log files
  - Same patterns redacted in `output_preview` in SQLite
  - `redact_secrets: false` disables redaction
  - Custom patterns addable via `redaction_patterns` config
  - Redaction does not affect response parsing (`BackendOutput.result_text`)
  - Unit tests with sample secret strings confirm redaction
- Verification:
  - `make verify`
  - Unit tests for redaction patterns
- Status: Todo

## Ticket SEC-3 — Warn on dangerous flags in `backend_args`

- Goal: Config validation emits warnings when `backend_args` contains known dangerous flags.
- In scope:
  - Known-bypass-flags list in `src/config/validation.rs`
  - Warn on duplicate flags (already injected by the backend)
  - Warn on dangerous flags from other backends
  - Advisory only — do not reject the config
- Out of scope:
  - Blocking dangerous flags
  - Validating arbitrary CLI flags beyond the known list
- Dependencies: SEC-1 (uses same validation infrastructure)
- Acceptance criteria:
  - Duplicate bypass flags produce a warning
  - Known dangerous flags from other backends produce a warning
  - Warnings at `tracing::warn!` level during config validation
  - Config still loads successfully
  - Unit test covers each warning case
- Verification:
  - `make verify`
- Status: Todo

## Ticket SEC-4 — Restrictive file permissions on state directory

- Goal: Enforce `0700` on `state_dir`, `0600` on DB and log files (Unix only).
- In scope:
  - `state_dir` → `chmod 0700` after `create_dir_all` in `src/bin/compas.rs`
  - SQLite DB → `chmod 0600` after pool creation
  - Log files → `OpenOptions::mode(0o600)` in `src/backend/process.rs`
  - Worker lock/log → same `mode(0o600)` in `src/bin/compas.rs`
  - All gated behind `#[cfg(unix)]`
- Out of scope:
  - Retroactively modifying existing state directories
  - Windows permissions
  - Encryption at rest
- Dependencies: none
- Acceptance criteria:
  - State directory created with 0700 on Unix
  - DB, log, lock files created with 0600 on Unix
  - All permission code gated behind `#[cfg(unix)]`
  - Existing directories not retroactively modified
- Verification:
  - `make verify`
  - Manual: `ls -la` on newly created state_dir and files
- Status: Todo

## Ticket SEC-5 — Warn on security-sensitive env vars in agent config

- Goal: Config validation warns when agent `env` overrides `PATH`, `LD_PRELOAD`, or similar.
- In scope:
  - Sensitive env var deny-pattern list in `src/config/validation.rs`
  - Patterns: `PATH`, `HOME`, `SHELL`, `USER`, `LD_PRELOAD`, `LD_LIBRARY_PATH`, `DYLD_INSERT_LIBRARIES`, `DYLD_LIBRARY_PATH`
  - Advisory warning, does not block config loading
  - Document `env` field behavior and risks in configuration guide
- Out of scope:
  - Blocking sensitive env vars
  - Filtering env vars at spawn time
- Dependencies: none
- Acceptance criteria:
  - Config validation warns for each sensitive env var override
  - Warning does not block config loading
  - Configuration guide documents env field behavior
  - Unit test covers PATH and LD_PRELOAD warnings
- Verification:
  - `make verify`
- Status: Todo

## Execution Order

1. SEC-1 (highest severity, config schema change affects all other tickets)
2. SEC-2 (medium severity, independent implementation)
3. SEC-3 (depends on SEC-1 validation infrastructure)
4. SEC-4 (independent, small)
5. SEC-5 (independent, small)

## Tracking Notes

- Backlog-first governance applies.
- Implementation commits should reference ticket IDs.
- SEC-1 is a breaking config change — all example configs and test fixtures must be updated.
- SEC-3 and SEC-5 can be combined into a single dispatch if capacity is limited.

## Execution Metrics

- Ticket: SEC-1
- Owner: --
- Complexity: M
- Risk: Medium
- Start: 2026-03-29 00:35 UTC
- End: --
- Duration: --
- Notes: --

- Ticket: SEC-2
- Owner: --
- Complexity: M
- Risk: Low
- Start: --
- End: --
- Duration: --
- Notes: --

- Ticket: SEC-3
- Owner: --
- Complexity: S
- Risk: Low
- Start: --
- End: --
- Duration: --
- Notes: --

- Ticket: SEC-4
- Owner: --
- Complexity: S
- Risk: Low
- Start: --
- End: --
- Duration: --
- Notes: --

- Ticket: SEC-5
- Owner: --
- Complexity: S
- Risk: Low
- Start: --
- End: --
- Duration: --
- Notes: --
