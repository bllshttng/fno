#!/usr/bin/env bash
# tests/ci/test_check_python_ledger.sh
#
# Exercises scripts/ci/check-python-ledger.sh against scratch repos. Every
# case asserts the exit code AND a line only that outcome prints, so a gate
# that skipped a check cannot pass by printing nothing.
#
# Run: bash tests/ci/test_check_python_ledger.sh

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GATE="$(cd "${SCRIPT_DIR}/../.." && pwd)/scripts/ci/check-python-ledger.sh"
[[ -f "$GATE" ]] || { echo "gate not found at $GATE" >&2; exit 1; }

# The caller's git config (signing, hooks) must not reach the scratch repos.
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

PASS=0; FAIL=0

# fresh: a repo whose budget gate holds the real PY_ALLOWED shape (a seeded
# minimal copy, so the parse is exercised against the format, not a stub
# guess), plus a ledger and three .py files with valid rows. scripts/gone.py
# exists so its delete-by row is live; the stale-row case removes it.
fresh() {
  rm -rf "$TMP/repo"
  git init -q "$TMP/repo"
  cd "$TMP/repo" || exit 1
  mkdir -p scripts/ci evals/fixtures
  cat > scripts/ci/check-file-budget.sh <<'BUDGET'
#!/usr/bin/env bash
# stand-in; this gate reads only its PY_ALLOWED block
PY_ALLOWED=(
    'tests/py/'
    'evals/fixtures/'
)
BUDGET
  printf 'x' > evals/fixtures/task.py
  printf 'x' > scripts/tool.py
  printf 'x' > scripts/gone.py
  cat > scripts/ci/python-ledger.tsv <<'LEDGER'
# path	verdict	reason
evals/fixtures/task.py	keep	harbor task data
scripts/tool.py	delete-by some-effort	dev tooling
scripts/gone.py	delete-by some-effort	dev tooling
LEDGER
  git add -A && git commit -qm base
}

commit() { git add -A && git commit -qm change; }

# check <label> <want_exit> <marker>
check() {
  local label="$1" want="$2" marker="$3" out got
  shift 3
  out="$(bash "$GATE" 2>&1)"; got=$?
  if [[ "$got" -eq "$want" && "$out" == *"$marker"* ]]; then
    PASS=$((PASS + 1))
  else
    FAIL=$((FAIL + 1))
    printf 'FAIL: %s\n  want exit %s with: %s\n  got exit %s:\n%s\n' "$label" "$want" "$marker" "$got" "$out"
  fi
}

# --- the two-sided invariant ---------------------------------------------------
fresh
check 'every live row and every row live passes' 0 'ok (3 tracked .py'

# --- a live .py with no row (AC1-HP) --------------------------------------------
fresh; printf 'x' > scripts/stray.py; commit
check 'a tracked .py with no row is refused by path' 1 \
  'scripts/stray.py is a tracked .py with no ledger row'

# --- a row naming a missing file -------------------------------------------------
fresh; git rm -q scripts/gone.py; commit
check 'a row over a deleted file is refused' 1 \
  'scripts/gone.py names a file that no longer exists; drop the row'

# --- row hygiene ------------------------------------------------------------------
fresh; printf 'scripts/tool.py\tdelete-by other-effort\talso dev tooling\n' >> scripts/ci/python-ledger.tsv; commit
check 'a duplicate row is refused' 1 'scripts/tool.py has more than one row'

fresh; printf 'scripts/two.py\tdelete-by x\n' > scripts/ci/python-ledger.tsv; commit
check 'a row without its reason column is malformed' 1 'is not path<TAB>verdict<TAB>reason'

fresh; printf 'scripts/tool.py\tretire\tdev tooling\n' > scripts/ci/python-ledger.tsv; commit
check 'an unknown verdict is refused' 1 "has verdict 'retire'"

fresh; printf 'scripts/tool.py\tdelete-by \tdev tooling\n' > scripts/ci/python-ledger.tsv; commit
check 'a delete-by with no effort is refused' 1 "has verdict 'delete-by '"

fresh; printf 'scripts/tool.py\tdelete-by x\t\n' > scripts/ci/python-ledger.tsv; commit
check 'an empty reason is refused' 1 'scripts/tool.py carries an empty reason'

# --- the keep rule reads the one homes list ---------------------------------------
fresh; printf 'scripts/tool.py\tkeep\tdev tooling\n' > scripts/ci/python-ledger.tsv; commit
check 'a keep outside PY_ALLOWED names the remedy' 1 \
  'extend PY_ALLOWED in scripts/ci/check-file-budget.sh'

# A tombstone is debt, not a home claim: it may sit outside PY_ALLOWED while
# its file lasts.
fresh
cat > scripts/ci/python-ledger.tsv <<'LEDGER'
# path	verdict	reason
evals/fixtures/task.py	keep	harbor task data
scripts/tool.py	tombstone	retired hook path stub
scripts/gone.py	delete-by some-effort	dev tooling
LEDGER
commit
check 'a tombstone outside the homes passes' 0 'ok (3 tracked .py'

# --- out-of-scope rows -------------------------------------------------------------
fresh; printf 'scripts/tool.sh\tdelete\tnot a py\n' >> scripts/ci/python-ledger.tsv; commit
check 'a row for a non-.py path is refused' 1 \
  'scripts/tool.sh is not a .py; the ledger records only that scope'

# --- misuse is a 2, never a pass ----------------------------------------------------
fresh; printf '#!/usr/bin/env bash\n' > scripts/ci/check-file-budget.sh; commit
check 'an unreadable PY_ALLOWED is misuse' 2 'could not read PY_ALLOWED'

fresh; rm scripts/ci/python-ledger.tsv; commit
check 'a missing ledger is misuse' 2 'missing scripts/ci/python-ledger.tsv'

printf '\n%d passed, %d failed\n' "$PASS" "$FAIL"
[[ "$FAIL" -eq 0 ]]
