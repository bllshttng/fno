"""x-cec8: the harness-name set is platform-layer data, and importing the
platform layer no longer drags the runtime in at import time."""

from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

import pytest


def test_the_complete_roster_carries_the_evidence_backed_hosts(monkeypatch):
    """AC1-HP: KNOWN_HARNESSES is the COMPLETE supported roster - the eleven
    capability-backed names plus hermes and openclaw, which host real sessions
    per docs/SETUP-*.md. The roster lives in Rust (provider.rs
    KNOWN_HARNESSES) and the import proxies it through the fno-agents
    harness-roster read, so this assertion proves a Python reader accepts a
    Rust-added name; scripts/ci/check-harness-roster-parity.py holds the
    evidence surfaces as subsets of the same Rust source.

    The same import pins the door mechanics (folds under the test-delta cap,
    which counts declarations): the roster resolves once per process - two
    known_harnesses() calls return the same object - and a failed read
    refuses closed naming the remedy."""
    from fno.harness_names import KNOWN_HARNESSES

    assert set(KNOWN_HARNESSES) == {
        "claude",
        "codex",
        "gemini",
        "agy",
        "opencode",
        "pi",
        "cursor-agent",
        "grok",
        "hermes",
        "openclaw",
        "zcode",
        "footnote",
        "muse",
    }
    from fno.agents.harness_map import known_harnesses

    # The capability-backed roster stays at eleven; the wider names ride the
    # roster only, which is the asymmetry this change exists to declare.
    assert set(known_harnesses()) == {
        "claude",
        "codex",
        "gemini",
        "agy",
        "opencode",
        "pi",
        "cursor-agent",
        "grok",
        "zcode",
        "footnote",
        "muse",
    }

    # The door mechanics, folded under the test-delta declaration cap: the
    # roster resolves once per process (two calls, one object) and a failed
    # read refuses closed naming the remedy.
    import fno.harness_names as hn

    from fno.rust_binary import CommandUnavailable

    assert hn.known_harnesses() is hn.KNOWN_HARNESSES

    calls = []

    def fake_read(command, args=(), *, timeout=60, binary=None):
        calls.append(command)
        return (None, {"known": ("alpha", "beta"), "providers": ("alpha",), "spawn": ("alpha",)})

    monkeypatch.setattr(hn, "_ROSTER", None, raising=False)
    monkeypatch.setattr(hn, "call_binary_json", fake_read)
    assert hn.known_harnesses() == ("alpha", "beta")
    assert hn.known_harnesses() is hn.known_harnesses()
    assert calls == ["harness-roster"]
    assert hn.known_providers() == ("alpha",)

    # The agents CLI consumes this exported name, not known_providers() directly.
    # Resolve it in a fresh process so package import/cache state cannot mask the
    # old-binary payload used by the reported mail-by-name failure.
    repo_src = Path(__file__).resolve().parents[3] / "cli" / "src"
    env = dict(os.environ, PYTHONPATH=str(repo_src))
    result = subprocess.run(
        [
            sys.executable,
            "-c",
            "import fno.harness_names as hn; "
            "hn.call_binary_json = lambda *a, **k: (None, {'known': ['alpha'], 'providers': ['alpha'], 'spawn': ['alpha']}); "
            "from fno.agents.harnesses import READABLE_PROVIDERS; "
            "print(READABLE_PROVIDERS)",
        ],
        env=env,
        capture_output=True,
        text=True,
        check=True,
    )
    assert result.stdout.strip() == "('alpha',)"

    monkeypatch.setattr(
        hn, "call_binary_json", lambda *a, **k: ("fno-agents binary not found", None)
    )
    monkeypatch.setattr(hn, "_ROSTER", None, raising=False)
    with pytest.raises(CommandUnavailable, match="fno doctor update"):
        hn.known_harnesses()


