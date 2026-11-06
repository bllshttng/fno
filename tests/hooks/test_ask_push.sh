#!/usr/bin/env bash
# hooks/ask-push.sh: the claude-push relay. A PostToolUse payload whose
# command ran `outstanding ask` (user rung) or `outstanding climb` to the
# user board, and whose stdout carries a `claude-push: <qid>\t<body>` line,
# yields exactly one additionalContext push instruction per qid: the claim
# dir under a private FNO_AGENTS_HOME makes the one-shot observable. Any
# other command, or a payload with no line, prints nothing. jq is whatever
# is on PATH.
set -uo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../.." || exit 1
HOOK="hooks/ask-push.sh"
[[ -f "$HOOK" ]] || { echo "FAIL: $HOOK not found from $(pwd)"; exit 1; }

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

pass=0
fail=0

check() {
    local name="$1" want="$2" got="$3"
    if [[ "$got" == "$want" ]]; then
        echo "  PASS: $name"
        pass=$((pass + 1))
    else
        echo "  FAIL: $name"
        echo "    wanted: $want"
        echo "    got: ${got:-<empty>}"
        fail=$((fail + 1))
    fi
}

# build_payload <command> <stdout>
build_payload() {
    jq -n --arg cmd "$1" --arg out "$2" \
        '{tool_input: {command: $cmd}, tool_response: {stdout: $out}, hook_event_name: "PostToolUse"}'
}

run_hook() {
    local home="$1" payload="$2"
    printf '%s' "$payload" | FNO_AGENTS_HOME="$home" bash "$HOOK"
}

QID="q-1a2b"
BODY="Question q-1a2b: Ship 0.3.1 - pick: ship now"

# 1. The happy path prints the push instruction once and claims the qid.
home1="$TMP/home1"
out1="$(run_hook "$home1" "$(build_payload "fno inbox outstanding ask \"which lane?\" --option ship" "q-1a2b
$(printf 'claude-push: %s\t%s' "$QID" "$BODY")")")"
check "happy path emits one instruction" 1 "$(printf '%s' "$out1" | grep -c 'PushNotification')"
check "instruction names the clear command" 1 "$(printf '%s' "$out1" | grep -c "outstanding clear $QID")"
check "claim dir created" "1" "$([[ -d "$home1/ask-push/$QID" ]] && echo 1 || echo 0)"

# 2. Dedup: a second fire for the same qid prints nothing.
out2="$(run_hook "$home1" "$(build_payload "fno inbox outstanding ask again" "q-1a2b
claude-push: $QID	$BODY")")"
check "second fire is silent" "" "$out2"

# 3. An unrelated command prints nothing and claims nothing.
home3="$TMP/home3"
out3="$(run_hook "$home3" "$(build_payload "git status" "claude-push: q-fake	body")")"
check "non-ask command is inert" "" "$out3"
check "non-ask claims nothing" "0" "$([[ -d "$home3/ask-push/q-fake" ]] && echo 1 || echo 0)"

# 4. No claude-push line in stdout: inert even on an ask command.
home4="$TMP/home4"
out4="$(run_hook "$home4" "$(build_payload "fno inbox outstanding ask --help" "usage: fno inbox outstanding ask")")"
check "ask without payload line is inert" "" "$out4"

# 5. Malformed payload (no tab): inert.
home5="$TMP/home5"
out5="$(run_hook "$home5" "$(build_payload "fno inbox outstanding climb q-9z --evidence x" "claude-push: q-9z-notab")")"
check "payload without tab is inert" "" "$out5"

# 6. The injection carries the phone body verbatim.
check "body verbatim in instruction" 1 "$(printf '%s' "$out1" | grep -Fc "$BODY")"

echo ""
echo "PASS=$pass FAIL=$fail"
[[ "$fail" -eq 0 ]]
