#!/usr/bin/env bash
# fno hook: SessionStart - wheel move session start
# The session-start producer for the 0.4.7 upgrade path (x-be9f, law
# d-df1f42c1): when the resolved front door is a uv tool wheel install whose
# receipt came from an index, start postinstall.sh detached so the move to
# the binary install lands while the session runs. The move swaps an existing
# fno for the same release from the same publisher, so the consent ruling
# (2026-10-01) does not apply here; a machine with no fno keeps the consent
# notice (frontdoor-nudge-session-start.sh) untouched.
#
# Print contract: one line only when there is something to say - a move in
# flight (one line naming the log), a fresh failed move (one line naming the
# log and the manual command). A finished move, a binary install, a source
# install, or no fno prints NOTHING. Stdout becomes session context.
set -uo pipefail

# Survive a caller env with no usable PATH (see worktree-write-protect.sh).
PATH="${PATH:+$PATH:}/usr/bin:/bin:/usr/sbin:/sbin"
export PATH

HOOK_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$HOOK_DIR/../scripts/lib/with-timeout.sh" 2>/dev/null || exit 0
source "$HOOK_DIR/lib/fno-bin.sh" 2>/dev/null || exit 0

STATE="${FNO_STATE_DIR:-$HOME/.fno}"

# Resolve the front door beyond this session's PATH, then classify its
# install by the receipt beside the venv bin (release_install.rs's
# receipt_is_index, in shell form).
FNO_BIN="$(fno_bin)"
[[ -n "$FNO_BIN" ]] || exit 0

# A binary install is done; a finished move prints nothing.
if [[ "$(cat "$STATE/install/channel" 2>/dev/null)" == "binary" ]]; then
  exit 0
fi

# Classify the front door's install: canonicalize the resolved door and read
# the receipt beside its parent bin.
DOOR="$(cd "$(dirname "$FNO_BIN")" && pwd -P)/$(basename "$FNO_BIN")"
REAL="$(command -v readlink >/dev/null 2>&1 && readlink -f "$DOOR" 2>/dev/null || true)"
REAL="${REAL:-$DOOR}"
BINPARENT="$(cd "$(dirname "$REAL")" && pwd -P)"
ROOT="$(cd "$(dirname "$BINPARENT")" && pwd -P)"
RECEIPT="$ROOT/uv-receipt.toml"
[[ -f "$RECEIPT" ]] || exit 0

# An index receipt is one whose fno requirement row carries no local key
# (directory, path, git, url, editable): the same test release_install.rs's
# receipt_is_index applies.
REQ="$(grep -F 'name = "fno"' "$RECEIPT" | head -1)"
[[ -n "$REQ" ]] || exit 0
case "$REQ" in
  *directory*|*path*|*git*|*url*|*editable*) exit 0 ;;
esac

# A move already in flight owns the swap; this session says nothing.
LOCK="$STATE/install/first-run-move.lock"
if [[ -f "$LOCK" ]]; then
  LPID="$(head -1 "$LOCK" 2>/dev/null)"
  if [[ -n "$LPID" ]] && ps -p "$LPID" >/dev/null 2>&1; then
    exit 0
  fi
fi

# A fresh failed move names its repair once.
BACKOFF="$STATE/install/first-run-backoff"
if [[ -f "$BACKOFF" ]] && find "$BACKOFF" -mmin -60 >/dev/null 2>&1; then
  echo "fno: the wheel-to-binary move failed recently; see $STATE/install/wheel-move.log and run: bash $HOOK_DIR/../.claude-plugin/postinstall.sh"
  exit 0
fi

INSTALLER="$(cd "$HOOK_DIR/.." && pwd -P)/.claude-plugin/postinstall.sh"
[[ -f "$INSTALLER" ]] || exit 0

LOG="$STATE/install/wheel-move.log"
echo "fno: moving your wheel install to the binary install (log: $LOG)"
nohup bash "$INSTALLER" >"$LOG" 2>&1 &
disown 2>/dev/null || true
exit 0