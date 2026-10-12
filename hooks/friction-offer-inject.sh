#!/usr/bin/env bash
# fno hook: PostToolUse - surface a pending friction offer once
# hooks/friction-offer-inject.sh -- print this session's friction offer once.
#
# PostToolUse and UserPromptSubmit hook. When a session hits friction and the
# guarded backlog match finds open nodes, crates/fno-agents/src/friction.rs
# writes <agents home>/friction/<session id>.offer. This hook prints that
# offer as additional context at the session's next tool call or prompt, then
# deletes it, so the offer lands once.
#
# It runs on every tool call, so the common case is a glob and an exit 0:
# no offer file anywhere means no stdin read and no subprocess. A friction
# signal is rare (about 60 a day across the fleet), so the slow path is too.
# Always exits 0; never blocks the call it rides on.

set -uo pipefail

DIR="${FNO_AGENTS_HOME:-${FNO_STATE_DIR:-$HOME/.fno}/agents}/friction"
shopt -s nullglob
offers=("$DIR"/*.offer)
shopt -u nullglob
((${#offers[@]})) || exit 0

# The shared bound every prompt-boundary hook carries. This hook makes no
# daemon call; sourcing it on the slow path only keeps the contract uniform.
# shellcheck source=../scripts/lib/with-timeout.sh
source "${BASH_SOURCE[0]%/*}/../scripts/lib/with-timeout.sh" 2>/dev/null || true

payload="$(cat)"
[[ "$payload" =~ \"session_id\"[[:space:]]*:[[:space:]]*\"([A-Za-z0-9_.-]+)\" ]] || exit 0
sid="${BASH_REMATCH[1]}"
event="PostToolUse"
if [[ "$payload" =~ \"hook_event_name\"[[:space:]]*:[[:space:]]*\"([A-Za-z]+)\" ]]; then
    event="${BASH_REMATCH[1]}"
fi

offer="$DIR/$sid.offer"
[[ -f "$offer" ]] || exit 0
# Claim the offer by renaming it first: two hooks of one session racing on
# one offer print it once between them.
claimed="$offer.$$"
mv "$offer" "$claimed" 2>/dev/null || exit 0
text="$(<"$claimed")"
rm -f "$claimed"

text="${text//\\/\\\\}"
text="${text//\"/\\\"}"
text="${text//$'\t'/\\t}"
text="${text//$'\r'/}"
text="${text//$'\n'/\\n}"
printf '{"hookSpecificOutput":{"hookEventName":"%s","additionalContext":"%s"}}\n' "$event" "$text"
exit 0
