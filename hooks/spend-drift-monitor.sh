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
# Caller dirs keep precedence; the usual node dirs supply the fallback. The
# version-manager dirs are the ones that hold a runnable node with no shell
# setup: volta, asdf and mise shims, and the fnm default alias.
PATH="${PATH:+$PATH:}/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin"
PATH="$PATH:$HOME/.volta/bin:$HOME/.asdf/shims:$HOME/.local/share/mise/shims:$HOME/.local/share/fnm/aliases/default/bin"
export PATH

here="$(cd "$(dirname "$0")" && pwd)"

if command -v node >/dev/null 2>&1; then
  exec node "$here/spend-drift-monitor.js"
fi

# nvm keeps no shim: each version has its own dir, and only a sourced nvm.sh
# puts one on PATH. Take the newest installed version.
nvm_node="$(printf '%s\n' "${NVM_DIR:-$HOME/.nvm}"/versions/node/*/bin/node | sort -V | tail -n 1)"
if [ -x "$nvm_node" ]; then
  exec "$nvm_node" "$here/spend-drift-monitor.js"
fi

# No node anywhere. Say so once per session, then stay quiet: the same line
# after every tool call teaches nothing. The first exit is non-zero so the
# harness shows the line; it never blocks the tool call.
sid="$(sed -n 's/.*"session_id"[[:space:]]*:[[:space:]]*"\([A-Za-z0-9_-]*\)".*/\1/p' | head -n 1)"
# A payload with no session id has no session to key on. It keys on the day
# instead, so one such session cannot silence the notice for every later one.
latch_dir="${LATCHES_DIR:-${STATE_DIR:-$HOME/.fno}/latches}"
marker="${latch_dir}/.spend-drift-no-node-${sid:-day-$(date +%Y%m%d)}"
if [ -e "$marker" ]; then
  exit 0
fi
mkdir -p "$latch_dir" 2>/dev/null || true
find "$latch_dir" -maxdepth 1 -type f -name '.spend-drift-no-node-*' -mtime +2 -delete 2>/dev/null || true
{ : > "$marker"; } 2>/dev/null
echo "fno spend-drift monitor is off: node is not on PATH. The spend cap and the model-drift check do not run in this session. Install Node.js 18 or later, or add its directory to PATH, then restart the session." >&2
exit 1
