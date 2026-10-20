#!/usr/bin/env bash
# Receipt contract for autocorrect-review.sh: a run with no key exits 2 and
# writes a start and an end receipt line naming the refusal; a run whose API
# call fails against a dead LOCAL endpoint (never the paid API) exits 1 and
# writes both lines naming the failure; a --help exit writes no receipts at
# all, because an argument error is not a run.
#
# FNO_BIN points at a stub that logs its argv, so the test also asserts the
# event payload the run emits: valid JSON carrying the outcome, never the
# empty -d "" an absent optional field once produced.

# No pipefail: the assertions grep text through a pipe, and a grep -q early
# exit can SIGPIPE the printf producer under load; pipefail would flip that
# race into a false negative. The producer's status is irrelevant here.
set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REVIEW="$SCRIPT_DIR/../autocorrect-review.sh"
PASS=0
FAIL=0

pass() {
    echo "  PASS: $1"
    PASS=$((PASS + 1))
}
fail() {
    echo "  FAIL: $1"
    FAIL=$((FAIL + 1))
}
summary() {
    echo ""
    echo "PASS=$PASS FAIL=$FAIL"
    [[ $FAIL -eq 0 ]]
}

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
FIXTURE="$TMP/packet.yaml"
printf 'review_id: receipts-test\nwindow: test\nentries: []\nimplicated_rules: []\n' > "$FIXTURE"

# A stub runner that logs its argv: stands in for the fno binary so the
# test sees the exact emit call without touching a real journal.
STUB="$TMP/fno-stub.sh"
cat > "$STUB" <<'STUB'
#!/bin/bash
printf '%s\n' "$*" >> "$FNO_STUB_LOG"
STUB
chmod +x "$STUB"

STUB_LOG="$TMP/fno-calls.log"

# ---- T01: no key -> exit 2, receipts naming the refusal, event payload ----
echo "T01: key-less run leaves a refusal receipt pair and a real event"
LOG1="$TMP/logs1"
OUT="$TMP/out1"
ERR="$TMP/err1"
: > "$STUB_LOG"
env -u ANTHROPIC_API_KEY FNO_BIN="$STUB" FNO_STUB_LOG="$STUB_LOG" AUTOCORRECT_LOG_DIR="$LOG1" \
    bash "$REVIEW" --packet-file "$FIXTURE" > "$OUT" 2> "$ERR"
RC=$?
if [[ $RC -eq 2 ]]; then
    pass "exit code is 2"
else
    fail "expected exit 2, got $RC"
fi
LOG="$LOG1/autocorrect-review.log"
if [[ -f "$LOG" ]]; then
    pass "receipt log exists"
else
    fail "receipt log missing at $LOG"
fi
if grep -q 'run start source=manual inputs=-' "$LOG"; then
    pass "start receipt written (packet not yet read: inputs=-)"
else
    fail "start receipt missing or wrong shape"
fi
if grep -q 'run end exit=2 outcome=refused reason=ANTHROPIC_API_KEY env var is required' "$LOG"; then
    pass "end receipt names the refusal"
else
    fail "end receipt missing or does not name the refusal"
fi
if [[ ! -s "$OUT" ]]; then
    pass "stdout stayed empty"
else
    fail "stdout carried bytes; the refusal must live in the receipts"
fi
if grep -q 'doctor event emit -t autocorrect_review_run -s bash -d ' "$STUB_LOG"; then
    pass "event emit ran through the stub"
else
    fail "no event emit call logged"
fi
PAYLOAD_LINE="$TMP/payload-line.txt"
grep -o '\-d {.*}' "$STUB_LOG" > "$PAYLOAD_LINE" || true
PYCHECK_RC=0
python3 - "$PAYLOAD_LINE" <<'PYCHECK' || PYCHECK_RC=1
import json
import sys

line = open(sys.argv[1]).read().strip()
prefix = "-d "
assert line.startswith(prefix + "{"), f"no -d payload logged: {line!r}"
data = json.loads(line[len(prefix):])
assert data.get("outcome") == "refused", data
assert data.get("exit_code") == 2, data
assert data.get("packet_source") == "manual", data
assert data.get("reason") == "ANTHROPIC_API_KEY env var is required", data
assert "patch_count" not in data and "output_path" not in data, data
PYCHECK
if [[ $PYCHECK_RC -eq 0 ]]; then
    pass "event payload carries the refusal fields, no empties"
else
    fail "event payload is not the refused run's JSON"
fi

# ---- T02: dead local endpoint -> exit 1, receipts name the failure ----
echo "T02: API failure run leaves a failure receipt pair"
LOG2="$TMP/logs2"
OUT2="$TMP/out2"
ERR2="$TMP/err2"
: > "$STUB_LOG"
env ANTHROPIC_API_KEY=receipts-test-fake-key \
    ANTHROPIC_API_URL=http://127.0.0.1:9 \
    FNO_BIN="$STUB" FNO_STUB_LOG="$STUB_LOG" \
    AUTOCORRECT_LOG_DIR="$LOG2" \
    bash "$REVIEW" --packet-file "$FIXTURE" --packet-source receipts-test > "$OUT2" 2> "$ERR2"
RC2=$?
if [[ $RC2 -eq 1 ]]; then
    pass "exit code is 1"
else
    fail "expected exit 1, got $RC2"
fi
LOG="$LOG2/autocorrect-review.log"
if grep -q 'run start source=receipts-test inputs=0' "$LOG"; then
    pass "start receipt carries the packet source and a zero input count"
else
    fail "start receipt missing the packet source or input count"
fi
if grep -q 'run end exit=1 outcome=failed reason=API call failed with HTTP' "$LOG"; then
    pass "end receipt names the API failure"
else
    fail "end receipt missing or does not name the API failure"
fi
grep -o '\-d {.*}' "$STUB_LOG" > "$PAYLOAD_LINE" || true
PYCHECK_RC=0
python3 - "$PAYLOAD_LINE" <<'PYCHECK' || PYCHECK_RC=1
import json
import sys

line = open(sys.argv[1]).read().strip()
prefix = "-d "
assert line.startswith(prefix + "{"), f"no -d payload logged: {line!r}"
data = json.loads(line[len(prefix):])
assert data.get("outcome") == "failed", data
assert data.get("exit_code") == 1, data
assert data.get("packet_source") == "receipts-test", data
assert str(data.get("reason", "")).startswith("API call failed with HTTP"), data
PYCHECK
if [[ $PYCHECK_RC -eq 0 ]]; then
    pass "failure event payload carries the failure fields"
else
    fail "failure event payload is not the failed run's JSON"
fi

# ---- T03: --help writes no receipts ----
echo "T03: argument errors are not runs"
LOG3="$TMP/logs3"
env AUTOCORRECT_LOG_DIR="$LOG3" \
    bash "$REVIEW" --help > /dev/null 2>&1
RC3=$?
if [[ $RC3 -eq 2 ]]; then
    pass "help exits 2"
else
    fail "expected help exit 2, got $RC3"
fi
if [[ ! -f "$LOG3/autocorrect-review.log" ]]; then
    pass "no receipt log for a --help exit"
else
    fail "--help wrote receipts; an argument error is not a run"
fi

summary
exit $?
