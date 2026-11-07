#!/usr/bin/env bash
# fno hook: SessionStart - wheel move session start
# The session-start producer for the 0.4.7 upgrade path (the wheel-removal
# migration law): when the resolved front door is a uv tool wheel install whose
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

# --- change 6: the daily install count and the floor line ----------------
# Channel for the count: binary (the marker), wheel (an index receipt
# beside the venv bin), other. Cheap reads only.
CHANNEL_KIND="other"
if [[ "$(cat "$STATE/install/channel" 2>/dev/null)" == "binary" ]]; then
  CHANNEL_KIND="binary"
fi
COUNT_STAMP="$STATE/install/fno_install_seen.stamp"
count_once_per_day() {
  find "$COUNT_STAMP" -mmin -1440 >/dev/null 2>&1 && return 0
  [[ "$("$FNO_BIN" config get telemetry.install_count.enabled 2>/dev/null)" == "false" ]] && return 0
  local token
  token="$(cat "$STATE/secrets/posthog-project-token" 2>/dev/null)"
  [[ -n "$token" ]] || token="${FNO_POSTHOG_PROJECT_TOKEN:-}"
  [[ -n "$token" ]] || return 0
  local id_file="$STATE/install/fno_install_seen.id"
  touch "$id_file" 2>/dev/null || return 0
  local id
  id="$(cat "$id_file" 2>/dev/null)"
  if [[ -z "$id" ]]; then
    id="$(head -c 16 /dev/urandom | od -An -tx1 | tr -d ' \n')"
    printf '%s\n' "$id" > "$id_file"
  fi
  local os arch
  os="$(uname -s)"; arch="$(uname -m)"
  local ver
  ver="$("$FNO_BIN" --version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+\.[0-9]+[^ ]*' | head -1)"
  local body
  body="$(printf '{"api_key": "%s", "batch": [{"event": "fno_install_seen", "distinct_id": "install:%s", "properties": {"fno_version": "%s", "channel": "%s", "os": "%s", "arch": "%s", "install_id": "%s"}}]}' \
    "$token" "$id" "$ver" "$CHANNEL_KIND" "$os" "$arch" "$id")"
  curl -fsSL --max-time 10 -H 'Content-Type: application/json' \
    -d "$body" "https://us.i.posthog.com/batch/" >/dev/null 2>&1 &&
    touch "$COUNT_STAMP"
}

# The floor line (change 6): the hook reads fno-release.json for an old CLI
# that cannot read it (pre-floor binaries). Best effort, one line, only
# when the installed version is below min_supported.
floor_line() {
  # The Rust floor cache (release_install.rs write_floor_cache) carries the
  # same fields with a fetched stamp; fresh within a day it answers offline
  # and SessionStart never waits on the network.
  local meta=""
  local cache="$STATE/install/fno-release-cache.json"
  if [[ -f "$cache" ]]; then
    local fetched now
    fetched="$(grep -o '"fetched":[0-9]*' "$cache" | head -1 | cut -d: -f2)"
    now="$(date +%s)"
    if [[ -n "$fetched" ]] && (( now - fetched < 86400 )); then
      meta="$(cat "$cache")"
    fi
  fi
  if [[ -z "$meta" ]]; then
    local url="https://github.com/${FNO_RELEASE_REPO:-fnodotdev/fno}/releases/latest/download/fno-release.json"
    meta="$(curl -fsSL --max-time 10 "$url" 2>/dev/null)" || return 0
  fi
  local floor
  floor="$(printf '%s' "$meta" | grep -o '"min_supported"[[:space:]]*:[[:space:]]*"[^"]*"' | head -1 | sed 's/.*"\(.*\)"$/\1/' 2>/dev/null)"
  [[ -n "$floor" ]] || return 0
  local ver
  ver="$("$FNO_BIN" --version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+\.[0-9]+[^ ]*' | head -1)"
  [[ -n "$ver" ]] || return 0
  if [[ "$(printf '%s\n%s\n' "$ver" "$floor" | sort -V | head -1)" != "$ver" ]]; then
    echo "fno $ver is below the supported floor $floor. Run: fno update"
  fi
}

# A binary install is done; a finished move prints nothing.
if [[ "$(cat "$STATE/install/channel" 2>/dev/null)" == "binary" ]]; then
  count_once_per_day
  floor_line
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
REQ_IS_INDEX=1
case "$REQ" in
  *directory*|*path*|*git*|*url*|*editable*) REQ_IS_INDEX=0 ;;
esac

# The count and the floor line answer for every classified install; only
# the move below stays wheel-only.
if [[ "$REQ_IS_INDEX" == 1 ]]; then
  CHANNEL_KIND="wheel"
fi
count_once_per_day
floor_line
if [[ "$REQ_IS_INDEX" == 0 ]]; then
  exit 0
fi

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
