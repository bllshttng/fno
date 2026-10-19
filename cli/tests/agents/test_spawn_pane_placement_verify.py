"""Bounded placement verification: a pane absent from the listing fails closed
(reap, no registry row) with a message naming the absence, and a missing row
is re-listed once before that verdict. Companion to
fno/agents/placement_verify.py."""

import json
import subprocess
from pathlib import Path

import pytest

from tests.agents.test_spawn_pane import FakeRunner, _spawn


def test_bounded_verification_absent_row_names_the_listing_not_wrong_tab(
    tmp_path: Path, monkeypatch
) -> None:
    # The pane was created (it gets reaped), yet the listing cannot see it.
    # That must read as an ABSENCE from a non-empty listing, never as
    # "wrong tab" - the two causes get different fixes.
    from fno.agents.mux_spawn import DispatchAskError
    from fno.agents.registry import load_registry

    listing = [
        {"pane_id": 5, "squad_id": 1, "tab_id": 12, "cwd": "/w", "child_pid": 4005},
        {"pane_id": 6, "squad_id": 1, "tab_id": 12, "cwd": "/w", "child_pid": 4006},
    ]
    runner = FakeRunner(ls_stdout=json.dumps(listing))
    with pytest.raises(DispatchAskError, match="absent from a 2-pane listing"):
        _spawn(monkeypatch, tmp_path, tab_id="id:12", runner=runner)
    assert runner.kill_calls
    assert load_registry() == []


def test_bounded_verification_relists_once_when_the_pane_is_missing(
    tmp_path: Path, monkeypatch
) -> None:
    # A read-after-write race: one absent listing re-lists; the second read
    # seeing the pane keeps the worker alive.
    monkeypatch.setattr("time.sleep", lambda _s: None)

    class LateListingRunner(FakeRunner):
        def __init__(self, **kwargs) -> None:
            super().__init__(**kwargs)
            self.ls_calls = 0

        def __call__(self, argv, **kwargs):
            if argv[1:4] == ["mux", "pane", "ls"]:
                self.ls_calls += 1
                if self.ls_calls == 2:
                    return subprocess.CompletedProcess(argv, 0, "[]", "")
            return super().__call__(argv, **kwargs)

    runner = LateListingRunner(
        ls_stdout=json.dumps(
            [{"pane_id": 7, "squad_id": 1, "tab_id": 12, "cwd": "/w", "child_pid": 4242}]
        )
    )
    _spawn(monkeypatch, tmp_path, tab_id="id:12", runner=runner)
    assert runner.ls_calls >= 3
    assert runner.kill_calls == []
