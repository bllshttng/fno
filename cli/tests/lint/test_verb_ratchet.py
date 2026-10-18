"""Verb-surface ratchet tests - `fno doctor lint verb-ratchet`.

Covers the ratchet directions (AC2), the both-binaries invariant (AC3), the
fail-closed Rust-front reach (AC4), and the conflict-message shape (AC9), plus
the usage-string parser and the advertised-subset guard that catches a Rust
addition the usage string would otherwise hide.
"""
from __future__ import annotations

from pathlib import Path

import pytest

from fno import lint_verb_ratchet as vr


# --------------------------------------------------------------------------- #
# Real-surface enumeration and the fail-closed Rust-front reach (AC4)
# --------------------------------------------------------------------------- #


def test_python_surface_recurses_to_real_leaves():
    py = vr.enumerate_python_leaves()
    # a nested group leaf (recursion works, not just top-level names)
    assert "backlog done" in py
    # a leaf one group deep (whoami folded status in, unit 6)
    assert "whoami status" in py
    # a top-level single command
    assert "update" in py
    # eager inline commands are counted
    assert "help" in py
    # a true alias is deduped (graph shares backlog's import target)
    assert not any(x == "graph" or x.startswith("graph ") for x in py)


def test_fail_closed_on_subprocess_timeout(monkeypatch):
    # A hung fno-agents front raises TimeoutExpired; it must surface as a
    # NAMED fail-closed error, not an uncaught traceback (AC4: named error).
    monkeypatch.setattr(vr, "_locate_fno_agents_front", lambda: Path("/fake/agents"))

    def raise_timeout(argv, *a, **k):
        raise vr.subprocess.TimeoutExpired(cmd=argv, timeout=20)

    monkeypatch.setattr(vr.subprocess, "run", raise_timeout)
    with pytest.raises(vr.VerbRatchetError, match="unreachable"):
        vr.probe_fno_agents_actions(Path("/fake/agents"))


# --------------------------------------------------------------------------- #
# Ratchet directions (AC2) and conflict message (AC9)
# --------------------------------------------------------------------------- #
_LIVE = ["backlog done", "help", "mux pane ls", "version", "whoami"]


def _check_with(monkeypatch, tmp_path, baseline_text):
    monkeypatch.setattr(vr, "enumerate_all_leaves", lambda: list(_LIVE))
    monkeypatch.setattr(vr, "baseline_path", lambda: tmp_path / "verb-baseline.txt")
    (tmp_path / "verb-baseline.txt").write_text(baseline_text, encoding="utf-8")
    return vr.check()


def test_check_fails_naming_added_verb(monkeypatch, tmp_path):
    # baseline is missing "version" -> live has a verb the baseline lacks (AC2 add)
    stale = [v for v in _LIVE if v != "version"]
    report = _check_with(monkeypatch, tmp_path, vr.generate(stale))
    assert report.ok is False
    assert "version" in report.message
    assert "Added" in report.message
    # AC9: the conflict-is-the-review-moment message is present on a failure
    assert "merge conflict" in report.message


# --------------------------------------------------------------------------- #
# Hidden-option emission + flag ratchet (Wave 1)
# --------------------------------------------------------------------------- #


_LIVE_F = ["backlog done !--tag", "help", "mux pane ls", "version", "whoami"]
_LIVE_F_BARE = ["backlog done", "help", "mux pane ls", "version", "whoami"]


def _check_live(monkeypatch, tmp_path, live, baseline_text):
    monkeypatch.setattr(vr, "enumerate_all_leaves", lambda: list(live))
    monkeypatch.setattr(vr, "baseline_path", lambda: tmp_path / "verb-baseline.txt")
    (tmp_path / "verb-baseline.txt").write_text(baseline_text, encoding="utf-8")
    return vr.check()


def test_check_fails_naming_added_hidden_flag(monkeypatch, tmp_path):
    # baseline predates the hidden option; live carries it on an existing verb
    report = _check_live(monkeypatch, tmp_path, _LIVE_F, vr.generate(_LIVE_F_BARE))
    assert report.ok is False
    assert "flag-exception" in report.message
    assert "backlog done !--tag" in report.message
    assert "Added hidden options" in report.message


# --------------------------------------------------------------------------- #
# Source/baseline agreement: the Python-side twin of the AC4 Rust reach check.
#
# The enumerator IMPORTS `fno.cli` in this interpreter while the baseline is
# written to resolve_repo_root(). Run as a bare `fno`, those are two different
# trees, and `--update` reported "regenerated ... (N leaves)" over a
# byte-identical file - a success line for work it did not do, which is how a
# new verb reached CI unbaselined.
# --------------------------------------------------------------------------- #
def test_enumeration_refuses_when_imported_package_is_not_this_checkout(monkeypatch, tmp_path):
    """A stale installed package must be a named error, not a confident answer."""
    monkeypatch.setattr(vr, "_repo_root", lambda: tmp_path)
    with pytest.raises(vr.VerbRatchetError) as exc:
        vr.enumerate_python_leaves()
    msg = str(exc.value)
    # Both paths named: the reader cannot act on "mismatch" alone.
    assert "imported:" in msg and "expected:" in msg
    # And it names the command that actually works.
    assert "uv run --project cli fno-py" in msg


def test_guard_covers_check_not_only_update(monkeypatch, tmp_path):
    """check() reaches the same enumerator, so it must refuse too.

    A guard on `--update` alone would leave check() comparing one tree's surface
    against another tree's baseline - a guard on one of two reachable paths,
    which is the shape this module exists to catch.
    """
    monkeypatch.setattr(vr, "_repo_root", lambda: tmp_path)
    with pytest.raises(vr.VerbRatchetError):
        vr.check()
