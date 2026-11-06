#!/usr/bin/env bash
# Post-install hook for the fno Claude Code plugin.
#
# Lands a complete `fno` (the front door plus the three fno-agents binaries)
# on PATH, routed by the plugin channel this tree came from (the version in
# plugin.json, which release.yml keeps distinct per channel):
#
#   every channel   1. fetch the release tarball for this plugin's version
#                      from GitHub Releases, verify its sha256, unpack to
#                      staging, run the staged front door's
#                      `fno update --adopt`. The Rust side owns the proof,
#                      swap, links, marker and the retire of an old wheel.
#   fallback        2. scripts/install/fno.sh pinned to the same version,
#                      which installs the same tarball through its own
#                      fetcher (curl or wget).
#
# No Python and no uv are involved. Every path logs which one it took, so the
# user knows what landed.
set -euo pipefail

# The terminal line every consumer reads (the session-start hook's notice, the
# skills, the channel-matrix smoke): name the exit code on the way out.
trap 'rc=$?; printf "installer exit %s\n" "$rc"' EXIT

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FNO_RELEASE_REPO="${FNO_RELEASE_REPO:-fnodotdev/fno}"

# Shared channel/version math (stable|rc|nightly detection, the tarball
# platform name). Absent the helper, fall back to the stable channel.
PLUGIN_VERSION_LIB="$(dirname "$SCRIPT_DIR")/scripts/release/plugin-version.sh"
if [[ -f "$PLUGIN_VERSION_LIB" ]]; then
  # shellcheck disable=SC1090
  source "$PLUGIN_VERSION_LIB"
else
  plugin_channel() { echo stable; }
  plugin_tarball_platform() { echo ""; }
fi

log() { printf "[fno postinstall] %s\n" "$*"; }
err() { printf "[fno postinstall] ERROR: %s\n" "$*" >&2; }
# Printed after a successful install so a new user knows the one configuration
# step exists (install never prompts). Optional: defaults work without it.
next_steps() { log "Next: run 'fno config setup run --check' to see what this machine still needs."; }

# The two steps fno.sh runs after a verified install (wire_harnesses,
# run_setup_once), so the plugin route leaves the same machine. Best effort:
# the CLI is installed already. FNO_NO_WIRE skips both, as it does there.
wire_and_setup() { # wire_and_setup <front-door path>
  if [[ -n "${FNO_NO_WIRE:-}" ]]; then
    log "FNO_NO_WIRE is set: skipping plugin wiring and the run-once setup."
    return 0
  fi
  log "wiring the fno plugin into detected agent CLIs (set FNO_NO_WIRE to skip this)..."
  "$1" config setup auto-wire >&2 || log "plugin wiring did not finish; run 'fno config setup auto-wire' to retry."
  "$1" config setup run --once ${FNO_YES:+--yes} --json >&2 || log "the run-once setup did not finish; run 'fno config setup run' to retry."
}

# Version this plugin declares: plugin.json is the ONE version field, and
# release.yml stamps it per channel on the nightly tag (stable's tree carries
# the plain release version). The staged front door's --adopt refuses a
# tarball that does not ship it.
src_version() {
  local v
  v="$(grep '"version"' "$SCRIPT_DIR/plugin.json" 2>/dev/null \
        | head -1 | sed -n -E 's/.*"version"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/p')" || true
  printf '%s' "$v"
}

FNO_BINARY_DIR="${FNO_STATE_DIR:-$HOME/.fno}/install/bin"
FNO_LINK_DIR="${FNO_INSTALL_DIR:-$HOME/.local/bin}"

# The post-install shim sweep: repair any fno* shim in the link dir that is
# missing, dangles, or resolves into a temp dir; fail loud when unrepairable.
shim_sweep() {
  local fno="$FNO_BINARY_DIR/fno"
  [[ -x "$fno" ]] || return 0
  "$fno" config setup shim-check --help >/dev/null 2>&1 || return 0
  "$fno" config setup shim-check --repair --bin-dir "$FNO_LINK_DIR" && return 0
  err "fno shims in $FNO_LINK_DIR are missing, dangle, or point into a temp dir and could not be relinked to the durable copy. Inspect: $fno config setup shim-check --bin-dir $FNO_LINK_DIR"
  return 1
}

# The release tag whose tarball ships this plugin's
# version. stable v<ver>, rc v<core>rc<n>, nightly the rolling tag.
release_tag_for_version() { # release_tag_for_version <PEP 440 version> <channel>
  local base n
  case "$2" in
    nightly) printf '%s\n' "nightly" ;;
    rc) base="${1%%rc*}"; n="${1##*rc}"; printf '%s\n' "v${base}rc${n}" ;;
    *) printf '%s\n' "v$1" ;;
  esac
}

# The primary install path on every channel: fetch the release tarball for
# THIS plugin's version plus its .sha256, verify the sum, unpack to staging,
# and let the staged front door adopt it (`fno update --adopt`: Rust owns the
# proof, swap, links, marker and the wheel retire). Returns non-zero on any
# failure; every failure prints its cause (a checksum mismatch swaps nothing
# and names itself).
try_binary_install() {
  local plat tag url sum_url stage sum_file tarball want got door
  plat="$(plugin_tarball_platform "$(uname -s)" "$(uname -m)")"
  [[ -n "$plat" ]] || { err "no release tarball is built for $(uname -s)/$(uname -m). With a Rust toolchain (https://rustup.rs), build from source: cargo install --locked fno"; return 1; }
  [[ -n "$SRC_VERSION" ]] || { err "plugin.json declares no readable version."; return 1; }
  command -v curl >/dev/null 2>&1 || { log "curl is not available; skipping the direct install."; return 1; }
  command -v tar >/dev/null 2>&1 || { log "tar is unavailable; skipping the direct install."; return 1; }
  tag="$(release_tag_for_version "$SRC_VERSION" "$CHANNEL")"
  url="https://github.com/${FNO_RELEASE_REPO}/releases/download/${tag}/fno-agents-${plat}.tar.gz"
  stage="${FNO_STATE_DIR:-$HOME/.fno}/install/adopt-$$"
  mkdir -p "$stage"
  log "fetching the release binaries: $url"
  if ! curl -fsSL --max-time 300 -o "$stage/tarball" "$url"; then
    err "the release tarball fetch failed (offline, or the release does not exist yet)."
    rm -rf "$stage"
    return 1
  fi
  sum_url="${url}.sha256"
  sum_file="$stage/tarball.sha256"
  if ! curl -fsSL --max-time 60 -o "$sum_file" "$sum_url"; then
    err "the .sha256 fetch failed: $sum_url"
    rm -rf "$stage"
    return 1
  fi
  tarball="$stage/fno-agents-$plat.tar.gz"
  mv "$stage/tarball" "$tarball"
  want="$(cut -d' ' -f1 "$sum_file")"
  if command -v sha256sum >/dev/null 2>&1; then
    got="$(sha256sum "$tarball" | cut -d' ' -f1)"
  else
    got="$(shasum -a 256 "$tarball" | cut -d' ' -f1)"
  fi
  if [[ "$got" != "$want" ]]; then
    err "sha256 mismatch for fno-agents-$plat.tar.gz (expected ${want:-none}, got ${got:-none}); swapping nothing."
    rm -rf "$stage"
    return 1
  fi
  if ! tar -xzf "$tarball" -C "$stage"; then
    err "the tarball did not unpack."
    rm -rf "$stage"
    return 1
  fi
  door="$stage/fno"
  [[ -x "$door" ]] || { err "the tarball carries no executable fno front door."; rm -rf "$stage"; return 1; }
  log "installing the binaries into $FNO_BINARY_DIR and linking them into $FNO_LINK_DIR..."
  if ! FNO_INSTALL_DIR="$FNO_LINK_DIR" "$door" update --adopt "$stage" >"$stage.adopt.log" 2>&1; then
    # A release older than --adopt: fno.sh places its binaries by hand.
    if grep -q 'unknown flag: --adopt' "$stage.adopt.log"; then
      log "fno $SRC_VERSION predates update --adopt; fno.sh places it instead."
    else
      cat "$stage.adopt.log" >&2
      err "the staged front door's --adopt run failed."
    fi
    rm -rf "$stage" "$stage.adopt.log"
    return 1
  fi
  cat "$stage.adopt.log" >&2
  rm -f "$stage.adopt.log"
  rm -rf "$stage"
  return 0
}

SRC_VERSION="$(src_version)"
CHANNEL="stable"
# Not a bare `[[ ]] &&` list: an empty SRC_VERSION would trip set -e there.
if [[ -n "$SRC_VERSION" ]]; then
  CHANNEL="$(plugin_channel "$SRC_VERSION")"
fi

# Register the global ~/.fno state dir on every success exit, so a plugin-only
# install gets the same additionalDirectories entry a clone user's setup.sh
# writes (scripts/ensure-global-dir.sh).
finish_success() {
  bash "$(dirname "$SCRIPT_DIR")/scripts/ensure-global-dir.sh" || true
}

# Already a binary install at this plugin's version: wire and set up only;
# `fno update` owns updates.
if [[ "$(cat "${FNO_STATE_DIR:-$HOME/.fno}/install/channel" 2>/dev/null)" == "binary" && -x "$FNO_BINARY_DIR/fno" ]]; then
  installed="$("$FNO_BINARY_DIR/fno" version --json 2>/dev/null | sed -n -E 's/.*"package"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/p')" || installed=""
  if [[ -n "$SRC_VERSION" && "$installed" == "$SRC_VERSION" ]]; then
    log "fno $installed already installed at $FNO_BINARY_DIR; skipping the download."
    shim_sweep || exit 1
    wire_and_setup "$FNO_BINARY_DIR/fno"
    finish_success
    exit 0
  fi
fi

# The direct install: the primary path on every channel.
if try_binary_install; then
  log "binary install adopted."
  shim_sweep || exit 1
  wire_and_setup "$FNO_BINARY_DIR/fno"
  log "restart your shell (or source your env) to pick up PATH."
  finish_success
  next_steps
  exit 0
fi

# The fallback: scripts/install/fno.sh, pinned to this plugin's version, so a
# curl-less machine with wget still lands the same release.
FNO_SH="$(dirname "$SCRIPT_DIR")/scripts/install/fno.sh"
if [[ -f "$FNO_SH" && -n "$SRC_VERSION" ]]; then
  log "falling back to scripts/install/fno.sh for fno $SRC_VERSION..."
  fno_sh_rc=0
  FNO_VERSION="$SRC_VERSION" sh "$FNO_SH" || fno_sh_rc=$?
  if [[ "$fno_sh_rc" -eq 0 ]]; then
    finish_success
    next_steps
    exit 0
  fi
  err "fno.sh could not install fno $SRC_VERSION (exit $fno_sh_rc)."
fi

err "fno is not installed. Retry once online, or install by hand: curl -fsSL fno.sh | sh"
exit 1
