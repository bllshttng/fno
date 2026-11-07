#!/usr/bin/env bash
# Exercises the company boundary gate against hermetic fixture repositories.
set -uo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
CHECK="$REPO_ROOT/scripts/ci/check-company-boundaries.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

FAILS=0
ok() { echo "  ok: $1"; }
fail() { echo "  FAIL: $1"; FAILS=$((FAILS + 1)); }

make_repo() {
    mkdir -p "$1/agents" "$1/skills" "$1/plugins"
}

echo "== help is discoverable =="
out="$(bash "$CHECK" --help 2>&1)"; rc=$?
[[ $rc -eq 0 ]] && ok "help exits zero" || fail "help failed: $out"
grep -q <<<"$out" 'plugins/<pack>/plugin.yaml' \
    && ok "help names the pack contract" || fail "pack contract missing: $out"

echo "== pack-marked root files are attributed to their source pack =="
PROJECTION="$TMP/projection"
make_repo "$PROJECTION"
mkdir -p "$PROJECTION/plugins/growth-studio" "$PROJECTION/skills/growth-launch"
printf 'id: growth-studio\n' > "$PROJECTION/plugins/growth-studio/plugin.yaml"
printf '%s\n' '---' 'name: growth-marketer' 'pack: growth-studio' '---' \
    > "$PROJECTION/agents/growth-marketer.md"
printf '%s\n' '---' 'name: growth-launch' 'pack: growth-studio' '---' \
    > "$PROJECTION/skills/growth-launch/SKILL.md"
out="$(bash "$CHECK" "$PROJECTION" 2>&1)"; rc=$?
[[ $rc -eq 0 ]] && ok "valid projections stay clean" || fail "projection fixture failed: $out"
grep -q <<<"$out" 'agents/growth-marketer.md -> growth-studio' \
    && ok "agent projection attributed" || fail "agent attribution missing: $out"
grep -q <<<"$out" 'skills/growth-launch -> growth-studio' \
    && ok "skill projection attributed" || fail "skill attribution missing: $out"

echo "== an orphaned pack marker is a violation =="
ORPHAN="$TMP/orphan"
make_repo "$ORPHAN"
printf '%s\n' '---' 'name: orphan' 'pack: no-such-pack' '---' \
    > "$ORPHAN/agents/orphan.md"
out="$(bash "$CHECK" "$ORPHAN" 2>&1)"; rc=$?
[[ $rc -eq 1 ]] && ok "orphan exits one" || fail "expected exit 1, got $rc: $out"
grep -q <<<"$out" \
    "agents/orphan.md:3: pack marker 'no-such-pack' names no plugins/no-such-pack/plugin.yaml" \
    && ok "orphan finding names file and marker" || fail "orphan evidence missing: $out"

echo "== the repository passes =="
out="$(bash "$CHECK" "$REPO_ROOT" 2>&1)"; rc=$?
[[ $rc -eq 0 ]] && ok "repository gate exits zero" || fail "repository gate failed: $out"

echo ""
if [[ $FAILS -eq 0 ]]; then
    echo "test_check_company_boundaries: ALL PASS"
    exit 0
fi
echo "test_check_company_boundaries: $FAILS FAILED"
exit 1
