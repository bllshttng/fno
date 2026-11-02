#!/usr/bin/env bash
# graph-resolve.sh -- resolve "ab-xxxxxxxx" graph IDs to plan_path.
#
# Source this file, then call resolve_arg with any user-supplied argument.
# Behaviors:
#   - Full ab-XXXXXXXX  -> exact match via `fno backlog get`; echoes plan_path
#       or soft-fails.
#   - Partial ab-XXXX..XXXXXXX (4-7 hex chars) -> prefix match through the
#       same command; echoes plan_path on a unique resolution, soft-fails (with
#       stderr) on ambiguity / no match.
#   - RESOLVE_FUZZY=1 + non-ab input -> title fuzzy match through the command.
#       Off by default because /target etc. pass raw feature descriptions
#       that must NOT be collapsed onto an existing graph node.
#   - Anything else -> echoes arg unchanged.
#
# Usage:
#   source scripts/lib/graph-resolve.sh
#   arg=$(resolve_arg "$1")
#
# Env:
#   RESOLVE_STRICT=1  exit nonzero on unknown / ambiguous queries
#   RESOLVE_FUZZY=1   opt into title fuzzy match for non-ab queries
#
# Design notes:
# - `fno backlog get` is the resolution seam: it owns the id/slug/bare-hex/
#   fuzzy tiers and the tracker-backend switch, so this shim never opens the
#   graph store itself. Store path overrides ride fno's own path config.
# - Soft fail by default. Downstream skills then try the echoed value as a
#   file path, which fails with a clearer error than a bash function dying
#   silently. RESOLVE_STRICT=1 opts into hard fail.

# Single fno-vs-external-vs-none classifier, shared with parse-claims-arg.sh so
# the id-shape test has one home. Sourced as a bundled sibling (BASH_SOURCE
# resolves beside this file at both the repo-root and skills/ locations).
source "$(dirname "${BASH_SOURCE[0]}")/node-id.sh"

resolve_arg() {
    local arg="$1"
    local kind
    kind="$(node_id_kind "$arg")"
    # External ids are opaque work handles, never graph-resolvable. Not even
    # under RESOLVE_FUZZY, which is for title matching rather than id matching.
    if [[ "$kind" == "external" ]]; then
        echo "$arg"
        return 0
    fi
    # Non-ids pass through unless the caller opted into title fuzzy matching.
    # Most /target callers pass raw feature descriptions that we must not
    # collapse to a graph node.
    if [[ "$kind" == "none" ]] && [[ "${RESOLVE_FUZZY:-0}" != "1" ]]; then
        echo "$arg"
        return 0
    fi
    if ! command -v "${FNO_AGENTS_FRONT:-fno}" >/dev/null 2>&1; then
        echo "[graph-resolve] fno CLI unavailable; using '$arg' as-is" >&2
        [[ "${RESOLVE_STRICT:-}" == "1" ]] && return 1
        echo "$arg"
        return 0
    fi
    # A GRAPH_JSON override is a sandbox contract (the shim's own tests, and
    # callers pinning a scratch graph): resolve against that file. Otherwise
    # resolve against the ambient store, backend-switched like every other
    # reader. Exit contract:
    #   0 plan_path | 1 no match | 3 no plan_path | 4 ambiguous
    #   6 external tracker backend -> pass the arg through unchanged
    local sandbox_result rc
    local door=("${FNO_AGENTS_FRONT:-fno}" backlog script-door resolve-plan "$arg")
    [[ -n "${GRAPH_JSON:-}" ]] && door+=(--graph "$GRAPH_JSON")
    sandbox_result=$("${door[@]}" 2>/dev/null)
    rc=$?
    [[ $rc -ne 0 ]] && sandbox_result=""
    if [[ $rc -eq 0 && -n "$sandbox_result" ]]; then
        echo "$sandbox_result"
        return 0
    fi
    if [[ $rc -eq 6 ]]; then
        echo "$arg"
        return 0
    fi
    if [[ $rc -eq 1 ]]; then
        echo "[graph-resolve] no match for '$arg'" >&2
    elif [[ $rc -eq 3 ]]; then
        echo "[graph-resolve] node '$arg' has no plan_path" >&2
    elif [[ $rc -eq 4 ]]; then
        echo "[graph-resolve] ambiguous '$arg'" >&2
    else
        echo "[graph-resolve] lookup failed (rc=$rc) for '$arg'" >&2
    fi
    [[ "${RESOLVE_STRICT:-}" == "1" ]] && return 1
    echo "$arg"
    return 0
}
