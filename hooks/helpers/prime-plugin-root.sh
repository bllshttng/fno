#!/usr/bin/env bash
# Prime ~/.fno/install/plugin-root with the caller's own plugin root.
#
# Persist the plugin root to ~/.fno/install/plugin-root so `fno do target init`
# and `fno gate set` can find their plugin scripts when run from a foreign
# project with no env hint. `fno` is a uv-tool install whose wheel does not
# carry hooks/, and CLAUDE_PLUGIN_ROOT is not propagated to arbitrary `fno`
# subprocesses - the pointer is then the only env-less source. The hook copy
# that runs can live in a linked worktree (a spawned worker's feature
# worktree), so a worktree root must never become the machine-global value:
# every env-less reader would run that unmerged branch, and the merge sweep
# would later leave the pointer dangling. A linked worktree has a .git FILE;
# a canonical checkout has a .git DIRECTORY; an installed stage has none.
# Read by fno.paths._read_persisted_plugin_root. errexit-safe.
#
# Every harness's session start calls this, not just the codex/gemini wrapper:
# a claude worker's init resolves through this pointer with no env hint, and a
# pointer only codex sessions ever prime serves it whatever copy codex last
# installed (measured 2026-10-30: a pre-4168 init copy refused a worker's own
# spawn handover). Idempotent: a pointer already naming $PLUGIN_ROOT is a
# no-op, so concurrent session starts converge instead of churning.
#
# Caller contract: PLUGIN_ROOT set to this hook's own plugin root. Best-effort
# throughout; a failed write never fails the session start.

prime_plugin_root_pointer() {
    [[ -f "$PLUGIN_ROOT/.claude-plugin/plugin.json" ]] || return 0
    [[ -f "$PLUGIN_ROOT/.git" ]] && return 0
    local home="${FNO_HOME:-$HOME/.fno}"
    local ptr="$home/install/plugin-root"
    if [[ -f "$ptr" ]] && [[ "$(cat "$ptr" 2>/dev/null)" == "$PLUGIN_ROOT" ]]; then
        return 0
    fi
    mkdir -p "$home/install" 2>/dev/null || return 0
    printf '%s\n' "$PLUGIN_ROOT" > "$ptr" 2>/dev/null || true
}
