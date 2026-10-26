#!/usr/bin/env bash
# test_state_lanes.sh - the populated lane must be able to go red.
#
# The populated lane claims a green means the populated state shape is
# faithfully replayed, not silently skipped. That claim rests on the lane
# being ABLE to fail: the pytest-side positive control (the state canary)
# must fail when the profile is populated and pass when it is not.
#
# This drives the NATIVE smoke runner over a one-step registry (the
# SMOKE_REGISTRY_FILE seam), so the lane env, the populated profile, the
# canary bracket, and the junit verdict all come from the shipped path -
# never a re-implementation beside it.
#
#   clean lane     -> control passes
#   populated lane -> control fails (the positive control firing)
#
# A populated lane that cannot fail means the control has been disarmed and
# every green from that lane is decorative.

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
  echo "state lanes: fno-agents is not on PATH; install the crate first"
  exit 2
fi

SEAM="$(mktemp -d)"
trap 'rm -rf "$SEAM"' EXIT
cat > "$SEAM/registry.json" <<'JSON'
{"steps": [["state canary pytest control", ".", "uv run --project cli pytest -q -p no:cacheprovider cli/tests/unit/test_state_canary.py"]]}
JSON

run_lane() {
  SMOKE_REGISTRY_FILE="$SEAM/registry.json" "$BIN" test smoke --state "$1" \
    --only 'state canary pytest control' >/dev/null 2>&1
  return $?
}

echo "state lanes: populated lane must be able to go red"

run_lane clean
clean_rc=$?
if [ "$clean_rc" -eq 0 ]; then
  pass "clean lane: state canary passes"
else
  fail "clean lane: state canary failed (rc=$clean_rc)"
fi

run_lane populated
populated_rc=$?
if [ "$populated_rc" -ne 0 ]; then
  pass "populated lane: state canary fails (rc=$populated_rc)"
else
  fail "populated lane: state canary passed, so the positive control is disarmed"
fi

echo
echo "  passed: $PASS  failed: $FAIL"
[ "$FAIL" -eq 0 ] || exit 1
exit 0
