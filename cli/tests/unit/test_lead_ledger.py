"""``fno agents lead ledger``: Python resolves the team and the paths; the
native lead-rundown verb owns the page assembly (the lead-history split).

The renderer's own truth lives in the Rust tests; these pin the Python-side
plumbing: the team gather + fold, the binary relay's argv, and the refusal
when the binary is missing.
"""
from __future__ import annotations

import json
from pathlib import Path

import pytest



def _role(**kw):
    base = {
        "holder": "lead",
        "level": 2,
        "scope": "e-1",
        "grantor": "human",
        "status": "busy",
        "agree": True,
        "reason": None,
        "role_source": "row",
        "scope_nodes": {
            "status": "ok",
            "counts": {"in_progress": 1, "done": 2},
            "total": 3,
            "omitted": 0,
            "nodes": [],
        },
    }
    base.update(kw)
    return base


def test_relay_hands_the_native_renderer_team_graph_and_out(
    tmp_path, monkeypatch
):
    from fno.lead import ledger as ledger_module
    from fno.lead.ledger import write_ledger

    team = {"roles": [_role()], "summary": {"total": 1}}
    seen = {}

    class Proc:
        returncode = 0
        stdout = ""
        stderr = ""

    def fake_run(argv, **kwargs):
        seen["argv"] = argv
        seen["team"] = json.loads(kwargs["input"])
        Path(argv[argv.index("--out") + 1]).write_text("<html></html>", encoding="utf-8")
        return Proc()

    stub = tmp_path / "stub-fno-agents"
    stub.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
    stub.chmod(0o755)
    monkeypatch.setattr(
        "fno.rust_binary.resolve_binary", lambda: str(stub)
    )
    monkeypatch.setattr(ledger_module.subprocess, "run", fake_run)

    out = tmp_path / "page.html"
    assert write_ledger(team, out) == out
    argv = seen["argv"]
    assert argv[1] == "lead-rundown"
    assert argv[argv.index("--team-json") + 1] == "-"
    assert seen["team"] == team
    assert "--graph" in argv
    assert out.exists()


def test_relay_surfaces_a_native_failure(tmp_path, monkeypatch):
    from fno.lead.ledger import write_ledger

    class Proc:
        returncode = 1
        stdout = ""
        stderr = "graph unreadable: no such file"

    monkeypatch.setattr(
        "fno.rust_binary.resolve_binary", lambda: "/bin/true"
    )
    monkeypatch.setattr(
        "fno.lead.ledger.subprocess",
        type("M", (), {"run": staticmethod(lambda argv, **k: Proc())}),
    )

    with pytest.raises(RuntimeError, match="graph unreadable"):
        write_ledger({"roles": []}, tmp_path / "x.html")
