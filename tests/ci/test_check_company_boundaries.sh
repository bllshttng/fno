#!/usr/bin/env bash
# Exercises the company boundary gate against hermetic fixture repositories.
set -uo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
CHECK="$REPO_ROOT/scripts/ci/check-company-boundaries.sh"
REPO_BASELINE="$REPO_ROOT/scripts/ci/company-boundary-baseline.txt"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

FAILS=0
ok() { echo "  ok: $1"; }
fail() { echo "  FAIL: $1"; FAILS=$((FAILS + 1)); }

make_repo() {
    local root="$1"
    mkdir -p "$root/cli/src/fno/events" \
        "$root/agents" "$root/skills" "$root/plugins"
    # The positive control's edge: platform -> platform, same-layer and
    # legal (the cross-layer pair left with the company package). The
    # fixture module is the one the gate still maps.
    printf 'from fno.paths import repo_root\n' \
        > "$root/cli/src/fno/events/store_client.py"
}

echo "== mode help is discoverable =="
out="$(bash "$CHECK" --help 2>&1)"; rc=$?
[[ $rc -eq 0 ]] && ok "help exits zero" || fail "help failed: $out"
grep -q <<<"$out" -- '--strict' \
    && grep -q <<<"$out" -- '--baseline' \
    && ok "help names strict and baseline modes" || fail "mode help missing: $out"
grep -q <<<"$out" 'strict.*existing violations' \
    && ok "help says strict remains red" || fail "strict semantics missing: $out"

echo "== clean module-granularity map observes its positive control =="
CLEAN="$TMP/clean"
make_repo "$CLEAN"
out="$(bash "$CHECK" --strict "$CLEAN" 2>&1)"; rc=$?
[[ $rc -eq 0 ]] && ok "clean fixture exits zero" || fail "clean fixture failed: $out"
grep -q <<<"$out" "positive control ok" \
    && ok "positive control is reported" || fail "positive control missing: $out"
grep -Eq <<<"$out" '[0-9]+ layers, [0-9]+ modules, 0 violations' \
    && ok "clean result reports measured coverage" || fail "coverage missing: $out"

printf 'value = 1\n' > "$CLEAN/cli/src/fno/unmapped.py"
out="$(bash "$CHECK" --strict "$CLEAN" 2>&1)"; rc=$?
grep -q <<<"$out" '1 layers, 1 modules, 0 violations' \
    && ok "coverage excludes unmapped modules" || fail "coverage over-counts: $out"

echo "== a broken scan fails closed =="
BROKEN="$TMP/broken"
make_repo "$BROKEN"
printf 'value = 1\n' > "$BROKEN/cli/src/fno/events/store_client.py"
out="$(bash "$CHECK" --strict "$BROKEN" 2>&1)"; rc=$?
[[ $rc -eq 2 ]] && ok "missing control exits two" || fail "expected exit 2, got $rc: $out"
grep -q <<<"$out" "positive control failed" \
    && ok "broken scan names the failed control" || fail "failure message missing: $out"
grep -q <<<"$out" "0 violations" \
    && fail "broken scan printed a clean verdict" || ok "broken scan never prints clean"

echo "== baseline mode fails closed on a broken baseline file =="
FIXTURE_BASELINE="$TMP/fixture-baseline.txt"
printf '%s\n' '# No known violations in this fixture.' > "$FIXTURE_BASELINE"
out="$(bash "$CHECK" --baseline --baseline-file "$TMP/missing.txt" "$CLEAN" 2>&1)"; rc=$?
[[ $rc -eq 2 ]] && ok "missing baseline fails closed" || fail "missing baseline exit $rc: $out"
printf '%s\n' 'opaque-count: 9' > "$FIXTURE_BASELINE"
out="$(bash "$CHECK" --baseline --baseline-file "$FIXTURE_BASELINE" "$CLEAN" 2>&1)"; rc=$?
[[ $rc -eq 2 ]] && ok "opaque baseline fails closed" || fail "opaque baseline exit $rc: $out"

echo "== pack-marked root files are attributed to their source pack =="
PROJECTION="$TMP/projection"
make_repo "$PROJECTION"
mkdir -p "$PROJECTION/plugins/growth-studio" "$PROJECTION/skills/growth-launch"
printf 'id: growth-studio\n' > "$PROJECTION/plugins/growth-studio/plugin.yaml"
printf '%s\n' '---' 'name: growth-marketer' 'pack: growth-studio' '---' \
    > "$PROJECTION/agents/growth-marketer.md"
printf '%s\n' '---' 'name: growth-launch' 'pack: growth-studio' '---' \
    > "$PROJECTION/skills/growth-launch/SKILL.md"
out="$(bash "$CHECK" --strict "$PROJECTION" 2>&1)"; rc=$?
[[ $rc -eq 0 ]] && ok "valid projections stay clean" || fail "projection fixture failed: $out"
grep -q <<<"$out" 'agents/growth-marketer.md -> growth-studio' \
    && ok "agent projection attributed" || fail "agent attribution missing: $out"
grep -q <<<"$out" 'skills/growth-launch -> growth-studio' \
    && ok "skill projection attributed" || fail "skill attribution missing: $out"
grep -q <<<"$out" 'no enforcement for fno-skills' \
    && grep -q <<<"$out" 'fno-mux' \
    && ok "uncovered seams named honestly" || fail "coverage caveat missing: $out"

echo "== an orphaned pack marker is a violation =="
ORPHAN="$TMP/orphan"
make_repo "$ORPHAN"
printf '%s\n' '---' 'name: orphan' 'pack: no-such-pack' '---' \
    > "$ORPHAN/agents/orphan.md"
out="$(bash "$CHECK" --strict "$ORPHAN" 2>&1)"; rc=$?
[[ $rc -eq 1 ]] && ok "orphan exits one" || fail "expected exit 1, got $rc: $out"
grep -q <<<"$out" \
    "agents/orphan.md:3: pack marker 'no-such-pack' names no plugins/no-such-pack/plugin.yaml" \
    && ok "orphan finding names file and marker" || fail "orphan evidence missing: $out"

echo "== checked-in baseline matches shipped findings; strict is red until it drains =="
out="$(bash "$CHECK" --strict "$REPO_ROOT" 2>&1)"; rc=$?
if grep -qE '^(cli/|layer cycle: )' "$REPO_BASELINE"; then
    [[ $rc -eq 1 ]] && ok "repository strict audit stays red" || fail "strict audit exit $rc: $out"
else
    [[ $rc -eq 0 ]] && ok "drained repository strict audit is green" || fail "strict audit exit $rc: $out"
fi
# Counts come from the baseline file, not a literal: a burndown PR should not
# have to edit this test, and a hardcoded number silently rots into a rubber
# stamp once it stops matching reality. Sourcing both from the baseline also
# catches the case the literal never could -- strict and baseline disagreeing.
expected_violations="$(grep -c '^cli/.*L[0-9].* -> L[0-9]' "$REPO_BASELINE")"
actual_violations="$(echo "$out" | grep -c '^  cli/.*L[0-9].* -> L[0-9]')"
[[ "$actual_violations" -eq "$expected_violations" ]] \
    && ok "strict audit reports the $expected_violations baselined violations" \
    || fail "strict reports $actual_violations, baseline holds $expected_violations: $out"
expected_cycle="$(grep '^layer cycle: ' "$REPO_BASELINE" || true)"
if [[ -n "$expected_cycle" ]]; then
    grep -q <<<"$out" "  $expected_cycle" \
        && ok "strict audit retains the baselined cycle" \
        || fail "expected '$expected_cycle' in strict output: $out"
else
    grep -q <<<"$out" 'layer cycle: ' \
        && fail "baseline holds no cycle but strict reports one: $out" \
        || ok "no cycle in baseline, none in strict"
fi
out="$(bash "$CHECK" --baseline --baseline-file "$REPO_BASELINE" "$REPO_ROOT" 2>&1)"; rc=$?
[[ $rc -eq 0 ]] && ok "repository baseline gate exits zero" || fail "repository baseline failed: $out"

echo ""
if [[ $FAILS -eq 0 ]]; then
    echo "test_check_company_boundaries: ALL PASS"
    exit 0
fi
echo "test_check_company_boundaries: $FAILS FAILED"
exit 1
