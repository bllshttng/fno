"""x-f8b1: the bind routes - one fact (the plan's node id) under two spellings.

``plan_claims`` (``fno.graph._intake``) is the parser authority; blueprint's
``mutate_doc.py`` re-spells the same rule because a skill script must stay
portable, and ``fno do target init`` first-binds at dispatch. The parity
harness feeds the SAME frontmatter to both readers so the spellings cannot
silently diverge again (x-7760 authored ``claims:`` while the only bind
writer keyed on ``node:``); the per-route tests pin each writer on top.
"""
from __future__ import annotations

import importlib.util
import subprocess
from pathlib import Path

import pytest
import yaml

from fno import target_cli
from fno.graph._intake import plan_claims

REPO_ROOT = Path(__file__).resolve().parents[3]
MUTATE_DOC_PATH = REPO_ROOT / "skills" / "blueprint" / "scripts" / "mutate_doc.py"
VALIDATE_PLAN = REPO_ROOT / "skills" / "blueprint" / "scripts" / "validate-plan.sh"


@pytest.fixture(scope="module")
def mutate_doc():
    spec = importlib.util.spec_from_file_location("mutate_doc_under_test", MUTATE_DOC_PATH)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def _write_fm(tmp_path: Path, name: str, fm: dict) -> Path:
    text = "---\n" + yaml.safe_dump(fm, sort_keys=False) + "---\n# body\n"
    path = tmp_path / name
    path.write_text(text, encoding="utf-8")
    return path


# -- the two-spelling parity harness (change 6) --

PARITY_ROWS = [
    # (name, frontmatter, the one id the bind resolves, or None)
    ("node-only", {"node": "x-7760"}, "x-7760"),
    ("claims-string", {"claims": "x-7760"}, "x-7760"),
    ("claims-list", {"claims": ["x-7760", "x-2222"]}, "x-7760"),
    ("both-agreeing", {"node": "x-7760", "claims": "x-7760"}, "x-7760"),
    ("both-disagreeing", {"node": "x-1111", "claims": "x-2222"}, None),
    ("neither", {"status": "ready"}, None),
]


def test_two_spelling_parity_table(mutate_doc, tmp_path):
    for name, fm, expected in PARITY_ROWS:
        path = _write_fm(tmp_path, f"{name}.md", fm)
        claimed = plan_claims(str(path))
        bind_id, warning = mutate_doc._bind_node_id(fm)
        if expected is None:
            assert bind_id is None, name
            if name == "both-disagreeing":
                # A broken plan binds nothing, and the parser offers no single
                # id either: both spellings are on record, neither is the bind.
                assert warning and "x-1111" in warning and "x-2222" in warning
                assert claimed == {"x-1111", "x-2222"}
            else:
                assert not warning
                assert claimed == set(), name
        else:
            assert bind_id == expected, name
            assert not warning, name
            assert expected in claimed, name


def test_resolver_rejects_empty_and_null_sentinels(mutate_doc):
    assert mutate_doc._bind_node_id({"node": ""}) == (None, None)
    assert mutate_doc._bind_node_id({"node": "null"}) == (None, None)
    assert mutate_doc._bind_node_id({"claims": ["", "null", "x-3333"]}) == ("x-3333", None)


# -- change 1: _sync_graph_status binds under both spellings --

def _arm_sync(monkeypatch, mutate_doc, calls):
    monkeypatch.setattr(
        mutate_doc.shutil, "which", lambda name: "/usr/bin/fno" if name == "fno" else None
    )

    class _Result:
        returncode = 0
        stderr = ""
        stdout = ""

    monkeypatch.setattr(
        mutate_doc.subprocess, "run", lambda cmd, **k: calls.append(cmd) or _Result()
    )


def test_sync_graph_status_binds_claims_only_plan(mutate_doc, monkeypatch, tmp_path):
    calls: list = []
    _arm_sync(monkeypatch, mutate_doc, calls)
    plan = tmp_path / "p.md"
    plan.write_text("# plan", encoding="utf-8")

    mutate_doc._sync_graph_status({"claims": "x-7760"}, plan)

    assert calls == [["fno", "backlog", "update", "x-7760", "plan_path", str(plan)]]


def test_sync_graph_status_binds_node_only_plan(mutate_doc, monkeypatch, tmp_path):
    calls: list = []
    _arm_sync(monkeypatch, mutate_doc, calls)

    mutate_doc._sync_graph_status({"node": "x-7760"}, tmp_path / "p.md")

    assert calls and calls[0][3] == "x-7760"


def test_sync_graph_status_refuses_disagreement(mutate_doc, monkeypatch, tmp_path, capsys):
    calls: list = []
    _arm_sync(monkeypatch, mutate_doc, calls)

    mutate_doc._sync_graph_status(
        {"node": "x-1111", "claims": "x-2222"}, tmp_path / "p.md"
    )

    assert calls == []  # bound nothing
    err = capsys.readouterr().err
    assert "x-1111" in err and "x-2222" in err


# -- change 2: init first-binds the graph pointer --

def _stub_run(calls, rc=0):
    class _Result:
        returncode = rc
        stderr = ""
        stdout = ""

    import subprocess as _subprocess

    real_run = _subprocess.run  # captured before the stub replaces it

    def _run(cmd, *a, **k):
        calls.append(list(cmd))
        if any(str(part).endswith("fno-agents") for part in cmd[:1]):
            # The hold verdict's receipt call is real infrastructure.
            return real_run(cmd, capture_output=True, text=True, check=False)
        return _Result()

    return _run














# -- change 3: the validator names the bind it cannot perform --

_LEGACY_PLAN = """---
node: {node}
status: ready
created: 2026-08-01
project: test
---
# Test plan

Body text.
"""


def _run_validator(plan: Path):
    return subprocess.run(
        ["bash", str(VALIDATE_PLAN), str(plan)],
        capture_output=True,
        text=True,
        timeout=180,
    )


def test_validator_pass_prints_bind_line(tmp_path):
    plan = tmp_path / "node-bearing.md"
    plan.write_text(_LEGACY_PLAN.format(node="x-7760"), encoding="utf-8")

    proc = _run_validator(plan)

    assert proc.returncode == 0, proc.stdout + proc.stderr
    assert (
        f"fno backlog update x-7760 plan_path={plan}" in proc.stderr
    ), proc.stderr


def test_validator_id_less_plan_prints_no_bind_line(tmp_path):
    plan = tmp_path / "id-less.md"
    plan.write_text(_LEGACY_PLAN.format(node=""), encoding="utf-8")

    proc = _run_validator(plan)

    assert proc.returncode == 0, proc.stdout + proc.stderr
    assert "fno backlog update" not in proc.stderr
