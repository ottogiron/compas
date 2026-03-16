# AX — Agent Experience

Design principles for building tools, APIs, and protocols that serve AI agents as primary consumers.

AX is to agents what UX is to humans. Agents can't "look around" when something fails — they rely entirely on the signals a system provides. Poor AX leads to bad recovery paths: agents bypass safety mechanisms, invent incorrect explanations, or stall silently.

## Principles

### 1. Diagnostic errors over opaque failures

Error messages must include what was attempted, what was found vs expected, and a suggested fix.

**Bad:** `Ticket ORCH-ARCH-1 not found in backlog`
**Good:** `Ticket ORCH-ARCH-1 found in orch-architecture.md but not as a markdown heading. Expected '## Ticket ORCH-ARCH-1' (H2). Check the heading level.`

The bad message led an agent to assume the ticket was closed and bypass the pre-commit hook with `--no-verify`.

### 2. Resilient contracts

Prefer universal, reliable signals over format-dependent ones. Make the happy path the simplest path.

- Exit code 0 is more reliable than parsing JSON from stdout.
- A file existing is more reliable than parsing its contents for a magic string.
- An HTTP status code is more reliable than parsing an error body.

When a richer signal (JSON, structured output) is useful, treat it as optional enrichment — not a requirement. The system must function correctly without it.

### 3. No escape hatches

Never give agents a path to bypass safety mechanisms. If a tool fails, the only valid path is diagnose and fix.

- Don't suggest `--no-verify`, `--force`, or `--skip-checks` in error messages shown to agents.
- If a safety gate fails, the error should guide toward resolving the root cause.
- Escape hatches exist for humans in emergencies, not for agents as default recovery.

### 4. State-aware inference

When agent output is ambiguous or missing, infer intent from system state rather than failing.

- Thread in `dispatch` state + agent exits 0 → infer `review-request`.
- Thread in `approved` state + agent exits 0 → infer `completion`.
- Agent returns wrong intent → normalize based on what the state machine expects.

This makes the system self-correcting. The agent doesn't need to know the protocol perfectly — the system meets it where it is.

### 5. Actionable failure responses

On timeout or failure, include what DID happen — not just what didn't.

**Bad:** `{"found": false, "timeout_secs": 120}`
**Good:** `{"found": false, "timeout_secs": 120, "unmatched_messages": [{"intent": "response", "from": "focused"}], "hint": "Agent responded with 'response' instead of 'review-request'."}`

The operator can immediately see the problem and recover, instead of manually investigating the thread.

## Applying AX

These principles apply anywhere agents interact with project tooling:

- **Orchestrator:** Intent inference, synthetic replies, timeout diagnostics.
- **Ticket tracker:** Diagnostic error messages, flexible format matching.
- **MCP tools:** Structured error returns with hints.
- **Dashboard:** Clear status indicators, actionable admin actions.

When building or modifying any tool surface, ask: *if an agent hits an error here, does it have enough information to recover correctly?*
