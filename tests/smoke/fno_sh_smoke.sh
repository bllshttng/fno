#!/usr/bin/env bash
# Clean-machine smoke for the fno.sh curl-one-liner channel.
#
# Given a release tarball (fno-agents-<platform>.tar.gz) and its .sha256
# beside it, run the REPO COPY of the installer against it (FNO_INSTALL_TARBALL)
# and prove:
#
#   1. `sh scripts/install/fno.sh` installs and reports success,
#   2. the four binaries land in ~/.fno/install/bin and are linked into the
#      PATH dir,
#   3. `fno --version`, `fno mux ls` and `fno-agents --version` answer,
#   4. no uv and no Python CLI is involved,
#   5. a tampered .sha256 refuses, names both digests, and installs nothing,
#   6. a second run is a clean no-op,
#   7. the PATH hint names the link dir.
#
# The whole run is isolated into a temp HOME and runs from a cwd outside any
# git repo. Per-check pass/fail; exits non-zero on any miss.
#
# Usage: fno_sh_smoke.sh <path-to-fno-agents-<platform>.tar.gz>
set -uo pipefail

TARBALL="${1:?usage: fno_sh_smoke.sh <fno-agents-<platform>.tar.gz>}"
TARBALL="$(cd "$(dirname "$TARBALL")" && pwd)/$(basename "$TARBALL")"
[ -f "$TARBALL" ] || { echo "FAIL[input] tarball not found: $TARBALL"; exit 1; }
[ -f "$TARBALL.sha256" ] || { echo "FAIL[input] no checksum beside it: $TARBALL.sha256"; exit 1; }

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
INSTALLER="$REPO_ROOT/scripts/install/fno.sh"
[ -f "$INSTALLER" ] || { echo "FAIL[input] installer not found at $INSTALLER"; exit 1; }

fail=0
pass() { printf 'PASS[%s] %s\n' "$1" "$2"; }
miss() { printf 'FAIL[%s] %s\n' "$1" "$2"; fail=1; }

BASE_TMP="$(mktemp -d)"
BASE_TMP="$(cd "$BASE_TMP" && pwd -P)"
trap 'rm -rf "$BASE_TMP"' EXIT
WORK="$BASE_TMP/work"
mkdir -p "$WORK" "$BASE_TMP/home"
cd "$WORK" || { echo "FAIL[env] cd to $WORK failed"; exit 1; }

export HOME="$BASE_TMP/home" SHELL=/bin/sh FNO_NO_WIRE=1
unset FNO_STATE_DIR FNO_INSTALL_DIR
BIN="$HOME/.fno/install/bin"
LINKS="$HOME/.local/bin"

run_capture() { OUT="$("$@" 2>&1)"; RC=$?; }

# --- check 5 first, on a clean HOME: a tampered sum installs nothing ---
cp "$TARBALL" "$BASE_TMP/bad.tar.gz"
printf '%064d  bad.tar.gz\n' 0 > "$BASE_TMP/bad.tar.gz.sha256"
run_capture env FNO_INSTALL_TARBALL="$BASE_TMP/bad.tar.gz" sh "$INSTALLER"
if [ "$RC" -ne 0 ] && printf '%s' "$OUT" | grep -q 'sha256 mismatch' && [ ! -e "$BIN/fno" ]; then
  pass "sha-refusal" "a tampered .sha256 refused (rc=$RC) and installed nothing"
else
  miss "sha-refusal" "rc=$RC bin present=$([ -e "$BIN/fno" ] && echo yes || echo no) out: $(printf '%s' "$OUT" | tail -1)"
fi

# --- check 1: first-run install succeeds ---
run_capture env FNO_INSTALL_TARBALL="$TARBALL" sh "$INSTALLER"
INSTALL_OUT="$OUT"
if [ "$RC" -eq 0 ]; then
  pass "install" "fno.sh installed the tarball (rc=0)"
else
  miss "install" "rc=$RC out: $(printf '%s' "$OUT" | tail -2)"
fi

# --- check 2: the four binaries in the install dir, linked into the PATH dir ---
for b in fno fno-agents fno-agents-daemon fno-agents-worker; do
  if [ -x "$BIN/$b" ] && [ -x "$LINKS/$b" ]; then pass "binary:$b" "in $BIN and linked into $LINKS"
  else miss "binary:$b" "absent from $BIN or $LINKS"; fi
done

# --- check 3: the front door and fno-agents answer ---
run_capture "$LINKS/fno" --version
if [ "$RC" -eq 0 ] && printf '%s' "$OUT" | grep -qE 'fno[[:space:]]+[0-9]+\.[0-9]+'; then
  pass "version" "fno --version answers ($OUT)"
else
  miss "version" "rc=$RC out: $(printf '%s' "$OUT" | tail -1)"
fi
run_capture "$LINKS/fno" mux ls
if [ "$RC" -eq 0 ]; then pass "mux" "fno mux ls answers (rc=0)"
else miss "mux" "rc=$RC out: $(printf '%s' "$OUT" | tail -1)"; fi
run_capture "$LINKS/fno-agents" --version
if [ "$RC" -eq 0 ]; then pass "fno-agents" "fno-agents --version answers"
else miss "fno-agents" "rc=$RC out: $(printf '%s' "$OUT" | tail -1)"; fi

# --- check 4: no uv and no Python CLI ---
if ! compgen -G "$LINKS/*py*" >/dev/null && ! printf '%s' "$INSTALL_OUT" | grep -qi 'uv tool'; then
  pass "no-python" "no Python link and no uv step"
else
  miss "no-python" "a Python CLI or a uv step appeared"
fi

# --- check 7: the PATH hint names the link dir ---
if printf '%s' "$INSTALL_OUT" | grep -qF "$LINKS"; then
  pass "path-hint" "the PATH step names $LINKS"
else
  miss "path-hint" "no line names $LINKS"
fi

# --- check 6: re-run is a clean no-op ---
run_capture sh "$INSTALLER"
if [ "$RC" -eq 0 ] && printf '%s' "$OUT" | grep -qi 'already installed' \
   && ! printf '%s' "$OUT" | grep -qi 'fetching the release'; then
  pass "idempotent" "second run detected the install and fetched nothing (rc=0)"
else
  miss "idempotent" "rc=$RC out: $(printf '%s' "$OUT" | tail -2)"
fi

echo "---"
if [ "$fail" -ne 0 ]; then
  echo "--- installer output (install run):"
  printf '%s\n' "$INSTALL_OUT"
  echo "fno.sh smoke: FAILED"
  exit 1
fi
echo "fno.sh smoke: all checks passed"
