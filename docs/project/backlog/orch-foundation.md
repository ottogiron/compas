# Aster-Orch Foundation — Infrastructure Prerequisites for Evolution

Status: Active
Owner: otto
Created: 2026-03-14

## Scope Summary

- Backend session continuity across dispatches (Claude, Codex, OpenCode)
- Backend output contract (structured response format to protect against CLI format changes)

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
- Status: Done

## Ticket ORCH-FOUND-2 — Backend Output Contract

- Goal: Define a structured JSON response format for all backends, replacing fragile text parsing of CLI output. Protect against silent breakage when backend CLIs change their output format.
- In scope:
  - Define a canonical response envelope: `{"intent": "...", "result": "...", "session_id": "...", "error": null}`
  - Update executor output parsing to expect the envelope format
  - Backend-specific wrappers: extract the envelope from each backend's native output format
    - Claude: already returns JSON with `result` and `session_id` — map to envelope
    - Codex: JSONL stream — extract final `item.completed` text + thread_id, wrap in envelope
    - OpenCode: JSONL stream — extract final text + sessionID, wrap in envelope
    - Gemini: JSON output — map to envelope
  - Unified `BackendOutput` struct in Rust (replaces ad-hoc parsing per backend)
  - Intent parsing moves from executor to a single `parse_intent()` function on the unified output
  - Graceful fallback: if a backend returns non-conforming output, wrap it as `{"intent": "response", "result": "<raw text>", "error": null}`
- Out of scope:
  - Changing what backends actually output (we wrap, not enforce)
  - Backend-specific error code classification (separate ticket: retry with error classification)
  - Streaming output parsing (separate ticket: ORCH-EVO-1)
- Dependencies: None
- Acceptance criteria:
  - All 4 backends produce a unified `BackendOutput` struct after trigger
  - Intent is parsed from the unified output, not per-backend text extraction
  - Existing intent parsing behavior is preserved (no regression)
  - Non-conforming output gracefully falls back to raw text with `response` intent
  - All existing tests pass with the unified output format
  - New unit tests for each backend's output mapping
- Verification:
  - `make verify` passes
  - Manual: dispatch to each backend, verify intent parsing works correctly
- Status: Done

## Execution Order

1. ~~ORCH-FOUND-1 (Session Continuity — done)~~
2. ~~ORCH-FOUND-2 (Backend Output Contract — done)~~

## Tracking Notes

- Session continuity pre-validated: Claude (`-r`), Codex (`exec resume`), OpenCode (`-s`) all confirmed working with smoke tests.
- Gemini skipped for sessions: resume is index-based (`-r <index>`), not ID-based — unsafe for concurrent agents.
- FOUND-2 was originally "Project-Based Configuration" — deferred to ORCH-TEAM-6 per orch-architect review. Replaced with backend output contract (higher foundation priority).
- Project-based config design is complete (Option B: projects as first-class concept) and documented in this repo's git history. Ready to implement when TEAM-6 is started.

## Execution Metrics

- Ticket: ORCH-FOUND-1
- Owner: orch-dev
- Complexity: M
- Risk: Medium
- Start: 2026-03-14
- End: 2026-03-14
- Duration: 00:30:00
- Notes: Implemented and smoke-tested. Session resume confirmed for Claude, Codex, OpenCode.

- Ticket: ORCH-FOUND-2
- Owner: orch-dev
- Complexity: M
- Risk: Medium
- Start: 2026-03-14 22:44 UTC
- End: 2026-03-14 22:44 UTC
- Duration: ~00:20:00
- Notes: Unified BackendOutput struct, parse_intent_from_text(), ErrorCategory. Foundation for EVO-12 retry.

## Closure Evidence

- (To be filled on batch completion)
