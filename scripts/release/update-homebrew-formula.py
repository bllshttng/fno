#!/usr/bin/env python3
"""Point a Homebrew tap formula at the published release binary tarballs.

Rewrites the macOS x86_64 + arm64 tarball `url`+`sha256` and the pinned
`version` in a `Formula/fno.rb` to the release artifacts GitHub serves for
<version>. Driven by the release workflow's Homebrew leg; also runnable by
hand for a one-off.

    update-homebrew-formula.py <version> <formula_path>
    update-homebrew-formula.py --self-check

The binary-tarball flywheel: brew installs the same `fno-agents-macos-*.tar.gz`
the CLI's own `fno update` verifies, so `brew upgrade` lands the binary
install directly. The checksums come from the `.sha256` files the release
publishes beside each tarball.

The live tap is the source of truth for `brew install`; the in-repo
scripts/install/homebrew/fno.rb is only a seed snapshot.
"""
from __future__ import annotations

import re
import sys
import time
import urllib.request

RELEASE_REPO = "fnodotdev/fno"
# The GitHub file index can lag the release by minutes; poll this long before
# giving up instead of failing a release leg on the race.
TRIES = 10
DELAY_SECONDS = 30
X64 = "macos-x64"
ARM64_NAME = "macos-arm64"
X86_64 = "x86_64"
ARM64 = "arm64"
_URL_RE = re.compile(r'url "([^"]*?/fno-agents-macos-(x64|arm64)\.tar\.gz)"')


def release_tag(version: str) -> str:
    """The release tag GitHub serves `<version>` under (release_install.rs's
    release_tag mapping): a plain version tags v<version>, an rc version
    v<core>rc<n>, a dev build ships on the rolling `nightly` tag."""
    m = re.fullmatch(r"(\d+\.\d+\.\d+)rc(\d+)", version)
    if m:
        return f"v{m.group(1)}rc{m.group(2)}"
    if ".dev" in version:
        return "nightly"
    return f"v{version}"
def _fetch_shas_once(tag: str) -> dict[str, tuple[str, str]]:
    """{arch_class: (url, sha256)} for the two macOS tarballs of <tag>."""
    out: dict[str, tuple[str, str]] = {}
    for name, arch in (
        (X64, X86_64),
        (ARM64_NAME, ARM64),
    ):
        url = f"https://github.com/{RELEASE_REPO}/releases/download/{tag}/fno-agents-{name}.tar.gz"
        with urllib.request.urlopen(url + ".sha256", timeout=30) as r:
            line = r.read().decode().strip().splitlines()[0]
        sha = line.split()[0]
        if len(sha) != 64:
            raise ValueError(f"{url}.sha256 names no sha256 line: {line[:80]}")
        out[arch] = (url, sha)
    return out


def fetch_shas(
    version: str,
    tries: int = TRIES,
    delay: int = DELAY_SECONDS,
    fetch=None,
    sleep=time.sleep,
) -> dict[str, tuple[str, str] | None] | None:
    """Poll the release until both macOS tarball checksums are served."""
    fetch = fetch or _fetch_shas_once
    tag = release_tag(version)
    last_err: Exception | None = None
    missing: list[str] = []
    for attempt in range(1, tries + 1):
        try:
            shas = fetch(tag) or {}
        except (OSError, ValueError) as e:
            last_err = e
        else:
            missing = [a for a in (X86_64, ARM64) if a not in shas]
            if not missing:
                return shas
        if attempt < tries:
            sys.stderr.write(
                f"fno {version} ({tag}): macOS tarballs not on the release yet"
                f" (attempt {attempt}/{tries}), retrying in {delay}s\n"
            )
            sleep(delay)
    if last_err is not None:
        sys.stderr.write(f"fno {version}: could not read the release checksums: {last_err}\n")
    else:
        sys.stderr.write(f"fno {version}: the release never served both checksums after {tries} tries (missing: {missing})\n")
    return None

def rewrite(text: str, version: str, wheels: dict[str, tuple[str, str]]) -> str:
    """Pin `version` and swap each macOS tarball url + the sha256 that follows it.

    The formula has two `url ".../fno-agents-macos-<arch>.tar.gz"` lines, each
    followed (a few lines later) by its own `sha256 "..."`. Walk line by line,
    remember the arch of the last url seen, and rewrite the next sha256 with
    that arch's checksum - so it is order-independent and never pairs a sha
    with the wrong arch.
    """
    out: list[str] = []
    pending: str | None = None
    for line in text.splitlines(keepends=True):
        m = _URL_RE.search(line)
        if m:
            arch = "x86_64" if m.group(2) == "x64" else "arm64"
            assert arch in wheels, (m.group(2), sorted(wheels))
            url, _ = wheels[arch]
            line = line[: m.start(1)] + url + line[m.end(1) :]
            pending = arch
        elif pending and 'sha256 "' in line:
            _, sha = wheels[pending]
            line = re.sub(r'sha256 "[^"]*"', f'sha256 "{sha}"', line)
            pending = None
        elif re.search(r'version "[^"]+"', line):
            line = re.sub(r'version "[^"]+"', f'version "{version}"', line)
        out.append(line)
    return "".join(out)


def self_check() -> None:
    sample = (
        '  url "https://github.com/fnodotdev/fno/releases/download/v0.1.0/fno-agents-macos-x64.tar.gz"\n'
        '  version "0.1.0"\n'
        '  sha256 "' + "deadbeef" * 8 + '"\n'
        "  on_macos do\n"
        "    on_arm do\n"
        '  url "https://github.com/fnodotdev/fno/releases/download/v0.1.0/fno-agents-macos-arm64.tar.gz"\n'
        '  sha256 "' + "cafebabe" * 8 + '"\n'
    )
    wheels = {X86_64: ("https://new/x64.tar.gz", "1" * 64), ARM64: ("https://new/arm.tar.gz", "2" * 64)}
    got = rewrite(sample, "0.2.1", wheels)
    assert 'version "0.2.1"' in got, got
    assert 'url "https://new/x64.tar.gz"' in got and f'sha256 "{"1" * 64}' in got, got
    assert 'url "https://new/arm.tar.gz"' in got and f'sha256 "{"2" * 64}' in got, got
    assert "0.1.0" not in got and "deadbeef" not in got and "cafebabe" not in got, got

    # The release-tag mapping: rc and nightly versions tag correctly.
    assert release_tag("0.4.7") == "v0.4.7"
    assert release_tag("0.4.7rc1") == "v0.4.7rc1"
    assert release_tag("0.4.7.dev.20261105") == "nightly"

    # Retry contract: keep polling until both checksums answer; a spent
    # budget returns None and names the cause on stderr. An incomplete
    # fetch result keeps polling too - it never reaches rewrite().
    calls: list[str] = []

    def flaky(tag: str):
        calls.append(tag)
        if len(calls) < 3:
            return {X86_64: ("https://new/x64.tar.gz", "1" * 64)}
        return {X86_64: ("https://new/x64.tar.gz", "1" * 64), ARM64: ("https://new/arm.tar.gz", "2" * 64)}

    got = fetch_shas("0.2.1", tries=5, delay=0, fetch=flaky, sleep=lambda _s: None)
    assert got is not None and ARM64 in got, got
    assert len(calls) == 3, calls
    assert fetch_shas("0.2.1", tries=3, delay=0, fetch=lambda _tag: {}, sleep=lambda _s: None) is None

    def refused(_tag: str):
        raise OSError("connection refused")

    assert fetch_shas("0.2.1", tries=3, delay=0, fetch=refused, sleep=lambda _s: None) is None
    print("self-check OK")

def main() -> int:
    if len(sys.argv) == 2 and sys.argv[1] == "--self-check":
        self_check()
        return 0
    if len(sys.argv) != 3:
        sys.stderr.write(__doc__)
        return 2
    version, path = sys.argv[1], sys.argv[2]
    wheels = fetch_shas(version)
    if wheels is None:
        return 1
    with open(path, encoding="utf-8") as f:
        text = f.read()
    with open(path, "w", encoding="utf-8") as f:
        f.write(rewrite(text, version, wheels))
    print(f"updated {path} -> fno {version}")
    return 0
if __name__ == "__main__":
    raise SystemExit(main())
