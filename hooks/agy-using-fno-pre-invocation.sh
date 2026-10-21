#!/usr/bin/env bash
# fno hook: PreInvocation - agy using-fno carrier
# agy runs hook commands as single program paths, so the harness is baked in
# here. PreInvocation fires before every model call and agy exposes no
# compaction event, so the preamble lands once per joined session: the marker
# and the taught stamp are two stats, and only a joined, untaught session pays
# for the fno-agents call. Fail-open: every other path emits `{}`.
set -u
PATH="${PATH:+$PATH:}/usr/bin:/bin"
export PATH
HOOK_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export FNO_HOOK_CONTEXT=1 FNO_HOOK_HARNESS=agy

PAYLOAD=""
[[ ! -t 0 ]] && PAYLOAD="$(cat 2>/dev/null || true)"
command -v jq >/dev/null 2>&1 || { echo '{}'; exit 0; }
SID="$(printf '%s' "$PAYLOAD" | jq -r '.conversationId // empty' 2>/dev/null)"
[[ -n "$SID" && "$SID" != */* ]] || { echo '{}'; exit 0; }
AGENTS_HOME="${FNO_AGENTS_HOME:-${HOME:-}/.fno/agents}"
# Layout: crates/fno-agents/src/join_marker.rs and mail_teach.rs.
[[ -e "$AGENTS_HOME/sessions/agy/$SID/joined" ]] || { echo '{}'; exit 0; }
[[ -e "$AGENTS_HOME/using_fno_teach/$SID.json" ]] && { echo '{}'; exit 0; }
printf '%s' "$PAYLOAD" | bash "$HOOK_DIR/session-start-using-fno.sh" PreInvocation
exit 0
