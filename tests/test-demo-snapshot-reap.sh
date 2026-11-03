#!/usr/bin/env bash
# test-demo-snapshot-reap.sh - a demo shot never leaves a draw loop behind.
#
# Guards two teardown contracts of scripts/ops/mux-demo-snapshot.sh:
#   1. A run killed mid-shot by SIGTERM or SIGINT still tears down its draw
#      loops. bash chains the EXIT cleanup from an untrapped TERM or HUP, but
#      an untrapped INT kills the shell without it, so the INT trap is the
#      load-bearing one; both signals run here.
#   2. A stale /tmp/fno-demo.* root older than 1 h from a crashed earlier run
#      is reaped, with its processes, by the next run.
#
# The fno binary is a stub: it spawns the real draw shells for `mux pane run`,
# answers `backlog idea` with JSON, and parks the final `mux serve` on a
# stop-file, so no real server or cargo build is needed. The stale root lives
# in the real /tmp because the script's reap globs that path; the test cleans
# it up either way.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
TMP=$(mktemp -d -t demo-snapshot-reap.XXXXXX)
PASS=0
FAIL=0

ok()  { PASS=$((PASS + 1)); printf 'ok - %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf 'FAIL - %s\n' "$1"; }

draw_procs() { # draw_procs <needle> : live draw() processes whose argv names the needle
  ps -axww -o command= | grep -F 'draw()' | grep -Fc -- "$1" || true
}

export PANES_FILE="$TMP/pane-pids"
export STOP_FILE="$TMP/serve-stop"
export SERVE_PID_FILE="$TMP/serve-pid"
export IDEAS_FILE="$TMP/ideas"

STALE=""
STALE_PID=""
SCRIPT_PID=""
RUN_ROOT=""
cleanup_test() {
  [ -n "$STALE_PID" ] && kill "$STALE_PID" 2>/dev/null
  if [ -n "$SCRIPT_PID" ] && kill -0 "$SCRIPT_PID" 2>/dev/null; then
    kill -KILL -- "-$SCRIPT_PID" 2>/dev/null || true
  fi
  if [ -s "$SERVE_PID_FILE" ]; then
    kill "$(cat "$SERVE_PID_FILE")" 2>/dev/null
  fi
  touch "$STOP_FILE"
  [ -n "$RUN_ROOT" ] && rm -rf -- "$RUN_ROOT"
  [ -n "$STALE" ] && rm -rf -- "$STALE"
  rm -rf -- "$TMP"
}
trap cleanup_test EXIT

# The stub: real draw shells, no real server. The stub creates the graph db
# dir and its nodes table because the demo's registry seed connects to the
# store the real binary would have written. The final serve parks on the
# stop-file, modelling the live compose the signal lands in.
STUB="$TMP/stub-fno"
cat >"$STUB" <<'STUB'
#!/usr/bin/env bash
set -u
mkdir -p "${HOME:-/nonexistent}/.fno/db" 2>/dev/null || true
case "$1" in
  backlog)
    if [ "$2" = idea ]; then
      n=$(($(cat "$IDEAS_FILE" 2>/dev/null || echo 0) + 1))
      printf '%s' "$n" >"$IDEAS_FILE"
      python3 - "${HOME:-/nonexistent}/.fno/db/graph.db" <<'PY'
import sqlite3, sys
conn = sqlite3.connect(sys.argv[1])
conn.execute("CREATE TABLE IF NOT EXISTS nodes (id TEXT PRIMARY KEY)")
conn.commit()
PY
      printf '{"id": "demo-%06d"}' "$n"
    fi
    exit 0 ;;
  mux)
    case "$2" in
      pane)
        if [ "$3" = run ]; then
          sep=0; i=0
          for a in "$@"; do
            i=$((i + 1))
            [ "$a" = "--" ] && sep=$i
          done
          if [ "$sep" -gt 0 ]; then
            "${@:sep+1}" </dev/null >/dev/null 2>&1 &
            printf '%s\n' "$!" >>"$PANES_FILE"
            printf '{"pane_id": %s}' "$!"
          fi
        fi
        exit 0 ;;
      serve)
        # The warm-up compose writes warm.svg and returns; the final compose
        # carries the caller's own --out (if any) and parks.
        prev=""
        for a in "$@"; do
          if [ "$prev" = "--out" ] && [ "${a##*/}" = "warm.svg" ]; then
            exit 0
          fi
          prev="$a"
        done
        printf '%s\n' "$$" >"$SERVE_PID_FILE"
        # Park for up to 60 s: the test must catch the run mid-shot. The
        # stop-file or the cap ends it either way.
        for i in $(seq 1 300); do
          [ -e "$STOP_FILE" ] && break
          sleep 0.2
        done
        exit 0 ;;
      *) exit 0 ;;
    esac ;;
  *) exit 0 ;;
esac
STUB
chmod +x "$STUB"

# seed_stale : a root backdated 2 h holding one live loop whose argv carries
# the root path, exactly the shape a killed earlier run leaves behind.
seed_stale() {
  STALE=$(mktemp -d /tmp/fno-demo.reapstaleXXXXXX)
  python3 - "$STALE" <<'PY'
import os, sys, time
old = time.time() - 7200
os.utime(sys.argv[1], (old, old))
PY
  sh -c 'while :; do sleep 1; done' "$STALE/" >/dev/null 2>&1 &
  STALE_PID=$!
}

scenario() { # scenario <signal> : one full run killed mid-shot by <signal>
  local sig="$1"
  : >"$PANES_FILE"
  : >"$SERVE_PID_FILE"
  rm -f "$STOP_FILE"
  seed_stale

  # The run gets its own process group, so the signal can hit the script and
  # its serve child together, exactly what a terminal Ctrl-C or a cancelled
  # CI job does to the whole group.
  SCRIPT_PID=$(python3 - "$STUB" "$REPO_ROOT/scripts/ops/mux-demo-snapshot.sh" \
    "$TMP/run-$sig.log" "$TMP/out-$sig.svg" <<'PY'
import os, subprocess, sys
stub, script, logpath, outpath = sys.argv[1:5]
log = open(logpath, "w")
env = dict(os.environ, FNO_BIN=stub, FNO_DEMO_KEEP="1")
p = subprocess.Popen(["bash", script, "--out", outpath],
                     stdout=log, stderr=log, env=env, start_new_session=True)
print(p.pid)
PY
)

  # Wait for the parked serve, so the signal lands mid-run.
  local i
  for i in $(seq 1 300); do
    [ -s "$SERVE_PID_FILE" ] && break
    kill -0 "$SCRIPT_PID" 2>/dev/null || break
    sleep 0.2
  done

  if [ -s "$PANES_FILE" ]; then ok "$sig: draw shells spawned during the run"; else
    bad "$sig: no draw shell ever spawned; test is vacuous"
  fi
  local live
  live=$(draw_procs /tmp/fno-demo.)
  if [ "$live" -gt 0 ]; then ok "$sig: draw loops alive mid-run ($live)"; else
    bad "$sig: no live draw loop mid-run; test is vacuous"
  fi
  if kill -0 "$STALE_PID" 2>/dev/null; then
    bad "$sig: stale draw loop survived the startup reap"
  else
    ok "$sig: stale draw loop reaped at startup"
  fi
  if [ -d "$STALE" ]; then bad "$sig: stale root survived the startup reap"; else
    ok "$sig: stale root reaped at startup"
  fi

  # The kill: signal to the whole group, the same shape as a terminal
  # Ctrl-C or a cancelled CI job.
  kill -"$sig" -- "-$SCRIPT_PID" 2>/dev/null || true
  for i in $(seq 1 300); do
    kill -0 "$SCRIPT_PID" 2>/dev/null || break
    sleep 0.2
  done
  if kill -0 "$SCRIPT_PID" 2>/dev/null; then
    bad "$sig: script still alive 60 s after the signal"
    kill -KILL "$SCRIPT_PID" 2>/dev/null || true
  else
    ok "$sig: script exited after the signal"
  fi

  RUN_ROOT=$(sed -n 's/^kept //p' "$TMP/run-$sig.log")
  if [ -n "$RUN_ROOT" ] && [ -d "$RUN_ROOT" ]; then
    ok "$sig: cleanup ran on the signal death (kept root reported)"
  else
    bad "$sig: no kept-root line: cleanup never ran on the signal death"
  fi
  if [ -n "$RUN_ROOT" ]; then
    local left
    left=$(draw_procs "$RUN_ROOT")
    if [ "$left" -eq 0 ]; then ok "$sig: no draw loop outlives the kill"; else
      bad "$sig: $left draw loop(s) outlived the kill under $RUN_ROOT"
    fi
    if pgrep -f -- "$RUN_ROOT/" >/dev/null 2>&1; then
      bad "$sig: processes under the kept root survived the teardown"
    else
      ok "$sig: no process under the kept root survived"
    fi
    rm -rf -- "$RUN_ROOT"
  fi
  RUN_ROOT=""
  STALE=""
  STALE_PID=""
}

# The GNU-stat shim: a BSD-style `stat -f %m` probe on GNU coreutils exits 1
# while still printing the filesystem stanza of the later operand, which once
# killed the demo script's age math. Put the shim first on PATH so every
# scenario runs the reap through that failure shape on every platform.
mkdir -p "$TMP/bin"
cat >"$TMP/bin/stat" <<'SHIM'
#!/usr/bin/env bash
if [ "$1" = "-f" ] && [ "$2" = "%m" ]; then
  printf '  File: %s\n' "$3" 2>/dev/null
  exit 1
fi
exec /usr/bin/stat "$@"
SHIM
chmod +x "$TMP/bin/stat"
export PATH="$TMP/bin:$PATH"

scenario TERM
scenario INT

printf '\n%d passed, %d failed\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ]
