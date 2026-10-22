"""Regression for the autouse provider-exec guard (ab-c1bf3552).

The ``_block_live_provider_exec`` fixture in ``conftest.py`` must stop a test
that reaches the Python dispatch path without isolating PATH from exec'ing a
*real* claude/codex/gemini binary and leaking a live session (e.g. an immortal
``claude --bg``). The safe pattern - a fake binary on a tmp-isolated PATH - is
already exercised by the rest of the suite (those execs are allowed); here we
prove the complementary half: an un-isolated provider exec is blocked.
"""
from __future__ import annotations

import subprocess

import pytest

from fno.agents.harnesses import claude as _claude


def test_guard_blocks_unisolated_provider_exec(tmp_path, monkeypatch):
    """With no fake on PATH, a bare provider name resolves to the ambient real
    binary (or nothing) - never a tmp-isolated fake - so the guard must raise."""
    # PATH = an empty tmp dir: no fake claude/codex/gemini is reachable.
    monkeypatch.setenv("PATH", str(tmp_path))
    seam = _claude._subprocess_run  # the autouse-patched guard wrapper
    with pytest.raises(AssertionError, match="live provider exec blocked"):
        seam(["claude", "--bg", "--name", "leaktest", "hi"])


def test_guard_blocks_live_launchctl_mutation(tmp_path, monkeypatch):
    """A mutating launchctl command resolved outside the tmp tree would change the
    operator's real launchd domain, so the guard must raise before any process
    starts (x-63aa: two unstubbed pr-watch install tests re-registered
    sh.fno.pr-watcher from a pytest tempdir and killed every launchd arm)."""
    monkeypatch.setenv("PATH", str(tmp_path))
    with pytest.raises(AssertionError, match="live launchctl mutation blocked"):
        subprocess.run(["launchctl", "bootstrap", "gui/501", "/tmp/x.plist"])

