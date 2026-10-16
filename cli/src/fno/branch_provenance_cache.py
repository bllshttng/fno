"""Per-repo branch-provenance cache: the pr-watch stranded leg writes it, the native graph.md render reads it. File contract: docs/state-root-inventory.md."""

from __future__ import annotations

import json
import logging
import os
import tempfile
from pathlib import Path

from fno.worktree_stranded import CLEAN

CACHE_RELPATH = ".fno/branch-provenance.json"


def write_cache(repo: Path, rows: list) -> bool:
    """Write the non-CLEAN rows when they differ from the cache; True then."""
    out = [
        {
            "branch": row.facts.get("branch"),
            "node": row.node,
            "klass": row.klass,
            "unpushed": row.unpushed,
            "has_remote": row.facts.get("has_remote"),
            "age": row.age,
            "pr_number": row.facts.get("pr_number"),
            "live": row.facts.get("live"),
        }
        for row in rows
        if row.klass != CLEAN
    ]
    if out == read_cache(repo):
        return False
    target = Path(repo) / CACHE_RELPATH
    try:
        target.parent.mkdir(parents=True, exist_ok=True)
        fd, tmp = tempfile.mkstemp(dir=target.parent, suffix=".tmp")
        with os.fdopen(fd, "w", encoding="utf-8") as f:
            json.dump(out, f)
        os.replace(tmp, str(target))
        return True
    except Exception as exc:  # noqa: BLE001 - display cache, never break the tick
        logging.getLogger(__name__).warning("branch_provenance_cache: write failed for %s: %s", repo, exc)
        return False


def read_cache(repo: Path) -> list[dict]:
    try:
        data = json.loads((Path(repo) / CACHE_RELPATH).read_text(encoding="utf-8"))
        return [r for r in data if isinstance(r, dict)] if isinstance(data, list) else []
    except (OSError, ValueError):
        return []
