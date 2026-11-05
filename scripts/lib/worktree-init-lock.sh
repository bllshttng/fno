#!/usr/bin/env bash
# The "initializing" lock git puts on a linked worktree while `git worktree add`
# builds it. Git removes the lock when the add finishes. An add that dies
# midway (a timeout kill, a crash, a failed hook) leaves it, and a locked
# worktree refuses `git worktree remove`, even with --force.
#
# Sourced by worktree setup (release on every exit) and by every removal path
# (release a stale one, then remove). A lock with any other reason is a human's
# or a tool's on purpose: these helpers never touch it.

# Seconds after which an "initializing" lock is no setup in flight but a leftover.
WT_INIT_LOCK_STALE_SECS="${WT_INIT_LOCK_STALE_SECS:-3600}"

# The linked worktree's admin dir, empty when $1 is not a linked worktree.
_wt_admin_dir() {
    local gd
    gd="$(git -C "${1:-}" rev-parse --absolute-git-dir 2>/dev/null)" || return 1
    case "$gd" in
        */worktrees/*) printf '%s\n' "$gd" ;;
        *) return 1 ;;
    esac
}

# Print the lock reason and return 0 when the worktree is locked; return 1 when
# it is not locked or the state cannot be read.
wt_lock_reason() {
    local admin
    admin="$(_wt_admin_dir "${1:-}")" || return 1
    [[ -f "$admin/locked" ]] || return 1
    local reason
    reason="$(cat "$admin/locked" 2>/dev/null)"
    printf '%s\n' "${reason:-locked}"
}

# Age of the lock file in seconds. Prints nothing when it cannot be read.
_wt_lock_age_secs() {
    local admin mtime now
    admin="$(_wt_admin_dir "${1:-}")" || return 1
    # GNU form first: BSD stat rejects -c, but GNU stat reads -f as filesystem
    # status and can exit 0 with junk for %m.
    mtime="$(stat -c %Y "$admin/locked" 2>/dev/null || stat -f %m "$admin/locked" 2>/dev/null)" || return 1
    [[ "$mtime" =~ ^[0-9]+$ ]] || return 1
    now="$(date +%s)"
    printf '%s\n' "$((now - mtime))"
}

# wt_lock_verdict <wt>: one word on stdout.
#   free         not locked
#   stale-init   "initializing" older than WT_INIT_LOCK_STALE_SECS: safe to release
#   fresh-init   "initializing" younger than that: a setup may still be running
#   other        locked for another reason (printed by wt_lock_reason)
wt_lock_verdict() {
    local reason age
    reason="$(wt_lock_reason "${1:-}")" || { echo free; return 0; }
    if [[ "$reason" != "initializing" ]]; then
        echo other
        return 0
    fi
    age="$(_wt_lock_age_secs "$1")" || { echo fresh-init; return 0; }
    if [[ "$age" -ge "$WT_INIT_LOCK_STALE_SECS" ]]; then
        echo stale-init
    else
        echo fresh-init
    fi
}

# wt_release_init_lock <wt> [min_age_secs]: unlock an "initializing" lock at
# least min_age_secs old (default 0: setup releasing its own lock). Returns 0
# when the tree is unlocked afterwards or was never locked by init, 1 when the
# lock stays.
wt_release_init_lock() {
    local wt="${1:-}" min_age="${2:-0}" reason age
    reason="$(wt_lock_reason "$wt")" || return 0
    [[ "$reason" == "initializing" ]] || return 1
    if [[ "$min_age" -gt 0 ]]; then
        age="$(_wt_lock_age_secs "$wt")" || return 1
        [[ "$age" -ge "$min_age" ]] || return 1
    fi
    git -C "$wt" worktree unlock "$wt" >/dev/null 2>&1
}
