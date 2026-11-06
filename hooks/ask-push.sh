#!/usr/bin/env bash
# fno hook: PostToolUse - relay a filed question to the phone
#
# A lead session that just ran `fno inbox outstanding ask` (user rung) or a
# climb that landed on the user's board printed a `claude-push:` payload
# line. This hook turns that line into one additionalContext instruction to
# send exactly one PushNotification from THIS session, so the tap opens the
# session and the user answers in chat. Workers never see it: their asks
# wake the lead rung instead. One push per question: the claim dir
# ${FNO_AGENTS_HOME:-$HOME/.fno/agents}/ask-push/<qid> is created atomically
# and a second fire for the same qid exits silently. ntfy stays the second
# path. Always exits 0; never blocks the call it rides on.

set -uo pipefail

command -v jq >/dev/null 2>&1 || exit 0

payload="$(cat 2>/dev/null || true)"
[ -n "$payload" ] || exit 0

cmd="$(jq -r '.tool_input.command // empty' <<<"$payload" 2>/dev/null)"
case "$cmd" in
    *"outstanding ask"*|*"outstanding climb"*) ;;
    *) exit 0 ;;
esac

stdout="$(jq -r '(.tool_response.stdout? // empty)' <<<"$payload" 2>/dev/null)"
line="$(printf '%s\n' "$stdout" | grep '^claude-push: ' | tail -1)"
[ -n "$line" ] || exit 0

rest="${line#claude-push: }"
qid="${rest%%$'\t'*}"
body="${rest#*$'\t'}"
# No tab, or an unsafe qid: the payload line is malformed, never push.
[[ "$qid" != "$rest" && "$qid" =~ ^[A-Za-z0-9_.-]+$ ]] || exit 0
[ -n "$body" ] || exit 0

# One push per question, across sessions: mkdir is the atomic claim.
dir="${FNO_AGENTS_HOME:-$HOME/.fno/agents}/ask-push"
mkdir -p "$dir" 2>/dev/null || exit 0
mkdir "$dir/$qid" 2>/dev/null || exit 0
# Claims older than a week are stale questions; prune best effort.
find "$dir" -mindepth 1 -maxdepth 1 -type d -mtime +7 -exec rm -rf {} + 2>/dev/null || true

event="$(jq -r '.hook_event_name // empty' <<<"$payload" 2>/dev/null)"
[[ "$event" =~ ^[A-Za-z]+$ ]] || event="PostToolUse"

text="Send exactly one PushNotification now with this message: $body. Tapping it opens this session; the user answers here in chat; record the answer with fno inbox outstanding clear $qid --answer \"<the user's words>\" --authority user. One push per question; never push routine progress. If the PushNotification tool is unavailable, say so once and move on."
text="${text//\\/\\\\}"
text="${text//\"/\\\"}"
text="${text//$'\t'/\\t}"
text="${text//$'\r'/}"
text="${text//$'\n'/\\n}"
printf '{"hookSpecificOutput":{"hookEventName":"%s","additionalContext":"%s"}}\n' "$event" "$text"
exit 0
