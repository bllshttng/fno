#!/usr/bin/env bash
# check-no-user-python.sh - the user path starts no Python. Shrink-only.
#
# The user path is what an installed plugin runs: hooks/, skills/,
# plugins/buddy/ and the user-path scripts named below. In it this counts
# every python start: a `python`/`python3` command word, an absolute-path
# start like `/usr/bin/python3`, `run-python.sh`, a `.py` file
# run as a command, and each tracked `.py` file
# itself (one unit per file; its own lines are not scanned). Comment lines,
# markdown, tests/ trees, and a line marked `user-project-python` (the
# user's own project deps, never fno's) do not count.
#
# Each file's count is held to its row in scripts/ci/user-python-debt.tsv:
#   - a file over its ceiling, or a counted file with no row, fails and names
#     each file:line;
#   - a row above the live count fails too: lower it (shrink-only, the same
#     rule as menu-caps-debt.tsv). A row whose file now counts 0 is removed.
# --max N also asserts the total is N or less. --print writes the live
# table in the debt-file shape, for seeding or lowering rows; never hand-edit
# a ceiling up.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
DEBT="$ROOT/scripts/ci/user-python-debt.tsv"
MAX=""
PRINT=0
while [[ $# -gt 0 ]]; do
    case "$1" in
        --max) MAX="${2:-}"; shift 2 ;;
        --print) PRINT=1; shift ;;
        -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
        *) echo "check-no-user-python: unknown argument: $1" >&2; exit 2 ;;
    esac
done
[[ -z "$MAX" || "$MAX" =~ ^[0-9]+$ ]] || { echo "check-no-user-python: --max needs a number" >&2; exit 2; }

cd "$ROOT" || exit 2
FILES="$(git ls-files -- hooks skills plugins/buddy \
    scripts/validate-plan.sh 'scripts/lib/worktree-*.sh' scripts/lib/worktree_occupancy.py \
    scripts/lib/skill_discovery.py scripts/setup.sh 'scripts/memory/*.sh' \
    scripts/speculate-compare.sh scripts/save-session.py 2>/dev/null \
    | grep -v -E '(^|/)(tests?|evals)/|\.test\.[a-z]+$|\.(md|ya?ml|tsv)$')" || FILES=""

# One hit per line: "path<TAB>line". A .py file is one hit at line 0.
SCAN='
    /^[[:space:]]*(#|\/\/)/ { next }
    /user-project-python/ { next }
    /(^|[^A-Za-z0-9_.\/-])python3?([[:space:]]|$|["\047);|&])/ \
      || /\/(usr\/)?bin\/python3?([[:space:]]|$|["\047);|&])/ \
      || /run-python\.sh/ \
      || /(^[[:space:]]*|exec[[:space:]]+|\$\()"?[$A-Za-z0-9_{}\/.-]*\.py"?([[:space:]]|$)/ {
        print f "\t" NR
    }'
TOMBSTONES="$(awk -F'\t' '$2 == "tombstone" { print $1 }' scripts/ci/python-ledger.tsv 2>/dev/null)"
scan_files() {
    local f
    while IFS= read -r f; do
        [[ -n "$f" && -f "$f" ]] || continue
        if [[ "$f" == *.py ]]; then
            # A tombstone stub keeps a retired hook path resolvable and is never run.
            printf '%s\n' "$TOMBSTONES" | grep -qxF -- "$f" || printf '%s\t0\n' "$f"
        else
            awk -v f="$f" "$SCAN" "$f" 2>/dev/null
        fi
    done
}
HITS="$(printf '%s\n' "$FILES" | scan_files)"

LIVE="$(printf '%s\n' "$HITS" | awk -F'\t' 'NF == 2 { n[$1]++ } END { for (k in n) print k "\t" n[k] }' | sort)"
TOTAL="$(printf '%s\n' "$LIVE" | awk -F'\t' 'NF == 2 { t += $2 } END { print t + 0 }')"

if [[ "$PRINT" -eq 1 ]]; then
    printf '%s\n' "$LIVE" | awk -F'\t' 'NF == 2 { print $1 "\t" $2 "\t-" }'
    exit 0
fi

[[ -f "$DEBT" ]] || { echo "check-no-user-python: missing $DEBT" >&2; exit 2; }

FAIL=0
# Rows: path, ceiling, note. Comment and blank lines skip.
REPORT="$(awk -F'\t' -v live_file=<(printf '%s\n' "$LIVE") '
    BEGIN { while ((getline l < live_file) > 0) { split(l, p, "\t"); if (p[1] != "") live[p[1]] = p[2] } }
    /^#/ || NF < 2 { next }
    {
        row[$1] = $2
        n = ($1 in live) ? live[$1] : 0
        if (n > $2 + 0) print "over\t" $1 "\t" n "\t" $2
        else if (n < $2 + 0) print "stale\t" $1 "\t" n "\t" $2
    }
    END { for (k in live) if (!(k in row)) print "new\t" k "\t" live[k] "\t0" }
' "$DEBT")"

lines_of() { printf '%s\n' "$HITS" | awk -F'\t' -v f="$1" '$1 == f { print "  " f ":" $2 }'; }

while IFS=$'\t' read -r kind path n ceiling; do
    [[ -n "$kind" ]] || continue
    FAIL=1
    case "$kind" in
        over)  echo "check-no-user-python: $path starts Python $n time(s), over its ceiling $ceiling:" ;;
        new)   echo "check-no-user-python: $path starts Python $n time(s) and has no debt row; port it to jq, bash or fno-agents:" ;;
        stale) echo "check-no-user-python: $path now counts $n, below its row's $ceiling; lower the row to $n (remove it at 0)" ;;
    esac
    [[ "$kind" == stale ]] || lines_of "$path"
done <<< "$REPORT"

if [[ -n "$MAX" && "$TOTAL" -gt "$MAX" ]]; then
    echo "check-no-user-python: $TOTAL python start(s) on the user path, over --max $MAX"
    FAIL=1
fi

[[ "$FAIL" -eq 0 ]] && echo "check-no-user-python: ok ($TOTAL python start(s) on the user path, each within its debt row)"
exit "$FAIL"
