#!/usr/bin/env bash
# The fix loop names the producer before any defensive check.
#
# Specimen (a carveout, 2026-08-20): finalize never rewrites a ledger
# row once one exists, so the first terminal recorded sticks and a merged PR
# stays wedged in the stop gate. No correct path makes that state. The class
# was answered with a fidelity waiver at the reader instead of a fix
# to finalize, and it came back as a second specimen.
#
# The skill is prose, so the contract is the prompt bytes a /fix run reads:
# the order of the loop, and the rule that sorts this specimen as
# "illegitimate -> fix the producer, discard the reader-side check".
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SKILL="$SCRIPT_DIR/.."
LOOP="$SKILL/references/fix-loop.md"
INVESTIGATE="$SKILL/references/investigate.md"

PASS=0
FAIL=0
pass() { echo "  PASS: $*"; PASS=$((PASS + 1)); }
fail() { echo "  FAIL: $*"; FAIL=$((FAIL + 1)); }
line_of() { grep -nF -- "$2" "$1" | head -1 | cut -d: -f1; }

test_cause_comes_before_the_edit() {
  echo "== the loop names the root cause before it makes the fix"
  local cause edit
  cause="$(line_of "$LOOP" '3. name the root cause (below) before you edit')"
  edit="$(line_of "$LOOP" '4. make one focused fix at the root cause')"
  if [ -n "$cause" ] && [ -n "$edit" ] && [ "$cause" -lt "$edit" ]; then
    pass "cause step (line $cause) precedes the edit step (line $edit)"
  else
    fail "cause step missing or after the edit (cause=$cause edit=$edit)"
  fi
}

test_specimen_sorts_as_illegitimate() {
  echo "== a stale row the code wrote itself sorts as illegitimate: fix the producer"
  if grep -qF -- '**illegitimate**: the code made this state itself, and no correct path makes it.' "$LOOP" \
    && grep -qF -- 'A defensive check at the consumer hides the defect' "$LOOP" \
    && grep -qF -- 'a defensive check over a state that no correct path makes' "$LOOP" \
    && grep -qF -- '- **Producer:** `file:line`' "$INVESTIGATE" \
    && grep -qF -- '- **State:** legitimate | illegitimate' "$INVESTIGATE"; then
    pass "the reader-side waiver shape is discarded, and investigate hands /fix the producer and verdict"
  else
    fail "the loop no longer sorts a self-made bad state to a producer fix"
  fi
}

test_cause_comes_before_the_edit
test_specimen_sorts_as_illegitimate

echo
echo "root-cause: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
