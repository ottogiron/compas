#!/usr/bin/env bash
# notify-slack.sh — Post compas lifecycle events to a Slack incoming webhook.
#
# Usage:
#   Configure as a lifecycle hook in config.yaml:
#
#     hooks:
#       on_execution_completed:
#         - command: ./examples/hooks/notify-slack.sh
#           timeout_secs: 10
#           env:
#             SLACK_WEBHOOK_URL: https://hooks.slack.com/services/T.../B.../xxx
#
# Required environment variables:
#   SLACK_WEBHOOK_URL  — Slack incoming webhook URL
#
# The script reads JSON event data from stdin (provided by compas) and posts
# a formatted message to the configured Slack channel.
#
# Dependencies: curl, jq (falls back to python3 if jq is unavailable)

set -euo pipefail

# ── Read event JSON from stdin ────────────────────────────────────────────
EVENT_JSON="$(cat)"

# ── Validate environment ─────────────────────────────────────────────────
if [ -z "${SLACK_WEBHOOK_URL:-}" ]; then
    echo "ERROR: SLACK_WEBHOOK_URL is not set" >&2
    exit 1
fi

# ── Extract fields from JSON ─────────────────────────────────────────────
extract_field() {
    local field="$1"
    if command -v jq >/dev/null 2>&1; then
        echo "$EVENT_JSON" | jq -r ".$field // empty"
    elif command -v python3 >/dev/null 2>&1; then
        echo "$EVENT_JSON" | python3 -c "
import sys, json
data = json.load(sys.stdin)
val = data.get('$field')
if val is not None:
    print(val)
"
    else
        echo ""
    fi
}

EVENT="$(extract_field event)"
THREAD_ID="$(extract_field thread_id)"
SUCCESS="$(extract_field success)"

# ── Build Slack message ──────────────────────────────────────────────────
if [ "$SUCCESS" = "true" ]; then
    ICON=":white_check_mark:"
    STATUS="succeeded"
elif [ "$SUCCESS" = "false" ]; then
    ICON=":x:"
    STATUS="failed"
else
    ICON=":information_source:"
    STATUS="$EVENT"
fi

SLACK_TEXT="${ICON} *compas* — \`${EVENT}\` on thread \`${THREAD_ID}\` (${STATUS})"

# ── Post to Slack ────────────────────────────────────────────────────────
PAYLOAD="$(jq -n --arg text "$SLACK_TEXT" '{"text":$text}')"
curl -s -X POST "$SLACK_WEBHOOK_URL" \
    -H "Content-Type: application/json" \
    -d "$PAYLOAD" \
    >/dev/null
