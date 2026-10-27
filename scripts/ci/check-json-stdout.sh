#!/usr/bin/env bash
# check-json-stdout.sh - the JSON-stdout house contract over fno commands.
#
# The scratch census (internal/fno/briefs/20260908-scratch-script-shapes-findings.md,
# section 4 rank 1) measured "wrap an fno command to parse its output" as the biggest
# recurring scratch shape: 238 files across 13 jobs. A wrapper exists because a
# -J command sometimes prints silence on a miss, shares stdout with prose, or makes
# the agent guess the envelope key. This gate runs a curated table of read-only
# commands and asserts the house contract: stdout is exactly one JSON document; the
# pinned rows also check the shape (error doc, empty list).
#
# Commands run through the repo CLI (uv run --project cli fno-py) with the FNO home
# pinned to a temp dir, so no machine state feeds the verdict. The native get
# execs fno-agents; the binary resolves FNO_AGENTS_BIN, then the checkout build,
# then the deployed binary, so the get row tests this tree's fix.
#
# Baseline waivers live in scripts/ci/json-stdout-baseline.tsv
# (`<row id>\t<reason>`), shrink-only by review: a row leaves as its fix lands,
# and a row enters only naming the observed violation.
#
# Run: bash scripts/ci/check-json-stdout.sh --selftest, then without the flag.
# Exit: 0 clean, 1 a contract violation, 2 the gate cannot be trusted here
#       (selftest refused, CLI missing).

# No set -e: run_row captures every command's rc itself, and a failing row
# must downgrade to WAIVED or FAIL, never kill the run.
set -uo pipefail

SELFTEST=0
if [[ "${1:-}" == "--selftest" ]]; then
    SELFTEST=1
fi

ROOT="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
cd "$ROOT"

CHECKER="scripts/ci/json_stdout_check.py"

# The repo CLI tests source truth; the fallback reads the deployed fno.
if command -v uv >/dev/null 2>&1; then
    CLI=(uv run --project cli fno-py)
else
    CLI=(fno)
fi

# The native get execs fno-agents. The checkout build outranks the deployed
# binary, so the get row tests this tree's fix; FNO_AGENTS_BIN stays
# caller-authoritative.
if [[ -z "${FNO_AGENTS_BIN:-}" && -x "$ROOT/crates/fno-agents/target/debug/fno-agents" ]]; then
    export FNO_AGENTS_BIN="$ROOT/crates/fno-agents/target/debug/fno-agents"
fi

# A temp FNO home: no machine state feeds the verdict, and FNO_CONFIG +
# FNO_GLOBAL_SETTINGS_PATH keep setup chatter and settings reads off the real
# vault.
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
export FNO_STATE_DIR="$TMP/state"
export FNO_AGENTS_HOME="$TMP/agents"
export FNO_SPACES_DIR="$TMP/spaces"
export FNO_CLAIMS_ROOT="$TMP/claims"
export FNO_BUS_DIR="$TMP/bus"
export FNO_EVENTS_PATH="$TMP/events.jsonl"
export FNO_CONFIG="$TMP/config.toml"
export FNO_GLOBAL_SETTINGS_PATH=/dev/null
: > "$FNO_CONFIG"

# run_row <expect> <row-id> <cmd...>: run one command row and print PASS, WAIVED
# (a baseline row matched), or FAIL with the checker's reason. The row id is
# the command argv without the CLI launcher, so a baseline author writes the
# command they are waiving, not the resolved launcher.
run_row() {
    local expect="$1"
    shift
    local row_id="$1"
    shift
    local out rc verdict reason
    out="$("$@" 2>/dev/null)"
    rc=$?
    verdict="$(printf '%s' "$out" | python3 -I "$CHECKER" "$expect" "$rc")"
    case "$verdict" in
        PASS*)
            echo "PASS $row_id"
            return 0
            ;;
    esac
    if [[ -f "$ROOT/scripts/ci/json-stdout-baseline.tsv" ]]; then
        reason="$(awk -F '\t' -v row="$row_id" '$1 == row { print $2; exit }' \
            "$ROOT/scripts/ci/json-stdout-baseline.tsv")"
        if [[ -n "$reason" ]]; then
            echo "WAIVED $row_id - $reason"
            return 0
        fi
    fi
    echo "FAIL $row_id: ${verdict#FAIL }"
    return 1
}

if [[ "$SELFTEST" == 1 ]]; then
    # The gate must fail a noisy command and a silent rc-1 command, and pass a clean
    # one, or nothing it says on the real table can be trusted. Exit 2 on any
    # miss.
    status=0
    v="$(run_row doc 'stub: noisy stdout' bash -c 'printf "chatter\n{}\n"')"
    [[ "$v" == FAIL* ]] || {
        echo "selftest refused: noisy stdout was not failed" >&2
        status=1
    }
    v="$(run_row doc 'stub: clean document' bash -c 'printf "{}\n"')"
    [[ "$v" == PASS* ]] || {
        echo "selftest refused: a clean document was not passed" >&2
        status=1
    }
    v="$(run_row doc 'stub: silent rc-1' bash -c 'exit 1')"
    [[ "$v" == FAIL* ]] || {
        echo "selftest refused: a silent rc-1 command was not failed" >&2
        status=1
    }
    v="$(run_row error-doc 'stub: error document rc-1' bash -c 'printf "{\"ok\": false, \"error\": \"x\"}\n"; exit 1')"
    [[ "$v" == PASS* ]] || {
        echo "selftest refused: an error document with rc 1 was not passed" >&2
        status=1
    }
    if [[ "$status" == 0 ]]; then
        echo "selftest ok"
    fi
    exit $((status == 0 ? 0 : 2))
fi

status=0
run_row doc "doctor lint registry -J" "${CLI[@]}" doctor lint registry -J || status=1
run_row error-doc "backlog -J get <miss>" "${CLI[@]}" backlog -J get x-zzzz9999nope || status=1
exit "$status"
