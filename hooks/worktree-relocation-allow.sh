#!/usr/bin/env bash
# fno hook: PermissionRequest - allow EnterWorktree under paths.worktrees_base
# Claude Code asks before EnterWorktree relocates a session outside
# <repo>/.claude/worktrees, and auto mode cannot approve that ask (policy:
# crates/fno-agents/src/hook/worktree_relocation_allow.rs). Never exec a
# candidate: an old build answers "unknown entry". Run each, deployed binary
# first; relay only exit 0. With no answer, print {} and the prompt stays.
stdin=$(cat)
errfile=$(mktemp -t wra-stderr.XXXXXX) || errfile=/dev/null
trap 'rm -f "$errfile"' EXIT
for bin in "$(command -v fno-agents 2>/dev/null)" "${FNO_AGENTS_BIN:-}" \
    "$PWD"/crates/fno-agents/target/{release,debug}/fno-agents; do
    [[ -n "$bin" && -x "$bin" ]] || continue
    if out="$(printf '%s' "$stdin" | "$bin" hook worktree-relocation-allow 2>"$errfile")"; then
        cat "$errfile" >&2
        printf '%s\n' "$out"
        exit 0
    fi
done
printf '%s\n' '{}'
