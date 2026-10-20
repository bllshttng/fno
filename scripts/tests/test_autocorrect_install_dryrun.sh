#!/usr/bin/env bash
# Installer contract for the weekly cadence: the Linux dry-run prints the
# Monday-09:00 managed block with stdout and stderr merged into
# autocorrect-review.log, and never moves the old monthly log. The launchd
# template carries the same schedule with both streams pointed at the one
# review log. The schedule is hard-coded (ruling d-4a6b06b4: no config keys).

set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INSTALLER="$SCRIPT_DIR/../install-autocorrect-cron.sh"
PLIST_TEMPLATE="$SCRIPT_DIR/../../templates/com.user.autocorrect.plist"
PASS=0
FAIL=0

pass() {
    echo "  PASS: $1"
    PASS=$((PASS + 1))
}
fail() {
    echo "  FAIL: $1"
    FAIL=$((FAIL + 1))
}
summary() {
    echo ""
    echo "PASS=$PASS FAIL=$FAIL"
    [[ $FAIL -eq 0 ]]
}

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
LOG_DIR="$TMP/logs"
mkdir -p "$LOG_DIR"

# ---- T01: the launchd template is weekly with a merged log ----
echo "T01: plist template schedule and merged log"
if grep -A1 '<key>Weekday</key>' "$PLIST_TEMPLATE" | grep -q '<integer>1</integer>'; then
    pass "Weekday 1 (Monday)"
else
    fail "Weekday is not 1"
fi
if grep -A1 '<key>Hour</key>' "$PLIST_TEMPLATE" | grep -q '<integer>9</integer>'; then
    pass "Hour 9"
else
    fail "Hour is not 9"
fi
if grep -A1 '<key>Day</key>' "$PLIST_TEMPLATE" | grep -q '<integer>'; then
    fail "monthly Day key still present"
else
    pass "no monthly Day key"
fi
REVIEW_LOG_HITS=$(grep -c 'autocorrect-review.log' "$PLIST_TEMPLATE" || true)
if [[ "$REVIEW_LOG_HITS" -eq 2 ]]; then
    pass "stdout and stderr both point at the review log"
else
    fail "expected 2 review-log paths (stdout + stderr), got $REVIEW_LOG_HITS"
fi
if grep -q 'autocorrect-monthly' "$PLIST_TEMPLATE"; then
    fail "template still names the monthly log"
else
    pass "no monthly log path left in the template"
fi
if grep -q -- '--packet-source weekly-cron' "$PLIST_TEMPLATE"; then
    pass "packet source names the weekly cron"
else
    fail "packet source is not weekly-cron"
fi
if grep -q -- '--severity S1,S2 --dry-run' "$PLIST_TEMPLATE"; then
    pass "pack runs dry-run so only the review advances the watermark"
else
    fail "pack does not run dry-run; a refused review loses its window"
fi

# ---- T02: Linux dry-run prints the weekly block, writes nothing ----
echo "T02: Linux dry-run schedule"
if [[ "$(uname)" == "Darwin" ]]; then
    echo "  SKIP: dry-run renders the plist on macOS; the cron block is Linux-only"
    PASS=$((PASS + 1))
else
    DRY_OUT="$TMP/dryrun.out"
    DRY_ERR="$TMP/dryrun.err"
    AUTOCORRECT_LOG_DIR="$LOG_DIR" bash "$INSTALLER" --dry-run > "$DRY_OUT" 2> "$DRY_ERR"
    if grep -F '0 9 * * 1' "$DRY_OUT" > /dev/null; then
        pass "cron line is Monday 09:00"
    else
        fail "cron line is not 0 9 * * 1"
    fi
    if grep -F 'autocorrect-review.log 2>&1' "$DRY_OUT" > /dev/null; then
        pass "cron line merges both streams into the review log"
    else
        fail "cron line does not merge into autocorrect-review.log"
    fi
    if grep -F -- '--severity S1,S2 --dry-run' "$DRY_OUT" > /dev/null; then
        pass "cron line runs pack dry-run so the review owns the watermark"
    else
        fail "cron line advances the watermark at pack time"
    fi
    if grep -F '; } >>' "$DRY_OUT" > /dev/null; then
        pass "pipeline is brace-grouped so pack stderr reaches the log"
    else
        fail "pipeline redirect binds to the review script only"
    fi
    if grep -q 'monthly' "$DRY_OUT"; then
        fail "dry-run block still names monthly"
    else
        pass "dry-run block carries no monthly schedule"
    fi
    if [[ ! -f "$LOG_DIR/autocorrect-review.log" ]]; then
        pass "dry-run created no log file"
    else
        fail "dry-run wrote a log file"
    fi
fi

# ---- T03: dry-run announces the one-time log move, performs none ----
echo "T03: one-time monthly log migration"
OLD_LOG="$LOG_DIR/autocorrect-monthly.log"
printf 'old monthly history\n' > "$OLD_LOG"
DRY_ERR3="$TMP/dryrun3.err"
AUTOCORRECT_LOG_DIR="$LOG_DIR" bash "$INSTALLER" --dry-run > /dev/null 2> "$DRY_ERR3"
if grep -q 'would move' "$DRY_ERR3"; then
    pass "dry-run announces the migration"
else
    fail "dry-run did not announce the monthly log migration"
fi
if [[ -f "$OLD_LOG" && ! -e "$LOG_DIR/autocorrect-review.log" ]]; then
    pass "dry-run moved nothing"
else
    fail "dry-run performed the move or lost the old log"
fi

summary
exit $?
