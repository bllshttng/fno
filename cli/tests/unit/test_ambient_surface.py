"""Every env var source reads must be classified as ambient or environment.

This is the guard that stops ``hermetic.py`` becoming the next allowlist that
loses to the fifth thing nobody thought of.

The three specimens on 2026-08-11 all had the same root shape: an author
sandboxed the channels they could think of, and the channel they could not
think of stayed open until a human noticed a red suite. A list is only as good
as the next person's memory, so this test replaces the memory. Add an env read
to source, and CI asks you - by name - whether a test may see the developer's
value for it.

Writing the decision down is the whole job; both answers are fine.
"""
from __future__ import annotations

import collections
import re
from pathlib import Path

import pytest

from fno.hermetic import classify

REPO_ROOT = Path(__file__).resolve().parents[3]

# The read shapes that reach the process environment, in both languages.
_PATTERNS = (
    r'environ\.get\(\s*["\']([A-Za-z_][A-Za-z0-9_]*)["\']',
    r'environ\[\s*["\']([A-Za-z_][A-Za-z0-9_]*)["\']',
    r'getenv\(\s*["\']([A-Za-z_][A-Za-z0-9_]*)["\']',
    r'env::var(?:_os)?\(\s*"([A-Za-z_][A-Za-z0-9_]*)"',
)

# Source trees only. A test file setting its own marker is not a leak; it is a
# test doing its job.
_SOURCE_ROOTS = (("cli/src", "*.py"), ("crates", "*.rs"))


def _env_reads() -> dict[str, set[str]]:
    """{env name -> {files reading it}} across the source trees."""
    found: dict[str, set[str]] = collections.defaultdict(set)
    for rel, glob in _SOURCE_ROOTS:
        root = REPO_ROOT / rel
        if not root.exists():  # pragma: no cover - a partial checkout
            continue
        for path in root.rglob(glob):
            parts = path.parts
            if "tests" in parts or "target" in parts or path.name.startswith("test_"):
                continue
            text = path.read_text(errors="replace")
            for pattern in _PATTERNS:
                for name in re.findall(pattern, text):
                    found[name].add(str(path.relative_to(REPO_ROOT)))
    return found


def test_the_probe_actually_finds_env_reads():
    """Positive control.

    An empty result would make the classification test below pass while
    measuring nothing - the absence-only failure. HOME is read ~35 times in
    source; if the probe cannot see it, the probe is broken, not the code.
    """
    reads = _env_reads()
    assert "HOME" in reads, "env-read probe found nothing; the regexes are stale"
    assert len(reads) > 20, f"probe found only {len(reads)} names; expected dozens"


def test_every_env_read_in_source_is_classified():
    """The guard itself."""
    unclassified = {
        name: sorted(files)[:3]
        for name, files in _env_reads().items()
        if classify(name) == "unclassified"
    }
    if unclassified:
        lines = [
            f"  {name}  (read in {', '.join(files)})"
            for name, files in sorted(unclassified.items())
        ]
        pytest.fail(
            "These env vars are read by source but not classified in "
            "fno/hermetic.py:\n"
            + "\n".join(lines)
            + "\n\nDecide for each one: does a test get to see the developer's "
            "value?\n"
            "  no  -> add it to _AMBIENT_NAMES (it gets scrubbed)\n"
            "  yes -> add it to _ENVIRONMENT with a comment saying why\n"
            "Either answer is fine. Leaving it undecided is the thing that "
            "broke three test suites on 2026-08-11.",
            pytrace=False,
        )


def test_classify_agrees_with_the_scrub():
    """The reporting view and the acting view must not drift.

    A classification that says "ambient" while neutralise keeps the value would
    make the guard decorative: green, and wrong.
    """
    import os
    import tempfile

    from fno.test_env_door import apply_env

    sandbox = Path(tempfile.mkdtemp())
    probes = {name: "ambient-value" for name in _env_reads()}
    # The door resolves the binary from this very dict; keep a real
    # FNO_AGENTS_BIN pin so the scrub is exercised at all. It is
    # environment-class, so the loop below never asserted it.
    real_bin = os.environ.get("FNO_AGENTS_BIN")
    if real_bin:
        probes["FNO_AGENTS_BIN"] = real_bin
    try:
        out = apply_env("neutralise", probes, sandbox=sandbox)
    except SystemExit:
        pytest.skip(
            "no fno-agents here can speak the env door (stale install); "
            "install the crate fresh or run through 'fno doctor test' in CI"
        )
    for name in probes:
        if classify(name) == "ambient":
            assert out.get(name) != "ambient-value", (
                f"{name} classifies as ambient but survived neutralise()"
            )


def test_a_received_env_scrubs_without_the_binary(monkeypatch):
    """The fence receipt degrades the door to a Python-side scrub.

    The smoke pytest legs delete the dev build on purpose so the parity
    suites skip, and the runner has already neutralised in-process - so the
    pins are right and no binary is needed. A marker planted after the fence
    (the identity-scrub guard spawns exactly that child) must still die, so
    the fallback scrubs through fno.hermetic's classifier, the same
    vocabulary the native assembly reads, and keeps the receipt standing.
    """
    from fno import test_env_door

    env = {
        "FNO_TEST_HERMETIC": "1",
        "HOME": "/anyone",
        "PATH": "/bin",
        "CODEX_THREAD_ID": "planted-in-the-child",
        "FNO_HARNESS_NAME": "planted-too",
    }
    monkeypatch.setattr(test_env_door.shutil, "which", lambda name: None)
    out = test_env_door.apply_env("neutralise", env, sandbox=Path("/sb"))
    assert "CODEX_THREAD_ID" not in out, "a planted marker survived the fallback"
    assert "FNO_HARNESS_NAME" not in out, "a planted FNO_ marker survived"
    assert out["FNO_TEST_HERMETIC"] == "1", "the receipt must survive the fallback"
    assert out["HOME"] == "/anyone", "the fallback must not move the pins"


def test_an_unreceived_env_still_refuses_without_the_binary(monkeypatch):
    """Positive control for the fence: no receipt, no binary, loud refusal.

    A direct pytest in the source tree carries no receipt; skipping the door
    there would run the suite on the live operator env, the exact failure
    mode the hermetic fence exists to prevent.
    """
    import pytest as _pytest

    from fno import test_env_door

    monkeypatch.setattr(test_env_door.shutil, "which", lambda name: None)
    with _pytest.raises(SystemExit) as excinfo:
        test_env_door.apply_env("neutralise", {"HOME": "/anyone"}, sandbox=Path("/sb"))
    assert excinfo.value.code == 3
