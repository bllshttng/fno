# Homebrew formula for the `fno` CLI - the brew install channel.
#
# This is the canonical source of the formula. It is copied verbatim into the
# own tap repo (github.com/<owner>/homebrew-fno) so `brew install <owner>/fno/fno`
# resolves it. Keeping the source here lets CI parse + audit it and lets the
# brew clean-machine smoke (tests/smoke/brew_formula_smoke.sh) prove the
# install + symlink mechanism against a freshly built release tarball.
#
# The binary-tarball flywheel (the wheel upgrade path): the formula installs
# the release binary tarball (`fno-agents-macos-<arch>.tar.gz`, the same
# artifact `fno update` verifies), so `brew upgrade` lands the binary install
# directly and brew users ride the same channel the CLI updates from. No
# Python dependency: the tarball carries the four Rust binaries and nothing
# else, and the Python CLI (still shipped by the bridge wheel) self-provisions
# on first use of a Python-side command. brew owns the keg + symlinks, so
# `brew uninstall`/`brew upgrade` are clean (Locked Decisions 2 + 3).
#
# NOTE: this is the SEED snapshot. The live tap (bllshttng/homebrew-fno, what
# `brew install` reads) is auto-bumped on every release by
# scripts/release/update-homebrew-formula.py ; these values are just the
# last-release snapshot, not the source of truth.
#
# Deps: none beyond macOS. The binaries are self-contained (the linux legs
# build musl-static; the macOS legs link the system libc), so the formula
# carries no depends_on rows and no network setup beyond brew's own fetch.
class Fno < Formula
  desc "Autonomous delivery pipeline CLI (footnote)"
  homepage "https://github.com/fnodotdev/fno"

  # The x86_64 tarball is the top-level default so a `url` is ALWAYS defined -
  # including on Linux, where the on_macos block is skipped. Homebrew validates
  # url presence at load (before requirements), so without a top-level url a
  # Linuxbrew user would hit a confusing `stable: url is missing` instead of
  # the clean `depends_on :macos` refusal below. arm64 overrides this url
  # inside on_macos.
  url "https://github.com/fnodotdev/fno/releases/download/v0.4.6rc2/fno-agents-macos-x64.tar.gz"
  # Explicit version: the release tarball name carries no version, so pin it
  # for `brew info`/upgrade and the tap's own audit.
  version "0.4.6rc2"
  sha256 "6996c8d1219c1eeaa94d8226755205786c766337fad0550c56bac611df7a7a94"
  license "Apache-2.0"

  depends_on :macos

  on_macos do
    on_arm do
      url "https://github.com/fnodotdev/fno/releases/download/v0.4.6rc2/fno-agents-macos-arm64.tar.gz"
      sha256 "e2eea84b20d78afebda296ddb628b7082ce6f6394a32c8d20afbadfb79d40180"
    end
  end

  def install
    # The release tarball carries the four binaries at its root (no wrapper
    # dir), so brew's extraction lands them at buildpath. libexec holds them;
    # the keg bin symlinks from there. The fno-agents* symlinks are the
    # load-bearing step (Locked Decision 4): the CLI invokes the binaries by
    # name on PATH. The `fno` symlink is the advertised front door: the Rust
    # mux that owns `fno` on PATH.
    libexec.install "fno", "fno-agents", "fno-agents-daemon", "fno-agents-worker"
    bin.install_symlink libexec/"fno"
    bin.install_symlink libexec/"fno-agents"
    bin.install_symlink libexec/"fno-agents-daemon"
    bin.install_symlink libexec/"fno-agents-worker"
  end

  test do
    # The front door runs from the keg bin: `mux ls --json` is native (no
    # Python needed), and an empty session list is exactly `[]` - a stable
    # marker on a clean runner where `mux ls` would print "no sessions".
    assert_match "[]", shell_output("#{bin}/fno mux ls --json")

    # All four Rust executables must be present + executable on the keg bin,
    # or the front door or a daemon/loop command would 127 at runtime. Fail
    # the test on any miss (no silent success for a half-installed CLI).
    %w[fno fno-agents fno-agents-daemon fno-agents-worker].each do |b|
      assert_predicate bin/b, :executable?, "#{b} missing from the keg bin"
    end
  end
end
