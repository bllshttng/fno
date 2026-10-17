#!/usr/bin/env bash
# fno hook: UserPromptSubmit - first-turn node bind
# A claude session whose FIRST user turn names exactly one open backlog node
# should show on the roster as started on that node, with no loop running.
# This shim captures the payload once, checks a session-keyed exactly-once
# marker, then pipes the payload to the fno-agents `node-bind` door under the
# shared bound and relays its one line. The bind holds no claim; the explicit
# door is `fno do target start <id> --steps`. Every failure exits 0 and prints
# nothing (AC17): a hook that fails closed on the first prompt would block the
# session (law d-bad5a42f: nothing optional on the foreground path).
# SILENT unless the door binds or lists: UserPromptSubmit exit-0 stdout is
# added to model context, so per-prompt chatter is context cost.
#
# Codex/opencode sessions are not covered: the hook entry is claude config
# only (docs/architecture/multi-cli-hooks.md).

set -uo pipefail

# Survive a caller env with no usable PATH (see worktree-write-protect.sh).
PATH="${PATH:+$PATH:}/usr/bin:/bin:/usr/sbin:/sbin"
export PATH

HOOK_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# The per-turn guard (tests/hooks/test_with_timeout.sh) requires every
# UserPromptSubmit hook that names a binary to carry the shared bound.
source "$HOOK_DIR/../scripts/lib/with-timeout.sh" 2>/dev/null || exit 0
# The fno-agents resolver, most-local first: a session PATH predates an
# in-session install, so bare `command -v` misreads an installed CLI as missing.
source "$HOOK_DIR/lib/agents-bin.sh" 2>/dev/null || true

# The payload arrives once; capture it before anything consumes stdin.
payload="$(cat 2>/dev/null)" || exit 0
[[ -n "$payload" ]] || exit 0

# The session key, read from the payload's real key (a planted "session_id"
# inside prompt text is data, never identity). Anything malformed or short
# disables the hook for this prompt - silently, like every other miss.
SID="$(printf '%s' "$payload" | jq -r '.session_id // empty' 2>/dev/null)"
case "$SID" in
'' | *[!A-Za-z0-9_-]*)
  exit 0
  ;;
esac
[[ "${#SID}" -ge 16 ]] || exit 0

# Exactly-once marker: session-keyed under agents/ first-turn-bind/ (owner and
# lifetime in docs/state-root-inventory.md). Written BEFORE the door so one
# session pays at most one attempt, whatever the door answers.
STATE="${FNO_STATE_DIR:-$HOME/.fno}"
MARKER="$STATE/agents/first-turn-bind/$SID"
[[ -e "$MARKER" ]] && exit 0
mkdir -p "$(dirname "$MARKER")" 2>/dev/null || exit 0
: >"$MARKER" 2>/dev/null || exit 0

BIN="$(fno_agents_bin "$PWD")"
[[ -n "$BIN" ]] || exit 0

# A FIXED 3s bound, not the load-aware budget: a budget skip would burn this
# session's one attempt on a busy machine, and the marker makes it final.
out="$(printf '%s' "$payload" | with_timeout 3 "$BIN" node-bind 2>/dev/null)"
rc=$?
if [[ "$rc" -eq 0 && -n "$out" ]]; then
  printf '%s\n' "$out"
fi
exit 0
