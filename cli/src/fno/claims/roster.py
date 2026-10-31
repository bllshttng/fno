from __future__ import annotations

from datetime import datetime
from types import MappingProxyType
from typing import Mapping, NamedTuple


_ADVISORY_WARNING_PREFIX = "roster advisory: "
_UNMEASURABLE_ROW_PREFIX = "unmeasurable-row: "


class RosterReading(NamedTuple):
    consulted: bool
    rows_scanned: int
    workers_by_node: dict
    reason: str = ""
    rows_by_session: Mapping = MappingProxyType({})
    rows_unresolved: int = 0
    unresolved_rows: tuple = ()
    unmeasurable_by_node: Mapping = MappingProxyType({})

    def workers_on(self, node_id: str) -> list:
        return self.workers_by_node.get(node_id, [])

    def row_for_session(self, session_id: str):
        return self.rows_by_session.get(session_id)


def read_roster(
    timeout: float = 10.0, require_live_probe: bool = True
) -> RosterReading:
    """Read the fleet once and index it by resolved node id.

    ``require_live_probe``: claim-status liveness refuses when the probe
    never ran; the worked overlay passes False (the registry-only view
    still carries attribution).
    """
    try:
        from fno.rust_binary import call_binary_json

        err, answer = call_binary_json("census", ["watchdog-fleet"], timeout=timeout)
        if err is not None or not isinstance(answer, dict):
            raise RuntimeError(f"fleet read failed: {err}")
        rows, warnings = answer["rows"], answer["warnings"]
    except Exception as exc:  # noqa: BLE001 - any failure must degrade loudly
        return RosterReading(False, 0, {}, f"{type(exc).__name__}: {exc}")

    # The harnesses' degraded-probe wording, as a literal: an import would be a layering edge.
    registry_only_mark = "falling back to registry-only view"
    unmeasurable: dict = {}
    blocking = []
    degraded_reason = ""
    for w in warnings:
        idx = w.find(_UNMEASURABLE_ROW_PREFIX)
        if idx == -1:
            degraded_probe = (not require_live_probe) and (registry_only_mark in w)
            if not w.startswith(_ADVISORY_WARNING_PREFIX) and not degraded_probe:
                blocking.append(w)
            elif degraded_probe and not degraded_reason:
                # Tolerated, not silent: the caller learns the probe degraded,
                # so a registry-only answer never masquerades as a full one.
                degraded_reason = w
            continue
        fields = dict(
            tok.split("=", 1)
            for tok in w[idx + len(_UNMEASURABLE_ROW_PREFIX):].split()
            if "=" in tok
        )
        if fields.get("node"):
            unmeasurable.setdefault(fields["node"], []).append(
                fields.get("name") or "unknown"
            )

    if blocking:
        return RosterReading(False, 0, {}, blocking[0])

    index: dict = {}
    by_session: dict = {}
    unresolved: list[dict] = []
    for r in rows:
        entry = {
            "name": r.get("name"),
            "state": r.get("state"),
            "cwd": r.get("cwd"),
            "row_id": str(r.get("row_id") or ""),
            "pid": r.get("pid"),
            "pid_start_time": r.get("pid_start_time"),
            "mux": r.get("mux"),
            "stopped_at": r.get("stopped_at"),
        }
        if r.get("node"):
            index.setdefault(r["node"], []).append(entry)
        else:
            unresolved.append(entry)
        if r.get("row_id"):
            by_session[str(r["row_id"])] = entry
    return RosterReading(True, len(rows), index, degraded_reason, by_session, len(unresolved), tuple(unresolved), unmeasurable)


_FINISHED_ROW_STATES = frozenset({"done", "completed", "exited", "killed"})


def classify_workers(workers: list) -> tuple[list, list, dict]:
    """(engaged, unmeasurable, verdicts-by-name) for one roster read."""
    from fno.agents.reachability import REACHABLE, UNKNOWN

    verdicts = {w.get("name") or "": _worker_reachability(w).verdict for w in workers}
    return (
        [w for w in workers if verdicts.get(w.get("name") or "") == REACHABLE],
        [w for w in workers if verdicts.get(w.get("name") or "") == UNKNOWN],
        verdicts,
    )


def _worker_reachability(worker: dict):
    """One roster row through the ONE shared predicate (task 1.1).

    REACHABLE engaged, UNREACHABLE finished, UNKNOWN its own arm - never
    engaged-by-default. The transcript outranks the supervisor word for EVERY
    row (a finished worker's row never leaves `working`); a terminal word
    with no transcript stays positive evidence of the end.
    """
    from fno.agents.reachability import (
        TRANSCRIPT_EVIDENCE_S,
        classify_reachability,
        pane_falsifier,
        pid_falsifier,
    )

    state = worker.get("state")
    try:
        import time

        from fno.rust_binary import call_binary_json

        err, tail = call_binary_json(
            "census",
            ["watchdog-tail", "--session", worker.get("row_id") or "", "--cwd", worker.get("cwd") or ""],
            timeout=10,
        )
        facts = tail if err is None and isinstance(tail, dict) and tail.get("has_transcript") else None
    except Exception:  # noqa: BLE001 - an unreadable transcript answers nothing
        facts = None
    # The pid falsifier fires only on an INCARNATION-PROVEN pid (a recorded
    # start token, the same bound the claims layer's hybrid arm uses): a bare
    # stale pid on a live worker must never read as death. A FRESH tail is a
    # resumed session's witness over even a proven corpse.
    pid = worker.get("pid")
    proven = pid is not None and worker.get("pid_start_time") is not None
    falsifier = pid_falsifier(pid, worker.get("pid_start_time")) if proven else None
    if falsifier is None:
        falsifier = pane_falsifier(worker.get("mux"))
    stamp = worker.get("stopped_at")
    try:
        stop_epoch = datetime.fromisoformat(stamp.replace("Z", "+00:00")).timestamp() if stamp else None
    except (AttributeError, ValueError):
        stop_epoch = None
    if facts is None:
        # No transcript: the supervisor word is the only evidence. An active
        # word stays reachable; a terminal word positively ended the row.
        if falsifier is not None:
            return classify_reachability(truth_state=None, age_s=None, falsifier=falsifier)
        if state in ("working", "watching", "your-move"):
            return classify_reachability(truth_state=state, age_s=None, falsifier=None)
        finished = f"finished-state:{state}" if state in _FINISHED_ROW_STATES else None
        return classify_reachability(truth_state=None, age_s=None, falsifier=finished)
    last_event_epoch = facts.get("last_event_epoch")
    if last_event_epoch is None:
        # A transcript PRESENT but undatable: the measured wrong answer read
        # this as engaged. It is UNKNOWN - a verdict about the instrument -
        # never engaged-by-default and never positively finished.
        return classify_reachability(truth_state=None, age_s=None, falsifier=falsifier)
    age = int(max(0.0, time.time() - last_event_epoch))
    if falsifier is not None and age <= TRANSCRIPT_EVIDENCE_S:
        falsifier = None
    if stop_epoch is not None and last_event_epoch <= stop_epoch:
        falsifier = f"stopped:{worker.get('stopped_at')}"
    if falsifier is None and age > TRANSCRIPT_EVIDENCE_S and state in _FINISHED_ROW_STATES:
        # A terminal word with a silent tail is positive evidence of the end.
        falsifier = f"finished-state:{state}"
    return classify_reachability(
        truth_state=facts.get("truth"),
        age_s=age,
        falsifier=falsifier,
    )
