#!/usr/bin/env bash
# fno hook: all events - one-release stub routing to session-state.sh
# hooks/inside-leg-report.sh - REMOVED NEXT RELEASE.
#
# Replaced by hooks/session-state.sh plus the `fno-agents hook session-state`
# entry (one Rust producer, per-harness adapters, the event map as data in
# harness_capabilities.toml). This stub keeps the old state-word vocabulary
# working for one release for anything still wired to this path: it reads the
# event name off the payload (the state word it was called with matches the
# event's map entry on every wired line) and execs the shim.
set -u
PATH="${PATH:+$PATH:}/usr/bin:/bin"
export PATH
STATE="${1:-working}"
case "$STATE" in
  working | blocked | done | model) ;;
  *) STATE="working" ;;
esac
INPUT="$(cat 2>/dev/null)" || INPUT=""
# jq replaces the old /usr/bin/python3 field read. A missing jq or a payload
# that fails to parse prints nothing, and EVENT falls to the default below,
# the same fail-open the python-less machines already took.
EVENT="$(printf '%s' "$INPUT" | jq -r 'if type == "object" then (.hook_event_name // "") else "" end' 2>/dev/null)"
case "$EVENT" in
  Notification|PreToolUse|UserPromptSubmit|Stop|PostModelSwitch) ;;
  *) EVENT="UserPromptSubmit" ;;
esac
HOOK_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
printf '%s' "$INPUT" | "$HOOK_DIR/session-state.sh" claude "$EVENT"
exit 0
