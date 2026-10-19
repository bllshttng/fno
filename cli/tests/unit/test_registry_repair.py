"""Twice by hand is a verb.

The operator repaired a poisoned `registry.json` by hand on 2026-08-28, and a
surviving `.bak.poison-repair` sibling shows someone did the same on
2026-08-09. Both times the steps were identical: hold the lock, assert no row
carries data belonging to the newer schema, back up, drop the empty unknown
keys, set the integer, replace atomically.

The third step is the point. The 2026-08-28 rollback was lossless BY LUCK: all
33 rows happened to carry the v20 fields empty. One real value written five
minutes later and no safe rollback would have existed. This verb turns that
luck into an assertion.

Prevention is not retroactive, which is why this ships even though the write
guard drives its expected frequency toward zero: a worktree that already wrote
a raised version leaves a file only a hand edit clears.
"""
from __future__ import annotations

import json
from pathlib import Path

import pytest

from fno.agents import registry as reg


_SESSIONS = iter(range(1, 10_000))


def _row(name: str, **extra) -> dict:
    # Each row its own session: the registry table keys rows on it.
    row = {
        "name": name,
        "cwd": "/Users/x/proj",
        "log_path": "/Users/x/proj/.fno/log",
        "harness": "claude",
        "harness_session_id": f"9a063cd3-69d4-415a-ada5-{next(_SESSIONS):012d}",
    }
    row.update(extra)
    return row


def _document(path: Path) -> dict:
    from fno.registry_door import read_registry_document

    return read_registry_document(path)[0]


def _poisoned(path: Path, version: int, rows: list[dict]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps({"schema_version": version, "agents": rows}, indent=2),
        encoding="utf-8",
    )


# --------------------------------------------------------------------------
# AC5-HP: the repair
# --------------------------------------------------------------------------


def test_apply_backs_up_restores_the_version_and_keeps_every_row(
    tmp_path: Path,
) -> None:
    path = tmp_path / "registry.json"
    ahead = reg.SCHEMA_VERSION + 1
    _poisoned(
        path,
        ahead,
        [
            _row("worker-1", a_field_from_the_future=None),
            _row("worker-2", a_field_from_the_future=""),
            _row("worker-3"),
        ],
    )

    plan = reg.repair_registry_schema(reg.SCHEMA_VERSION, path=path, apply=True)

    data = _document(path)
    assert data["schema_version"] == reg.SCHEMA_VERSION
    assert [a["name"] for a in data["agents"]] == ["worker-1", "worker-2", "worker-3"]
    assert all("a_field_from_the_future" not in a for a in data["agents"])
    assert plan.dropped == {
        "worker-1": ["a_field_from_the_future"],
        "worker-2": ["a_field_from_the_future"],
    }

    backups = list(tmp_path.glob("registry.json.bak.schema-repair-*"))
    assert len(backups) == 1
    assert json.loads(backups[0].read_text())["schema_version"] == ahead


# --------------------------------------------------------------------------
# AC6-ERR: the assertion that makes it safe
# --------------------------------------------------------------------------


def test_a_row_carrying_real_newer_schema_data_refuses(tmp_path: Path) -> None:
    path = tmp_path / "registry.json"
    _poisoned(
        path,
        reg.SCHEMA_VERSION + 1,
        [
            _row("worker-1"),
            _row("worker-2", a_field_from_the_future="a real value"),
        ],
    )
    before = _document(path)

    with pytest.raises(reg.RegistryRepairRefused) as excinfo:
        reg.repair_registry_schema(reg.SCHEMA_VERSION, path=path, apply=True)

    message = str(excinfo.value)
    assert "worker-2" in message
    assert "a_field_from_the_future" in message
    assert _document(path) == before
    assert list(tmp_path.glob("*.bak.*")) == []


@pytest.mark.parametrize("value", [0])
def test_a_falsy_but_real_value_is_still_data(tmp_path: Path, value) -> None:
    """``0`` and ``False`` are values a newer schema meant to store. Only
    ``None`` and an empty string, list, or object read as "never written"."""
    path = tmp_path / "registry.json"
    _poisoned(
        path,
        reg.SCHEMA_VERSION + 1,
        [_row("worker-1", a_field_from_the_future=value)],
    )

    with pytest.raises(reg.RegistryRepairRefused):
        reg.repair_registry_schema(reg.SCHEMA_VERSION, path=path, apply=True)


# --------------------------------------------------------------------------
# It refuses rather than guessing
# --------------------------------------------------------------------------


@pytest.mark.parametrize("on_disk_delta", [0])
def test_a_version_not_strictly_above_the_target_refuses(
    tmp_path: Path, on_disk_delta: int
) -> None:
    path = tmp_path / "registry.json"
    _poisoned(path, reg.SCHEMA_VERSION + on_disk_delta, [_row("worker-1")])

    with pytest.raises(reg.RegistryRepairRefused):
        reg.repair_registry_schema(reg.SCHEMA_VERSION, path=path, apply=True)


# --------------------------------------------------------------------------
# The verb itself
# --------------------------------------------------------------------------


def test_the_cli_verb_is_registered_hidden_and_dry_runs_by_default(
    tmp_path: Path,
) -> None:
    from typer.testing import CliRunner

    from fno.agents.cli import agents_app

    path = tmp_path / "registry.json"
    _poisoned(
        path,
        reg.SCHEMA_VERSION + 1,
        [_row("worker-1", a_field_from_the_future=None)],
    )
    before = _document(path)

    result = CliRunner().invoke(
        agents_app,
        ["registry-repair", "--to", str(reg.SCHEMA_VERSION), "--path", str(path)],
    )

    assert result.exit_code == 0, result.output
    assert "worker-1" in result.output
    assert "a_field_from_the_future" in result.output
    assert _document(path) == before

    applied = CliRunner().invoke(
        agents_app,
        [
            "registry-repair",
            "--to",
            str(reg.SCHEMA_VERSION),
            "--path",
            str(path),
            "--apply",
        ],
    )
    assert applied.exit_code == 0, applied.output
    assert _document(path)["schema_version"] == reg.SCHEMA_VERSION
