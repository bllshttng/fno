#!/usr/bin/env bash
# tests/no-python-qualification.sh
#
# The product ships no Python. Runs everyday fno commands under strace and
# fails if any process in the tree tries to exec python or uv. A failed
# attempt (ENOENT on a PATH probe) counts too: it is still a Python leg.
#
# Usage: bash tests/no-python-qualification.sh [FNO_BIN] [FNO_AGENTS_BIN]
# Defaults: the debug builds under crates/*/target.
# Linux only (strace); elsewhere it prints a skip and exits 0.

set -uo pipefail

if [[ "$(uname -s)" != Linux ]]; then
  echo "no-python-qualification: skip (strace needs Linux; this is $(uname -s))"
  exit 0
fi
command -v strace >/dev/null || { echo "no-python-qualification: strace is not installed" >&2; exit 2; }

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
FNO_BIN="${1:-$ROOT/crates/fno/target/debug/fno}"
FNO_AGENTS_BIN="${2:-$ROOT/crates/fno-agents/target/debug/fno-agents}"
for b in "$FNO_BIN" "$FNO_AGENTS_BIN"; do
  [[ -x "$b" ]] || { echo "no-python-qualification: no binary at $b; build it first" >&2; exit 2; }
done

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
# A fresh HOME and state root: nothing ambient may answer for the binaries.
mkdir -p "$TMP/home" "$TMP/state" "$TMP/bin" "$TMP/repo"
ln -s "$FNO_BIN" "$TMP/bin/fno"
ln -s "$FNO_AGENTS_BIN" "$TMP/bin/fno-agents"
git -C "$TMP/repo" init -q

export HOME="$TMP/home" FNO_STATE_DIR="$TMP/state" FNO_AGENTS_BIN="$FNO_AGENTS_BIN"
export PATH="$TMP/bin:$PATH"

FAIL=0
run() {
  local label="$*" log="$TMP/trace.log" hits
  ( cd "$TMP/repo" && strace -f -qq -e trace=execve -o "$log" fno "$@" >/dev/null 2>&1 )
  # The exec'd path is execve's first argument; match its basename.
  hits="$(grep -E 'execve\("[^"]*/(python[0-9.]*|uv|uvx)"' "$log" || true)"
  if [[ -n "$hits" ]]; then
    FAIL=1
    printf 'FAIL: fno %s execs Python or uv:\n%s\n' "$label" "$hits"
  else
    printf 'ok: fno %s\n' "$label"
  fi
}

run --version
run --help
run version --json
run config get paths
run backlog list --json
run doctor --json

[[ "$FAIL" -eq 0 ]] && echo "no-python-qualification: ok (no python or uv exec in any command)"
exit "$FAIL"
