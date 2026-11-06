#!/usr/bin/env bash
# The container proof. From a clean 0.4.5 wheel install in
# ubuntu:24.04, prove both upgrade paths land the candidate binary.
#
# Leg A (one fno update): 0.4.5's `fno update` runs `uv tool upgrade fno`;
#   the candidate must win that resolution (find-links or PyPI rc), and the
#   first call after it performs the first-run move.
# Leg B (one plugin update): the candidate plugin tree's wheel-move
#   session-start producer starts postinstall detached; the detached run
#   fetches the rc tarball and adopts the binary.
#
# Usage: bash tests/release/wheel-upgrade-path.sh --candidate 0.4.6rc1 \
#          [--repo fnodotdev/fno] [--wheel-dir <dir with candidate wheels>]
# Writes .fno/tmp/wheel-upgrade-path/<run>/result.json with legs_on_binary.
set -euo pipefail
cd "$(git rev-parse --show-toplevel 2>/dev/null || pwd -P)"

CANDIDATE=""
REPO="${FNO_RELEASE_REPO:-fnodotdev/fno}"
WHEEL_DIR=""
OUT_DIR=".fno/tmp/wheel-upgrade-path/$(date +%s)"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --candidate) CANDIDATE="${2:?--candidate needs a version}"; shift 2 ;;
    --repo) REPO="${2:?--repo needs owner/name}"; shift 2 ;;
    --wheel-dir) WHEEL_DIR="$(cd "$2" && pwd -P)"; shift 2 ;;
    --out) OUT_DIR="$2"; shift 2 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done
[[ -n "$CANDIDATE" ]] || { echo "usage: $0 --candidate <version> [--repo owner/name] [--wheel-dir dir]" >&2; exit 2; }
command -v docker >/dev/null 2>&1 || { echo "docker is required"; exit 1; }
mkdir -p "$OUT_DIR"

# One leg per container, same setup: uv + a clean 0.4.5 wheel install, and
# for leg B the candidate plugin tree mounted at /repo.
run_leg() { # run_leg <letter> <extra docker args...>
  local letter="$1"; shift
  local setup leg
  setup="$(mktemp "${TMPDIR:-/tmp}/fno-leg-setup.XXXXXX")"
  cat >"$setup" <<'SETUP'
set -euo pipefail
apt-get update -qq && apt-get install -y -qq curl ca-certificates >/dev/null
curl -fsSL https://astral.sh/uv/install.sh | sh
export PATH="$HOME/.local/bin:$PATH"
uv tool install fno==0.4.5
# A == pin refuses `uv tool upgrade`; the real 0.4.5 stable install predates
# the pin (postinstall ran the by-name install), so drop the pin from the
# receipt to reproduce that unpinned receipt exactly.
sed -i '/specifier/d' "$(uv tool dir)/fno/uv-receipt.toml"
SETUP

  leg="$setup.leg"
  if [[ "$letter" == "A" ]]; then
    cat >"$leg" <<'LEGA'
set -euo pipefail
apt-get update -qq && apt-get install -y -qq jq >/dev/null
export PATH="$HOME/.local/bin:$PATH"
# Leg A: 0.4.5's own fno update runs uv tool upgrade fno; the candidate must
# win the resolution. UV_PRERELEASE=allow lets an rc win over PyPI stable.
export UV_PRERELEASE=allow
fno update
# The first ordinary call after the update performs the move and execs the
# binary front door. FNO_PY unset: the moved install keeps the tool only when
# a forwarded command still needs fno-py (the candidate wheel ships it).
"$HOME/.local/bin/fno" --version
VER="$("$HOME/.local/bin/fno" --version | grep -oE '[0-9]+\.[0-9]+\.[0-9]+[^ ]*' | head -1)"
[[ "$VER" == "$CANDIDATE" ]] || { echo "leg A: fno --version printed $VER, want $CANDIDATE"; exit 1; }
TGT="$(readlink -f "$HOME/.local/bin/fno")"
[[ "$TGT" == "$HOME/.fno/install/bin/"* ]] || { echo "leg A: $TGT is not under the binary install"; exit 1; }
[[ "$(cat "$HOME/.fno/install/channel")" == "binary" ]] || { echo "leg A: channel marker is not binary"; exit 1; }
echo '{"leg_a": 1}' > /host/A.result
LEGA
  fi

  if [[ "$letter" == "B" ]]; then
    cat >"$leg" <<'LEGB'
set -euo pipefail
apt-get update -qq && apt-get install -y -qq jq >/dev/null
export PATH="$HOME/.local/bin:$PATH"
# Leg B: the candidate plugin tree's wheel-move producer runs as a plugin
# update's session start would. The detached postinstall fetches the rc
# tarball and adopts; the loop waits for the marker.
CLAUDE_PLUGIN_ROOT=/repo
export CLAUDE_PLUGIN_ROOT
bash /repo/hooks/wheel-move-session-start.sh
for i in $(seq 1 120); do
  [[ "$(cat "$HOME/.fno/install/channel" 2>/dev/null)" == "binary" ]] && break
  sleep 1
done
[[ "$(cat "$HOME/.fno/install/channel" 2>/dev/null)" == "binary" ]] || { echo "leg B: the detached move never landed the marker"; exit 1; }
TGT="$(readlink -f "$HOME/.local/bin/fno")"
[[ "$TGT" == "$HOME/.fno/install/bin/"* ]] || { echo "leg B: $TGT is not under the binary install"; exit 1; }
"$HOME/.local/bin/fno" --version >/dev/null
VER="$("$HOME/.local/bin/fno" --version | grep -oE '[0-9]+\.[0-9]+\.[0-9]+[^ ]*' | head -1)"
[[ "$VER" == "$CANDIDATE" ]] || { echo "leg B: fno --version printed $VER, want $CANDIDATE"; exit 1; }
echo '{"leg_b": 1}' > /host/B.result
LEGB
  fi

  # Mount a scratch dir at /host for the leg result, plus the repo for leg B.
  local -a mounts=("-v" "$OUT_DIR:/host")
  local -a envs=("-e" "CANDIDATE=$CANDIDATE")
  if [[ -n "$WHEEL_DIR" ]]; then
    mounts+=("-v" "$WHEEL_DIR:/wheels:ro")
    envs+=("-e" "UV_FIND_LINKS=/wheels")
  else
    envs+=("-e" "FNO_RELEASE_REPO=$REPO")
  fi
  if [[ "$letter" == "B" ]]; then
    mounts+=("-v" "$(pwd -P):/repo:ro")
  fi
  cp "$setup" "$OUT_DIR/$letter.setup"
  cp "$leg" "$OUT_DIR/$letter.leg"
  docker run --rm "${envs[@]}" "${mounts[@]}" ubuntu:24.04 \
    bash -c "bash /host/$letter.setup && bash /host/$letter.leg"
  rm -f "$setup" "$leg"
}

run_leg A
run_leg B

# Each container starts with an empty /host and writes its own verdict file,
# so the run folds the two into result.json.
A="$([[ -f "$OUT_DIR/A.result" ]] && echo 1 || echo 0)"
B="$([[ -f "$OUT_DIR/B.result" ]] && echo 1 || echo 0)"
printf '{"legs_on_binary": %d}\n' "$((A + B))" > "$OUT_DIR/result.json"
cat "$OUT_DIR/result.json"
[[ "$((A + B))" -eq 2 ]] || { echo "wheel-upgrade-path: FAILED (legs_on_binary=$((A+B)))"; exit 1; }
echo "wheel-upgrade-path: both legs landed on the binary"
