#!/usr/bin/env bash
# fno hook: PreInvocation - agy session-state producer shim
# agy runs hook commands as single program paths (no argument splitting), so
# the event name is baked into the script, not passed by the registration.
# One line of policy: exec the `fno-agents hook session-state` entry; the
# state map is data in harness_capabilities.toml ([harness.agy.hooks.
# session_state.wiring]). The entry self-bounds its RPC and tty writes at
# 2 s; the shim wraps the whole call in the shared helper like every
# daemon-shelling producer.
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
with_timeout 2 "$BIN" hook session-state --harness agy PreInvocation
exit 0
