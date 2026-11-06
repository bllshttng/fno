#!/usr/bin/env bash
# A dead setup leaves git's "initializing" lock on a worktree. The lock makes
# `git worktree remove --force` refuse, so the cleanup sweep must read it, name
# it, and release a stale one; setup must release its own.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
FAILS=0
check() { if [[ "$2" == "$3" ]]; then echo "ok   $1"; else echo "FAIL $1: want '$3' got '$2'"; FAILS=$((FAILS + 1)); fi; }

REPO="$TMP/repo"
git init -q -b main "$REPO"
git -C "$REPO" -c user.name=t -c user.email=t@t commit -q --allow-empty -m init
git -C "$REPO" worktree add -q -b feature/x "$TMP/wt"
LOCK="$REPO/.git/worktrees/wt/locked"

# shellcheck source=../scripts/lib/worktree-init-lock.sh
source "$ROOT/scripts/lib/worktree-init-lock.sh"

check "unlocked tree reads free" "$(wt_lock_verdict "$TMP/wt")" free

echo initializing > "$LOCK"
check "young init lock reads fresh-init" "$(wt_lock_verdict "$TMP/wt")" fresh-init
wt_release_init_lock "$TMP/wt" 3600
check "young init lock survives the stale-bound release" "$([[ -f "$LOCK" ]] && echo kept)" kept

touch -t 202001010000 "$LOCK"
check "old init lock reads stale-init" "$(wt_lock_verdict "$TMP/wt")" stale-init
git -C "$REPO" worktree remove --force "$TMP/wt" 2>/dev/null
check "git refuses to remove a locked tree" "$([[ -d "$TMP/wt" ]] && echo present)" present
wt_release_init_lock "$TMP/wt" 3600
check "stale init lock releases" "$([[ -f "$LOCK" ]] && echo kept || echo gone)" gone
git -C "$REPO" worktree remove --force "$TMP/wt"
check "tree removes after release" "$([[ -d "$TMP/wt" ]] && echo present || echo gone)" gone

git -C "$REPO" worktree add -q -b feature/y "$TMP/wt2"
LOCK="$REPO/.git/worktrees/wt2/locked"
echo "held by a human" > "$LOCK"
touch -t 202001010000 "$LOCK"
check "foreign lock reads other" "$(wt_lock_verdict "$TMP/wt2")" other
wt_release_init_lock "$TMP/wt2" 0
check "foreign lock is never released" "$([[ -f "$LOCK" ]] && echo kept)" kept

exit "$FAILS"
