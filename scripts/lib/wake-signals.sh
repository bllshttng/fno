#!/usr/bin/env bash
# wake-signals.sh - log-only observation of kind=question wake-signals.
#
# Lifted from hooks/target-stop-hook.sh (Phase 1 of stop-hook refactor).
# Reads kind=question wake-signals from <space>/wake-signals/ and
# appends one wake_signal_observed JSON line per signal to
# hook-events.jsonl. LOG-ONLY: does NOT delete signals (SessionStart /
# UserPromptSubmit own that). Does NOT refuse exit or trip any detector.
#
# The read answers through the fno-agents binary's hidden `wake-signals`
# op (crates/fno-agents/src/wake_read.rs); the Python wheel no longer
# ships the wake module.
#
# Graceful-degrade: a missing fno-agents binary is silently skipped so
# hosts without the Rust runtime are unaffected, exactly as the old
# missing-module degrade was.
#
# Requires (set by caller):
#   REPO_ROOT  - project repo root (current working tree)
#   STATE_DIR  - .fno/ directory for the project

_observe_wake_signals() {
    local sigs
    sigs=$(fno-agents wake-signals --kind question --root "$REPO_ROOT" 2>/dev/null) || return 0

    [[ "$sigs" == "[]" ]] && return 0

    echo "$sigs" | python3 -c "
import json, sys, time
for sig in json.load(sys.stdin):
    print(json.dumps({
        'event': 'wake_signal_observed',
        'signal_id': sig.get('signal_id', ''),
        'kind': sig.get('kind', ''),
        'from': sig.get('from_project', ''),
        'msg_id': sig.get('msg_id', ''),
        'ts': time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime()),
    }))
" >> "$STATE_DIR/hook-events.jsonl" 2>/dev/null || return 0
}
