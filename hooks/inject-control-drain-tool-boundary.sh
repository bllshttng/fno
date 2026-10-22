#!/usr/bin/env bash
# fno hook: PreToolUse - mail drain at the tool boundary.
#
# A control body that demoted durable waits on the turn-boundary mail push,
# which fires only at a prompt boundary; a worker holding one long turn never
# reaches one, so a merge freeze could not stop it. This hook lands CONTROL
# mail at the next tool call instead, and ordinary mail too when
# `inbox.tool_boundary_ordinary` is on. The shell layer is a cheap gate only:
# three stat calls against the sender-stamped control flags, plus one builtin
# `-nt` test of the bus log against this session's stamp, which the verb sets
# after each pass. Then the fno-agents verb owns config, scan, cursors, dedup,
# and rendering. A config-redirected bus degrades to prompt-boundary
# delivery, never a loss.

set -uo pipefail

# Survive a caller env with no usable PATH (see worktree-write-protect.sh).
PATH="${PATH:+$PATH:}/usr/bin:/bin:/usr/sbin:/sbin"
export PATH

IFS= read -r -d '' INPUT || true
SID="$(jq -r '.session_id // empty' 2>/dev/null <<<"$INPUT")" || SID=""
if [[ -z "$SID" ]]; then
  # No jq, or jq failed: the drain must still run, so read the leading key by
  # hand. The harness writes session_id first; anchoring at the opening brace
  # keeps a "session_id" inside tool_input from faking the identity.
  re='^[[:space:]]*\{[[:space:]]*"session_id"[[:space:]]*:[[:space:]]*"([0-9a-f-]{16,})"'
  [[ "$INPUT" =~ $re ]] && SID="${BASH_REMATCH[1]}"
fi
[[ "$SID" =~ ^[0-9a-f-]{16,}$ ]] || exit 0

BUS="${FNO_STATE_DIR:-$HOME/.fno}/bus"
FIRST8="${SID:0:8}"
LAST8="${SID: -8}"
if [[ ! -f "$BUS/control-pending/$FIRST8.flag" && ! -f "$BUS/control-pending/$SID.flag" && ! -f "$BUS/control-pending/$LAST8.flag" ]] \
  && [[ ! "$BUS/messages.jsonl" -nt "$BUS/tool-boundary/$SID.stamp" ]]; then
  exit 0
fi

command -v fno-agents >/dev/null 2>&1 || exit 0

# Stdout of the verb IS the hook payload: it streams through fd 3 untouched,
# byte-for-byte (same discipline as inject-mail-notify.sh). A miss never
# blocks the tool call it rode in on.
exec 3>&1
drain_err="$(fno-agents mail-inject --control-drain --bus-dir "$BUS" --session "$SID" 2>&1 1>&3 3>&-)"
exec 3>&-
exit 0
