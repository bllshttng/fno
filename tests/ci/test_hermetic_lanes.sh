#!/usr/bin/env bash
# test_hermetic_lanes.sh - the dirty lane must be able to go red.
#
# `fno doctor test smoke --ambient dirty` claims that a green run means the ambient
# surface is complete. That claim rests entirely on the lane being ABLE to
# fail: a success condition built on an absence cannot distinguish "no leaks"
# from "the instrument never ran".
#
# So this asserts both halves against the canary, which reads a channel
# the env builder deliberately does not scrub:
#
#   clean lane -> canary unset  -> the canary step PASSES
#   dirty lane -> canary leaks  -> the canary step FAILS
#
# A failure of the second half means the positive control has been disarmed -
# usually by someone widening a scrub rule until it swallows the canary - and
# the dirty lane has silently become decorative.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$REPO_ROOT" || exit 1

PASS=0
FAIL=0
pass() { echo "  PASS: $1"; PASS=$((PASS + 1)); }
fail() { echo "  FAIL: $1"; FAIL=$((FAIL + 1)); }

BIN="${FNO_AGENTS_BIN:-fno-agents}"
if ! command -v "$BIN" >/dev/null 2>&1 && [ ! -x "$BIN" ]; then
  echo "hermetic lanes: fno-agents is not on PATH; install the crate first"
  exit 2
fi

SEAM="$(mktemp -d)"
trap 'rm -rf "$SEAM"' EXIT
cat > "$SEAM/registry.json" <<'JSON'
{"steps": [["ambient canary control", ".", "bash tests/ci/canaries/ambient-canary.sh"]]}
JSON

# Run the canary through the native smoke runner, in one lane. Going through
# the runner is the point: that is the boundary the real smoke run uses. The
# clean lane must be clean BY CONSTRUCTION, so the canary is dropped from this
# process first: under `--ambient dirty` this script is itself a poisoned child.
run_lane() {
  env -u AMBIENT_LEAK_CANARY SMOKE_REGISTRY_FILE="$SEAM/registry.json" \
    "$BIN" test-run doctor smoke --ambient "$1" \
    --only 'ambient canary control' >/dev/null 2>&1
  return $?
}

echo "hermetic lanes: the dirty lane must be able to go red"

run_lane clean
clean_rc=$?
if [ "$clean_rc" -eq 0 ]; then
  pass "clean lane: canary passes (no ambient state reaches a step)"
else
  fail "clean lane: canary failed (rc=$clean_rc); something poisons the env outside --ambient dirty"
fi

run_lane dirty
dirty_rc=$?
if [ "$dirty_rc" -ne 0 ]; then
  pass "dirty lane: canary fails (rc=$dirty_rc) - the lane can detect a leak"
else
  fail "dirty lane: canary PASSED, so the positive control is disarmed.
       The dirty lane can no longer prove it detects anything, and a green run
       from it is not evidence. Check whether a scrub rule in the env builder
       widened far enough to swallow AMBIENT_LEAK_CANARY."
fi

echo
echo "  passed: $PASS  failed: $FAIL"
[ "$FAIL" -eq 0 ] || exit 1
exit 0
