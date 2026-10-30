"""What a spawn records about the node it launched.

``dispatch`` and ``cli`` re-export these names, so every import site and every
test that patches one keeps working. Fields: docs/architecture/node-provenance.md.
"""
from __future__ import annotations

import os
import sys
from typing import Optional


def _capture_parent_edge() -> tuple[Optional[str], Optional[str], Optional[str]]:
    """Capture the spawning session's ambient identity from environment variables.

    Returns ``(session_id, harness, cwd)`` — all three are strings or None.
    Precedence applies within one harness family; markers from two families
    attribute NOTHING (a foreign inherited marker must not be laundered into
    the parent record). Never raises; always returns a triple
    (missing fields degrade to None).

    Harness detection order (Task 2.2):
      CODEX_THREAD_ID        -> harness="codex"
      CLAUDE_CODE_SESSION_ID -> harness="claude"
      CODEX_SESSION_ID       -> harness="codex"
      GEMINI_SESSION_ID      -> harness="gemini"
      OPENCODE_SESSION_ID    -> harness="opencode"
    """
    # OWNED, not precedence: this triple is stamped onto the SPAWNED
    # row as its `spawned_by_*` edge, so an inherited marker records a stranger
    # as the parent for the life of that row. An ambiguous resolve records no
    # lineage rather than a wrong one.
    from fno.claims.self_identity import resolve_self_identity

    identity = resolve_self_identity()

    # $PWD may be unset (non-interactive shells, cron, daemonized procs); fall
    # back to os.getcwd(), which for a `fno agents spawn` subprocess is the
    # spawning session's cwd (inherited), so the parent cwd is always captured.
    parent_cwd: Optional[str] = (os.environ.get("PWD") or os.getcwd()).strip() or None

    # with NO marker the walk still names the family, and an ANCESTOR
    # cannot be the stranger an inherited MARKER can. Gated on an empty marker
    # set, never a missing harness: a present marker that resolved to nothing
    # is a contradiction, and / rule that attributes nothing.
    harness = identity.harness
    if not harness and not identity.markers_present:
        from fno.claims.session_pid import resolve_session_harness

        try:
            harness = resolve_session_harness()
        except Exception:  # noqa: BLE001 - the walk never fails a spawn
            harness = None

    if identity.session_id:
        return identity.session_id, harness, parent_cwd

    # Hosted-session tier. A daemon-hosted session's shells have no harness
    # ancestor for the prover to walk (zsh under a keeper under the daemon),
    # so the strict proof returns nothing in the session's OWN lead shell:
    # measured 2026-10-28, a lead portal shell's spawn fell to the dispatcher
    # path with CLAUDE_CODE_SESSION_ID set. A marker naming a LIVE registry
    # row of the same harness is the daemon's own adoption record - it minted
    # that row from the hosted stream - so the match is a second witness, not
    # raw-env trust. Best-effort: an unreadable registry keeps the refusal.
    try:
        from fno.agents.registry import load_registry
        from fno.agents.whoami import _find_by_session

        registry = load_registry()
    except Exception:  # noqa: BLE001 - the voucher never fails a spawn
        registry = None
    for _marker, family, value in identity.markers_present:
        if registry is None:
            break
        try:
            row = _find_by_session(registry, value, family)
        except Exception:  # noqa: BLE001 - the voucher never fails a spawn
            continue
        if row is not None and row.status == "live":
            return value, family, parent_cwd

    return identity.session_id, harness, parent_cwd


def _lineage_reason(session_id: Optional[str]) -> Optional[str]:
    """Why a mint with no parent session could not name one: the identity
    resolution's disposition and markers, or None when a session was
    captured. A null can be CORRECT (a foreign inherited marker would
    record a stranger as parent); the defect was its silence. Pure: no
    printing, so a register or fallback path can carry a reason without
    emitting the spawn notice.
    """
    if session_id:
        return None
    try:
        from fno.claims.self_identity import resolve_self_identity

        identity = resolve_self_identity()
        markers = ",".join(m for m, _h, _v in identity.markers_present) or "no markers"
        return f"identity disposition={identity.disposition}, markers={markers}"
    except Exception:  # noqa: BLE001 - the notice never breaks the spawn
        return "identity unreadable"


def _report_unlinked_parent(session_id: Optional[str]) -> Optional[str]:
    """Name an unrecorded parent edge in the spawn output, and return the
    reason so the spawn event can carry it : the event holds either
    a session id or this reason, never both empty.
    """
    reason = _lineage_reason(session_id)
    if reason is not None:
        print(f"spawn: parent edge NOT recorded ({reason}); this worker will not "
              f"appear in its spawner's orphan check", file=sys.stderr)
    return reason


def build_spawn_provenance(
    *,
    explicit_origin: Optional[dict] = None,
    explicit_owner: Optional[dict] = None,
    cause: Optional[str] = None,
) -> Optional[dict]:
    """The Python half of the one spawn door (v33): the validated origin+owner
    record. ``None`` when a session caller cannot be proven. Explicit producers
    MUST name a mission/role owner; ``cause`` speaks the vocabulary minus ``sob``."""
    if explicit_origin is not None and explicit_owner is not None:
        _validate_explicit_provenance(explicit_origin, explicit_owner, cause)
        return {"origin": explicit_origin, "owner": explicit_owner}

    # Explicit dispatch context outranks ambient capture.
    carried_origin = os.environ.get("FNO_SPAWN_ORIGIN")
    carried_owner = os.environ.get("FNO_SPAWN_OWNER")
    if carried_origin or carried_owner:
        if carried_origin and not carried_owner:
            raise ValueError("FNO_SPAWN_ORIGIN and FNO_SPAWN_OWNER must be exported together")
        import json as _json
        owner = _json.loads(carried_owner) if carried_owner else None
        if carried_origin:
            origin = _json.loads(carried_origin)
            return build_spawn_provenance(explicit_origin=origin, explicit_owner=owner, cause=cause)
        # Owner-only carrier: WHO ANSWERS rides; WHO CAUSED it is ambient.
        from fno.claims.self_identity import resolve_self_identity

        identity = resolve_self_identity()
        if not (identity.session_id and identity.harness):
            return None
        parent = {"harness": identity.harness, "session_id": identity.session_id, "cwd": (os.environ.get("PWD") or os.getcwd()).strip()}
        return {"origin": {"kind": "session", "parent": parent, "invocation": None}, "owner": owner}

    from fno.claims.self_identity import resolve_self_identity

    identity = resolve_self_identity()
    session_id = identity.session_id
    harness = identity.harness
    cwd = (os.environ.get("PWD") or os.getcwd()).strip()
    if not session_id or not harness:
        return None
    parent = {"harness": harness, "session_id": session_id, "cwd": cwd}
    return {
        "origin": {"kind": "session", "parent": parent, "invocation": None},
        "owner": {"kind": "session", **parent},
    }


def _validate_explicit_provenance(origin: dict, owner: dict, cause: Optional[str]) -> None:
    """Mirror the door's rules for explicit non-session provenance."""
    source = origin.get("source") if origin.get("kind") == "non_session" else None
    if source is None:
        return
    if source.get("kind") not in ("daemon", "launch_agent"):
        return
    if owner.get("kind") not in ("mission", "role"):
        raise ValueError(
            "daemon/launch-agent origin requires a mission or role owner; "
            "the daemon starter is never substituted"
        )
    declared = source.get("cause")
    if cause is not None and declared is None:
        source["cause"] = cause
        declared = cause
    if declared == "sob":
        raise ValueError("cause 'sob' is retired; it cannot ride a spawn")
    if declared is not None:
        from fno.agents.naming import dispatch_sources

        if declared not in dispatch_sources():
            raise ValueError(f"cause {declared!r} is not in the naming-codes vocabulary")

