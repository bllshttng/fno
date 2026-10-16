"""How a backlog node dict is built.

``_build_backlog_node`` is the single builder every wheel creation verb
routes through (decompose, capture promote, retro land), and
``_session_provenance`` is the ambient origin it stamps. The plan-less
``new`` verb that lived beside them rides the native create door now, which
retired the last writer that built its dict inline. cli.py re-exports the
private names so the lazy ``from fno.graph.cli import _build_backlog_node``
imports in capture.py and retro/land.py keep resolving.
"""

from __future__ import annotations

import json
import os
from datetime import datetime, timezone
from pathlib import Path
from typing import Optional

import typer

from fno.graph._constants import (
    REQUEST_ORIGIN_DEFAULT,
    REQUEST_ORIGINS,
    SOURCE_KIND_DEFAULT,
    validate_source_kind,
)

ORIGIN_EVIDENCE_HELP = "Producing-event reference (mail id, event id, fu-id, path). With --source-kind from_observation/from_supervisor this is what makes the node an agent discovery; without it the origin stays unknown."
ENCOUNTER_EVIDENCE_HELP = "Record why the creator encountered this node. Optional."
DESCRIPTION_HELP = "Alias for --details. Reads more naturally for an idea-stage row. Mutually exclusive with --details."
SOURCE_KIND_HELP = "organic|from_inbox|from_observation|from_supervisor|operator_request. Mark an operator ask with operator_request."
SOURCE_NODE_HELP = "Origin node this filing came out of (id, slug, or bare hex). Overrides ambient capture. Refuses if it does not resolve."
RELATED_HELP = "Related node ids/slugs (asserted, symmetric, non-blocking). Repeat or comma-separate. Refuses an id that does not resolve."
TAG_HELP = "Tag (repeatable, lowercase-kebab)."


def _scan_md_field(text: str, key: str) -> Optional[str]:
    """First ``<key>: <value>`` value in a target-state.md, matched-quote-stripped.

    Local mirror of ``fno.agents.whoami._scan_field`` so ``graph`` does not import
    ``agents`` (avoids an import cycle). ``None`` if the key is absent.
    """
    import re

    # ^\s* tolerates indentation; (.+) captures the whole value so a path/title
    # containing spaces is not truncated at the first space (\S+ would).
    pattern = re.compile(rf"^\s*{re.escape(key)}:\s*(.+)")
    for line in text.splitlines():
        match = pattern.match(line)
        if match:
            value = match.group(1).strip()
            if len(value) >= 2 and value[0] == value[-1] and value[0] in ('"', "'"):
                value = value[1:-1]
            return value
    return None


def _session_provenance(
    running_cwd: Optional[str] = None,
    *,
    source_node: Optional[str] = None,
    known_ids: Optional[set] = None,
) -> dict:
    """Parent-edge provenance for a node born inside a live session.

    Returns ``source_session_id`` / ``source_harness`` / ``source_cwd`` /
    ``source_node_id`` / ``source_plan_path``; every key degrades to ``None``
    and the function NEVER raises. Origin precedence: an explicit
    ``--source-node`` (already validated, taken as given), then the owned
    manifest (claude-only, ownership-proven), then ``FNO_NODE`` from the
    spawner - not harness-gated, the only origin signal a codex/opencode
    worker has. With ``known_ids``, an ambient id absent from the snapshot
    degrades to ``None`` rather than stamping a dangling edge, and the dropped
    token comes back in ``source_node_dropped``; resolve it inside the locked
    mutator so the check costs no extra read.

    ``source_cwd`` is the originating SESSION's cwd - the key claude
    transcript dirs are slugged by - kept distinct from the node's durable
    ``cwd`` (the canonical root) because the read-back resolver needs it.
    Manifest ownership is proven as ``whoami.find_held_node`` does: the
    manifest's claude session id must equal this process's, so a stale or
    foreign worktree manifest never leaks a node this session does not hold.
    Node + plan resolution is claude-only; other harnesses stamp session +
    harness and degrade the rest.
    """
    cwd = running_cwd if running_cwd is not None else os.getcwd()

    from fno.claims.self_identity import resolve_self_identity

    identity = resolve_self_identity()
    session = getattr(identity, "session_id", None)
    harness = getattr(identity, "harness", None)

    source_node_id: Optional[str] = None
    source_plan_path: Optional[str] = None
    if session and harness == "claude":
        try:
            text = (Path(cwd) / ".fno" / "target-state.md").read_text(encoding="utf-8")
            # Current key is claude_session_id; old-key fallback for one release.
            manifest_claude_sid = _scan_md_field(text, "claude_session_id") or _scan_md_field(
                text, "claude_transcript_id"
            )
            if manifest_claude_sid == session:
                nid = _scan_md_field(text, "graph_node_id")
                if nid and nid.lower() != "null":
                    source_node_id = nid
                plan = _scan_md_field(text, "plan_path")
                if plan and plan.lower() != "null":
                    source_plan_path = plan
        except (OSError, ValueError):
            pass

    if source_node_id is None:
        source_node_id = (os.environ.get("FNO_NODE") or "").strip() or None

    dropped: Optional[str] = None
    if source_node_id is not None and known_ids is not None and source_node_id not in known_ids:
        dropped, source_node_id = source_node_id, None

    if source_node:
        # The explicit flag wins, so nothing was dropped: an ambient candidate
        # that lost a precedence contest is not a capture failure to report.
        source_node_id, dropped = source_node, None

    return {
        "source_session_id": session,
        "source_harness": harness,
        # session cwd is the transcript-resolver key; only meaningful with a session.
        "source_cwd": cwd if session else None,
        "source_node_id": source_node_id,
        "source_plan_path": source_plan_path,
        # Not a node field. An ambient signal naming a node the graph no longer
        # has is the one case worth telling the operator about: capture silently
        # regressing to nothing is what this feature exists to catch.
        "source_node_dropped": dropped,
    }


def stamp_request_origin(
    *, source_kind: str | None, birth_channel: str, origin_evidence: str | None
) -> "tuple[str | None, str | None]":
    """One birth record through the native owner (`fno-agents node-origin`).

    Returns the caller's evidence; Rust owns every origin category.
    """
    import subprocess

    ref = (origin_evidence or "").strip() or None
    origin = REQUEST_ORIGIN_DEFAULT
    refused = "--source-kind operator_request could not be verified: fno-agents node-origin gave no answer"
    try:
        from fno.rust_binary import resolve_binary

        binary = resolve_binary()
        if binary is not None:
            record = [{"source_kind": source_kind, "birth_channel": birth_channel, "origin_evidence": ref}]
            proc = subprocess.run(
                [str(binary), "node-origin", "resolve", "--payload", "-"],
                input=json.dumps(record),
                capture_output=True,
                text=True,
                timeout=30,
            )
            rows = (json.loads(proc.stdout) if proc.returncode == 0 else None) or {}
            rows = rows.get("results")
            if isinstance(rows, list) and len(rows) == 1 and rows[0].get("origin") in REQUEST_ORIGINS:
                origin, refused = rows[0]["origin"], rows[0].get("refused") or refused
    except Exception:  # noqa: BLE001 - fail open; birth never invents origin
        pass
    if source_kind == "operator_request" and origin != "operator_request":
        typer.echo(f"Error: {refused}", err=True)
        raise typer.Exit(code=1)
    return origin, ref


def _build_backlog_node(
    *,
    title: str,
    type_: str = "feature",
    parent: Optional[str] = None,
    project: Optional[str] = None,
    cwd: Optional[str] = None,
    priority: str = "p2",
    blocks_everything: bool = False,
    difficulty: Optional[str],
    difficulty_source: str = "filed",
    domain: str = "code",
    blocked_by: Optional[list[str]] = None,
    roadmap_id: Optional[str] = None,
    vision_path: Optional[str] = None,
    details: Optional[str] = None,
    size: Optional[str] = None,
    batch: Optional[str] = None,
    plan_path: Optional[str] = None,
    tags: Optional[list[str]] = None,
    source_node: Optional[str] = None,
    known_ids: Optional[set] = None,
    out: Optional[dict] = None,
    source_kind: str = SOURCE_KIND_DEFAULT,
    origin_channel: str = "new",
    origin_evidence: Optional[str] = None,
    source: Optional[str] = None,
    source_project: Optional[str] = None,
    source_inbox_msg: Optional[str] = None,
    artifact_url: Optional[str] = None,
    completion_note: Optional[str] = None,
    source_session_id: Optional[str] = None,
) -> dict:
    """Build a backlog node dict shared by the Python creation verbs``.

    ``out``, when given, receives metadata ABOUT the capture that is not itself
    a node field (currently ``source_node_dropped``). A separate channel rather
    than a transient key on the returned dict, so a caller that does not know to
    strip it cannot persist it into the graph.

    Centralizes the field set so a schema addition (e.g. a new graph
    field) shows up in every entry-creating verb at once. ``difficulty`` is
    required so every caller makes the routing decision explicitly instead of
    silently minting an unroutable node. ``difficulty_source`` names the writer
    in the history entry. The returned dict has no ``id`` - the caller assigns
    one inside its locked mutator so duplicate-ID checks happen against the
    live snapshot.
    """
    from fno.graph._constants import ID_PREFIX  # noqa: F401

    # Chokepoint validation: the vocabulary is enforced where the field is
    # written, so a new writer cannot mint an out-of-vocabulary value even if
    # it skips its own CLI-level check.
    validate_source_kind(source_kind)
    origin, origin_evidence_ref = stamp_request_origin(
        source_kind=source_kind, birth_channel=origin_channel, origin_evidence=origin_evidence
    )

    # Parent-edge provenance: stamped from the running session's env +
    # manifest, or from an explicit --source-node. Centralized here so
    # every creator verb (add/idea/decompose) self-describes its origin.
    prov = _session_provenance(source_node=source_node, known_ids=known_ids)
    if out is not None:
        out["source_node_dropped"] = prov["source_node_dropped"]
    return {
        "id": None,  # caller fills inside locked mutator
        "parent": parent,
        "tags": list(tags or []),
        "title": title,
        "type": type_,
        "project": project,
        "cwd": cwd,
        "priority": priority,
        "blocks_everything": blocks_everything,
        "difficulty": difficulty,
        "difficulty_history": (
            [{"value": difficulty, "source": difficulty_source, "ts": datetime.now(timezone.utc).isoformat()}]
            if difficulty is not None
            else []
        ),
        "domain": domain,
        "blocked_by": list(blocked_by or []),
        "session_id": None,
        "locked_at": None,
        "completed_at": None,
        "has_brief": False,
        "roadmap_id": roadmap_id,
        "vision_path": vision_path,
        "details": details,
        "size": size,
        "batch": batch,
        "cost_usd": None,
        "cost_sessions": [],
        "plan_path": plan_path,
        "pr_number": None,
        "pr_url": None,
        "merge_status": None,
        "created_at": datetime.now(timezone.utc).isoformat(),
        "source": source,
        "source_kind": source_kind,
        "source_project": source_project,
        "source_inbox_msg": source_inbox_msg,
        "artifact_url": artifact_url,
        "completion_note": completion_note,
        "source_session_id": source_session_id or prov["source_session_id"],
        "source_harness": prov["source_harness"],
        "source_cwd": prov["source_cwd"],
        "source_node_id": prov["source_node_id"],
        "source_plan_path": prov["source_plan_path"],
        "request_origin": origin,
        "origin_evidence": origin_evidence_ref,
    }
