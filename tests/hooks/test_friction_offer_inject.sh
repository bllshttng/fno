#!/usr/bin/env bash
# Test for hooks/friction-offer-inject.sh.
#
# Verifies:
#   Skip    : no .offer file -> exit 0, no output, stdin never read; mean wall
#             time over 200 runs printed for the PR body.
#   Once    : <sid>.offer -> printed once as additionalContext for the event the
#             payload names, then deleted; a second run prints nothing.
#   Scope   : another session's offer is never printed and never deleted.
#   Wiring  : hooks.json and codex-hooks.json register it under PostToolUse
#             and UserPromptSubmit.
#
# Exit codes: 0 pass, 1 assertion failed, 77 skipped (missing deps).

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
HOOK="${REPO_ROOT}/hooks/friction-offer-inject.sh"

fail() { printf '[friction-offer-hook] FAIL: %s\n' "$*" >&2; exit 1; }
pass() { printf '[friction-offer-hook] PASS: %s\n' "$*"; }
skip() { printf '[friction-offer-hook] SKIP: %s\n' "$*" >&2; exit 77; }

command -v python3 &>/dev/null || skip "python3 not on PATH"
[[ -x "$HOOK" ]] || fail "hook not executable at $HOOK"
bash -n "$HOOK" || fail "bash -n rejected $HOOK"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
export FNO_AGENTS_HOME="$WORK/agents"
DIR="$FNO_AGENTS_HOME/friction"
mkdir -p "$DIR"
SID="cccccccc-3333-4333-8333-333333333333"
OTHER="bbbbbbbb-2222-4222-8222-222222222222"

# Skip: 200 runs with no offer file, timed for the PR body.
mean_ms="$(python3 - "$HOOK" <<'PY'
import subprocess, sys, time
hook = sys.argv[1]
start = time.monotonic()
for _ in range(200):
    p = subprocess.run(["bash", hook], stdin=subprocess.DEVNULL, capture_output=True, timeout=5)
    if p.returncode != 0 or p.stdout:
        print("BAD"); sys.exit(0)
print(f"{(time.monotonic() - start) * 1000 / 200:.2f}")
PY
)"
[[ "$mean_ms" != "BAD" ]] || fail "no-offer run printed or exited non-zero"
pass "no offer: 200 runs, exit 0, silent, mean ${mean_ms} ms"

# A stdin that stays open for 3s proves the no-offer path never reads it.
out="$( (sleep 3; echo '{}') | python3 -c '
import subprocess, sys, time
t = time.monotonic()
subprocess.run(["bash", sys.argv[1]], stdin=sys.stdin, timeout=5)
print(f"{time.monotonic() - t:.2f}")' "$HOOK")"
python3 -c "import sys; sys.exit(0 if float('$out') < 2 else 1)" \
    || fail "no-offer run waited on stdin (${out}s)"
pass "no offer: stdin never read"

printf 'line one "quoted"\n  - x-0002: a title\n' > "$DIR/$SID.offer"
printf 'not yours\n' > "$DIR/$OTHER.offer"
# A nested session_id that comes first must not claim that session's offer.
nested="{\"tool_input\": {\"session_id\": \"$OTHER\"}, \"session_id\": \"$SID\", \"hook_event_name\": \"UserPromptSubmit\"}"
payload="$nested"
first="$(printf '%s' "$payload" | bash "$HOOK")"
ctx="$(printf '%s' "$first" | python3 -c '
import json, sys
d = json.load(sys.stdin)["hookSpecificOutput"]
print(d["hookEventName"]); print(d["additionalContext"], end="")')"
[[ "$(head -1 <<<"$ctx")" == "UserPromptSubmit" ]] || fail "event name not echoed: $first"
grep -q 'line one "quoted"' <<<"$ctx" || fail "offer text lost: $first"
grep -q 'x-0002: a title' <<<"$ctx" || fail "offer second line lost: $first"
[[ ! -e "$DIR/$SID.offer" ]] || fail "offer not deleted after surfacing"
second="$(printf '%s' "$payload" | bash "$HOOK")"
[[ -z "$second" ]] || fail "offer surfaced twice: $second"
[[ -f "$DIR/$OTHER.offer" ]] || fail "another session's offer was touched"
pass "offer printed once for its own top-level session, then deleted"

python3 - "$REPO_ROOT" <<'PY' || fail "hook not registered under PostToolUse and UserPromptSubmit"
import json, sys
root = sys.argv[1]
for name in ("hooks.json", "codex-hooks.json"):
    d = json.load(open(f"{root}/hooks/{name}"))["hooks"]
    for event in ("PostToolUse", "UserPromptSubmit"):
        cmds = [h.get("command", "") for e in d.get(event, []) for h in e.get("hooks", [])]
        assert any("friction-offer-inject.sh" in c for c in cmds), (name, event)
PY
pass "registered in hooks.json and codex-hooks.json"
