"""`fno carveout update` - correct a carve-out without changing its identity.

The verb exists because the old correction path was `resolve` then `add`, which
has two defects a test can pin. It minted a new id, so an id already quoted in a
PR body or a mail became a dead pointer. And it was lossy: two writes, and a
failure between them left the ledger holding neither row. That happened live.
"""
from __future__ import annotations

from pathlib import Path

import pytest
from typer.testing import CliRunner

from fno.carveout.core import (
    CarveoutError,
    CarveoutNotFound,
    add_carveout,
    read_carveouts,
    update_carveout,
)
from fno.cli import app

runner = CliRunner()


@pytest.fixture
def ledger_root(tmp_path, monkeypatch) -> Path:
    """A repo root whose carve-out ledger is a temp file, for core and CLI alike."""
    (tmp_path / ".fno").mkdir()
    import fno.carveout.core as core

    # The CLI imports this inside each command, so patching the core module is
    # the only patch needed; there is no module-level name on the cli module.
    monkeypatch.setattr(core, "resolve_carveout_root", lambda: tmp_path)
    monkeypatch.setattr("fno.paths.resolve_repo_root", lambda *a, **k: tmp_path)
    monkeypatch.delenv("CLAUDECODE_SESSION_ID", raising=False)
    return tmp_path


def _seed(root: Path, **over) -> str:
    kwargs = {"kind": "deferred", "description": "worded badly"}
    kwargs.update(over)
    cv, _ = add_carveout(root, storage_root=root, **kwargs)
    return cv.id


def _rows(root: Path) -> list[dict]:
    return read_carveouts(root)


# -- the identity guarantee --


def test_the_id_survives_an_edit(ledger_root):
    cid = _seed(ledger_root)
    rec = update_carveout(ledger_root, cid, description="worded correctly")
    assert rec["id"] == cid
    assert rec["description"] == "worded correctly"


def test_one_edit_leaves_exactly_one_row(ledger_root):
    """The lossy window closed: one locked rewrite, not a remove plus an append."""
    cid = _seed(ledger_root)
    update_carveout(ledger_root, cid, description="new text")
    rows = [r for r in _rows(ledger_root) if r["id"] == cid]
    assert len(rows) == 1


# -- refusals --


def test_an_absent_id_is_refused_not_created(ledger_root):
    """Create-on-miss would resurrect a row /fno:ship pr merged already consumed."""
    _seed(ledger_root)
    with pytest.raises(CarveoutNotFound):
        update_carveout(ledger_root, "cv-deadbeef", description="new")
    assert len(_rows(ledger_root)) == 1


def test_an_unwritable_ledger_raises_rather_than_reporting_a_clean_noop(
    ledger_root, monkeypatch
):
    """The contract that differs from consume_carveouts.

    That function returns 0 for both "already gone" and "could not write",
    because both leave its caller's invariant intact. Here they differ: telling
    an operator the correction landed while the old wording is still on disk is
    the failure this whole node is about.
    """
    cid = _seed(ledger_root)

    def _boom(*a, **k):
        raise OSError("read-only file system")

    monkeypatch.setattr("fno.carveout.core._rewrite_jsonl", _boom)
    with pytest.raises(CarveoutError):
        update_carveout(ledger_root, cid, description="new text")


def test_a_torn_write_leaves_the_whole_ledger_intact(ledger_root, monkeypatch):
    """The ledger is replaced atomically, not truncated in place.

    An in-place `write_text` that dies between truncate and flush loses EVERY
    carve-out, not just the row being edited. The temp-file-plus-replace shape
    means a failure at the last step leaves the original bytes untouched.
    """
    import os

    first = _seed(ledger_root, description="row one")
    second = _seed(ledger_root, description="row two")
    from fno.paths import project_log
    path = project_log("carveouts.jsonl", project_root=ledger_root)
    before = path.read_text()

    def _boom(*a, **k):
        raise OSError("interrupted at the rename")

    monkeypatch.setattr(os, "replace", _boom)
    with pytest.raises(CarveoutError):
        update_carveout(ledger_root, second, description="corrected")

    assert path.read_text() == before
    assert {r["id"] for r in _rows(ledger_root)} == {first, second}


# -- the CLI surface --


def test_no_field_given_is_a_usage_error(ledger_root):
    """A no-op that prints a success line is the lie this verb exists to stop."""
    cid = _seed(ledger_root)
    res = runner.invoke(app, ["carveout", "update", cid])
    assert res.exit_code == 2
    assert "nothing to update" in res.output


def test_crossing_the_backfill_boundary_warns_and_still_applies(ledger_root):
    """A warning, not a refusal: the row changes consumer, but the edit is legitimate."""
    cid = _seed(ledger_root, kind="deferred")
    res = runner.invoke(app, ["carveout", "update", cid, "-k", "backfill"])
    assert res.exit_code == 0, res.output
    assert "/fno:ship pr merged" in res.output
    assert _rows(ledger_root)[0]["kind"] == "backfill"
