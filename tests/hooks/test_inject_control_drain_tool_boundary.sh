#!/usr/bin/env bash
# Contract tests for hooks/inject-control-drain-tool-boundary.sh: the shell
# gate only. A stub fno-agents logs its argv, so no build is needed; the
# command's own render and cursor contract lives in mail_control_drain.rs.

set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HOOK="$HERE/../../hooks/inject-control-drain-tool-boundary.sh"
[[ -f "$HOOK" ]] || { echo "FAIL: hook missing at $HOOK"; exit 1; }

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

SID="ffffabcd-1111-2222-3333-444455556666"
BUS="$TMP/state/bus"
LOG="$TMP/args.log"
PASS=0
FAIL=0
ok()  { PASS=$((PASS + 1)); echo "ok: $1"; }
bad() { FAIL=$((FAIL + 1)); echo "FAIL: $1"; }

mkdir -p "$TMP/bin" "$TMP/nojq"
printf '#!/usr/bin/env bash\nprintf "%%s\\n" "$*" >> "%s"\n' "$LOG" > "$TMP/bin/fno-agents"
# A jq that answers like a missing binary: the hook must still read the id.
printf '#!/usr/bin/env bash\nexit 127\n' > "$TMP/nojq/jq"
chmod +x "$TMP/bin/fno-agents" "$TMP/nojq/jq"

fresh() {
    rm -rf "$TMP/state"
    mkdir -p "$BUS"
    : > "$LOG"
    echo '{"id":"m1"}' > "$BUS/messages.jsonl"
}

run_hook() {  # $1 extra PATH prefix
    PATH="$1:$TMP/bin:$PATH" FNO_STATE_DIR="$TMP/state" bash "$HOOK" \
        <<<"{\"session_id\":\"$SID\",\"tool_name\":\"Bash\",\"tool_input\":{}}"
}

called() { grep -q -- "--control-drain --bus-dir $BUS --session $SID" "$LOG"; }

# AC4: jq missing, control flag set: the drain still runs.
fresh
mkdir -p "$BUS/control-pending" "$BUS/tool-boundary"
touch "$BUS/control-pending/${SID:0:8}.flag"
touch -t 209901010000 "$BUS/tool-boundary/$SID.stamp"
run_hook "$TMP/nojq"
called && ok "jq missing still drains a control flag" || bad "jq missing skipped the drain"

# No flag, stamp ahead of the bus: the gate skips the command.
fresh
mkdir -p "$BUS/tool-boundary"
touch -t 209901010000 "$BUS/tool-boundary/$SID.stamp"
run_hook ""
called && bad "fresh stamp still ran the command" || ok "fresh stamp skips the command"

# No flag, no stamp: the bus counts as changed, so the command runs.
fresh
run_hook ""
called && ok "missing stamp runs the command" || bad "missing stamp skipped the command"

echo "passed=$PASS failed=$FAIL"
[[ "$FAIL" -eq 0 ]]
