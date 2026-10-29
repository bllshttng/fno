#!/usr/bin/env bash
# Resolve an interpreter for hook scripts: python3, python, then the
# interpreter uv manages (fno installs through uv, so a uv python exists
# where system python3 does not - 2026-10-29 clean-box install). Hooks never
# block: with no interpreter at all, emit {} and exit 0.
set -uo pipefail

# The appended fallback keeps the resolver alive under a caller env with no
# usable PATH (the worktree-write-protect pattern). FNO_HOOK_PY_FALLBACK_PATH
# replaces the fallback dirs for the hook test suite, which simulates a box
# with no system python by pointing PATH at a directory holding only a fake
# uv. Never set it outside tests.
PATH="${PATH:+$PATH:}${FNO_HOOK_PY_FALLBACK_PATH:-/usr/bin:/bin:/usr/sbin:/sbin}"
export PATH

for py in python3 python; do
    command -v "$py" >/dev/null 2>&1 && exec "$py" "$@"
done
if command -v uv >/dev/null 2>&1; then
    py="$(uv python find 2>/dev/null)" && [[ -n "$py" && -x "$py" ]] && exec "$py" "$@"
fi
echo '{}'
exit 0
