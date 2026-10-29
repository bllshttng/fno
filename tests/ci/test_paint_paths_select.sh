#!/usr/bin/env bash
# tests/ci/test_paint_paths_select.sh
#
# Test harness for scripts/ci/paint-paths-select.sh.
#
# Scenarios:
#   T01 - no .fno/config.toml, no paint file touched   -> paint=false
#   T12 - no .fno/config.toml, a mux client file       -> paint=true
#         (the built-in list the gate falls back to)
#   T13 - a config with no visual_paint_paths key      -> paint=true (built-in)
#   T02 - a changed file under a dir/** pattern        -> paint=true
#   T03 - changed files touching no pattern            -> paint=false
#   T04 - prefix matched without the path boundary     -> paint=false
#         (crates/fno/src/clientx must not match
#         crates/fno/src/client/**; this is the gate's
#         path_matches_paint semantics)
#   T05 - a pattern with no slash matches a basename   -> paint=true
#   T06 - an exact path pattern matches only itself    -> paint=true
#   T07 - the single-string config form                -> paint=true
#   T08 - an unparseable config                        -> paint=true (built-in)
#   T09 - a non-list, non-string value                 -> disarm, rc=0, paint=false
#   T11 - an explicitly empty list                     -> disarm, rc=0, paint=false
#         (the gate treats an explicit empty list as its
#         own override, distinct from a missing config)
#   T10 - the changed-file read fails                  -> refuse: rc=1, no output
#         written, stderr names the refusal
#
# Exit codes: 0 pass, 1 fail, 77 skip (missing deps)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
SELECT_SCRIPT="${REPO_ROOT}/scripts/ci/paint-paths-select.sh"

fail() { printf '[paint-paths-select-test] FAIL: %s\n' "$*" >&2; exit 1; }
pass() { printf '[paint-paths-select-test] PASS: %s\n' "$*"; }

[[ -f "${SELECT_SCRIPT}" ]] || fail "script not found at ${SELECT_SCRIPT}"
bash -n "${SELECT_SCRIPT}" || fail "script failed bash -n"

command -v uv >/dev/null 2>&1 || exit 77
command -v gh >/dev/null 2>&1 || exit 77

TMP="$(mktemp -d -t paint-paths-select-test-XXXXXX)"
trap 'rm -rf "$TMP"' EXIT

# Stub gh: answers the changed-files read from STUB_FILES; STUB_FAIL=1 makes
# the read fail the way an API outage would.
printf '%s\n' \
  '#!/usr/bin/env bash' \
  'if [[ "$1" == "api" ]]; then' \
  '  [[ -n "${STUB_FAIL:-}" ]] && exit 1' \
  '  cat "${STUB_FILES:?}"' \
  '  exit 0' \
  'fi' \
  'exit 64' > "$TMP/gh"
chmod 755 "$TMP/gh"

printf 'docs/x.md\ncrates/fno/src/client/org_board.rs\n' > "$TMP/mix_files"
printf 'docs/x.md\n' > "$TMP/doc_files"
printf 'crates/fno/src/clientx/other.rs\n' > "$TMP/boundary_files"
printf 'crates/fno/Cargo.lock\n' > "$TMP/lock_files"
printf 'docs/style-rules.md\n' > "$TMP/exact_files"

# Runs the script from $TMP (not the checkout) to pin cwd-independence: uv's
# --project and the config read resolve from the script's own checkout root.
case_run() {
  local name="$1" expect_rc="$2" expect_paint="$3" fixture="$4" config="$5"
  if [[ -n "$config" ]]; then
    printf '%b' "$config" > "$TMP/$name.config.toml"
  fi
  local out="$TMP/$name.out"
  # `|| rc=$?` keeps the expected non-zero exit of the script under test from
  # tripping this harness's own set -e.
  local rc=0
  (
    cd "$TMP" || exit 9
    export GITHUB_REPOSITORY="bllshttng/fno" PR_NUMBER="99"
    export STUB_FILES="$fixture" GITHUB_OUTPUT="$out" PATH="$TMP:$PATH"
    if [[ -n "$config" ]]; then
      export PAINT_CONFIG_PATH="$TMP/$name.config.toml"
    else
      unset PAINT_CONFIG_PATH
    fi
    bash "${SELECT_SCRIPT}" > "$TMP/$name.stdout" 2> "$TMP/$name.stderr"
  ) || rc=$?
  local got="none"
  [[ -f "$out" ]] && got="$(command grep '^paint=' "$out" || true)"
  if [[ "$expect_paint" == "none" ]]; then
    [[ "$rc" -eq "$expect_rc" && "$got" == "none" ]] \
      || fail "$name: expected rc=$expect_rc with no output, got rc=$rc '$got' ($(cat "$TMP/$name.stderr"))"
  else
    [[ "$rc" -eq "$expect_rc" && "$got" == "paint=$expect_paint" ]] \
      || fail "$name: expected rc=$expect_rc paint=$expect_paint, got rc=$rc '$got' ($(cat "$TMP/$name.stderr"))"
  fi
  pass "$name"
}

case_run "T01-no-config"        0 "false" "$TMP/doc_files"      ""
case_run "T02-prefix-match"     0 "true"  "$TMP/mix_files"      '[merge]\nvisual_paint_paths = ["crates/fno/src/client/**"]\n'
case_run "T03-no-touch"         0 "false" "$TMP/doc_files"      '[merge]\nvisual_paint_paths = ["crates/fno/src/client/**"]\n'
case_run "T04-boundary-guard"   0 "false" "$TMP/boundary_files" '[merge]\nvisual_paint_paths = ["crates/fno/src/client/**"]\n'
case_run "T05-bare-basename"    0 "true"  "$TMP/lock_files"     '[merge]\nvisual_paint_paths = ["Cargo.lock"]\n'
case_run "T06-exact-path"       0 "true"  "$TMP/exact_files"    '[merge]\nvisual_paint_paths = ["docs/style-rules.md"]\n'
case_run "T07-single-string"    0 "true"  "$TMP/mix_files"      '[merge]\nvisual_paint_paths = "crates/fno/src/client/**"\n'
case_run "T08-unparseable"      0 "true"  "$TMP/mix_files"      '[merge\nvisual_paint_paths = ["crates/fno/src/client/**"]\n'
case_run "T09-non-list-value"   0 "false" "$TMP/mix_files"      '[merge]\nvisual_paint_paths = 3\n'
case_run "T11-empty-list"       0 "false" "$TMP/mix_files"      '[merge]\nvisual_paint_paths = []\n'
case_run "T12-no-config-builtin" 0 "true" "$TMP/mix_files"      ""
case_run "T13-no-key-builtin"   0 "true"  "$TMP/mix_files"      '[merge]\nmerge_strategy = "squash"\n'

export STUB_FAIL=1
case_run "T10-refusal"          1 "none"  "$TMP/mix_files"      '[merge]\nvisual_paint_paths = ["crates/fno/src/client/**"]\n'
unset STUB_FAIL

pass "all cases green"
