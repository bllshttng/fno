#!/usr/bin/env bash
# test-new-user-hooks.sh - the 0.4.5 new-user hook fixes, at the hook layer.
#
# Drives the REAL hook scripts against a freshly built worktree fno-agents
# with an isolated agents home. Covers:
#   AC1  fresh install: the SessionStart due check injects the using-fno
#        preamble while NO session on the machine has joined; one joined
#        marker anywhere closes the window and the hook emits {}
#   AC2  join trigger: a prompt naming /fno: lands the preamble on the same
#        turn and writes the session's joined marker

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SBX="$(mktemp -d "${TMPDIR:-/tmp}/fno-new-user-hooks.XXXXXX")"
trap 'rm -rf "$SBX"' EXIT

AGENTS_BIN="${FNO_AGENTS_BIN:-$REPO_ROOT/crates/fno-agents/target/debug/fno-agents}"
if [[ ! -x "$AGENTS_BIN" ]]; then
    (cd "$REPO_ROOT/crates/fno-agents" && cargo build --bins --quiet) || {
        echo "FAIL: could not build fno-agents" >&2
        exit 1
    }
fi

export FNO_AGENTS_BIN="$AGENTS_BIN"
export FNO_AGENTS_HOME="$SBX/agents"
export HOME="$SBX"
export CLAUDE_PLUGIN_ROOT="$REPO_ROOT"
export FNO_HOOK_CONTEXT=1
# The join case measures the hook contract, not this host's load. The 3s
# production default holds in CI; the test seam only widens the wall a
# loaded container can eat.
export FNO_JOIN_BUDGET_SECS=10
export PATH="$REPO_ROOT/crates/fno-agents/target/debug:$PATH"

pass() { echo "ok - $1"; }
fail() { echo "FAIL - $1" >&2; exit 1; }

# Warm the binary AND the state store once: AC2's join path runs the hook's
# own budget, and first-touch page-in plus first-time graph.db creation are
# cold-start cost, not the contract under test. A --check pays both reads.
FNO_HOOK_CONTEXT=1 "$AGENTS_BIN" report --kind join --harness claude \
    --session-id warmup --preamble-due --mark --check >/dev/null 2>&1 || true

# --- AC1: the fresh-install window ------------------------------------------
T1="$SBX/t1.jsonl"
printf '%s\n' '{"type":"user","message":{"role":"user","content":"hi"}}' > "$T1"
PAYLOAD_SESSION_START='{"session_id":"fresh-1","transcript_path":"'"$T1"'","source":"startup"}'

OUT="$(printf '%s\n' "$PAYLOAD_SESSION_START" | bash "$REPO_ROOT/hooks/session-start-using-fno.sh")"
[[ "$OUT" == *"footnote-enabled"* ]] || fail "AC1: fresh install did not inject the preamble"
pass "AC1: fresh install injects the preamble"

OUT2="$(printf '%s\n' "$PAYLOAD_SESSION_START" | bash "$REPO_ROOT/hooks/session-start-using-fno.sh")"
[[ "$OUT2" == *"footnote-enabled"* ]] || fail "AC1: the window did not stay open for a second fresh session"
pass "AC1: the window stays open for later fresh sessions"

mkdir -p "$FNO_AGENTS_HOME/sessions/codex/other-session"
touch "$FNO_AGENTS_HOME/sessions/codex/other-session/joined"

OUT3="$(printf '%s\n' "$PAYLOAD_SESSION_START" | bash "$REPO_ROOT/hooks/session-start-using-fno.sh")"
[[ "$OUT3" == "{}" ]] || fail "AC1: a joined marker did not close the window (got: $OUT3)"
pass "AC1: one joined marker closes the window"

# --- AC2: the join trigger ---------------------------------------------------
PAYLOAD_JOIN='{"session_id":"fresh-2","transcript_path":"'"$T1"'","prompt":"/fno:setup please","hook_event_name":"UserPromptSubmit"}'
OUT4="$(printf '%s\n' "$PAYLOAD_JOIN" | CLAUDE_CODE_SESSION_ID=fresh-2 bash "$REPO_ROOT/hooks/session-join-prompt.sh")"
[[ "$OUT4" == *"footnote-enabled"* ]] || fail "AC2: the join prompt did not land the preamble"
[[ -e "$FNO_AGENTS_HOME/sessions/claude/fresh-2/joined" ]] || fail "AC2: the joined marker was not written"
pass "AC2: the join prompt lands the preamble and writes the marker"

echo "all new-user hook tests passed"
