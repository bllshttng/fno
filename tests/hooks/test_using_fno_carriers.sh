#!/usr/bin/env bash
# The opencode, pi and agy using-fno carriers: each runs the shared hook with
# the env and payload its plugin sends, and gets the preamble only when
# fno-agents answers `due` for a joined session.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
HOOK="$ROOT/hooks/session-start-using-fno.sh"
AGY="$ROOT/hooks/agy-using-fno-pre-invocation.sh"
W="$(mktemp -d)"
trap 'rm -rf "$W"' EXIT

cat >"$W/fa" <<'STUB'
#!/bin/sh
echo "$*" >>"$STUB_LOG"
[ "${STUB_DUE:-}" = 1 ] && echo due
exit 0
STUB
chmod +x "$W/fa"
export FNO_AGENTS_BIN="$W/fa" STUB_LOG="$W/log" FNO_AGENTS_HOME="$W/agents"
unset CLAUDE_PLUGIN_ROOT CODEX_PLUGIN_ROOT CLAUDE_CODE_SESSION_ID CODEX_THREAD_ID FNO_PLATFORM || true
COMPACT='{"hook_event_name":"SessionStart","source":"compact","session_id":"s1"}'

context_of() { python3 -c 'import json,sys; print(json.load(sys.stdin)["hookSpecificOutput"]["additionalContext"][:40])'; }
fail() { echo "FAIL: $*"; exit 1; }

# opencode: the plugin's /bin/sh runner sets CLAUDE_PLUGIN_ROOT; the
# FNO_HOOK_HARNESS prefix must still win.
out="$(printf '%s' "$COMPACT" | STUB_DUE=1 CLAUDE_PLUGIN_ROOT="$ROOT" FNO_HOOK_HARNESS=opencode bash "$HOOK")"
printf '%s' "$out" | context_of | grep -q EXTREMELY_IMPORTANT || fail "opencode got no preamble: $out"
grep -q -- '--harness opencode --session-id s1 .*--compacted --check' "$W/log" || fail "opencode args: $(cat "$W/log")"
out="$(printf '%s' "$COMPACT" | CLAUDE_PLUGIN_ROOT="$ROOT" FNO_HOOK_HARNESS=opencode bash "$HOOK")"
[[ "$out" == "{}" ]] || fail "an unjoined opencode session got a preamble"

# pi: runBounded runs `env FNO_HOOK_HARNESS=pi bash <hook>` with no plugin root.
: >"$W/log"
out="$(printf '%s' "$COMPACT" | STUB_DUE=1 env FNO_HOOK_HARNESS=pi bash "$HOOK")"
printf '%s' "$out" | context_of | grep -q EXTREMELY_IMPORTANT || fail "pi got no preamble: $out"
grep -q -- '--harness pi --session-id s1' "$W/log" || fail "pi args: $(cat "$W/log")"

# agy: an unjoined session costs no fno-agents call; a joined, untaught one
# gets injectSteps; a taught one is silent again.
: >"$W/log"
AGY_IN='{"conversationId":"a1","invocationNum":3}'
out="$(printf '%s' "$AGY_IN" | STUB_DUE=1 bash "$AGY")"
[[ "$out" == "{}" && ! -s "$W/log" ]] || fail "an unjoined agy session ran the check: $out"
mkdir -p "$FNO_AGENTS_HOME/sessions/agy/a1" && : >"$FNO_AGENTS_HOME/sessions/agy/a1/joined"
out="$(printf '%s' "$AGY_IN" | STUB_DUE=1 bash "$AGY")"
printf '%s' "$out" | python3 -c 'import json,sys; assert "EXTREMELY_IMPORTANT" in json.load(sys.stdin)["injectSteps"][0]["ephemeralMessage"]' \
    || fail "agy got no injectSteps: $out"
mkdir -p "$FNO_AGENTS_HOME/using_fno_teach" && : >"$FNO_AGENTS_HOME/using_fno_teach/a1.json"
out="$(printf '%s' "$AGY_IN" | STUB_DUE=1 bash "$AGY")"
[[ "$out" == "{}" ]] || fail "a taught agy session got the preamble again"

echo "using-fno carriers: opencode, pi and agy inject only for a joined session"
