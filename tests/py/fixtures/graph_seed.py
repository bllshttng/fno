"""Seed fixture rows into the SQLite store, bypassing intake policy."""

from __future__ import annotations

import json
import sqlite3
import sys
from pathlib import Path
from typing import Iterable


def _store_is_live(connection: sqlite3.Connection, tables: set[str]) -> bool:
    """True once the native one-time setup has stamped the store.

    Mirrors backlog's schema_needs_ensure: schema 4 plus open setup 1. Past
    that point no later open folds parked blob rows, so seeds must go
    through the live write path.
    """
    if "graph_meta" not in tables:
        return False
    versions = dict(
        connection.execute(
            "SELECT key, value FROM graph_meta"
            " WHERE key IN ('schema_version', 'open_setup_version')"
        )
    )
    try:
        schema = int(versions.get("schema_version", "0"))
        setup = int(versions.get("open_setup_version", "0"))
    except ValueError:
        return False
    return schema >= 4 and setup >= 1


def seed_graph(path: Path, entries: Iterable[dict] | str | bytes | dict) -> list[dict]:
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    if isinstance(entries, bytes):
        entries = entries.decode("utf-8")
    if isinstance(entries, str):
        entries = json.loads(entries)
    if isinstance(entries, dict):
        entries = entries.get("entries")
    if isinstance(entries, Iterable) and not isinstance(entries, (str, bytes, dict, list)):
        entries = list(entries)
    if not isinstance(entries, list):
        raise TypeError("graph seed must be an entries list or an entries document")
    rows = [dict(entry) for entry in entries]
    store_path = path.with_suffix(".db")
    connection = sqlite3.connect(store_path)
    tables = {
        row[0]
        for row in connection.execute(
            "SELECT name FROM sqlite_master WHERE type = 'table'"
        )
    }
    stored = sum(
        connection.execute(f"SELECT COUNT(*) FROM {table}").fetchone()[0]
        for table in ("nodes", "nodes_raw")
        if table in tables
    )
    if stored == 0 and not _store_is_live(connection, tables):
        connection.execute(
            "CREATE TABLE IF NOT EXISTS entries ("
            "id TEXT PRIMARY KEY, ordinal INTEGER NOT NULL, row TEXT NOT NULL)"
        )
        for ordinal, entry in enumerate(rows):
            node_id = entry.get("id")
            if isinstance(node_id, str):
                connection.execute(
                    "INSERT INTO entries(id, ordinal, row) VALUES (?, ?, ?)",
                    (node_id, ordinal, json.dumps(entry)),
                )
        connection.commit()
        connection.close()
        from _bins import backlog_rows

        backlog_rows(include_archived=True, path=path)
        return rows
    connection.close()
    from _bins import store_request

    snap = store_request("begin", {}, path=path)
    current = {row["id"]: row for row in snap.get("entries", [])}
    wanted = {row["id"] for row in rows}
    changed = [row for row in rows if current.get(row["id"]) != row]
    removed = [node_id for node_id in current if node_id not in wanted]
    digests = snap.get("base_digests", {})
    touched = [row["id"] for row in changed] + removed
    # plan_rungs stays null: a supplied rung map, even empty, makes the keeper
    # re-derive status and demote every pending seed to idea. The fixture seeds
    # stored statuses verbatim.
    store_request(
        "commit_rows",
        {
            "base_version": snap["version"],
            "base_digests": {i: digests[i] for i in touched if i in digests},
            "changed": changed,
            "removed": removed,
            "plan_rungs": None,
            "attempt": 1,
        },
        path=path,
    )
    return rows


def read_graph_strict(path: Path) -> list[dict]:
    from _bins import backlog_rows

    return backlog_rows(include_archived=True, path=Path(path))


def commit_rows_via_store(path: Path, mutate) -> list[dict]:
    return seed_graph(Path(path), mutate(read_graph_strict(path)))


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("usage: graph_seed.py <graph.json anchor>")
    seed_graph(Path(sys.argv[1]), sys.stdin.buffer.read())
