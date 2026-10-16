#!/usr/bin/env bash
# Angle G (blast radius) replayed against a real miss.
#
# Specimen: fnh 07eb611 (2026-10-13) shipped a TUI (src/tui/mod.rs). fno's
# harness_capabilities.toml kept "# No TUI exists" for the footnote harness,
# written 2026-10-01 in 4763132583, still on main on 2026-10-16. A review of
# the fnh diff read only fnh, so nothing pointed at the fno row. The fix lands
# in a separate PR that wires the footnote pane lane.
#
# The fixture rebuilds that shape: a reviewed repo that adds a TUI, reviewed
# from a linked worktree, and a sibling repo carrying the stale claim. The lane
# step lists siblings with sibling-roots.py, then greps each sibling for the
# surface name. The replay must land on the stale line.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
HELPER="$REPO_ROOT/skills/review/scripts/sibling-roots.py"
LANE="$REPO_ROOT/skills/review/references/single-lane.md"

PASS=0
FAIL=0
pass() { echo "  PASS: $*"; PASS=$((PASS + 1)); }
fail() { echo "  FAIL: $*"; FAIL=$((FAIL + 1)); }

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

mkrepo() {
  git -c init.templateDir= init -q -b main "$1"
  git -C "$1" -c user.email=t@t -c user.name=t commit -q --allow-empty -m init
}

mkrepo "$TMP/fnh"
mkdir -p "$TMP/fnh/src/tui"
printf '//! footnote on a terminal opens a ratatui client of footnote acp.\n' > "$TMP/fnh/src/tui/mod.rs"
git -C "$TMP/fnh" add -A
git -C "$TMP/fnh" -c user.email=t@t -c user.name=t commit -q -m 'feat(tui): footnote on a terminal opens a ratatui client'
git -C "$TMP/fnh" worktree add -q "$TMP/fnh-wt" -b review-wt

mkrepo "$TMP/fno"
cat > "$TMP/fno/harness_capabilities.toml" <<'TOML'
[harness.footnote.permission_response.deny]
supported = false

# No TUI exists: the loop is a one-shot child process per input.
[harness.footnote.resume_strategy.forms.interactive_create]
kind = "unsupported"
TOML
git -C "$TMP/fno" add -A
git -C "$TMP/fno" -c user.email=t@t -c user.name=t commit -q -m 'harness rows'
# The checkout moves on without the claim; the committed ref still carries it.
rm "$TMP/fno/harness_capabilities.toml"

mkrepo "$TMP/elsewhere"

cat > "$TMP/workspaces.json" <<JSON
{"footnote": {"projects": [
   {"name": "fnh", "path": "$TMP/fnh"},
   {"name": "fno", "path": "$TMP/fno"},
   {"name": "gone", "path": "$TMP/not-on-disk"},
   {"name": "pathless"}]},
 "other": {"projects": [{"name": "elsewhere", "path": "$TMP/elsewhere"}]}}
JSON

test_sibling_resolution() {
  echo "== siblings resolve from a linked worktree; an unlisted repo exits 3, not a zero"
  local out rc lonely_rc
  out="$(python3 "$HELPER" "$TMP/fnh-wt" < "$TMP/workspaces.json")"
  rc=$?
  mkrepo "$TMP/lonely"
  python3 "$HELPER" "$TMP/lonely" < "$TMP/workspaces.json" >/dev/null 2>&1
  lonely_rc=$?
  if [ "$rc" -eq 0 ] && [ "$out" = "fno	$(cd "$TMP/fno" && pwd -P)" ] && [ "$lonely_rc" -eq 3 ]; then
    pass "only the on-disk, pathful sibling in the same workspace is listed; unlisted exits 3"
  else
    fail "rc=$rc out='$out' lonely_rc=$lonely_rc"
  fi
}

test_lane_replay_lands_on_the_stale_claim() {
  echo "== the lane's sweep (helper, then git grep on each sibling ref) finds the stale TUI claim"
  local hits="" name path
  while IFS=$'\t' read -r name path; do
    hits+="$(git -C "$path" grep -n -F -e 'TUI' HEAD || true)"
  done < <(python3 "$HELPER" "$TMP/fnh-wt" < "$TMP/workspaces.json")
  if [[ "$hits" == *"harness_capabilities.toml:4:# No TUI exists"* ]] \
    && grep -qF 'fno config get work.workspaces | python3 "${SKILL_DIR}/scripts/sibling-roots.py" .' "$LANE" \
    && grep -qF 'siblings: unconfigured' "$LANE"; then
    pass "sweep names the stale line by file:line, and Angle G runs this helper and maps exit 3"
  else
    fail "sweep missed the claim, or Angle G no longer runs the helper; hits='$hits'"
  fi
}

test_sibling_resolution
test_lane_replay_lands_on_the_stale_claim

echo
echo "blast-radius: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
