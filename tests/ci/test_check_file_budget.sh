#!/usr/bin/env bash
# tests/ci/test_check_file_budget.sh
#
# Exercises scripts/ci/check-file-budget.sh against scratch repos: a local
# origin holding main, and a feature branch whose diff is the case under test.
# Every case asserts the exit code AND a line only that outcome prints, so a
# gate that skipped a path cannot pass by printing nothing.
#
# Run: bash tests/ci/test_check_file_budget.sh

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GATE="$(cd "${SCRIPT_DIR}/../.." && pwd)/scripts/ci/check-file-budget.sh"
[[ -f "$GATE" ]] || { echo "gate not found at $GATE" >&2; exit 1; }

# The caller's git config (signing, hooks) must not reach the scratch repos.
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
unset FILE_BUDGET_LINES PR_BASE_REF PR_REMOTE FILE_BUDGET_BASE_SHA GITHUB_REPOSITORY

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

PASS=0; FAIL=0
OK='check-file-budget: ok (no over-budget file grew)'

# Distinct content per file, so rename detection pairs only what a case moves.
lines() { seq -f "$2 %g" 1 "$1"; }

# fresh: a new origin whose main holds the base files, and a feature branch
# checked out on top of it. The caller stays in the work tree.
fresh() {
  rm -rf "$TMP/origin.git" "$TMP/repo"
  git init -q --bare "$TMP/origin.git"
  git init -q "$TMP/repo"
  cd "$TMP/repo" || exit 1
  git symbolic-ref HEAD refs/heads/main
  mkdir -p scripts
  lines 150 tool > scripts/tool.py
  lines 40 big > scripts/big.sh
  git add -A && git commit -qm base
  git remote add origin "$TMP/origin.git"
  git push -q origin main
  git checkout -q -b feature
}

commit() { git add -A && git commit -qm change; }

# check <label> <want_exit> <marker> [VAR=value...]
check() {
  local label="$1" want="$2" marker="$3" out got
  shift 3
  out="$(env "$@" bash "$GATE" 2>&1)"; got=$?
  if [[ "$got" -eq "$want" && "$out" == *"$marker"* ]]; then
    PASS=$((PASS + 1))
  else
    FAIL=$((FAIL + 1))
    printf 'FAIL: %s\n  want exit %s with: %s\n  got exit %s:\n%s\n' "$label" "$want" "$marker" "$got" "$out"
  fi
}

# --- the Python location rule --------------------------------------------------
# A NEW .py path outside the allowed homes is refused; a file that exists at
# the base keeps its home. The homes: tests/py, plugins/hermes,
# evals/harness-fit, evals/fixtures, scripts/release.
fresh; lines 5 t > scripts/stray.py; commit
check 'a new .py outside the allowed homes is refused' 1 \
  'scripts/stray.py is a new Python file outside the allowed homes'

fresh; mkdir -p cli/src/fno; lines 5 t > cli/src/fno/new.py; commit
check 'the deleted runtime tree is no Python home' 1 \
  'cli/src/fno/new.py is a new Python file outside the allowed homes'

fresh
mkdir -p tests/py/unit plugins/hermes/promise-tag-reader evals/harness-fit/run evals/fixtures scripts/release
lines 5 t > tests/py/unit/test_new.py
lines 5 t > plugins/hermes/promise-tag-reader/new.py
lines 5 t > evals/harness-fit/run/new.py
lines 5 t > evals/fixtures/new.py
lines 5 t > scripts/release/new.py
commit
# A silent setup failure would leave HEAD == BASE, and the empty diff prints
# the same ok line: assert the files landed before the case can pass.
for p in tests/py/unit/test_new.py plugins/hermes/promise-tag-reader/new.py \
         evals/harness-fit/run/new.py evals/fixtures/new.py scripts/release/new.py; do
  git cat-file -e "HEAD:$p" || { printf 'FAIL: setup lost %s\n' "$p" >&2; FAIL=$((FAIL + 1)); }
done
check 'each allowed home admits a new .py' 0 "$OK"

fresh; lines 5 grow >> scripts/tool.py; commit
check 'an existing stray .py may be edited in place' 0 "$OK"

fresh; git mv scripts/tool.py scripts/tool2.py; commit
check 'a rename into a disallowed home is refused' 1 \
  'scripts/tool2.py is a new Python file outside the allowed homes'

fresh; mkdir -p evals/fixtures; git mv scripts/tool.py evals/fixtures/tool.py; commit
check 'a rename into an allowed home passes' 0 "$OK"

fresh; mkdir -p scripts/release-notes; lines 5 t > scripts/release-notes/gen.py; commit
check 'an allowlist prefix matches whole segments only' 1 \
  'scripts/release-notes/gen.py is a new Python file outside the allowed homes'

# --- the per-file budget ------------------------------------------------------
fresh; lines 5 grow >> scripts/big.sh; commit
check 'an over-budget file may not grow' 1 \
  'scripts/big.sh is 45 lines (budget 20) and this change grows it by +5/-0' FILE_BUDGET_LINES=20

fresh; git mv scripts/big.sh scripts/huge.sh; lines 5 grow >> scripts/huge.sh; commit
check 'a renamed over-budget file may not grow' 1 \
  'scripts/huge.sh is 45 lines (budget 20) and this change grows it by +5/-0' FILE_BUDGET_LINES=20

fresh; mkdir -p cli; git mv scripts/big.sh cli/big.sh; commit
check 'a moved over-budget file is measured against its old path' 0 \
  'ok cli/big.sh 40 lines, change +0/-0 (net 0); no grow' FILE_BUDGET_LINES=20

fresh; lines 5 grow >> scripts/big.sh; commit
check 'the push alarm names the owed shrink' 1 \
  'The next change touching this file must shrink it by at least 5 lines.' \
  FILE_BUDGET_LINES=20 FILE_BUDGET_BASE_SHA="$(git rev-parse main)"

fresh; git rm -q scripts/big.sh; commit
check 'a deleted over-budget file passes' 0 "$OK" FILE_BUDGET_LINES=20

out="$(FILE_BUDGET_LINES=garbage bash "$GATE" 2>&1)"; got=$?
if [[ "$got" -eq 2 && "$out" == *"FILE_BUDGET_LINES must be a number"* ]]; then
  PASS=$((PASS + 1))
else
  FAIL=$((FAIL + 1))
  printf 'FAIL: garbage env is refused loudly\nexit %s\n%s\n' "$got" "$out"
fi

# --- uncommitted work ---------------------------------------------------------
# The diffs read commits, so a worker measuring mid-change would read +0 for
# work it has not committed. The gate must name that, never print a clean zero.
fresh; lines 150 grow >> scripts/big.sh
check 'an uncommitted edit is named, not read as no growth' 0 \
  'uncommitted changes to gated files are not counted'

fresh; lines 5 new > scripts/new.sh
check 'an untracked file is named, not read as no growth' 0 \
  'uncommitted changes to gated files are not counted'

fresh; lines 5 grow >> scripts/big.sh; commit
out="$(bash "$GATE" 2>&1)"
if [[ "$out" == *"$OK"* && "$out" != *'not counted'* ]]; then
  PASS=$((PASS + 1))
else
  FAIL=$((FAIL + 1))
  printf 'FAIL: a clean tree prints no uncommitted warning\n%s\n' "$out"
fi

# --- a stale local copy of the gate -------------------------------------------
# Main replaced the gate after this branch was cut, so the local run measures
# with an older rule than CI will. Warn; the exit code stays the same.
fresh
mkdir -p scripts/ci
echo 'v1' > scripts/ci/check-file-budget.sh
commit
git push -q origin HEAD:main
git checkout -q main
git merge -q --no-edit feature
echo 'v2' >> scripts/ci/check-file-budget.sh
commit
git push -q origin main
git checkout -q feature
check 'a stale gate copy warns and keeps the verdict' 0 'predates main'

fresh
mkdir -p scripts/ci
echo 'branch copy' > scripts/ci/check-file-budget.sh
commit
out="$(bash "$GATE" 2>&1)"; got=$?
if [[ "$got" -eq 0 && "$out" == *'ok'* && "$out" != *'predates main'* ]]; then
  PASS=$((PASS + 1))
else
  FAIL=$((FAIL + 1))
  printf 'FAIL: a branch that edits the gate itself prints no stale-copy warning\nexit %s\n%s\n' "$got" "$out"
fi

printf '\n%d passed, %d failed\n' "$PASS" "$FAIL"
[[ "$FAIL" -eq 0 ]]
