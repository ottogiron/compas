# Known Issues — Aster Orchestrator

## MCP transport latency on large transcripts

**Severity:** Low
**Status:** Open

`orch_transcript` for threads with many long messages can be slow due to JSON serialization over stdio MCP transport. Not a problem for typical thread sizes (<50 messages).

**Workaround:** Use `orch_poll` with `since_reference` for incremental reads instead of full transcript.

## Dashboard polling overhead

**Severity:** Low
**Status:** Open (addressed by ORCH-EVO-2)

Dashboard polls SQLite at a fixed interval. No push-based updates. Can feel sluggish for real-time monitoring of fast-moving executions.

**Planned fix:** ORCH-EVO-2 (Event Broadcast Channel) will enable push-based dashboard updates.
