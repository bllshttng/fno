"""Tracker-owned backlog commands refuse under an external backend (task 4.2).

The census (scripts/diagnostics/tracker-consumers.py --commands) is the
enumerating instrument; these tests pin its load-bearing behaviors in the
suite: every live registry command carries exactly one classification, the
shared refusal fires on the wrapped callback BEFORE any graph read/write,
and an injected unguarded command is named by the detector.
"""
from __future__ import annotations
from tests.fixtures.graph_seed import seed_graph

import json
import os
import subprocess

import pytest
from typer.testing import CliRunner

import fno.graph.cli as graph_cli
from fno.cli import app
from fno.graph.store import read_graph_strict, store_export_status

runner = CliRunner()


def _registry_labels():
    for group, typer_app in graph_cli.iter_backlog_registry():
        for info in typer_app.registered_commands:
            name = info.name or ""
            yield (f"{group} {name}" if group else name), info


def test_every_live_command_is_classified_exactly_once():
    """The registry walk: no unclassified command, no double classification, and
    the two positive controls (a known creation command, a known surviving read
    command) sit in the stated classes."""
    tracker_owned = footnote_owned = 0
    graph_cli.classify_backlog_commands()  # lazy: stamps the markers this reads
    for label, info in _registry_labels():
        cb = info.callback
        assert cb is not None, label
        t = getattr(cb, "_fno_tracker_owned", False)
        f = getattr(cb, "_fno_footnote_owned", False)
        assert t or f, f"unclassified command: {label}"
        assert not (t and f), f"double-classified command: {label}"
        tracker_owned += t
        footnote_owned += f
    # Positive controls - absence means the registry drifted and the census
    # must be re-pinned, not silently passed. The tracker-owned control moved
    # from `new` (now the native create door) to `contain`.
    labels = dict(_registry_labels())
    assert getattr(labels["contain"].callback, "_fno_tracker_owned", False)
    assert getattr(labels["get"].callback, "_fno_footnote_owned", False)
    # Re-pinned when the batch family left the wheel for the native lane,
    # again when capture's nine leaves followed it, again when decompose
    # followed intake: seven tracker-owned commands remain. The next wheel
    # departure re-pins again.
    assert tracker_owned >= 7 and footnote_owned > 7


def test_tracker_owned_commands_refuse_under_external(tmp_path, monkeypatch):
    """The shared guard fires exit 1, names the backend, and no local graph
    write happens (the contradictory file is byte-identical after). The
    tracker-owned leaves answer natively now and their guard rode along, so
    the refusal asserts at the binary under the same env."""
    g = tmp_path / "graph.json"
    seed_graph(g, json.dumps({"entries": []}))
    before = (read_graph_strict(g), store_export_status(g))
    monkeypatch.setenv("FNO_TRACKER_BACKEND", "github")
    monkeypatch.setenv("FNO_CLAIMS_ROOT", str(tmp_path / "claims"))
    from fno.rust_binary import find_dev_binary

    binary = find_dev_binary()
    if binary is None:
        pytest.skip("no dev fno-agents build")
    proc = subprocess.run(
        [str(binary), "backlog", "add", "A thing"],
        capture_output=True,
        text=True,
        env={**os.environ, "FNO_STATE_DIR": str(tmp_path)},
    )
    out = proc.stderr
    assert proc.returncode == 1, out
    assert "github" in out and "refused" in out
    assert (read_graph_strict(g), store_export_status(g)) == before


def test_footnote_owned_read_command_still_works_under_external(tmp_path, monkeypatch):
    """The refusal is scoped: `get` (footnote-owned) still answers through
    the seam under the same backend selection."""
    import fno.rust_binary as rb

    class _Door:
        """The door's read and sidecar-get shapes for one fixture id."""

        def door(self, op, payload):
            if op == "read":
                if payload["id"] == "EXT-1":
                    return {"node": {"id": "EXT-1", "title": "Ext item",
                                     "state": "open", "parent": None,
                                     "blocked_by": []}}
                return {"not_found": True}
            if op == "sidecar-get":
                return {"sidecar": {"id": payload["id"]}}
            raise AssertionError(f"this read never calls {op}")

    _door = _Door()
    real_call = rb.command_call

    def _door_call(command, payload, *args, **kwargs):
        op = payload.get("tracker") if isinstance(payload, dict) else None
        if op:
            return _door.door(op, payload)
        return real_call(command, payload, *args, **kwargs)

    monkeypatch.setattr(rb, "command_call", _door_call)
    g = tmp_path / "graph.json"
    seed_graph(g, json.dumps({"entries": []}))
    monkeypatch.setattr("fno.paths.graph_json", lambda: g)
    monkeypatch.setenv("FNO_TRACKER_BACKEND", "github")

    r = runner.invoke(app, ["backlog", "get", "EXT-1"], catch_exceptions=False)
    assert r.exit_code == 0, r.output
    assert json.loads(r.output)["title"] == "Ext item"
