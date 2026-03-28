# Config Field Rename: target_repo_root → default_workdir

Status: Done
Owner: otto
Created: 2026-03-21

## Scope Summary

- Rename `target_repo_root` → `default_workdir` in OrchestratorConfig
- Add serde alias for backward compatibility
- Update all references across code, tests, and docs

## Ticket ORCH-RENAME-1 — Rename target_repo_root to default_workdir

- Goal: Rename the config field to better describe its purpose as the default working directory fallback
- In scope:
  - Rename struct field + add `#[serde(alias = "target_repo_root")]`
  - Update all field accesses, error messages, test names, YAML fixtures
  - Update documentation (README, examples, ADRs, migration guide)
  - Add backward compatibility test
  - Add CHANGELOG entry
- Out of scope:
  - Removing the serde alias (kept for backward compat)
- Dependencies: none
- Acceptance criteria:
  - `make verify` passes
  - Old config key `target_repo_root` still works via serde alias
  - New key `default_workdir` used in all examples, docs, and tests
- Verification:
  - `make verify`
- Status: Done

## Execution Metrics

- Ticket: ORCH-RENAME-1
- Owner: (pending)
- Complexity: (pending)
- Risk: (pending)
- Start: 2026-03-21 13:17 UTC
- End: (pending)
- Duration: (pending)
- Notes: (pending)
