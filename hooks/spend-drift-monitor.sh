#!/usr/bin/env bash
# fno hook: PostToolUse - launch the spend + model-drift monitor
# hooks/spend-drift-monitor.sh - find node, then run spend-drift-monitor.js.
#
# The monitor is the one hook that needs Node.js. A session started without a
# login shell can have no node on PATH: macOS skips path_helper there, and nvm
# loads only in an interactive shell. A bare `node` in the hook config then
# failed on every tool call ("/bin/sh: node: command not found"), and the
# spend cap and the drift check never ran.
#
# Caller dirs keep precedence; the usual node dirs supply the fallback.
PATH="${PATH:+$PATH:}/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin"
export PATH

here="$(cd "$(dirname "$0")" && pwd)"

if command -v node >/dev/null 2>&1; then
  exec node "$here/spend-drift-monitor.js"
fi

# No node anywhere. Say so once per session, then stay quiet: the same line
# after every tool call teaches nothing. The first exit is non-zero so the
# harness shows the line; it never blocks the tool call.
sid="$(sed -n 's/.*"session_id"[[:space:]]*:[[:space:]]*"\([A-Za-z0-9_-]*\)".*/\1/p' | head -n 1)"
marker="${TMPDIR:-/tmp}/fno-spend-drift-no-node-${sid:-unknown}"
if [ -e "$marker" ]; then
  exit 0
fi
: > "$marker" 2>/dev/null
echo "fno spend-drift monitor is off: node is not on PATH. The spend cap and the model-drift check do not run in this session. Install Node.js 18 or later, or add its directory to PATH, then restart the session." >&2
exit 1
