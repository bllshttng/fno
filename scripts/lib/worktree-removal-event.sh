#!/usr/bin/env bash
# worktree-removal-event.sh - one worktree_removed event row per removal.
#
# Sourced by every removal executor (archive-worktree.sh for the guarded path
# the merged sweep and the post-merge ritual reach; worktree-lifecycle.sh for
# the age sweep's direct remove). Before this, NO removal path emitted
# anything: a live worker losing its tree left no row, and 2239 journal rows
# contained zero removals, so recurrence was unattributable. The row records
# what the guards read at decision time, so a removal that skipped the claim
# check is visible after the fact, not only prevented before it.
#
# JSON string escaping is minimal by design: backslash and double quote, the
# two characters that break a JSON string literal. Path, branch, and evidence
# strings come from git and fno-managed layout, not free text.

_wt_json_escape() {
    printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g'
}

# _wt_emit_removal_event <repo-root> <path> <caller> <claim> <reason> [branch] [forced] [reclaimed-bytes]
#
# Best-effort but LOUD: a failed emit never blocks the removal. Prefer the
# explicit FNO_BIN, then the checkout build, then the deployed front.
_wt_emit_removal_event() {
    local root="$1" path="$2" caller="$3" claim="$4" reason="$5"
    local branch="${6:-}" forced="${7:-false}" reclaimed_bytes="${8:-0}"
    local data rc=1
    data="$(printf '{"path":"%s","caller":"%s","claim":"%s","reason":"%s","branch":"%s","forced":%s,"reclaimed_bytes":%s}' \
        "$(_wt_json_escape "$path")" "$(_wt_json_escape "$caller")" \
        "$(_wt_json_escape "$claim")" "$(_wt_json_escape "$reason")" \
        "$(_wt_json_escape "$branch")" "$forced" "$reclaimed_bytes")"
    local front=""
    if [[ -n "${FNO_BIN:-}" ]]; then
        front="$FNO_BIN"
    elif [[ -n "$root" && -x "$root/crates/fno/target/debug/fno" ]]; then
        front="$root/crates/fno/target/debug/fno"
    elif [[ -n "$root" && -x "$root/crates/fno/target/release/fno" ]]; then
        front="$root/crates/fno/target/release/fno"
    elif command -v fno >/dev/null 2>&1; then
        front="$(command -v fno)"
    fi
    if [[ -n "$front" ]]; then
        "$front" doctor event emit -t worktree_removed -s bash -d "$data" >/dev/null 2>&1 && rc=0
        if [[ -n "${FNO_AGENTS_HOME:-}" ]]; then
            local agents_events="$FNO_AGENTS_HOME/events.jsonl"
            if [[ "$agents_events" != "$root/.fno/events.jsonl" ]]; then
                "$front" doctor event emit -t worktree_removed -s bash -d "$data" \
                    --events "$agents_events" >/dev/null 2>&1 || true
            fi
        fi
    fi
    if [[ "$rc" -ne 0 ]]; then
        echo "worktree-removal-event: row NOT emitted for $path caller=$caller (no usable fno); removal proceeds, row lost" >&2
    fi
    return 0
}
