"""Failure-streak derivation from the walker's events.

Pure, IO-light helpers for the Python verbs that still read a streak (the epic
status view). The streak is DERIVED from the walker's existing events, not a
new walker write or a persistent counter field.

Event envelope: the walker journals ``{"ts","type","source":"loop",
"data":{"unit_id",...}}`` (crates/fno-agents/src/loop_runtime.rs); for the
megawalk / target drivers ``unit_id`` IS the backlog node id, so the streak
keys on ``data.unit_id``. The flat agents-emitter envelope
(``{...,"kind":...}``) is accepted too - the shape ``emit_undefer_boundary``
writes. Classification: failure = ``node_failed`` or
``node_closed{close=parked}``; reset = ``node_closed{close=closed}`` or
``node_undeferred``; everything else (including a dispatch-refusal close)
never counts, never resets.
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable, Optional

_FAIL_TYPE = "node_failed"
_CLOSE_TYPE = "node_closed"
_UNDEFER_TYPE = "node_undeferred"


def events_path() -> Path:
    """The global events log the walker mirrors loop ``node_*`` events into:
    ``paths.state_dir()`` / ``events.jsonl`` (the config-aware resolver; a
    literal ``~/.fno`` is rejected by the no-hardcoded-paths guard).
    ``read_events()`` also consumes the Rust agents-home mirror when it
    differs, so a custom state dir cannot split writer from reader. Resolved
    at call time so the conftest ``$HOME`` redirect is honored in tests."""
    from fno import paths

    return paths.state_dir() / "events.jsonl"


def emit_undefer_boundary(node_id: str, path: Optional[Path] = None) -> None:
    """Append a ``node_undeferred`` reset boundary for ``node_id``: the flat
    envelope ``_classify`` accepts, to the same log ``read_events`` consumes,
    so writer and reader cannot drift. Best-effort, but warns to stderr: a
    reset that silently never lands leaves the node one maintain pass from
    re-deferral on stale history."""
    import sys
    from datetime import datetime, timezone

    # The path resolution is inside the try, not just the write: events_path()
    # reads settings, and a malformed settings.yaml must not break an undefer.
    target: Optional[Path] = None
    try:
        target = path if path is not None else events_path()
        # The flat envelope the old file append wrote becomes canonical at the
        # storage boundary; _classify accepts either shape on the read side.
        record = {
            "ts": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
            "type": _UNDEFER_TYPE,
            "source": "backlog",
            "data": {"unit_id": node_id},
        }
        from fno.events.store_client import emit_envelope

        emit_envelope(record, target)
    except Exception as exc:  # noqa: BLE001 - never break the undefer
        print(
            f"fno backlog: warning: {_UNDEFER_TYPE} boundary for {node_id} "
            f"to {target or '<unresolved>'}: {exc}",
            file=sys.stderr,
        )


def _default_event_paths() -> list[Path]:
    from fno import paths

    configured = events_path()
    rust_mirror = paths.agents_home_dir().parent / "events.jsonl"
    return list(dict.fromkeys((configured, rust_mirror)))


def merge_event_histories(*histories: Iterable[dict]) -> list[dict]:
    """Union mirrored histories by occurrence count and timestamp order."""
    merged: list[tuple[int, dict]] = []
    max_counts: dict[str, int] = {}
    for history in histories:
        local_counts: dict[str, int] = {}
        for rec in history:
            key = json.dumps(rec, sort_keys=True, separators=(",", ":"))
            local_counts[key] = local_counts.get(key, 0) + 1
            if local_counts[key] > max_counts.get(key, 0):
                merged.append((len(merged), rec))
        for key, count in local_counts.items():
            max_counts[key] = max(max_counts.get(key, 0), count)
    merged.sort(key=lambda item: (str(item[1].get("ts") or ""), item[0]))
    return [rec for _, rec in merged]


def read_events(
    path: Optional[Path] = None,
    types: Optional[list[str]] = None,
) -> list[dict]:
    """Read raw event envelopes across retained rotation, oldest first; the
    ``.1`` generation is read first so order survives rotation. A truncated
    or non-JSON line is skipped; absent files yield []. ``types`` narrows the
    store-side read to those envelopes; the unfiltered read costs seconds on
    a grown store."""
    targets = [path] if path is not None else _default_event_paths()
    histories: list[list[dict]] = []
    for target in targets:
        # SQL authority: the store beside each journal answers in commit
        # order; the import inside the native writer already folded the
        # rotated generation, so no per-file walk remains here.
        from fno.events.store_client import query_rows

        histories.append(query_rows(target, types=list(types) if types is not None else None))
    return merge_event_histories(*histories)


@dataclass(frozen=True)
class _Ev:
    node_id: str
    kind: str  # "fail" | "reset"


def _classify(raw: object) -> Optional[_Ev]:
    """Map one raw envelope to a node-scoped (node_id, kind), or None to skip.

    Handles both the walker envelope ``{"type","data":{"unit_id"...}}`` and the
    flat agents envelope ``{"kind","unit_id"...}``. Anything that is not a
    node-scoped failure / reset signal returns None.
    """
    if not isinstance(raw, dict):
        return None
    etype = raw.get("type") or raw.get("kind")
    _data = raw.get("data")
    data = _data if isinstance(_data, dict) else raw
    node_id = data.get("unit_id") or data.get("node_id") or data.get("graph_node_id")
    if not isinstance(node_id, str) or not node_id:
        return None
    if etype == _FAIL_TYPE:
        return _Ev(node_id, "fail")
    if etype == _UNDEFER_TYPE:
        return _Ev(node_id, "reset")
    if etype == _CLOSE_TYPE:
        close = data.get("close")
        if close == "parked":
            return _Ev(node_id, "fail")
        if close == "closed":
            return _Ev(node_id, "reset")
        # "refused" (dispatch refusal) and any other close: neither fail nor reset.
        return None
    return None


def _window_to_reset(node_id: str, events: Iterable[object]) -> list[tuple[object, Optional[_Ev]]]:
    """The streak window: raw events for ``node_id``
    newest -> oldest to the first reset; unrelated events ride through."""
    out: list[tuple[object, Optional[_Ev]]] = []
    for raw in reversed(list(events)):
        ev = _classify(raw)
        if ev is not None:
            if ev.node_id != node_id:
                continue
            if ev.kind == "reset":
                break
        out.append((raw, ev))
    return out


def consecutive_failures(node_id: str, events: Iterable[object]) -> int:
    """Count consecutive failure events for ``node_id`` since the most recent
    reset boundary (a success close or an undefer), scanning newest -> oldest.
    Dispatch-refusals and unrelated events neither inflate nor reset the
    streak; a node with zero failure events yields 0 (Boundaries)."""
    return sum(
        1 for _raw, ev in _window_to_reset(node_id, events) if ev is not None and ev.kind == "fail"
    )
