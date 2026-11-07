#!/usr/bin/env bash
# check-python-ledger.sh - every tracked .py has one verdict row.
#
# scripts/ci/python-ledger.tsv records a fate for every tracked .py: keep,
# tombstone, delete, delete-by <effort>, port-by <effort>. This
# gate holds the ledger to the tree it describes, in both directions:
#
#   - a tracked .py with no row is an unrecorded Python file.
#     The file-budget gate bars NEW .py outside the allowed homes, but a
#     legacy file can still appear by rename or slip through unjudged; the
#     refusal names it.
#   - a row naming a file that no longer exists is a stale verdict: the
#     deletion landed, so the row must drop (the ledger is shrink-only data,
#     the same rule as menu-caps-debt.tsv).
#   - a keep row must sit inside a PY_ALLOWED home. The homes are read out of
#     scripts/ci/check-file-budget.sh at run time - the same list that gates
#     new .py births - so "where Python may live forever" has exactly one
#     definition. A keep outside the homes refuses; the remedy is to extend
#     PY_ALLOWED there, or to take a delete/port verdict instead.
#   - verdicts, reasons and single rows are validated, because
#     check-no-user-python.sh reads verdict=="tombstone" from column 2 and
#     trusts it.
#
# Run: bash scripts/ci/check-python-ledger.sh
# Exit: 0 every live .py has exactly one valid row; 1 findings;
#       2 misuse (a missing ledger, a missing budget gate, an unreadable
#       PY_ALLOWED) - a gate that cannot read its inputs never reports a pass.

set -uo pipefail

ROOT="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
cd "$ROOT" || exit 2

LEDGER="scripts/ci/python-ledger.tsv"
BUDGET_GATE="scripts/ci/check-file-budget.sh"

case "${1:-}" in
    -h|--help) sed -n '2,/^set -uo pipefail/ { /^set -uo pipefail/q; s/^# \{0,1\}//p; }' "$0"; exit 0 ;;
    "") ;;
    *) echo "check-python-ledger: unknown argument: $1" >&2; exit 2 ;;
esac

[[ -f "$LEDGER" ]] || { echo "check-python-ledger: missing $LEDGER" >&2; exit 2; }
[[ -f "$BUDGET_GATE" ]] || { echo "check-python-ledger: missing $BUDGET_GATE - the homes list lives in its PY_ALLOWED" >&2; exit 2; }

HOMES="$(sed -n '/^PY_ALLOWED=(/,/^)/p' "$BUDGET_GATE" \
    | sed -n "s/^[[:space:]]*'\(.*\)'[[:space:]]*$/\1/p")"
[[ -n "$HOMES" ]] || { echo "check-python-ledger: could not read PY_ALLOWED out of $BUDGET_GATE" >&2; exit 2; }
# BSD awk refuses a multi-line -v value, so the homes travel by environment.
export PY_LEDGER_HOMES="$HOMES"

# Every tracked .py. An empty set is a pass, not an error.
LIVE="$(git -c core.quotepath=off ls-files -- '*.py')"
LIVE_COUNT="$(printf '%s\n' "$LIVE" | awk 'NF { n++ } END { print n + 0 }')"

# One awk pass over the ledger with the live list in memory. Every finding
# names its path, so a red run reads as a work list.
REPORT="$(awk -F'\t' -v live_file=<(printf '%s\n' "$LIVE") '
    BEGIN {
        while ((getline l < live_file) > 0) if (l != "") live[l] = 1
        n = split(ENVIRON["PY_LEDGER_HOMES"], h, "\n")
    }
    /^#/ || NF == 0 { next }
    NF != 3 { print "malformed\t" $1; next }
    {
        path = $1; verdict = $2; reason = $3
        if (path in row) { print "duplicate\t" path; next }
        row[path] = verdict
        if (path !~ /\.py$/) { print "out-of-scope\t" path; next }
        if (!(path in live)) { print "missing-file\t" path }
        if (verdict !~ /^(keep|tombstone|delete)$/ && verdict !~ /^(delete|port)-by .+/) {
            print "bad-verdict\t" path "\t" verdict
        }
        if (reason ~ /^[[:space:]]*$/) print "no-reason\t" path
        if (verdict == "keep") {
            inside = 0
            for (i = 1; i <= n; i++) if (h[i] != "" && index(path, h[i]) == 1) inside = 1
            if (!inside) print "keep-outside-home\t" path
        }
    }
    END { for (p in live) if (!(p in row)) print "no-row\t" p }
' "$LEDGER")"

FAIL=0
while IFS= read -r line; do
    [[ -n "$line" ]] || continue
    FAIL=1
    kind="${line%%$'\t'*}"
    rest="${line#*$'\t'}"
    case "$kind" in
        no-row)
            echo "check-python-ledger: $rest is a tracked .py with no ledger row; give it a verdict" ;;
        missing-file)
            echo "check-python-ledger: $rest names a file that no longer exists; drop the row" ;;
        duplicate)
            echo "check-python-ledger: $rest has more than one row; keep exactly one" ;;
        malformed)
            echo "check-python-ledger: $rest is not path<TAB>verdict<TAB>reason" ;;
        out-of-scope)
            echo "check-python-ledger: $rest is not a .py; the ledger records only that scope" ;;
        bad-verdict)
            echo "check-python-ledger: ${rest%%$'\t'*} has verdict '${rest#*$'\t'}'; a verdict is keep | tombstone | delete | delete-by <effort> | port-by <effort>" ;;
        no-reason)
            echo "check-python-ledger: $rest carries an empty reason; a verdict says why" ;;
        keep-outside-home)
            echo "check-python-ledger: $rest is a keep outside every PY_ALLOWED home; extend PY_ALLOWED in scripts/ci/check-file-budget.sh (the one homes list) or take a delete/port verdict" ;;
    esac
done <<< "$REPORT"

[[ "$FAIL" -eq 0 ]] && echo "check-python-ledger: ok ($LIVE_COUNT tracked .py, each with exactly one verdict row)"
exit "$FAIL"
