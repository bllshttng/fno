#!/usr/bin/env bash
# fno hook: Stop - agy session-state producer shim
# The Stop twin of agy-session-state-pre-invocation.sh: agy runs hook
# commands as single program paths (no argument splitting), so the event
# name is baked into the script. Registered BESIDE the target stop adapter,
# never inside it: the loop-check contract and this telemetry push fail
# independently. agy ignores this handler's stdout (the decision contract
# stays with the stop adapter), so the fire-and-forget entry's silence is
# the safe output.
set -u
PATH="${PATH:+$PATH:}/usr/bin:/bin"
export PATH
HOOK_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Fail closed like the producer it replaces: a missing helper makes this
# fire-and-forget hook exit 0 instead of blocking unbounded.
source "$HOOK_DIR/../scripts/lib/with-timeout.sh" 2>/dev/null || exit 0
# shellcheck source=lib/agents-bin.sh
source "$HOOK_DIR/lib/agents-bin.sh" 2>/dev/null || exit 0
REPO_ROOT="$(git -C "$PWD" rev-parse --show-toplevel 2>/dev/null || echo "$PWD")"
BIN="$(fno_agents_bin "$REPO_ROOT")"
# No binary -> nothing to report to; stay silent (the inside leg is best-effort).
[[ -z "$BIN" ]] && exit 0
with_timeout 2 "$BIN" hook session-state --harness agy Stop
exit 0
