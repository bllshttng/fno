"""Tests for the Codex/OpenCode generated-handle collision census."""

from __future__ import annotations

import json
import sqlite3
from datetime import datetime, timezone
from pathlib import Path

import pytest

from fno.agents import handle_collisions


NOW = datetime(2026, 7, 30, 12, tzinfo=timezone.utc).timestamp()
RECENT = "2026-07-29T12:00:00Z"
OLD = "2026-07-01T12:00:00Z"


def _epoch_ms(timestamp: str) -> int:
    return int(datetime.fromisoformat(timestamp).timestamp() * 1000)


def _rollouts(root: Path, rows: list[tuple[str, str]]) -> None:
    root.mkdir(parents=True, exist_ok=True)
    for index, (session_id, timestamp) in enumerate(rows):
        payload = {
            "type": "session_meta",
            "payload": {"id": session_id, "timestamp": timestamp, "cwd": "/project"},
        }
        (root / f"rollout-{index}.jsonl").write_text(json.dumps(payload) + "\n")


def _database(path: Path, rows: list[tuple[str, object]]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    con = sqlite3.connect(path)
    con.execute("CREATE TABLE session (id TEXT, time_created INTEGER)")
    con.executemany("INSERT INTO session VALUES (?, ?)", rows)
    con.commit()
    con.close()


def _census(tmp_path: Path, codex_rows, opencode_rows):
    codex = tmp_path / "codex"
    database = tmp_path / "opencode.db"
    _rollouts(codex, codex_rows)
    _database(database, [(sid, _epoch_ms(stamp)) for sid, stamp in opencode_rows])
    return handle_collisions.run_census(
        codex_sessions_dir=codex,
        opencode_db_path=database,
        opencode_storage_dir=tmp_path / "storage",
        window_days=7,
        now=NOW,
    )


def test_ac1_hp_measures_uuidv7_and_ses_prefix_clusters_without_duplicates(tmp_path):
    """AC1-HP: full/window metrics compare legacy last-eight with canonical first-eight."""
    codex_a = "019fb417-aaaa-7e73-b8b4-000000000001"
    opencode_a = "ses_abcd11111111"
    reports = _census(
        tmp_path,
        [
            (codex_a, RECENT),
            (codex_a, RECENT),
            ("019fb417-bbbb-7e73-b8b4-000000000002", RECENT),
            ("019fa000-cccc-7e73-b8b4-000000000003", OLD),
        ],
        [
            (opencode_a, RECENT),
            (opencode_a, RECENT),
            ("ses_abcd22222222", RECENT),
            ("ses_old_33333333", OLD),
        ],
    )

    for report in reports.values():
        assert report.full.session_count == 3
        assert report.full.legacy == handle_collisions.CollisionMetrics(0, 0, 1)
        assert report.full.canonical == handle_collisions.CollisionMetrics(1, 2, 2)
        assert report.window.session_count == 2
        assert report.window.legacy == handle_collisions.CollisionMetrics(0, 0, 1)
        assert report.window.canonical == handle_collisions.CollisionMetrics(1, 2, 2)


def test_ac4_err_codex_corrupt_is_not_reported_as_zero(tmp_path):
    """AC4-ERR: a corrupt Codex corpus fails closed."""
    codex = tmp_path / "codex"
    codex.mkdir()
    (codex / "rollout-bad.jsonl").write_text("{broken")
    database = tmp_path / "opencode.db"
    _database(database, [("ses_one", _epoch_ms(RECENT))])

    with pytest.raises(handle_collisions.CorpusUnavailable, match="codex"):
        handle_collisions.run_census(
            codex_sessions_dir=codex,
            opencode_db_path=database,
            opencode_storage_dir=tmp_path / "storage",
            window_days=7,
            now=NOW,
        )


def test_ac4_err_opencode_bad_record_is_not_reported_as_zero(tmp_path):
    """AC4-ERR: a malformed OpenCode record fails closed."""
    codex = tmp_path / "codex"
    _rollouts(codex, [("codex-one", RECENT)])
    database = tmp_path / "opencode" / "opencode.db"
    _database(database, [("ses_bad", "not-an-integer")])

    with pytest.raises(handle_collisions.CorpusUnavailable, match="opencode"):
        handle_collisions.run_census(
            codex_sessions_dir=codex,
            opencode_db_path=database,
            opencode_storage_dir=tmp_path / "storage",
            window_days=7,
            now=NOW,
        )


def test_ac4_err_main_exits_nonzero_for_canonical_window_collision(tmp_path):
    """AC4-ERR: a canonical collision in either discovery window fails the check."""
    codex = tmp_path / "codex"
    database = tmp_path / "opencode.db"
    _rollouts(codex, [("codex-one", RECENT), ("codex-two", RECENT)])
    _database(database, [("ses_unique", _epoch_ms(RECENT))])
    code = handle_collisions.main(
        ["--codex-sessions-dir", str(codex), "--opencode-db", str(database)],
        now=NOW,
        canonical_fn=lambda _session_id: "collision",
    )
    assert code == 1


def test_ac4_err_main_emits_unavailable_not_zero_metrics(tmp_path, capsys):
    """AC4-ERR: unavailable evidence is named without fake zero rows."""
    code = handle_collisions.main(
        [
            "--codex-sessions-dir",
            str(tmp_path / "missing-codex"),
            "--opencode-db",
            str(tmp_path / "missing.db"),
            "--opencode-storage-dir",
            str(tmp_path / "missing-storage"),
        ],
        now=NOW,
    )
    captured = capsys.readouterr()
    assert code == 2
    assert "unavailable" in captured.err.lower()
    assert "sessions=0" not in captured.out
