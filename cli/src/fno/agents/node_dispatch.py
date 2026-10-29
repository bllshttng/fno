"""Node seed readers for the native dispatch owner."""

from __future__ import annotations

from typing import Optional



def _with_dispatch_command(row: dict) -> dict:
    """The row with the legacy stored key surfaced as ``dispatch_command``.

    A row the store has not migrated yet (or one a defer/undefer rewrote) can
    carry only the old key; every reader below looks up the new one."""
    legacy = row.get("dispatch_verb")  # verb-wording: allow (stored key)
    if legacy and not row.get("dispatch_command"):
        return dict(row, dispatch_command=legacy)
    return row


def find_node_row(node: str) -> Optional[dict]:
    """The graph row for a node id or slug, or None on an unreadable graph.
    One lookup so the door, the seam and retask read the SAME row."""
    row = _find_node_row(node)
    return _with_dispatch_command(row) if row is not None else None


def _find_node_row(node: str) -> Optional[dict]:
    try:
        from fno.graph.load import load_graph
        from fno.graph.store import GRAPH_JSON, read_nodes_by_ids

        fast = read_nodes_by_ids(GRAPH_JSON, [node])
        for candidate in fast["entries"] if fast is not None else load_graph():
            if not candidate.get("archived_at") and (candidate.get("id") == node or candidate.get("slug") == node):
                return candidate
    except Exception:  # noqa: BLE001 - an unreadable graph cannot seed a spawn
        return None
    return None


def _command_answer(row: Optional[dict], *, node_id: Optional[str] = None) -> tuple:
    from fno.agents.harness_map import DispatchResolveError
    from fno.graph.store import GRAPH_JSON, _client_for
    payload = dict(row or {}, id=(row or {}).get("id") or node_id)
    params: dict = {"entries": [payload]}
    # The blueprint floor rides with the row so the lifecycle table answers
    # from config; an unreadable config leaves the lean default in force.
    try:
        from pathlib import Path as _Path

        from fno.config import load_settings, load_settings_for_repo

        node_cwd = (row or {}).get("cwd")
        settings_obj = (
            load_settings_for_repo(_Path(node_cwd)) if node_cwd else load_settings()
        )
        params["blueprint_floor"] = settings_obj.dispatch.blueprint_floor
    except Exception:  # noqa: BLE001 - unreadable config -> the lean default
        pass
    try:
        answer = _client_for(GRAPH_JSON).request("effective_command", params)
        return answer["verb"], answer["note"]
    except RuntimeError as exc:
        raise DispatchResolveError(str(exc).replace("store error (invalid): ", "")) from exc


def node_effective_command(
    row: Optional[dict], *, node_id: Optional[str] = None
) -> Optional[str]:
    """The lifecycle table's answer for a node row, or None on abstain:
    one answer per node, shared by every door. Accepts a None row; raises
    DispatchResolveError on an unanswerable node."""
    return _command_answer(row, node_id=node_id)[0]

