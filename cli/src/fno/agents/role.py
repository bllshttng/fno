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

import os
from dataclasses import asdict, replace
from typing import Any, Optional

#: The bottom rung. Kept as a bound rather than a magic number so the stored
#: value stays far inside the Rust row's ``role_level: Option<u32>``.
MAX_ROLE_LEVEL = 2

#: Separator for a level-0 scope. The registry field is a single string - the
#: Rust side only custodies and displays it (``role_scope.as_deref()`` in
#: client.rs is its one read) - so a portfolio scope is stored as its member
#: project names joined by this, rather than costing a schema version bump and
#: ~90 mechanical edits across two crates for a field the daemon never reasons
#: about.
SCOPE_SEPARATOR = ","


class RoleScopeError(ValueError):
    """A scope that names no territory this ladder recognizes."""


#: Sentinel for "the caller carries an agent identity but the registry could not
#: be read to resolve it to a row." Distinct from ``None`` (an attended human
#: with no identity at all), so an unreadable registry fails CLOSED in
#: :func:`grant_error` instead of authorizing like a human. An authorization
#: check that cannot read the registry does not know the caller is human, so it
#: must refuse rather than assume the most privileged answer.
REGISTRY_UNREADABLE: Any = object()

#: Sentinel for "the caller carries an agent identity but no registry row
#: matches it": distinct from ``None`` (attended human) and
#: :data:`REGISTRY_UNREADABLE` (read failed). The registry WAS read, the caller
#: claims to be an agent, but is not joined - it holds no verified authority,
#: so :func:`grant_error` refuses with the heal rather than authorize. Covers
#: the clean-miss path ``_find_by_session`` answers with ``None``, not raising.
AGENT_UNREGISTERED: Any = object()


def calling_agent_row():
    """The calling session's registry row; :func:`grant_error` treats each
    outcome differently:

    - ``None`` - an attended human. A human may grant any scope.
    - an ``AgentEntry`` - a joined agent; :func:`grant_error` checks its role.
    - :data:`REGISTRY_UNREADABLE` - the caller HAS an agent identity but the
      registry could not be read. Flattening this to ``None`` would let a read
      failure promote any worker to human authority - fail-open on "you cannot
      hand down authority you do not hold" - so it surfaces as a sentinel.
    - :data:`AGENT_UNREGISTERED` - identity present, registry read, but no row
      matches (a just-spawned worker, a session without ``/fno-me``). The clean
      miss flows out as ``None`` without this sentinel, the same fail-open one
      branch over; surfaces so :func:`grant_error` refuses with the heal.

    Resolved the same way ``fno whoami`` does, so "who am I" has one answer.
    """
    from fno.agents.registry import load_registry
    from fno.agents.whoami import _find_by_session
    from fno.agents.self_stamp import resolve_self_identity

    ident = resolve_self_identity()
    if ident.disposition in {
        "ambiguous",
        "invalid",
        "contradiction",
        "name_only",
    } or (ident.disposition == "canonical" and not ident.session_id):
        return REGISTRY_UNREADABLE
    if not ident.session_id or not ident.harness:
        if (os.environ.get("FNO_AGENT_SELF") or "").strip():
            return REGISTRY_UNREADABLE
        return None
    try:
        row = _find_by_session(load_registry(), ident.session_id, ident.harness)
    except Exception:
        # Surface the failure, not flatten it: None means "attended human" and
        # would authorize any grant. The caller refuses on the sentinel instead.
        return REGISTRY_UNREADABLE
    if row is None:
        # Identity present, registry read, no match: an agent the registry does
        # not know yet. _find_by_session returns None on a clean miss WITHOUT
        # raising, so this never reached the except above - returning None here
        # would authorize like a human, the same fail-open one branch over.
        return AGENT_UNREGISTERED
    return row



def canonical_scope(scopes: list[str]) -> str:
    """The stored form: one name, or sorted unique members joined.

    Canonical on purpose. The one-live-role guard compares scopes by equality,
    so ``{web,etl}`` and ``{etl,web}`` must reduce to the SAME string or a second
    role over one portfolio slips through spelled in a different order.
    """
    return SCOPE_SEPARATOR.join(sorted({s.strip() for s in scopes if s.strip()}))


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


def _graph_index() -> Optional[dict[str, dict]]:
    """Return the readable role graph as an id index, or ``None``.

    ONE parse serves the availability probe and every per-row node lookup its
    callers make. ``read_entries`` raises ``ExternalMetadataUnavailable`` under
    an external tracker backend, so ``None`` is a distinct answer from an empty
    index: without it a containment or agreement check would silently degrade to
    "not contained" / "not in the graph" on a machine it never read.
    """
    from fno.graph.api import read_entries

    try:
        entries = read_entries("agents.role")
    except Exception:
        return None
    by_id: dict[str, dict] = {}
    for entry in entries:
        node_id = entry.get("id") if isinstance(entry, dict) else None
        if isinstance(node_id, str) and node_id:
            by_id[node_id] = entry
    return by_id


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


def resolve_role(scopes: list[str], *, graph_entry=None) -> "tuple[int, str]":
    """``scopes`` -> the (rung, stored scope) they imply, derived together by the
    Rust ladder (``territory::resolve_role``). This shim sends its own alias map
    and member rows, so the answer reads the sources this module reads.

    Raises :class:`RoleScopeError` when the scopes name no territory - the
    refusal that keeps implementers unpromoted - or when the resolver cannot
    answer.
    """
    from fno.agents.spawn_overlay_client import SpawnOverlayUnavailable, spawn_overlay_call

    members = split_scope(canonical_scope(scopes))
    projects = {m: canon for m in members if (canon := _canonical_project(m))}
    by_id = None if graph_entry or len(members) == len(projects) else _graph_index()
    entry_of = graph_entry or (by_id.get if by_id is not None else (lambda _id: None))
    rows = [row for m in members if m not in projects and (row := entry_of(m)) is not None]
    try:
        answer = spawn_overlay_call(
            {"kind": "role-resolve", "scopes": members, "projects": projects, "entries": rows}
        )
    except SpawnOverlayUnavailable as exc:
        raise RoleScopeError(f"the ladder resolver did not answer: {exc}") from exc
    if answer.get("refusal"):
        raise RoleScopeError(answer["refusal"])
    try:
        return int(answer["level"]), str(answer["scope"])
    except (KeyError, TypeError, ValueError) as exc:
        raise RoleScopeError(f"the ladder resolver answered malformed JSON: {answer!r}") from exc


def _alias_map(scopes, rows=()) -> dict[str, str]:
    """The canonical project for every scope member or row project that names
    one: the alias facts the Rust scope rules read from this module's config."""
    names = {m for scope in scopes for m in split_scope(scope)}
    names.update(r.get("project") for r in rows if isinstance(r.get("project"), str))
    return {n: canon for n in names if n and (canon := _canonical_project(n))}


def _member_rows(scopes, by_id: dict) -> list[dict]:
    """The graph rows ``by_id`` holds for the members of ``scopes``."""
    members = {m for scope in scopes for m in split_scope(scope)}
    return [by_id[m] for m in sorted(members) if m in by_id]


def grant_error(
    requested_scope: str,
    caller_row,
    *,
    allow_terminal_recovery: bool = False,
    allow_succession: bool = False,
) -> Optional[str]:
    """Why this caller may not grant a role over ``requested_scope``, or None.

    You cannot hand down authority you do not hold. The decision is Rust's
    (``territory::grant_refusal``, kind ``role-grant``): an attended human
    (``caller_row`` None) may grant anything, an agent must hold a live role
    that strictly contains the request, and a caller the check cannot place
    (:data:`REGISTRY_UNREADABLE`, :data:`AGENT_UNREGISTERED`, a terminal row)
    fails closed. This shim sends the facts only Python reads: the caller's
    kind, a stray ``FNO_AGENT_SELF``, the alias map and the member rows. A
    check that cannot answer refuses; an unverifiable grant is never authorized.
    """
    from fno.agents.spawn_overlay_client import SpawnOverlayUnavailable, spawn_overlay_call

    if caller_row is None:
        kind = "human"
    elif caller_row is REGISTRY_UNREADABLE:
        kind = "unreadable"
    elif caller_row is AGENT_UNREGISTERED:
        kind = "unregistered"
    else:
        kind = "agent"
    held = getattr(caller_row, "role_scope", None) if kind == "agent" else None
    held = held if isinstance(held, str) else None
    stray = (os.environ.get("FNO_AGENT_SELF") or "").strip() if kind == "unreadable" else ""
    if stray:
        from fno.agents.self_stamp import resolve_self_identity

        stray = "" if resolve_self_identity().session_id else stray
    rows = _member_rows([requested_scope], (_graph_index() or {}) if held else {})
    status = getattr(caller_row, "status", None) if kind == "agent" else None
    try:
        answer = spawn_overlay_call({
            "kind": "role-grant", "op": "check", "requested": requested_scope,
            "caller": {"kind": kind, "status": None if status is None else str(status), "role_scope": held},
            "allow_terminal_recovery": allow_terminal_recovery, "allow_succession": allow_succession,
            "stray_agent_self": stray, "projects": _alias_map([held, requested_scope], rows),
            "entries": rows,
        })
    except SpawnOverlayUnavailable as exc:
        return f"cannot verify the grantor's authority: the grant check did not answer ({exc})"
    if not isinstance(answer, dict) or "refusal" not in answer:
        return f"cannot verify the grantor's authority: the grant check answered {answer!r}"
    return answer["refusal"] or None


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



def settle_spawn_role(
    rows: list,
    *,
    scope: str,
    plan: dict,
    exclude_name: Optional[str] = None,
    successor: Optional[str] = None,
    successor_harness: Optional[str] = None,
    successor_session: Optional[str] = None,
    successor_cwd: Optional[str] = None,
    stamp: bool = False,
    level: Optional[int] = None,
    grantor: Optional[str] = None,
) -> "tuple[list, str, list]":
    """Apply Rust's lock-time row updates and return vacated rows for journaling.
    The native owner checks occupancy and the successor's carried identity; a missing
    or malformed answer declines without changing the caller's rows.
    """
    from fno.agents.spawn_overlay_client import SpawnOverlayUnavailable, spawn_overlay_call

    try:
        answer = spawn_overlay_call({
            "kind": "spawn-team", "op": "apply", "scope": scope, "exclude_name": exclude_name,
            "plan": plan, "successor": successor, "rows": [asdict(row) for row in rows],
            "successor_identity": {"harness": successor_harness, "session_id": successor_session, "cwd": successor_cwd},
            "stamp": stamp, "level": level, "grantor": grantor,
        })
        outcome = answer["outcome"]
        if outcome not in ("granted", "succeeded", "declined"):
            raise ValueError("invalid spawn-team outcome")
        vacated = [(replace(rows[int(i)], **fields), cause) for i, cause, fields in answer["vacated"]]
        updates = {int(i): replace(rows[int(i)], **fields) for i, fields in answer["updates"].items()}
    except (SpawnOverlayUnavailable, LookupError, TypeError, ValueError):
        return rows, "declined", []
    return [updates.get(i, row) for i, row in enumerate(rows)], outcome, vacated


def adopt_drawn_name(plan: Optional[dict], name: str) -> str:
    """Adopt the settle's draw: a succession, or a fresh grant's --name redraw."""
    return (plan or {}).get("draw", {}).get("name") or name


def plan_spawn_role(
    scope: str,
    caller_row,
    succession: bool,
    exclude_name: Optional[str] = None,
    proposed_name: Optional[str] = None,
    take_over: Optional[str] = None,
) -> "tuple[Optional[str], Optional[dict]]":
    """Check authority, then ask team_settle for a pre-launch occupancy plan.
    Return the refusal and native answer. A failed read refuses before launch;
    settlement rechecks that plan under the registry lock.
    """
    grant_problem = grant_error(
        scope, caller_row, allow_terminal_recovery=True, allow_succession=succession,
    )
    if grant_problem is not None:
        return grant_problem, None
    from fno.agents.registry import load_registry
    from fno.agents.spawn_overlay_client import SpawnOverlayUnavailable, spawn_overlay_call

    # The machine draws successor names; "redraw" is the reserved ask for a
    # fresh draw, never a name (the pool excludes it and the refusals teach it).
    redraw = proposed_name is not None and proposed_name.strip().lower() == "redraw"
    if redraw:
        proposed_name = None
    caller_name = getattr(caller_row, "name", None)
    caller = {"kind": "agent", "name": caller_name} if caller_name else {"kind": "human"}
    try:
        rows = load_registry()
    except Exception as exc:
        return f"cannot decide role occupancy: the registry could not be read ({exc})", None
    payload = {
        "kind": "role-settle",
        "scope": scope,
        "succession": succession,
        "proposed_name": proposed_name,
        "redraw": redraw,
        "caller": caller,
        "exclude_name": exclude_name,
        "rows": [asdict(row) for row in rows],
    }
    if take_over is not None:
        payload["take_over"] = {"reason": take_over}
    try:
        answer = spawn_overlay_call(payload)
    except SpawnOverlayUnavailable as exc:
        return f"cannot decide role occupancy: {exc}", None
    return answer.get("refusal"), answer



def spawn_team_write(op: str, **payload: Any) -> dict:
    """One spawn-team write op in Rust (team_spawn.rs), with the journal
    path this process resolves. Prints the op's notices to stderr."""
    import sys

    from fno import paths
    from fno.agents.spawn_overlay_client import spawn_overlay_call

    answer = spawn_overlay_call({"kind": "spawn-team", "op": op, **payload,
                                 "events": str(paths.global_events_json()),
                                 "registry": str(paths.agents_registry_path())})
    for note in answer.get("notices", []):
        print(note, file=sys.stderr)
    return answer



def journal_spawn_role(outcome: Optional[str], vacated: list, *, name, level, scope, grantor) -> None:
    """Journal one committed spawn write and arm the scope's missions."""
    spawn_team_write("record", outcome=outcome, name=name, level=level, scope=scope,
                     grantor=grantor, vacated=[(asdict(row), cause) for row, cause in vacated])



def role_validation_error(level: Any, scope: Any) -> Optional[str]:
    """Ask the native stamp validator; project resolution remains a caller fact."""
    from fno.agents.spawn_overlay_client import SpawnOverlayUnavailable, spawn_overlay_call

    try:
        return spawn_overlay_call({
            "kind": "spawn-team", "op": "validate", "level": None if level is None else str(level),
            "scope": scope if scope is None or isinstance(scope, str) else repr(scope),
            "level_repr": repr(level), "scope_repr": repr(scope),
            "level_is_int": isinstance(level, int) and not isinstance(level, bool),
            "scope_is_str": isinstance(scope, str),
            "projects": [m for m in split_scope(scope if isinstance(scope, str) else None)
                         if _canonical_project(m)],
        })["refusal"]
    except SpawnOverlayUnavailable as exc:
        return f"cannot validate promotion: {exc}"
