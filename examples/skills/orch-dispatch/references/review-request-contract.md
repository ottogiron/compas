# Review-Request Contract

Every `review-request` from a worker agent must include these fields.

## Required Fields

| Field | Description | Example |
| --- | --- | --- |
| **TL;DR** | One-line summary of what was done | "Added authentication middleware to API routes" |
| **Files Changed** | List of file paths modified | `src/middleware/auth.ts`, `src/routes/api.ts`, `tests/auth.test.ts` |
| **Verification** | Command run + pass/fail result | `npm test` — all tests pass |
| **Next Action** | What the operator should do | "Please review and approve for merge" |

## Additional Fields (Multi-Ticket/Batch Work)

| Field | Description |
| --- | --- |
| **Ticket/Batch ID** | Reference to active ticket or batch |
| **Design Intent** | Why this approach was chosen |
| **Known Risks/Gaps** | Anything deferred or uncertain |

## Operator Contract Check

When a worker sends a `review-request`, the operator verifies:

1. All 4 required fields present? If not, reject immediately.
2. Files changed align with task scope? Flag unrelated changes.
3. Re-run the verification command — does it actually pass?
4. No secrets, credentials, or `.env` files included?

After the contract check passes, the operator dispatches to a reviewer agent for code review (see `orch-dispatch` Step 6), or skips review for trivial changes.
