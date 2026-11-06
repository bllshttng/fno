#!/bin/sh
# fno.sh - the no-prerequisite install channel for fno.
#
# Served at https://fno.sh and run as:
#
#     curl -fsSL fno.sh | sh
#
# The only thing the user must already have is curl (or wget). The script
# installs the release binaries; no Python and no uv are involved:
#
#   1. pick this machine's release tarball (fno-agents-<platform>.tar.gz, the
#      four binaries: fno, fno-agents, fno-agents-daemon, fno-agents-worker),
#   2. fetch it and its .sha256 from the GitHub release, and verify the sum
#      (a mismatch names both digests and touches nothing),
#   3. unpack it to a staging dir and run the staged `fno update --adopt`:
#      the front door proves the version, swaps the binaries into
#      ~/.fno/install/bin, links them into the PATH dir, and moves an old
#      wheel install over,
#   4. verify `fno version --json` and `fno-agents --version` answer, then put
#      the PATH dir on PATH in the shell profile (opt out with
#      FNO_NO_MODIFY_PATH to get a manual hint only).
#
# POSIX sh ONLY (runs unmodified under dash and macOS /bin/sh): no bashisms, no
# arrays, no `local`, no `set -o pipefail`. Fully non-interactive - it is its own
# stdin under `curl | sh`, so it can never `read` a prompt. Every configuration
# is an env var.
#
# Env knobs:
#   FNO_VERSION          pin an exact release (`FNO_VERSION=1.2.3` -> tag v1.2.3)
#   FNO_INSTALL_TARBALL  install from a local tarball path or URL instead of the
#                        GitHub release (its sum is read from `<tarball>.sha256`);
#                        the clean-machine smokes use it before a release exists
#   FNO_INSTALL_DIR      the PATH dir the binaries are linked into (default
#                        ~/.local/bin); the binaries themselves live in
#                        ~/.fno/install/bin
#   FNO_NO_MODIFY_PATH   set to any non-empty value to skip the profile edit and
#                        print a manual PATH hint instead
#   FNO_NO_WIRE          set to any non-empty value to skip the plugin wiring step
#                        (every agent CLI on PATH gets the fno plugin otherwise;
#                        `fno do wizard` still chooses by hand)
#
# The channel is read from the TREE this script sits in: the nightly tag stamps
# a dev version into .claude-plugin/plugin.json, so an install run from inside a
# nightly tree installs the nightly release binaries beside the nightly skills
# and hooks. Served standalone at fno.sh there is no tree and the latest stable
# release applies.
set -eu

# --- output helpers --------------------------------------------------------
# Progress + info go to stderr so a `curl | sh` caller's stdout stays clean and
# every failure path is detectable (message on stderr, non-zero exit).
say()  { printf 'fno: %s\n' "$*" >&2; }
die()  { printf 'fno: %s\n' "$*" >&2; exit "${2:-1}"; }

have() { command -v "$1" >/dev/null 2>&1; }

# fetch_to <url> <file>: download over HTTPS, preferring curl, else wget.
fetch_to() {
	if have curl; then
		curl -fsSL --max-time 300 -o "$2" "$1"
	elif have wget; then
		wget -qO "$2" "$1"
	else
		die "neither curl nor wget is available to fetch the release; install one and re-run."
	fi
}

# sha256_of <file>: the hex digest, from sha256sum or shasum.
sha256_of() {
	if have sha256sum; then
		sha256sum "$1" | cut -d' ' -f1
	elif have shasum; then
		shasum -a 256 "$1" | cut -d' ' -f1
	else
		die "neither sha256sum nor shasum is available to verify the release."
	fi
}

# --- plugin channel detection -----------------------------------------------
# plugin_channel and friends come from scripts/release/plugin-version.sh, the
# same helper .claude-plugin/postinstall.sh sources. Sourced only when it sits
# beside this script (an in-tree run); the served fno.sh copy needs nothing.
# $0 under `curl | sh` is the shell name, so the paths below never resolve and
# the stable channel stands.
FNO_RELEASE_REPO="${FNO_RELEASE_REPO:-fnodotdev/fno}"
FNO_DECLARED_VERSION=
FNO_CHANNEL=stable
detect_channel() {
	_dir=$(cd "$(dirname "$0")" 2>/dev/null && pwd) || return 0
	[ -f "$_dir/../release/plugin-version.sh" ] || return 0
	# shellcheck disable=SC1090,SC1091
	. "$_dir/../release/plugin-version.sh"
	[ -f "$_dir/../../.claude-plugin/plugin.json" ] || return 0
	FNO_DECLARED_VERSION=$(sed -n -E 's/.*"version"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/p' "$_dir/../../.claude-plugin/plugin.json" | head -n 1)
	[ -n "$FNO_DECLARED_VERSION" ] || { FNO_DECLARED_VERSION=; return 0; }
	FNO_CHANNEL=$(plugin_channel "$FNO_DECLARED_VERSION")
}

# pep_tag <version>: the release tag of a version in either spelling
# (1.2.3 -> v1.2.3, 0.4.0-rc.1 or 0.4.0rc1 -> v0.4.0rc1).
pep_tag() {
	printf 'v%s\n' "$(printf '%s' "$1" | sed -E 's/-dev\.([0-9]+)$/.dev\1/; s/-rc\.([0-9]+)$/rc\1/')"
}

# Choose the release to install. FNO_VERSION wins, then the tree's channel;
# stable takes the latest release. Sets FNO_TAG (`latest` means the newest
# non-prerelease, read through GitHub's latest-release redirect).
FNO_TAG=
resolve_source() {
	# FNO_VERSION, when PROVIDED (even empty), must be a real version - a malformed
	# pin fails loudly rather than silently installing latest.
	if [ "${FNO_VERSION+x}" = x ]; then
		case "$FNO_VERSION" in
			"")
				die "FNO_VERSION is set but empty; unset it to install the latest, or set an exact version like FNO_VERSION=1.2.3." ;;
			*[!0-9A-Za-z.+_-]*)
				die "FNO_VERSION='$FNO_VERSION' is not a valid version; use an exact version like FNO_VERSION=1.2.3." ;;
			[0-9]*)
				FNO_TAG=$(pep_tag "$FNO_VERSION")
				return 0 ;;
			*)
				die "FNO_VERSION='$FNO_VERSION' must start with a digit (e.g. 1.2.3)." ;;
		esac
	fi
	case "$FNO_CHANNEL" in
		nightly) FNO_TAG=nightly ;;
		rc) FNO_TAG=$(pep_tag "$FNO_DECLARED_VERSION") ;;
		*) FNO_TAG=latest ;;
	esac
}

# The release asset URL for this platform and tag.
release_url() {
	if [ "$FNO_TAG" = latest ]; then
		printf 'https://github.com/%s/releases/latest/download/fno-agents-%s.tar.gz\n' "$FNO_RELEASE_REPO" "$1"
	else
		printf 'https://github.com/%s/releases/download/%s/fno-agents-%s.tar.gz\n' "$FNO_RELEASE_REPO" "$FNO_TAG" "$1"
	fi
}

# --- install ---------------------------------------------------------------
FNO_STATE="${FNO_STATE_DIR:-${HOME:-}/.fno}"
FNO_BIN="$FNO_STATE/install/bin"
FNO_LINK_DIR="${FNO_INSTALL_DIR:-${HOME:-}/.local/bin}"

# install_tarball <source> <platform>: fetch (or copy) the tarball and its
# .sha256, verify, unpack, and adopt it through the staged front door.
install_tarball() {
	_src="$1"
	_stage="$FNO_STATE/install/stage-$$"
	rm -rf "$_stage"
	mkdir -p "$_stage"
	_tar="$_stage/fno-agents-$2.tar.gz"
	case "$_src" in
		*://*)
			say "fetching the release binaries: $_src"
			fetch_to "$_src" "$_tar" || { rm -rf "$_stage"; die "the release tarball fetch failed (offline, or the release does not exist yet): $_src"; }
			fetch_to "$_src.sha256" "$_tar.sha256" || { rm -rf "$_stage"; die "the .sha256 fetch failed: $_src.sha256"; }
			;;
		*)
			[ -f "$_src" ] || { rm -rf "$_stage"; die "FNO_INSTALL_TARBALL=$_src is not a file."; }
			[ -f "$_src.sha256" ] || { rm -rf "$_stage"; die "no checksum beside the tarball: $_src.sha256"; }
			cp "$_src" "$_tar"
			cp "$_src.sha256" "$_tar.sha256"
			;;
	esac
	_want=$(cut -d' ' -f1 "$_tar.sha256")
	_got=$(sha256_of "$_tar")
	if [ "$_got" != "$_want" ]; then
		rm -rf "$_stage"
		die "sha256 mismatch for fno-agents-$2.tar.gz: expected ${_want:-none}, got ${_got:-none}. Nothing was installed."
	fi
	tar -xzf "$_tar" -C "$_stage" || { rm -rf "$_stage"; die "the tarball did not unpack."; }
	[ -x "$_stage/fno" ] || { rm -rf "$_stage"; die "the tarball carries no executable fno front door."; }
	say "installing the binaries into $FNO_BIN and linking them into $FNO_LINK_DIR..."
	_log="$_stage.adopt.log"
	if FNO_INSTALL_DIR="$FNO_LINK_DIR" "$_stage/fno" update --adopt "$_stage" >"$_log" 2>&1; then
		cat "$_log" >&2
	elif grep -q 'unknown flag: --adopt' "$_log"; then
		# A front door released before --adopt existed: place it the same way.
		place_binaries "$_stage" || { rm -rf "$_stage" "$_log"; die "could not place the binaries into $FNO_BIN and $FNO_LINK_DIR."; }
	else
		cat "$_log" >&2
		rm -rf "$_stage" "$_log"
		die "the staged front door's --adopt run failed (its error is above)."
	fi
	rm -rf "$_stage" "$_log"
}

# place_binaries <dir>: what `fno update --adopt` does, for a front door that
# predates it: each binary and each PATH link lands as a temp file renamed
# over the old one, then the channel marker reads binary.
place_binaries() {
	mkdir -p "$FNO_BIN" "$FNO_LINK_DIR" || return 1
	for _b in fno fno-agents fno-agents-daemon fno-agents-worker; do
		[ -x "$1/$_b" ] || { say "the tarball carries no $_b."; return 1; }
		cp "$1/$_b" "$FNO_BIN/.$_b.new" && chmod 755 "$FNO_BIN/.$_b.new" && mv -f "$FNO_BIN/.$_b.new" "$FNO_BIN/$_b" || return 1
		ln -sf "$FNO_BIN/$_b" "$FNO_LINK_DIR/.$_b.new" && mv -f "$FNO_LINK_DIR/.$_b.new" "$FNO_LINK_DIR/$_b" || return 1
	done
	printf 'binary\n' >"$FNO_STATE/install/channel"
}

# Prove the installed binaries answer before declaring success.
verify_install() {
	_out=$("$FNO_BIN/fno" version --json 2>&1) || die "INCOMPLETE install: '$FNO_BIN/fno version --json' failed: $(printf '%s' "$_out" | head -n 1)"
	FNO_VERIFIED_VERSION=$(printf '%s' "$_out" | sed -n -E 's/.*"package"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/p' | head -n 1)
	"$FNO_BIN/fno-agents" --version >/dev/null 2>&1 || die "INCOMPLETE install: '$FNO_BIN/fno-agents --version' failed."
}

# The post-install shim sweep: repair any fno* shim in the link dir that is
# missing, dangles, or resolves into a temp dir; fail loud when unrepairable.
shim_sweep() {
	"$FNO_BIN/fno" config setup shim-check --help >/dev/null 2>&1 || return 0
	"$FNO_BIN/fno" config setup shim-check --repair --bin-dir "$FNO_LINK_DIR" && return 0
	say "INCOMPLETE install: fno shims in $FNO_LINK_DIR are missing, dangle, or point into a temp dir and could not be relinked. Inspect: $FNO_BIN/fno config setup shim-check --bin-dir $FNO_LINK_DIR"
	return 1
}

# --- plugin auto-wire (detect-and-wire) -------------------------------------
# Every agent CLI on PATH gets the fno plugin without asking, and the command
# names every outcome. Best-effort: the CLI is already installed.
wire_harnesses() {
	[ -n "${FNO_NO_WIRE:-}" ] && return 0
	say "wiring the fno plugin into detected agent CLIs (set FNO_NO_WIRE to skip this)..."
	if ! "$FNO_BIN/fno" config setup auto-wire >&2; then
		say "plugin wiring did not finish; run 'fno do wizard' to wire by hand."
	fi
}

# The one run-once setup (config + wiring), best effort. FNO_YES or a non-TTY
# stdin takes the recommended defaults; FNO_NO_WIRE skips it too.
run_setup_once() {
	[ -n "${FNO_NO_WIRE:-}" ] && return 0
	_yes=""
	[ -n "${FNO_YES:-}" ] && _yes="--yes"
	if _out=$("$FNO_BIN/fno" config setup run --once $_yes --json 2>&1); then
		printf '%s\n' "$_out"
	fi
}

# The profile a PATH line goes into, by login shell.
shell_profile() {
	case "$(basename "${SHELL:-sh}")" in
		zsh) printf '%s\n' "${ZDOTDIR:-$HOME}/.zshrc" ;;
		bash)
			if [ "$(uname -s 2>/dev/null)" = Darwin ]; then
				printf '%s\n' "$HOME/.bash_profile"
			else
				printf '%s\n' "$HOME/.bashrc"
			fi
			;;
		*) printf '%s\n' "$HOME/.profile" ;;
	esac
}

# --- success report --------------------------------------------------------
report_success() {
	[ -n "${FNO_VERIFIED_VERSION:-}" ] || FNO_VERIFIED_VERSION="(version unreadable)"
	say "installed fno $FNO_VERIFIED_VERSION into $FNO_BIN."
	shim_sweep || :
	case ":${PATH:-}:" in
		*":$FNO_LINK_DIR:"*)
			wire_harnesses
			say "done. run 'fno --help' for the CLI."
			return 0
			;;
	esac
	if [ -n "${FNO_NO_MODIFY_PATH:-}" ]; then
		say "installed, but $FNO_LINK_DIR is not on your PATH yet (FNO_NO_MODIFY_PATH set)."
		say "add it for this and future shells, e.g.:"
		say "    export PATH=\"$FNO_LINK_DIR:\$PATH\""
		wire_harnesses
		say "done. run 'fno --help' once $FNO_LINK_DIR is on PATH."
		return 0
	fi
	_profile=$(shell_profile)
	_line="export PATH=\"$FNO_LINK_DIR:\$PATH\"  # added by fno.sh"
	if [ -f "$_profile" ] && grep -F "# added by fno.sh" "$_profile" >/dev/null 2>&1; then
		say "$_profile already adds $FNO_LINK_DIR to PATH."
	elif printf '\n%s\n' "$_line" >>"$_profile" 2>/dev/null; then
		say "added $FNO_LINK_DIR to PATH in $_profile."
	else
		say "installed, but could not edit $_profile; add $FNO_LINK_DIR to PATH yourself."
	fi
	say "restart your shell, or run this to use fno now:"
	say "    export PATH=\"$FNO_LINK_DIR:\$PATH\""
	wire_harnesses
	say "done. run 'fno --help' to get started."
}

# --- main ------------------------------------------------------------------
main() {
	# `--yes` (or FNO_YES=1): hand the run-once setup the same answer, so an
	# agent install never prompts. `sh -s -- --yes` is the pipe form.
	for _arg in "$@"; do
		case "$_arg" in
			--yes) FNO_YES=1 ;;
		esac
	done

	# footnote is not supported on native Windows yet (the agent runtime uses
	# POSIX flock + Unix-domain sockets).
	case "$(uname -s 2>/dev/null)" in
		MINGW* | MSYS* | CYGWIN* | Windows_NT)
			die "footnote is not supported on native Windows yet. Please run under WSL2 (a Linux environment)."
			;;
	esac
	[ -n "${HOME:-}" ] || die "HOME is not set; fno installs under \$HOME/.fno."

	case "$(uname -s 2>/dev/null)/$(uname -m 2>/dev/null)" in
		Darwin/arm64) _plat=macos-arm64 ;;
		Darwin/x86_64) _plat=macos-x64 ;;
		Linux/x86_64) _plat=linux-x64 ;;
		Linux/aarch64 | Linux/arm64) _plat=linux-arm64 ;;
		*)
			die "no release binaries for $(uname -s 2>/dev/null)/$(uname -m 2>/dev/null): releases ship macOS arm64, macOS x86_64, Linux x86_64 and Linux aarch64. With a Rust toolchain (https://rustup.rs), build from source: cargo install --locked fno"
			;;
	esac

	detect_channel

	# Idempotency: a working binary install and no explicit pin is a no-op
	# success, never a redundant download. `fno update` updates it.
	if [ -z "${FNO_INSTALL_TARBALL:-}" ] && [ "${FNO_VERSION+x}" != x ] \
		&& [ "$(cat "$FNO_STATE/install/channel" 2>/dev/null)" = binary ] \
		&& [ -x "$FNO_BIN/fno" ] && "$FNO_BIN/fno" version --json >/dev/null 2>&1; then
		say "fno is already installed at $FNO_BIN - nothing to do (fno update updates it)."
		verify_install
		report_success
		run_setup_once
		return 0
	fi

	if [ -n "${FNO_INSTALL_TARBALL:-}" ]; then
		install_tarball "$FNO_INSTALL_TARBALL" "$_plat"
	else
		resolve_source
		case "$FNO_CHANNEL" in
			nightly) say "channel nightly (${FNO_DECLARED_VERSION}): installing the nightly release binaries." ;;
			rc) say "channel rc: installing the candidate $FNO_TAG." ;;
		esac
		install_tarball "$(release_url "$_plat")" "$_plat"
	fi
	verify_install
	report_success
	run_setup_once
}

main "$@"
