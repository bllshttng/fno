"""The role vocabulary: what a scope is, what rung it implies, and what a
grantor may hand down. ``registry`` owns the three row fields; this module owns
their meaning. Its one file write is the role journal: the events a grant,
a vacate, or a spawn-time handoff leaves in ``~/.fno/events.jsonl``.

THE LADDER IS THREE RUNGS, EACH A FACT ABOUT THE SCOPE:

    0   several projects   scope names 2+ config projects (a portfolio)
    1   one project        scope names one config project
    2   a set of epics     every scope names a backlog node with type == "epic"

No rung for an implementer: a non-epic node is work, not territory, so a role
aimed at one is REFUSED - which is why no caller passes a level;
``resolve_role`` reads the rung off the scope, and a scope naming no
territory has none to read. Callers hand-type no altitude (the old surface let a
backwards ladder - 0 is the TOP - silently mint wrong authority).

CANONICALIZATION CONTRACT: the one-live-role guard compares stored scopes by
exact equality, so ``resolve_role`` guarantees every stamp is canonical
(aliases resolved, members sorted and deduped). The only gap is a pre-redesign
``--promote level=,scope=`` row storing a raw alias spelling - that path is
deleted and was unreleased; re-role such a row rather than merging spellings.
"""
from __future__ import annotations

from typing import Any, Optional


#: Separator for a level-0 scope. The registry field is a single string - the
#: Rust side only custodies and displays it (``role_scope.as_deref()`` in
#: client.rs is its one read) - so a portfolio scope is stored as its member
#: project names joined by this, rather than costing a schema version bump and
#: ~90 mechanical edits across two crates for a field the daemon never reasons
#: about.
SCOPE_SEPARATOR = ","


def split_scope(scope: Optional[str]) -> list[str]:
    """The members of a stored scope; a single-name scope yields one element.

    Guards on type, not just truthiness: a corrupted registry row can carry a
    non-string ``role_scope`` (e.g. a stray int from a hand-edit), and
    ``5.split(...)`` would raise past every caller here, including
    ``fno agents team``, which promises to exit 0 on a read.
    """
    if not scope or not isinstance(scope, str):
        return []
    return [s for s in (part.strip() for part in scope.split(SCOPE_SEPARATOR)) if s]


def _canonical_project(name: str) -> Optional[str]:
    """``name`` resolved to its CANONICAL project name, or None if it is not a
    project. Projects are declared in config
    (``work.workspaces.<ws>.projects[].name``) and need no backlog node - which is
    why the top two rungs cannot be derived from the graph alone.

    Returning the canonical name rather than a bool is load-bearing: the resolver
    accepts a project's ``short_name`` alias too, so a caller can spell one
    territory two ways. Storing the raw spelling would let `alpha` and its alias
    `a` sit as two live roles over one project (the guard compares scopes by
    equality), and naming both at once would read as a two-project portfolio
    instead of one project named twice.

    The resolver is native (``fno-agents spawn-overlay project resolve``); any
    failure degrades to None exactly as the in-process import once did.
    """
    try:
        import subprocess

        from fno.rust_binary import resolve_binary

        binary = resolve_binary()
        if binary is None:
            return None
        proc = subprocess.run(
            [str(binary), "spawn-overlay", "project", "resolve", name],
            capture_output=True,
            text=True,
            check=False,
            timeout=30,
        )
        if proc.returncode != 0:
            return None
        return proc.stdout.strip()
    except Exception:
        return None


def _canonical_members(scope: Optional[str]) -> set:
    """Alias-normalized member set of a stored scope; empty for None/blank.

    The equality and overlap checks here read this one normalization; the
    Rust containment rule (``loop_lead::canonical_members``) applies the same
    one over the alias map :func:`_alias_map` sends.
    """
    return {(_canonical_project(m) or m) for m in split_scope(scope)}


def _territory_key(scope: Optional[str]) -> frozenset[str]:
    """The hashable normalized territory used by all scope equality checks."""
    return frozenset(_canonical_members(scope))


def _same_territory(a: Optional[str], b: Optional[str]) -> bool:
    """Do two stored scopes name the same territory, aliases normalized?"""
    left, right = _territory_key(a), _territory_key(b)
    return bool(left) and left == right


def _derived_level(scope: Optional[str]) -> Optional[int]:
    """The rung the SCOPE sits on, read off its members, never a stored number:
    all projects is 1/0 by count, none is an epic set (2), a mix is ``None`` -
    the undecidable case the rivalry guard fails closed on.
    """
    members = _canonical_members(scope)
    if not members:
        return None
    project_members = {m for m in members if _canonical_project(m)}
    if project_members and len(project_members) == len(members):
        return 1 if len(members) == 1 else 0
    if not project_members:
        return 2
    return None


def role_scope_matches(held: Optional[str], requested: Optional[str]) -> bool:
    """Will the row-keyed lead readers accept a role over ``held`` for
    ``requested``? Territory equality, aliases normalized.

    NOT a containment check: grant answers "may this role grant that scope"
    with the ladder, while the readers answer a string question
    (``leads/{role_scope}.md`` built verbatim; ``done`` refuses on
    ``own != scope``), so a project role does NOT satisfy an epic manifest.
    A first cut reused the containment rule and silenced exactly the state the
    warning exists to make loud. Reuse the rule that matches the QUESTION.
    A blank scope on either side answers False.
    """
    if not held or not requested:
        return False
    return _same_territory(held, requested)


def role_answers_to(held: Optional[str], requested: Optional[str]) -> bool:
    """Does a live role over ``held`` answer when ``requested`` is addressed?

    Territory equality, plus one widening: a rung-2 epic set answers for any
    subset of its members. Not containment (``territory::scope_contains``): a project or portfolio role
    never answers for a narrower scope, because its team may hold that role.
    """
    if role_scope_matches(held, requested):
        return True
    asked = _canonical_members(requested)
    return bool(asked) and _derived_level(held) == 2 and asked <= _canonical_members(held)


def resolve_to_lead(scope: str, *, registry_path=None) -> list[str]:
    """Every live row holding role ``scope`` right now, by name, sorted.

    Read at send time, never off a handle a peer learned while that handle was
    promoted; a pointer written at departure goes stale the second time the role
    moves. Empty is vacant, one is the holder, more is the split role
    ``fno agents team`` already reports. A rung-2 epic set answers for each
    of its members."""
    from fno.agents.registry import TERMINAL_STATUSES, load_registry

    rows = load_registry(path=registry_path) if registry_path else load_registry()
    return sorted(
        {
            row.name
            for row in rows
            if getattr(row, "role_level", None) is not None
            and role_answers_to(getattr(row, "role_scope", None), scope)
            and getattr(row, "status", None) not in TERMINAL_STATUSES
        }
    )


def lead_loop_enabled() -> bool:
    """Resolve ``config.lead.enabled``, fail-safe to OFF like every gate here."""
    try:
        from fno.config import load_settings

        return bool(load_settings().lead.enabled)
    except Exception:  # noqa: BLE001
        return False


def arm_lead_manifest(
    scope: str,
    harness_session_id: str,
    *,
    owner_cwd: Optional[str] = None,
    role_level: Optional[int] = None,
    role_scope: Optional[str] = None,
    role_grantor: Optional[str] = None,
    model: Optional[str] = None,
    harness: Optional[str] = None,
    row: Any = None,
) -> Optional[str]:
    """Arm the scope's lead manifest the moment a role becomes authoritative.

    The Rust creator (``spawn-team`` op ``arm``) checks the session id and
    writes; with the lead loop off it removes a stale manifest and answers
    None. A row fills every field the caller left out: the holder's own
    harness and requested model, never the arming caller's. Every refusal
    raises ValueError, which the arming callers degrade on.
    """
    from fno.agents.spawn_overlay_client import SpawnOverlayUnavailable, spawn_overlay_call

    if row is not None:
        owner_cwd = owner_cwd or getattr(row, "cwd", None)
        role_level = role_level if role_level is not None else getattr(row, "role_level", None)
        role_scope = role_scope if role_scope is not None else getattr(row, "role_scope", None)
        role_grantor = role_grantor if role_grantor is not None else getattr(row, "role_grantor", None)
        model = model or getattr(row, "requested_model", None)
        harness = harness or getattr(row, "harness", None)
    try:
        answer = spawn_overlay_call({
            "kind": "spawn-team", "op": "arm", "scope": scope, "lead_enabled": lead_loop_enabled(),
            "harness_session_id": harness_session_id, "owner_cwd": owner_cwd,
            "role_level": role_level, "role_scope": role_scope, "role_grantor": role_grantor,
            "model": model, "harness": harness,
        })
    except SpawnOverlayUnavailable as exc:
        raise ValueError(f"lead manifest: {exc}") from exc
    if answer.get("error"):
        raise ValueError(answer["error"])
    return answer.get("path")


def lead_manifest_call(op: str, **payload: Any) -> dict[str, Any]:
    """One round-trip to the Rust lead manifest ops (``spawn-overlay`` kind
    ``lead``); an unreachable binary answers ``{"error": ...}``."""
    from fno.agents.spawn_overlay_client import SpawnOverlayUnavailable, spawn_overlay_call

    try:
        return spawn_overlay_call({"kind": "lead", "op": op, **payload})
    except SpawnOverlayUnavailable as exc:
        return {"error": str(exc)}
