#!/usr/bin/env bash
# fno hook: SessionStart - session start using fno
# Inject the using-fno SKILL.md as additionalContext, for a session that
# joined fno only.
#
# A session joins at its first fno skill or command (crates/fno-agents/src/
# join_marker.rs). An unjoined session gets no preamble once any session on
# the machine has joined; before that (a fresh install), the install itself
# is the opt-in and every session reads due. A joined one gets it once, and
# again after each compaction: `fno-agents report --kind join
# --preamble-due --mark` answers `due` and stamps it taught.
#
# Usage: session-start-using-fno.sh [EVENT] [join]
#   EVENT  the hookEventName to answer with (default SessionStart). The
#          prompt and Skill join hooks pass UserPromptSubmit / PostToolUse.
#   join   join the session first. Only a deliberate trigger passes it: the
#          call clears FNO_HOOK_CONTEXT for that one join.
# The hook payload on stdin supplies session_id and transcript_path.
#
# Output: hookSpecificOutput.additionalContext (claude, codex, and the
# opencode and pi carriers), injectSteps (agy), or `additional_context`
# (Cursor) / top-level `additionalContext` (Copilot).
#
# Failure mode: hooks must NEVER block. Any failure emits `{}`.

set -uo pipefail

# Survive a caller env with no usable PATH (see worktree-write-protect.sh).
PATH="${PATH:+$PATH:}/usr/bin:/bin:/usr/sbin:/sbin"
export PATH

EVENT="${1:-SessionStart}"
JOIN="${2:-}"
HOOK_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

PLUGIN_ROOT="${CLAUDE_PLUGIN_ROOT:-${CODEX_PLUGIN_ROOT:-}}"
if [[ -z "${PLUGIN_ROOT}" ]]; then
    PLUGIN_ROOT="$(cd "${HOOK_DIR}/.." && pwd)"
fi

# Prime the machine plugin-root pointer before any early exit below: this hook
# noops when the join preamble is not due, and a prime behind that gate would
# run once per machine instead of every session start. The claude, opencode,
# pi, agy, cursor and copilot carriers all start through here and none of them
# runs session-start.sh, so without this the env-less init resolves whatever
# copy the last codex or gemini session primed (measured 2026-10-30: a stale
# copy refused a worker's own spawn handover). Library mode sources the
# function out of session-start.sh and runs none of that hook's body.
FNO_SESSION_START_LIB=1 source "$HOOK_DIR/session-start.sh" 2>/dev/null || true
unset FNO_SESSION_START_LIB
prime_plugin_root_pointer || true

noop() { echo '{}'; exit 0; }

SKILL_PATH="${PLUGIN_ROOT}/skills/using-fno/SKILL.md"
if [[ ! -r "${SKILL_PATH}" ]]; then
    echo "session-start-using-fno: SKILL.md missing or unreadable at ${SKILL_PATH}" >&2
    noop
fi

PAYLOAD=""
[[ ! -t 0 ]] && PAYLOAD="$(cat 2>/dev/null || true)"
payload_field() {
    [[ -n "$PAYLOAD" ]] && command -v jq >/dev/null 2>&1 || return 0
    printf '%s' "$PAYLOAD" | jq -r --arg k "$1" '.[$k] // empty' 2>/dev/null || true
}

# The same harness and session-id env register-session-start.sh reads. The
# opencode, pi and agy carriers name their harness in FNO_HOOK_HARNESS and
# pass the session id in the payload (agy: camelCase conversationId).
if [[ -n "${FNO_HOOK_HARNESS:-}" ]]; then
    HARNESS="$FNO_HOOK_HARNESS"; SESSION_ID=""
elif [[ "${FNO_PLATFORM:-}" == "codex" || -n "${CODEX_PLUGIN_ROOT:-}" ]]; then
    HARNESS="codex"; SESSION_ID=""
    if [[ -z "${CODEX_THREAD_ID:-}" || -z "${CODEX_SESSION_ID:-}" \
          || "${CODEX_THREAD_ID}" == "${CODEX_SESSION_ID}" ]]; then
        SESSION_ID="${CODEX_THREAD_ID:-${CODEX_SESSION_ID:-}}"
    fi
elif [[ -n "${CLAUDE_PLUGIN_ROOT:-}" ]]; then
    HARNESS="claude"; SESSION_ID="${CLAUDE_CODE_SESSION_ID:-}"
else
    noop
fi
[[ -z "$SESSION_ID" ]] && SESSION_ID="$(payload_field session_id)"
[[ -z "$SESSION_ID" ]] && SESSION_ID="$(payload_field conversationId)"
[[ -z "$SESSION_ID" ]] && noop

# shellcheck source=lib/agents-bin.sh
source "$HOOK_DIR/lib/agents-bin.sh"
# shellcheck source=scripts/lib/with-timeout.sh
source "$HOOK_DIR/../scripts/lib/with-timeout.sh" 2>/dev/null || noop
BIN="$(fno_agents_bin "$PLUGIN_ROOT")"
[[ -z "$BIN" ]] && noop

ARGS=(report --kind join --harness "$HARNESS" --session-id "$SESSION_ID" --preamble-due --mark)
TRANSCRIPT="$(payload_field transcript_path)"
[[ -n "$TRANSCRIPT" ]] && ARGS+=(--transcript "$TRANSCRIPT")
[[ "$(payload_field source)" == "compact" || "$EVENT" == "PostCompact" ]] && ARGS+=(--compacted)
if [[ "$JOIN" == "join" ]]; then
    DUE="$( unset FNO_HOOK_CONTEXT; with_timeout "${FNO_JOIN_BUDGET_SECS:-3}" "$BIN" "${ARGS[@]}" 2>/dev/null </dev/null || true )"
else
    DUE="$( with_timeout "${FNO_JOIN_BUDGET_SECS:-3}" "$BIN" "${ARGS[@]}" --check 2>/dev/null </dev/null || true )"
fi
[[ "$DUE" == "due" ]] || noop

SKILL_BODY="$(cat "${SKILL_PATH}" 2>/dev/null)" || {
    echo "session-start-using-fno: could not read ${SKILL_PATH}" >&2
    noop
}

# JSON-escape via bash parameter substitution. Each ${s//old/new} is a
# single C-level pass - orders of magnitude faster than a character loop.
escape_for_json() {
    # macOS bash 3.2 decodes the active locale inside ${s//pat/repl} and
    # takes ~20s wall on this 11KB body (measured 2026-10-08; 0.9s with this
    # pin). Escaping \\, ", CR, LF, TAB is byte-safe in the C locale: UTF-8
    # continuation bytes never collide with those ASCII bytes, so the output
    # is identical to the multibyte pass.
    local LC_ALL=C
    export LC_ALL
    local s="$1"
    s="${s//\\/\\\\}"
    s="${s//\"/\\\"}"
    s="${s//$'\n'/\\n}"
    s="${s//$'\r'/\\r}"
    s="${s//$'\t'/\\t}"
    printf '%s' "$s"
}

SKILL_ESCAPED="$(escape_for_json "${SKILL_BODY}")"

# Wrap the skill body in framing that makes it look load-bearing at the
# top of the conversation. The EXTREMELY_IMPORTANT marker triggers the
# harness's high-attention path.
CONTEXT="<EXTREMELY_IMPORTANT>\nYou are in a footnote-enabled project.\n\n**Below is the full content of the 'using-fno' skill - your introduction to the two surfaces (slash-command workflows and the fno CLI). Re-read it when in doubt about which surface to use:**\n\n${SKILL_ESCAPED}\n</EXTREMELY_IMPORTANT>"

# We use printf instead of heredoc to dodge a bash 5.3+ heredoc bug.
if [[ "$HARNESS" == "agy" ]]; then
    # agy PreInvocation injects through injectSteps (see agy-team-inject.sh).
    printf '{"injectSteps":[{"ephemeralMessage":"%s"}]}\n' "${CONTEXT}"
elif [[ -n "${CURSOR_PLUGIN_ROOT:-}" ]]; then
    printf '{\n  "additional_context": "%s"\n}\n' "${CONTEXT}"
elif [[ -n "${COPILOT_CLI:-}" ]]; then
    printf '{\n  "additionalContext": "%s"\n}\n' "${CONTEXT}"
else
    printf '{\n  "hookSpecificOutput": {\n    "hookEventName": "%s",\n    "additionalContext": "%s"\n  }\n}\n' "${EVENT}" "${CONTEXT}"
fi

exit 0
