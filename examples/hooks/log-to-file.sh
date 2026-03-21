#!/usr/bin/env bash
# log-to-file.sh — Append compas lifecycle events to a log file with timestamps.
#
# Usage:
#   Configure as a lifecycle hook in config.yaml:
#
#     hooks:
#       on_execution_completed:
#         - command: ./examples/hooks/log-to-file.sh
#       on_thread_closed:
#         - command: ./examples/hooks/log-to-file.sh
#
# Optional environment variables:
#   COMPAS_HOOK_LOG_FILE  — Path to the log file (default: /tmp/compas-hooks.log)
#
# The script reads JSON event data from stdin (provided by compas) and appends
# a timestamped line to the log file. Each line has the format:
#
#   <ISO-8601 timestamp>  <json>
#
# Dependencies: date (coreutils)

set -euo pipefail

# ── Read event JSON from stdin ────────────────────────────────────────────
EVENT_JSON="$(cat)"

# ── Resolve log file path ────────────────────────────────────────────────
LOG_FILE="${COMPAS_HOOK_LOG_FILE:-/tmp/compas-hooks.log}"

# ── Generate ISO-8601 timestamp ──────────────────────────────────────────
TIMESTAMP="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"

# ── Append to log ────────────────────────────────────────────────────────
echo "${TIMESTAMP}  ${EVENT_JSON}" >> "$LOG_FILE"
