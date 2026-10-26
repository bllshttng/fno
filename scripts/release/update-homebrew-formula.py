#!/usr/bin/env python3
"""Point a Homebrew tap formula at a published PyPI version of `fno`.

Rewrites the macOS x86_64 + arm64 wheel `url`+`sha256` and the pinned `version`
in a `Formula/fno.rb` to the wheels PyPI is serving for <version>. Driven by the
release workflow's Homebrew leg ; also runnable by hand for a one-off.

    update-homebrew-formula.py <version> <formula_path>
    update-homebrew-formula.py --self-check

The live tap is the source of truth for `brew install`; the in-repo
scripts/install/homebrew/fno.rb is only a seed snapshot.
"""
from __future__ import annotations

import json
import re
import sys
import time
import urllib.request

PYPI = "https://pypi.org/pypi/fno/{}/json"
# The PyPI file index can lag the upload by minutes; poll this long before
# giving up instead of failing a release leg on the race.
TRIES = 10
DELAY_SECONDS = 30
# Arch substrings as they appear in the wheel filenames + the formula url lines.
X64 = "macosx_10_12_x86_64"
ARM = "macosx_14_0_arm64"
_URL_RE = re.compile(r'url "([^"]*?(' + re.escape(X64) + "|" + re.escape(ARM) + r')[^"]*\.whl)"')


def _fetch_wheels_once(version: str) -> dict[str, tuple[str, str]]:
    """{arch_substr: (url, sha256)} for the two macOS wheels of <version>."""
    with urllib.request.urlopen(PYPI.format(version), timeout=30) as r:
        data = json.load(r)
    out: dict[str, tuple[str, str]] = {}
    for u in data["urls"]:
        fn = u["filename"]
        if fn.endswith(".whl"):
            for arch in (X64, ARM):
                if arch in fn:
                    out[arch] = (u["url"], u["digests"]["sha256"])
    return out


def fetch_wheels(
    version: str,
    tries: int = TRIES,
    delay: int = DELAY_SECONDS,
    fetch=None,
    sleep=time.sleep,
) -> dict[str, tuple[str, str]] | None:
    """Poll PyPI until both macOS wheels are listed, then return them.

    The upload can land minutes before the PyPI file index serves the wheels
    (a 200 can still carry an incomplete file list), so read up to `tries`
    times, `delay` seconds apart. Returns None once the budget is spent, with
    the reason on stderr. `fetch` and `sleep` are injectable for --self-check.
    """
    fetch = fetch or _fetch_wheels_once
    missing: list[str] = []
    last_err: Exception | None = None
    for attempt in range(1, tries + 1):
        try:
            wheels = fetch(version)
        except (OSError, ValueError) as e:  # network error, bad status, or non-JSON body
            last_err = e
        else:
            missing = [a for a in (X64, ARM) if a not in wheels]
            if not missing:
                return wheels
        if attempt < tries:
            sys.stderr.write(
                f"fno {version}: macOS wheels not on PyPI yet"
                f" (attempt {attempt}/{tries}), retrying in {delay}s\n"
            )
            sleep(delay)
    if last_err is not None:
        sys.stderr.write(f"fno {version}: could not fetch wheels from PyPI: {last_err}\n")
    else:
        sys.stderr.write(f"fno {version}: macOS wheels not on PyPI after {tries} tries: {missing}\n")
    return None


def rewrite(text: str, version: str, wheels: dict[str, tuple[str, str]]) -> str:
    """Pin `version` and swap each macOS wheel url + the sha256 that follows it.

    The formula has two `url ".../<arch>.whl"` lines, each followed (a few lines
    later) by its own `sha256 "..."`. Walk line by line, remember the arch of the
    last url seen, and rewrite the next sha256 with that arch's digest - so it is
    order-independent and never pairs a sha with the wrong arch.
    """
    out: list[str] = []
    pending: str | None = None
    for line in text.splitlines(keepends=True):
        m = _URL_RE.search(line)
        if m:
            url, _ = wheels[m.group(2)]
            line = line[: m.start(1)] + url + line[m.end(1) :]
            pending = m.group(2)
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
        '  url "https://old/fno-0.1.0-py3-none-macosx_10_12_x86_64.whl", using: :nounzip\n'
        '  version "0.1.0"\n'
        '  sha256 "' + "deadbeef" * 8 + '"\n'
        "  on_macos do\n"
        "    on_arm do\n"
        '      url "https://old/fno-0.1.0-py3-none-macosx_14_0_arm64.whl", using: :nounzip\n'
        '      sha256 "' + "cafebabe" * 8 + '"\n'
    )
    wheels = {X64: ("https://new/x64.whl", "1" * 64), ARM: ("https://new/arm.whl", "2" * 64)}
    got = rewrite(sample, "0.2.1", wheels)
    assert 'version "0.2.1"' in got, got
    assert 'url "https://new/x64.whl"' in got and f'sha256 "{"1" * 64}"' in got, got
    assert 'url "https://new/arm.whl"' in got and f'sha256 "{"2" * 64}"' in got, got
    assert "0.1.0" not in got and "deadbeef" not in got and "cafebabe" not in got, got

    # Retry contract: keep polling until the wheel set is complete; a spent
    # budget returns None and names the cause on stderr.
    calls: list[str] = []

    def flaky(v: str) -> dict[str, tuple[str, str]]:
        calls.append(v)
        if len(calls) < 3:
            return {X64: ("https://new/x64.whl", "1" * 64)}
        return {X64: ("https://new/x64.whl", "1" * 64), ARM: ("https://new/arm.whl", "2" * 64)}

    got = fetch_wheels("0.2.1", tries=5, delay=0, fetch=flaky, sleep=lambda _s: None)
    assert got is not None and ARM in got, got
    assert len(calls) == 3, calls
    assert fetch_wheels("0.2.1", tries=3, delay=0, fetch=lambda v: {}, sleep=lambda _s: None) is None

    def refused(v: str) -> dict[str, tuple[str, str]]:
        raise OSError("connection refused")

    assert fetch_wheels("0.2.1", tries=3, delay=0, fetch=refused, sleep=lambda _s: None) is None
    print("self-check OK")


def main() -> int:
    if len(sys.argv) == 2 and sys.argv[1] == "--self-check":
        self_check()
        return 0
    if len(sys.argv) != 3:
        sys.stderr.write(__doc__)
        return 2
    version, path = sys.argv[1], sys.argv[2]
    wheels = fetch_wheels(version)
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
