#!/usr/bin/env bash
# fno hook: SessionStart - daemon liveness
# The second x-322e gate: with the fno-agents daemon down, nothing wakes a
# lead on a daemon beat. The daemon lazy-starts on the next daemon-routed
# command, so the gap is exactly the quiescent machine: no session, no tick,
# no lead wake until someone types. SessionStart is the one surface that
# fires anyway, so it names the down state.
#
# The probe is a raw socket connect with the client's own down predicate
# (missing or refused reads down; anything else says nothing), not
# `fno-agents status`: that verb also scans the tick journals and costs
# ~1.9s, over the session-start hook budget. The connect never lazy-starts
# a daemon, no-op aside from a socket connect, so the line is evidence,
# not a self-heal.
set -uo pipefail

# Survive a caller env with no usable PATH (see worktree-write-protect.sh).
PATH="${PATH:+$PATH:}/usr/bin:/bin:/usr/sbin:/sbin"
export PATH

HOOK_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HOOK_DIR/.." && pwd)"
# shellcheck source=lib/agents-bin.sh
source "$HOOK_DIR/lib/agents-bin.sh"
BIN="$(fno_agents_bin "$ROOT")"
[[ -z "$BIN" ]] && exit 0

SOCK="${FNO_AGENTS_HOME:-${HOME:-}/.fno/agents}/supervisor.sock"
DOWN="$(python3 - "$SOCK" <<'PYEOF'
import socket
import sys

path = sys.argv[1]
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.settimeout(1)
try:
    s.connect(path)
except (FileNotFoundError, ConnectionRefusedError):
    print("down")
except OSError:
    pass
finally:
    s.close()
PYEOF
)"

[[ "$DOWN" == "down" ]] || exit 0

printf '{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"[fno] the fno-agents daemon is down: lead-wake, watchdog and daemon beats are not firing. Run: fno-agents restart"}}\n'
exit 0
