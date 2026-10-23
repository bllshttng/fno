"""Node seed readers for the native dispatch owner."""

from __future__ import annotations

import dataclasses
import sys
from pathlib import Path
from typing import Optional



@dataclasses.dataclass
class NodeSeed:
    """The rendered seed for a node-driven spawn (change 1), plus the
    launch workdir resolution change 1 moved into the spawn door."""

    node_id: str
    message: str
    slug: Optional[str]
    plan_path: Optional[str]
    env: dict
    receipt: dict
    recorded_cwd: Optional[str]

    def ensure_launch_workdir(self, harness: str) -> Optional[Path]:
        """The launch workdir for a node-seeded spawn with no explicit cwd
        source: the worktree ensure's answer, never the node's recorded cwd
        (the canonical checkout for every organically filed node). A ``None``
        answer already printed its hold line - the caller exits 2 and the node
        stays claimable rather than launching on canonical main."""
        return ensure_launch_workdir(self.recorded_cwd, self.node_id, harness)


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


def render_node_seed(node: str, *, harness: Optional[str]) -> Optional[NodeSeed]:
    """Render a node's seed (command line + brief env) for the spawn door.

    Prints its own refusal and returns None when the node carries no
    dispatch_command, or when the seed render refuses (an unanswerable lifecycle
    or an over-budget brief) - a truncated brief is never seeded.
    """
    from fno.agents.harness_map import DispatchResolveError, resolve_dispatch
    from fno.provenance.autobrief import resolve_dispatch_brief

    seed_rec: Optional[dict] = find_node_row(node)
    seed_node_id = (seed_rec or {}).get("id") or node
    if not isinstance(seed_rec, dict) or not str(seed_rec.get("dispatch_command") or "").strip():
        print(
            f"refusing node-seeded spawn: node {seed_node_id} carries no "
            "dispatch_command and no message was typed; an idle worker holds "
            "a fleet slot and reads as alive. Encode one with `fno backlog "
            f"update {seed_node_id} dispatch_command=<command>`.",
            file=sys.stderr,
        )
        return None
    try:
        node_brief, node_brief_source = resolve_dispatch_brief(seed_rec)
        resolved_seed = resolve_dispatch(
            harness=harness,
            node_id=str(seed_node_id),
            command_name=str(seed_rec.get("dispatch_command")).strip(),
            lifecycle=_command_answer(seed_rec),
            brief=node_brief,
            trigger="autonomous",
        )
    except DispatchResolveError as exc:
        # An unanswerable lifecycle or an explicit >8KB brief (the one
        # failure the brief chain leaves to this gate) refuses here, before
        # any worker exists; a truncated brief is never seeded.
        print(str(exc), file=sys.stderr)
        return None
    return NodeSeed(
        node_id=str(seed_node_id),
        message=resolved_seed["command"],
        slug=seed_rec.get("slug"),
        plan_path=seed_rec.get("plan_path"),
        env=resolved_seed.get("env") or {},
        receipt={
            "verb_source": "declared",
            "brief_source": node_brief_source,
        },
        recorded_cwd=seed_rec.get("_resolved_cwd") or seed_rec.get("cwd"),
    )


def ensure_launch_workdir(
    recorded_cwd: Optional[str], node_id: str, harness: str
) -> Optional[Path]:
    """Resolve the launch workdir through the launch-workdir seam command (Rust);
    the hold line on any refusal, the node id keys the resumed tree."""
    from fno.rust_binary import CommandUnavailable, command_call

    payload = {
        "recorded_cwd": str(Path(recorded_cwd) if recorded_cwd else Path.cwd()),
        "node": node_id,
        "harness": harness,
    }
    try:
        answer = command_call(
            "launch-workdir",
            payload,
            CommandUnavailable,
            timeout=150,
            passthrough_stderr=True,
        )
    except CommandUnavailable:
        answer = None
    if answer is None or "hold" in answer:
        print(
            f"fno agents spawn: worktree ensure refused or misconfigured for "
            f"{node_id}; holding the node rather than launching on canonical "
            "main",
            file=sys.stderr,
        )
        return None
    return Path(answer["workdir"])
