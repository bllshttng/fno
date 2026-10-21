#!/usr/bin/env bash
# fno hook: UserPromptSubmit and PostToolUse Skill - session join trigger
# A prompt that names an fno command (/fno:<verb> or $fno:<verb>), or a Skill
# call of an fno:* skill, joins this session to fno and lands the using-fno
# preamble on the same turn. A joined session's later prompt carries the
# preamble when it is due again (after a compaction, or after a join that the
# fno CLI made, which cannot inject context itself).
#
# Every other prompt in an unjoined session costs one stat and emits `{}`.
# Hooks never block: every failure path emits `{}`.
set -uo pipefail

PATH="${PATH:+$PATH:}/usr/bin:/bin:/usr/sbin:/sbin"
export PATH
HOOK_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

noop() { echo '{}'; exit 0; }

PAYLOAD=""
[[ ! -t 0 ]] && PAYLOAD="$(cat 2>/dev/null || true)"
[[ -z "$PAYLOAD" ]] && noop
command -v jq >/dev/null 2>&1 || noop

EVENT="$(printf '%s' "$PAYLOAD" | jq -r '.hook_event_name // "UserPromptSubmit"' 2>/dev/null)"
case "$EVENT" in
    PostToolUse)
        TRIGGER="$(printf '%s' "$PAYLOAD" | jq -r '(.tool_input.skill // "") | startswith("fno:")' 2>/dev/null)"
        ;;
    *)
        EVENT="UserPromptSubmit"
        TRIGGER="$(printf '%s' "$PAYLOAD" | jq -r '(.prompt // "") | test("(^|\\s)[/$]fno:[a-z]")' 2>/dev/null)"
        ;;
esac

if [[ "$TRIGGER" == "true" ]]; then
    printf '%s' "$PAYLOAD" | bash "$HOOK_DIR/session-start-using-fno.sh" "$EVENT" join
    exit 0
fi

# Not a trigger: only a joined session goes on to the due check. The marker
# layout is crates/fno-agents/src/join_marker.rs::marker_path.
if [[ "${FNO_PLATFORM:-}" == "codex" ]]; then
    HARNESS="codex"; SID="${CODEX_THREAD_ID:-${CODEX_SESSION_ID:-}}"
else
    HARNESS="claude"; SID="${CLAUDE_CODE_SESSION_ID:-}"
fi
[[ -z "$SID" ]] && SID="$(printf '%s' "$PAYLOAD" | jq -r '.session_id // empty' 2>/dev/null)"
[[ -z "$SID" ]] && noop
AGENTS_HOME="${FNO_AGENTS_HOME:-${HOME:-}/.fno/agents}"
[[ -e "$AGENTS_HOME/sessions/$HARNESS/$SID/joined" ]] || noop
printf '%s' "$PAYLOAD" | bash "$HOOK_DIR/session-start-using-fno.sh" "$EVENT"
exit 0
