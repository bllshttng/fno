"""Bank loader + load-time validation (US1, AC4-EDGE).

The load-time discipline is the point: a task without a mechanical grade is
rejected naming the id and file, and an all-trivial grade warns.
"""
from __future__ import annotations

from pathlib import Path

import pytest

from fno.evals import bank


def _write(p: Path, text: str) -> Path:
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(text, encoding="utf-8")
    return p


VALID = """
id: t-valid
tier: regression
grade:
  - kind: exit
    command: "pytest -q"
    expect: 0
  - kind: file-exists
    path: out.txt
  - kind: grep
    path: out.txt
    pattern: PASS
timeout_minutes: 10
tags: [smoke]
"""


def test_load_valid_task(tmp_path: Path) -> None:
    task = bank.load_task(_write(tmp_path / "t.yaml", VALID))
    assert task.id == "t-valid"
    assert task.tier == "regression"
    assert task.prompt is None  # grade-only
    assert [c.kind for c in task.grade] == ["exit", "file-exists", "grep"]
    assert task.grade[0].command == "pytest -q"
    assert task.grade[2].pattern == "PASS"
    assert task.timeout_minutes == 10
    assert task.tags == ["smoke"]


# AC4-EDGE: a bank YAML with an empty grade list exits non-zero naming id + file.
def test_empty_grade_rejected_naming_id_and_file(tmp_path: Path) -> None:
    p = _write(tmp_path / "bad.yaml", "id: t-nograde\ntier: capability\ngrade: []\n")
    with pytest.raises(bank.BankError) as exc:
        bank.load_task(p)
    msg = str(exc.value)
    assert "t-nograde" in msg
    assert "bad.yaml" in msg


# Silent-failure-hunter countermeasure: an all-trivial exit grade warns.
def test_decorative_grade_warns(tmp_path: Path) -> None:
    p = _write(tmp_path / "d.yaml",
               "id: t-deco\ntier: regression\ngrade:\n  - {kind: exit, command: 'true'}\n")
    with pytest.warns(UserWarning, match="always passes"):
        bank.load_task(p)


def test_discover_bank_duplicate_id_rejected(tmp_path: Path) -> None:
    _write(tmp_path / "one.yaml", "id: dup\ntier: regression\ngrade:\n  - {kind: exit, command: 'pytest'}\n")
    _write(tmp_path / "two.yaml", "id: dup\ntier: capability\ngrade:\n  - {kind: exit, command: 'pytest'}\n")
    with pytest.raises(bank.BankError, match="duplicate"):
        bank.discover_bank(tmp_path)


def test_seed_bank_loads() -> None:
    """The committed seed bank must itself be valid (self-demonstrating)."""
    repo_root = Path(__file__).resolve().parents[3]
    seed = repo_root / "evals" / "bank"
    if not seed.is_dir():
        pytest.skip("seed bank not present in this checkout")
    tasks = bank.discover_bank(seed)
    tiers = {t.tier for t in tasks}
    assert "capability" in tiers and "regression" in tiers
    assert any("ci-flake" in t.tags for t in tasks)


# --------------------------------------------------------------------------- #
# lane resolution (x-fd52 AC1-HP/AC1-ERR): join a NAME against the existing
# config.routing.models inventory - never a second model/effort enum.
# --------------------------------------------------------------------------- #


def test_resolve_lane_reads_matching_routing_models_row(monkeypatch) -> None:
    """resolve_lane rides resolve_inventory: the same fold the slot lanes
    join against, never a second model/effort enum."""
    from fno import route_resolve as rr

    row = rr.InventoryRow(name="astra-high", harness="codex",
                          model="gpt-6-astra", effort="high")
    inv = rr.Inventory(rows={"astra-high": row}, declared=True)
    monkeypatch.setattr(rr, "resolve_inventory", lambda: inv)
    coord = bank.resolve_lane("astra-high")
    assert coord.name == "astra-high"
    assert coord.harness == "codex"
    assert coord.model == "gpt-6-astra"
    assert coord.effort == "high"


def test_resolve_lane_unknown_name_raises_naming_known_lanes(monkeypatch) -> None:
    from fno import route_resolve as rr

    rows = {n: rr.InventoryRow(name=n, harness="", model="")
            for n in ("astra-high", "sol-low")}
    monkeypatch.setattr(rr, "resolve_inventory", lambda: rr.Inventory(rows=rows, declared=True))
    with pytest.raises(bank.LaneError, match=r"astra-high.*sol-low|sol-low.*astra-high"):
        bank.resolve_lane("no-such-lane")
