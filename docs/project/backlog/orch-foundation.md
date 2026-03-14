# Aster-Orch Foundation — Infrastructure Prerequisites for Evolution

Status: Active
Owner: otto
Created: 2026-03-14

## Scope Summary

- Backend session continuity across dispatches (Claude, Codex, OpenCode)
- Project-based configuration redesign (per-project repo_root, agents scoped to projects)

## Ticket ORCH-FOUND-1 — Backend Session Continuity

- Goal: Store backend CLI session IDs in SQLite and resume sessions on follow-up dispatches to the same thread+agent, giving agents full conversational context across multi-turn orchestration.
- In scope:
  - Add `backend_session_id TEXT` column to `executions` table
  - Executor looks up prior session ID for same thread+agent before triggering
  - Claude backend: persist session ID to DB instead of in-memory `ProcessTracker`; resume with `-r <session_id>`
  - Codex backend: implement resume with `exec resume <thread_id> "prompt"`; extract `thread_id` from JSONL `thread.started` event
  - OpenCode backend: stop deleting sessions after execution; resume with `-s <session_id>`; keep extracting `sessionID` from JSONL
  - Store returned session ID in `executions.backend_session_id` after each execution
  - Remove in-memory `ProcessTracker.real_session_ids` HashMap (replaced by DB persistence)
- Out of scope:
  - Gemini backend (resume is index-based, not ID-based — unsafe for concurrent agents)
  - Session expiry/cleanup policies
  - Session forking (`--fork-session` / `--fork`)
  - Dashboard display of session continuity status
- Dependencies: None
- Acceptance criteria:
  - First dispatch to a thread creates a new backend session; session ID is stored in `executions.backend_session_id`
  - Follow-up dispatch to the same thread+agent resumes the prior session; agent has context from previous turns
  - Worker restart does not lose session IDs (persisted in SQLite, not in-memory)
  - Concurrent agents on different threads get independent sessions (no cross-contamination)
  - Claude: uses `-r <session_id>` and `--append-system-prompt` on resume
  - Codex: uses `exec resume <thread_id> "prompt"` on resume
  - OpenCode: uses `-s <session_id>` on resume; sessions are NOT deleted after execution
  - Backends without a prior session ID gracefully start a new session (no error)
  - All existing tests continue to pass
  - New integration test: dispatch → complete → re-dispatch to same thread → verify agent remembers prior context
- Verification:
  - `make verify` passes (all tests including new session continuity tests)
  - Manual: dispatch to Claude agent, close thread, re-dispatch with changes-requested, verify agent references prior work
- Status: Todo

## Ticket ORCH-FOUND-2 — Project-Based Configuration

- Goal: Replace global `target_repo_root` with per-project configuration, enabling agents scoped to different repositories under a single orchestrator instance.
- In scope:
  - New `ProjectConfig` struct: `id`, `repo_root`, optional per-project `orchestration` overrides
  - Config schema: top-level `projects[]` with agents nested under each project
  - Agent aliases must be globally unique across all projects
  - Per-project orchestration settings (execution_timeout, max_triggers_per_agent, trigger_intents, stale_active_secs) with fallback to global defaults
  - Remove `target_repo_root` from top-level config (replaced by `projects[].repo_root`)
  - Add `project_id TEXT` column to `threads` and `executions` tables
  - Worker resolves agent → project → repo_root when spawning executions
  - `orch_dispatch` resolves project automatically from agent alias (no project parameter needed)
  - `orch_list_agents` shows project context per agent
  - `orch_status` and `orch_batch_status` include project_id in response
  - Dashboard shows project context in ops/history views
  - Migrate production config (aster) and dev config (aster-orch) to new schema
- Out of scope:
  - Backward compatibility with old config format (clean break, manual migration)
  - Per-project notification/webhook settings
  - Shared agents across multiple projects
  - `orch_dispatch` project parameter (resolve from agent)
- Dependencies: None
- Acceptance criteria:
  - Config with multiple projects parses and validates correctly
  - Agents in different projects work in different repo_root directories
  - Per-project orchestration overrides merge correctly with global defaults
  - `project_id` is stored on threads and executions
  - Dashboard shows project context
  - Both production and dev configs migrated and working
  - All existing tests updated and passing
- Verification:
  - `make verify` passes
  - Manual: dispatch to agents in different projects, verify each works in its project's repo_root
  - Manual: verify dashboard shows project context
- Status: Todo

## Execution Order

1. ORCH-FOUND-1 (Session Continuity — independent, high immediate value)
2. ORCH-FOUND-2 (Project-Based Config — larger change, enables multi-project)

## Tracking Notes

- Session continuity pre-validated: Claude (`-r`), Codex (`exec resume`), OpenCode (`-s`) all confirmed working with smoke tests.
- Gemini skipped: resume is index-based (`-r <index>`), not ID-based — unsafe for concurrent agents.
- Project config is a clean break from the current schema — no backward compatibility needed.
- Both tickets are foundational for ORCH-EVO and ORCH-TEAM batches.

## Execution Metrics

- Ticket: ORCH-FOUND-1
- Owner: TBD
- Complexity: M
- Risk: Medium
- Start:
- End:
- Duration:
- Notes:

- Ticket: ORCH-FOUND-2
- Owner: TBD
- Complexity: L
- Risk: Medium
- Start:
- End:
- Duration:
- Notes:

## Closure Evidence

- (To be filled on batch completion)
