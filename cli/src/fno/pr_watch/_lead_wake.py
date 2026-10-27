from __future__ import annotations

from pathlib import Path
from typing import Any, Optional


def _lead_op(op: str, **payload: Any) -> dict:
    """The Rust lead manifest, wake-ledger and scope ops (``lead`` kind)."""
    from fno.agents.role import lead_manifest_call

    return lead_manifest_call(op, **payload)


_GRAPH_ENTRIES_MEMO: dict = {"ident": None, "entries": None}


def graph_entries(path: Optional[Path] = None) -> list:
    """The tick's graph read, one real read per graph identity: an unchanged
    graph is byte-identical, so the memo is not staleness. Sweep discovery is
    the tick's only reader now that the wake phase moved into the daemon,
    and the memo keeps the 15 MB store at one real read per tick."""
    from fno.graph.api import wire_rows
    from fno.paths import graph_json
    from fno.graph.api import active_backend_name

    try:
        if active_backend_name() != "graph":
            return []
        gpath = path or graph_json()
        ident = _lead_op("graph-ident", path=str(gpath)).get("ident")
        if ident is not None and _GRAPH_ENTRIES_MEMO["ident"] == ident:
            return _GRAPH_ENTRIES_MEMO["entries"]
        entries = wire_rows(path=gpath)
        if ident is not None:
            _GRAPH_ENTRIES_MEMO.update(ident=ident, entries=entries)
        return entries
    except Exception:  # noqa: BLE001 - an unreadable graph is no signal
        return []
