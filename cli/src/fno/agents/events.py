"""fno.agents.events — JSONL event emitter for agent dispatch.

Phase 1 minimum: append one well-formed JSON line per call to
``~/.fno/events.jsonl`` (resolved via ``paths.state_dir()``). The
schema is open-ended — every call carries ``ts`` (ISO8601 UTC) and
``kind`` (string), plus arbitrary keyword data that flattens into the
top-level JSON object.

The existing project-level ``fno.events`` module is intentionally
NOT reused — it carries schema validation, mkdir-mutex locking, and
provenance bindings tied to target sessions, none of which are needed
for cross-CLI agent dispatch events. Keeping the agents emitter minimal
keeps the substrate decoupled.

Event kind constants
====================

Callers SHOULD reference ``KIND_*`` constants instead of inlining string
literals. The kinds are open-set (new code may emit new kinds without
touching this module) but the canonical names live here so a single
grep surfaces every event the dispatch/provider layers can produce.

Phase 5 (MCP channel + streaming) additions are grouped at the bottom.
"""
from __future__ import annotations

import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import TYPE_CHECKING, Any, Optional

from fno import paths

if TYPE_CHECKING:
    pass


def _utc_now_iso() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def emit(kind: str, *, path: Optional[Path] = None, **data: Any) -> None:
    """Append one well-formed JSON line to the agents events log.

    Telemetry emission is best-effort: an ``OSError`` (disk full,
    permission denied, parent dir unwritable) is logged to stderr and
    swallowed so a failed log write cannot break the primary dispatch.

    Args:
        kind: Event kind (e.g. ``agent_ask_started``, ``agent_ask_done``).
        path: Override the events file path. Defaults to
            ``paths.state_dir() / "events.jsonl"``.
        **data: Arbitrary keyword fields that flatten into the JSON object
            alongside ``ts`` and ``kind``.
    """
    if kind == "agent_removed":
        data = dict(data)
        data.setdefault("harness", data.get("provider") or "unknown")
        data.setdefault("registry_changed", bool(data.get("registry_changed", False)))
        data.setdefault("actor", "operator")
        data.setdefault(
            "reason",
            data.get("teardown_error") or data.get("error") or "rm-request",
        )
        data.setdefault(
            "request_id",
            f"agent-rm:{data.get('name') or 'unknown'}",
        )
        touched = data.get("worktree_touched")
        if not isinstance(touched, bool):
            touched = bool(data.get("worktree_receipt"))
        data["worktree_touched"] = touched
        data.setdefault("worktree_outcome", "removed" if touched else "not-touched")
        data.setdefault("reclaimed_bytes", 0)

    target = path if path is not None else (paths.state_dir() / "events.jsonl")
    # Put ts and kind LAST so a stray data={"ts": ..., "kind": ...} kwarg
    # cannot overwrite the canonical fields. The dict's order-preserving
    # right-to-left merge gives the mandatory fields final say.
    ts = _utc_now_iso()
    record = {**data, "ts": ts, "kind": kind}
    diagnostic = " ".join(str(value) for value in data.values())
    # The store commit is the write boundary. The envelope carries the
    # canonical fields (type/source/data) for SQL-side readers; the legacy
    # top-level keys (kind + flattened kwargs) ride along because committed
    # rows round-trip them verbatim, which keeps every kind-keyed reader on
    # the shared history working through the same rows.
    records = [
        {
            **record,
            "type": kind,
            # The envelope source is the channel identity ('agents'): a
            # caller-supplied ``source`` kwarg is a legacy top-level field
            # (e.g. register_session's --source resume), not a declared
            # envelope source, and stamping it there refuses every such row
            # at the judge. The legacy value still rides in ``data``.
            "source": "agents",
            "data": dict(data),
        }
    ]
    if kind != "provider_rate_limited" and "429" in diagnostic and "1313" in diagnostic:
        records.append(
            {
                "provider": data.get("route_provider") or data.get("provider") or "unknown",
                "account": data.get("account") or data.get("account_id"),
                "observed_at": record["ts"],
                "ts": _utc_now_iso(),
                "kind": "provider_rate_limited",
                "type": "provider_rate_limited",
                "source": "agents",
                "data": {
                    "provider": data.get("route_provider") or data.get("provider") or "unknown",
                    "account": data.get("account") or data.get("account_id"),
                },
            }
        )
    from fno.events.store_client import EventStoreUnavailable, emit_envelope

    for item in records:
        try:
            emit_envelope(item, target)
        except (EventStoreUnavailable, OSError) as exc:
            print(
                f"fno agents: warning: events.emit({kind!r}) to {target}: {exc}",
                file=sys.stderr,
            )


def daemon_lifecycle_log() -> Path:
    """The daemon's agent-lifecycle log: where births and deaths are joinable."""
    return paths.agents_home_dir() / "events.jsonl"


def _emit_daemon_envelope(
    kind: str, data: dict[str, Any], *, source: str = "python"
) -> None:
    """Write one record in the daemon's unified envelope to the daemon
    lifecycle log.

    The Rust daemon nests the payload under ``data`` and stamps the kind as
    ``type`` (crates/fno-agents/src/events.rs). A Python birth that shared the
    file but used the flat ``{..., ts, kind}`` shape would not be joinable with
    a daemon death by one reader: ``rec["data"]["name"]`` works on the death and
    KeyErrors on the flat birth. Writing the same envelope from both sides is
    what makes the lineage tree reconstructable from a single file.
    Best-effort: OSError is swallowed (a failed log write must not break spawn).
    """
    record = {
        "ts": _utc_now_iso(),
        "type": kind,
        "source": source,
        "data": data,
    }
    try:
        from fno.events.store_client import emit_envelope

        emit_envelope(record, daemon_lifecycle_log())
    except Exception as exc:  # noqa: BLE001 - a failed log write must not break spawn
        print(f"fno agents: warning: daemon envelope {kind}: {exc}", file=sys.stderr)


# ---------------------------------------------------------------------
# Classified session-id transitions. Written to the SAME daemon
# lifecycle log births and deaths use, so a succession is joinable with the
# predecessor's eventual `agent_row_reaped` from one file - the lineage event
# must exist before any reap can erase the row it names.
# ---------------------------------------------------------------------
KIND_SESSION_TRANSITION_CLASSIFIED = "session_transition_classified"
KIND_SESSION_TRANSITION_DEFERRED = "session_transition_deferred"
KIND_SESSION_TRANSITION_ALREADY_APPLIED = "session_transition_already_applied"


def emit_session_transition(
    kind: str,
    *,
    name: str,
    harness: str,
    predecessor_session_id: str,
    successor_session_id: str,
    classification: Optional[str] = None,
    basis: Optional[str] = None,
    source: Optional[str] = None,
) -> None:
    """Record one classified (or deferred) A->B session transition.

    ``kind`` is one of the ``session_transition_*`` constants above. The
    record positively names A, B, the classification, and the liveness basis
    the verdict was read from, so an auditor never has to infer which
    evidence produced the shape the registry now shows. Best-effort: the
    underlying envelope write swallows OSError.
    """
    data: dict[str, Any] = {
        "name": name,
        "harness": harness,
        "predecessor_session_id": predecessor_session_id,
        "successor_session_id": successor_session_id,
        "evidence_at": _utc_now_iso(),
    }
    if classification:
        data["classification"] = classification
    if basis:
        data["basis"] = basis
    if source:
        data["source"] = source
    _emit_daemon_envelope(kind, data)


# The skip kind stays Python-side (the merge command's cleanup gate owns it);
# the requested mint is native (crates/fno-agents/src/merge_reap.rs).
KIND_MERGE_CLEANUP_SKIPPED = "merge_cleanup_skipped"

# Taken verbatim from the ritual's archive leg so the two legs cannot drift.
MERGE_CLEANUP_SKIP_REASONS = (
    "gh-unavailable",
    "unparseable-pr-json",
    "not-merged",
    "no-branch",
    "emit-failed",
)


def emit_merge_cleanup_skipped(
    *,
    repo: str,
    project: str,
    pr: int,
    reason: str,
    branch: Optional[str] = None,
    detail: str = "",
    session_id: Optional[str] = None,
    harness: Optional[str] = None,
) -> None:
    """Say that a merge minted no cleanup request, and which precondition was
    unmet. ``reason`` is a closed set, so an unclassifiable row is refused."""
    if reason not in MERGE_CLEANUP_SKIP_REASONS:
        raise ValueError(
            f"unknown merge cleanup skip reason: {reason!r}; "
            f"expected one of {', '.join(MERGE_CLEANUP_SKIP_REASONS)}"
        )
    _emit_daemon_envelope(
        KIND_MERGE_CLEANUP_SKIPPED,
        {
            "repo": repo,
            "project": str(project),
            "pr": int(pr),
            "reason": reason,
            "branch": branch,
            "detail": detail,
            "session_id": session_id,
            "harness": harness,
        },
    )
