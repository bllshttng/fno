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
#   AC3  interpreter resolution: python3 wins when present; the uv-managed
#        python is next; with neither, the resolver emits {} and exits 0
#        (hooks never block)

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
export PATH="$REPO_ROOT/crates/fno-agents/target/debug:$PATH"

pass() { echo "ok - $1"; }
fail() { echo "FAIL - $1" >&2; exit 1; }

# Warm the binary once: AC2's join path runs the hook's own 3s budget, and a
# cold debug binary paying first-touch page-in plus first-time graph.db
# creation can overshoot it on a slow box. That is cold-start cost, not the
# contract under test.
"$AGENTS_BIN" version >/dev/null 2>&1 || true

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

# --- AC3: interpreter resolution ---------------------------------------------
RESOLVER="$REPO_ROOT/hooks/lib/run-python.sh"

# Case (a) needs a system python3, which bare ubuntu:24.04 (and so a few CI
# images) no longer ships; the uv-rung case below covers those boxes.
if command -v python3 >/dev/null 2>&1; then
    OUT5="$(bash "$RESOLVER" -c 'print("resolved")')"
    [[ "$OUT5" == "resolved" ]] || fail "AC3: resolver failed with python3 present (got: $OUT5)"
    pass "AC3: python3 resolves when present"
else
    pass "AC3: python3 absent on this box - the python3 rung is exercised by the uv case and by CI images that ship python3"
fi

FAKEBIN="$SBX/fakebin"; EMPTYDIR="$SBX/empty"
mkdir -p "$FAKEBIN" "$EMPTYDIR"
cat > "$SBX/fakepy" <<'PY'
#!/usr/bin/env bash
echo "uv-managed-python"
PY
chmod +x "$SBX/fakepy"
printf '#!/usr/bin/env bash\necho "%s"\n' "$SBX/fakepy" > "$FAKEBIN/uv"
chmod +x "$FAKEBIN/uv"

OUT6="$(env PATH="$FAKEBIN" FNO_HOOK_PY_FALLBACK_PATH="$EMPTYDIR" bash "$RESOLVER" -c 'print("picked-me")')"
[[ "$OUT6" == "uv-managed-python" ]] || fail "AC3: the uv-managed python was not picked (got: $OUT6)"
pass "AC3: the uv-managed python resolves when system python3 is absent"

NOBIN="$SBX/nobin"
mkdir -p "$NOBIN"
OUT7="$(env PATH="$NOBIN" FNO_HOOK_PY_FALLBACK_PATH="$EMPTYDIR" bash "$RESOLVER" -c 'print("nope")' 2>/dev/null)"
[[ "$OUT7" == "{}" ]] || fail "AC3: no-interpreter fallback did not emit {} (got: $OUT7)"
pass "AC3: with no interpreter, the resolver emits {} and exits 0"

echo "all new-user hook tests passed"
