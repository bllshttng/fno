"""A seeded footnote pane spawn submits its seed.

footnote's capability row sets ``ready_marker = "unsupported"``, so the
spawn types the seed the moment a frame paints. That send used to reach the
mux before any registry row tied the pane to its session, and the send gate
refused it (exit 23): a labelled pane whose session id resolves to nothing
must not be typed into. The spawn knows the id (it minted it and rode it
into the launch spec), so the run transport now carries ``--fno-id`` and the
server records the resume birthright the gate reads.

These tests pin the Python half with the mux subprocess faked at the
``runner`` seam (the Rust gate family in
``crates/fno/src/server/tests/pane_send_gate_tests.rs`` pins the server
half): a seeded footnote spawn submits through the same unaddressed send,
and the run transport carries the preassigned id. A provider whose binding
strategy is not ``preassigned`` keeps its transport unchanged.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from tests.agents.test_spawn_pane import _spawn


@pytest.fixture(autouse=True)
def _graphless_seed_resolver(monkeypatch):
    """The seed-word gate fails open on an unreadable graph; pin that here so
    the plumbing tests' fake node words never depend on the ambient store."""

    def _unreadable(path=None, **_):
        raise ValueError("graph store unreadable")

    monkeypatch.setattr("fno.graph.load.load_graph", _unreadable)


def _stub_spec(monkeypatch) -> dict:
    """Answer the Rust owner's spec ask with a path, capturing the fno_id."""
    captured: dict = {}

    def _fake_spec(cwd, model, permission_mode, fno_id=None):
        captured["fno_id"] = fno_id
        return "/tmp/footnote-spec.json"

    monkeypatch.setattr(
        "fno.agents.spawn_axes_client.footnote_pane_spec", _fake_spec
    )
    return captured


def test_seeded_footnote_pane_spawn_submits_and_names_the_preassigned_id(
    tmp_path: Path, monkeypatch
) -> None:
    spec = _stub_spec(monkeypatch)
    _, runner = _spawn(
        monkeypatch, tmp_path, provider="footnote", message="Reply READY."
    )

    run_call = next(c for c in runner.calls if c[1:4] == ["mux", "pane", "run"])
    # The spawn preassigns the id and hands the SAME one to the launch spec.
    assert "--fno-id" in run_call, f"run transport must carry the id: {run_call}"
    assert run_call[run_call.index("--fno-id") + 1] == spec["fno_id"]

    # The seed submits: readiness painted, so the send goes out. Before the
    # birthright existed this send was the exit-23 refusal on a live server.
    send_calls = [c for c in runner.calls if c[1:4] == ["mux", "pane", "send"]]
    assert send_calls, "a seeded footnote spawn must submit its seed"
    send_call = send_calls[0]
    assert "--text" in send_call
    assert "Reply READY." in send_call[send_call.index("--text") + 1]
    # The seed rides the ordinary unaddressed send; the addressability lives
    # in the run transport's birthright, not in a per-send flag.
    assert "--fno-id" not in send_call

    # The registry row binds the same id: the preassigned binding closes the
    # window the birthright covers.
    from fno.agents.registry import load_registry

    rows = load_registry()
    assert len(rows) == 1
    assert rows[0].harness_session_id == spec["fno_id"]


def test_non_preassigned_provider_run_keeps_its_transport_unchanged(
    tmp_path: Path, monkeypatch
) -> None:
    # claude's binding strategy is `preassigned-or-session-start`, never the
    # plain preassigned arm, so no birthright flag rides its run transport.
    _, runner = _spawn(monkeypatch, tmp_path, provider="claude", message="hi")
    run_call = next(c for c in runner.calls if c[1:4] == ["mux", "pane", "run"])
    assert "--fno-id" not in run_call
