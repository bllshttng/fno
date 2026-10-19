"""The codex sandbox probe bridge: the verdict is the Rust verb's answer, an
unavailable owner reads unknown, and a guessed reachable is impossible."""
from __future__ import annotations

import shutil
import sys

import pytest

from fno.agents.sandbox_probe import probe_codex_sandbox


def _stub_verb(monkeypatch, answer=None, raises=None):
    """Pin the Rust verb seam; record each request payload."""
    calls: list[dict] = []

    def call(verb, payload, _err):
        calls.append({"verb": verb, **payload})
        if raises is not None:
            raise raises
        return answer if answer is not None else {}

    import fno.rust_binary as rust

    monkeypatch.setattr(rust, "verb_call", call)
    return calls


def test_bridge_maps_the_envelopes_verdict_blocked_and_note(tmp_path, monkeypatch):
    calls = _stub_verb(
        monkeypatch,
        answer={"verdict": "blocked", "note": "no network grant",
                "blocked": [["gh", "error connecting"], ["git", "cannot lock"]]},
    )
    probe = probe_codex_sandbox(tmp_path)
    assert probe.verdict == "blocked"
    assert probe.blocked == [("gh", "error connecting"), ("git", "cannot lock")]
    assert probe.note == "no network grant"
    assert calls == [{"verb": "sandbox-probe", "cwd": str(tmp_path), "mode": None}]


def test_an_absent_verdict_reads_unknown_never_reachable(tmp_path, monkeypatch):
    _stub_verb(monkeypatch, answer={"note": "half an answer"})
    probe = probe_codex_sandbox(tmp_path)
    assert probe.verdict == "unknown"


def test_an_unavailable_owner_is_unknown_with_the_reason(tmp_path, monkeypatch):
    from fno.rust_binary import VerbUnavailable

    _stub_verb(monkeypatch, raises=VerbUnavailable("no binary"))
    probe = probe_codex_sandbox(tmp_path)
    assert probe.verdict == "unknown"
    assert "no binary" in probe.note


@pytest.mark.smoke
@pytest.mark.skipif(
    shutil.which("codex") is None or sys.platform != "darwin",
    reason="needs the codex binary and macOS seatbelt",
)
def test_live_ref_lock_without_the_grant_is_blocked(tmp_path, monkeypatch):
    repo = tmp_path / "repo"
    repo.mkdir()
    import subprocess

    for argv in (
        ["git", "init", "-q"],
        ["git", "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "x"],
    ):
        subprocess.run(argv, cwd=repo, check=True)
    probe = probe_codex_sandbox(repo)
    assert probe.verdict == "blocked", probe
