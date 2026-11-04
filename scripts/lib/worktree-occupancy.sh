#!/usr/bin/env bash
# Worktree occupancy classification bridge.
#
# wt_classify_pids <worktree> <newline-separated pids>
#
# Prints one tab-separated row per pid:
#   pid <tab> verdict <tab> action <tab> job <tab> reason <tab> cmd
# verdict is `holds` or `inert`; action is `keep`, `terminate` or `retire`.
#
# Fail closed: a non-zero classifier exit, a missing fno-agents, or a row
# count that differs from the pid count reads every pid as
# `holds keep - classifier unavailable -`. A tree with any holds row is kept.
# Absence of a recognised holder is never proof a tree is free.

_wt_occupancy_failclosed() {
    local pid
    while IFS= read -r pid; do
        [[ -z "$pid" ]] && continue
        printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$pid" holds keep - "classifier unavailable" -
    done <<< "$1"
}

wt_classify_pids() {
    local wt="$1" pids="$2" root out="" rc=0 n_pids=0 n_rows=0
    local bin=""
    root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
    if [[ -n "${FNO_WT_OCCUPANCY_CMD:-}" ]]; then
        # Test seam: the hook battery stubs the classifier. Production sets nothing.
        # shellcheck disable=SC2086  # one pid per argv word, by contract
        out="$("$FNO_WT_OCCUPANCY_CMD" "$wt" $pids 2>/dev/null)" || rc=$?
    else
        if [[ -f "${root}/hooks/lib/agents-bin.sh" ]]; then
            # shellcheck source=/dev/null
            source "${root}/hooks/lib/agents-bin.sh"
        fi
        if declare -F fno_agents_bin >/dev/null; then
            bin="$(fno_agents_bin "$root")"
        else
            bin="$(command -v fno-agents || true)"
        fi
        if [[ -z "$bin" ]]; then
            rc=1
        else
            # One row per pid: the idle login shell lane, then the classifier
            # (crates/fno-agents/src/occupancy_classify.rs).
            # shellcheck disable=SC2086  # one pid per argv word, by contract
            out="$("$bin" occupancy-login --classify "$wt" $pids 2>/dev/null)" || rc=$?
        fi
    fi
    n_pids="$(printf '%s\n' "$pids" | grep -c .)"
    if [[ "$rc" -eq 0 && -n "$out" ]]; then
        n_rows="$(printf '%s\n' "$out" | grep -c .)"
    fi
    if [[ "$rc" -ne 0 || "$n_rows" -ne "$n_pids" ]]; then
        _wt_occupancy_failclosed "$pids"
        return 0
    fi
    printf '%s\n' "$out"
}

# The receipt the sweep prints beside a kept or would-archive tree: one
# indented line per classified hit.
wt_occupancy_print_rows() {
    printf '%s\n' "$1" | awk -F '\t' 'NF >= 6 {
        cmd = $6
        if (length(cmd) > 80) cmd = substr(cmd, 1, 80)
        printf "    %s %s %s | %s\n", $1, $2, $5, cmd
    }'
}
