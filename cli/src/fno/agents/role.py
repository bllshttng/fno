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


def role_reading(row) -> Optional[dict[str, Any]]:
    """The one rendering of a role, or ``None`` when ``row`` holds none.

    Reads ``row.role_label`` (``registry.AgentEntry.role_label``) rather than
    re-deriving the "L{level} {scope}" string, so ``fno whoami`` and ``fno agents
    whoami`` cannot drift into two renderings of the same fact (the Rust
    ``fno agents top`` formats the same ``L{level} {scope}`` label). Safe on the :func:`calling_agent_row` sentinels and on
    ``None``: neither carries ``role_label``, so ``getattr`` answers ``None``
    rather than raising.
    """
    label = getattr(row, "role_label", None)
    if label is None:
        return None
    grantor = getattr(row, "role_grantor", None) or "human"
    level = getattr(row, "role_level", None)
    scope = getattr(row, "role_scope", None)
    return {
        "level": level,
        "scope": scope,
        "grantor": grantor,
        "label": label,
        "text": f"{label} (by {grantor})",
    }


def current_role() -> Optional[dict[str, Any]]:
    """This session's role reading, or ``None`` - never raises.

    ``fno whoami`` must render byte-for-byte unchanged for an attended human
    shell (AC8-EDGE), so any failure resolving the caller's identity or
    registry row degrades to "no role" rather than surfacing.
    """
    try:
        return role_reading(calling_agent_row())
    except Exception:
        return None


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


def _territories_overlap(a: Optional[str], b: Optional[str]) -> bool:
    """Do two stored scopes share ANY territory, aliases normalized?

    The one-live-role scans key on this, not on equality: a stored set
    (a portfolio, or a rung-2 epic set) already rules each of its members,
    so a second role over any member alone is a double rule even though no
    two stored strings are equal.
    """
    left, right = _territory_key(a), _territory_key(b)
    return bool(left) and bool(right) and bool(left & right)


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


def _role_rivals(
    a_scope: Optional[str],
    a_level: Optional[int],
    b_scope: Optional[str],
    b_level: Optional[int],
) -> bool:
    """Do two live roles double-rule territory, ladder-aware?

    The ladder's team is legitimate (a portfolio's team IS project leads),
    so rivalry is rung-scoped: same rung double-rules on any shared member,
    different rungs only on the same territory outright. Rungs derive from
    the members - a row stamped ``level=0`` over ``e-1,e-2`` is exactly how a
    bypass used to switch this guard off. Stored levels tie-break only when
    derivation cannot classify either side; otherwise overlap surfaces.
    """
    a_rung = _derived_level(a_scope)
    b_rung = _derived_level(b_scope)
    if a_rung is None and b_rung is None:
        if a_level is not None and b_level is not None and a_level != b_level:
            return _same_territory(a_scope, b_scope)
    elif a_rung is not None and b_rung is not None and a_rung != b_rung:
        return _same_territory(a_scope, b_scope)
    return _territories_overlap(a_scope, b_scope)


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


class RolePromotionError(RuntimeError):
    """An attended in-place grant that refused without changing the registry."""


def _locked_identity(rows, row) -> Any:
    """Find the row resolved BEFORE the lock by name AND session; no answer refuses."""
    from fno.agents.spawn_overlay_client import SpawnOverlayUnavailable, spawn_overlay_call
    ids = ("harness_session_id", "cc_session_id", "short_id")
    session = next((getattr(row, f) for f in ids if getattr(row, f)), None)
    try:
        answer = spawn_overlay_call({
            "kind": "role-identity",
            "rows": [asdict(row) for row in rows],
            "expect": {"name": row.name, "harness_session_id": session},
        })
    except SpawnOverlayUnavailable as exc:
        raise RolePromotionError(f"identity check unavailable ({exc})") from exc
    return rows[answer["index"]] if answer.get("matched") else None


def emit_role_vacated(
    *,
    scope: Optional[str],
    level: Optional[int],
    holder: Optional[str],
    holder_session: Optional[str],
    grantor: Optional[str],
    cause: str,
    successor: Optional[str] = None,
) -> None:
    """Journal one role leaving its holder, after the registry write commits.

    An departure and a role lost to a bug are indistinguishable from outside
    until one of these lines lands, so the team answers from the record, never
    from testimony.
    """
    from fno.agents import events

    events.emit(
        "agent_role_vacated", scope=scope, level=level, holder=holder,
        holder_session=holder_session, grantor=grantor, cause=cause,
        successor=successor,
    )


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


def adopt_drawn_name(succession: bool, plan: Optional[dict], name: str) -> str:
    """Adopt a succession settle's draw: the machine names the successor."""
    return (plan or {}).get("draw", {}).get("name") or name if succession else name


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


def _widen_answer(scope: str, caller, target_name: str) -> dict:
    """Rust's role-widen answer; a missing/old binary answers ``{}`` (fails closed)."""
    from fno.agents.spawn_overlay_client import SpawnOverlayUnavailable, spawn_overlay_call

    by_id = _graph_index() or {}
    fields = ("name", "status", "role_scope", "role_grantor",
              "harness_session_id", "cc_session_id", "pid")
    member_ids = dict.fromkeys(split_scope(scope) + split_scope(getattr(caller, "role_scope", None)))
    try:
        return spawn_overlay_call({
            "kind": "role-widen",
            "requested": scope,
            "target": target_name,
            "caller": {f: getattr(caller, f, None) for f in fields},
            "members": [by_id.get(m) for m in member_ids],
        })
    except SpawnOverlayUnavailable:
        return {}


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


def arm_promoted_missions(scope: Optional[str]) -> Optional[list[str]]:
    """Mission-arm every open epic container in a promoted scope; the ids
    newly armed, or None when the graph write failed."""
    from fno.agents.spawn_overlay_client import SpawnOverlayUnavailable

    try:
        return spawn_team_write("missions", scope=scope or "")["armed"]
    except SpawnOverlayUnavailable as exc:
        import sys

        print(f"role: WARNING: mission arming failed for scope {scope!r} ({exc}); "
              "arm it with: fno backlog advance --epic <id>", file=sys.stderr)
        return None


def journal_spawn_role(outcome: Optional[str], vacated: list, *, name, level, scope, grantor) -> None:
    """Journal one committed spawn write and arm the scope's missions."""
    spawn_team_write("record", outcome=outcome, name=name, level=level, scope=scope,
                     grantor=grantor, vacated=[(asdict(row), cause) for row, cause in vacated])


def reclaim_role(handle: Optional[str] = None) -> dict[str, Any]:
    """Return a transferred role to its recorded grantor.

    Reclaim is a registry transfer, not a spawn: the current holder is cleared
    and the live row named by ``role_grantor`` receives the same territory and
    level in one locked write. A caller may identify the current holder by
    ``handle`` from an attended shell; without one, the current session must be
    the holder. The grantor is treated as an opaque registry identity and is
    resolved against every session-id field the registry accepts.
    """
    from fno.agents.registry import (
        AgentResolutionError,
        TERMINAL_STATUSES,
        resolve_agent,
        update_registry,
    )

    caller = calling_agent_row()
    if handle:
        # The handle form is the attended operator's reach, and only that: a
        # registered agent that learns a peer's handle must not strip that
        # peer's role by name. The no-handle form below is the one agent
        # path, and it only ever expires the caller's own role. The
        # unreadable/unregistered sentinels refuse too - an identity the
        # registry cannot resolve to "attended human" gets no handle reach.
        if caller is not None:
            raise RolePromotionError(
                "reclaim by handle is an attended-shell action: run it "
                "without --handle inside the successor session, or from a shell "
                "holding no agent identity"
            )
        try:
            holder_snapshot = resolve_agent(handle).entry
        except AgentResolutionError as exc:
            raise RolePromotionError(str(exc)) from exc
    else:
        holder_snapshot = caller
    if holder_snapshot in (None, REGISTRY_UNREADABLE, AGENT_UNREGISTERED):
        raise RolePromotionError(
            "reclaim needs the current promoted holder: run it inside the successor "
            "session or pass that registered handle from an attended shell"
        )
    scope = getattr(holder_snapshot, "role_scope", None)
    level = getattr(holder_snapshot, "role_level", None)
    grantor_key = getattr(holder_snapshot, "role_grantor", None)
    if level is None or not isinstance(scope, str) or not scope.strip():
        raise RolePromotionError("this session holds no role to reclaim")
    if not grantor_key or grantor_key == "human":
        raise RolePromotionError(
            f"role over {scope!r} has no registry grantor to reclaim; "
            "a human grant cannot be returned automatically"
        )

    holder_name = holder_snapshot.name
    holder_session = (
        getattr(holder_snapshot, "harness_session_id", None)
        or getattr(holder_snapshot, "cc_session_id", None)
        or getattr(holder_snapshot, "short_id", None)
        or holder_name
    )
    receipt: dict[str, Any] = {}

    def _reclaim(rows: list) -> list:
        holder = _locked_identity(rows, holder_snapshot)
        if holder is None or holder.status in TERMINAL_STATUSES:
            raise RolePromotionError(
                f"cannot reclaim {scope!r}: current holder {holder_name!r} "
                "is no longer live"
            )
        if holder.role_scope != scope or holder.role_level != level:
            raise RolePromotionError(
                f"cannot reclaim {scope!r}: holder {holder_name!r} changed "
                "its role while reclaim was in flight"
            )
        target = next(
            (
                row
                for row in rows
                if row.name != holder_name
                and grantor_key
                in {
                    row.name,
                    getattr(row, "harness_session_id", None),
                    getattr(row, "cc_session_id", None),
                    getattr(row, "short_id", None),
                }
            ),
            None,
        )
        if target is None:
            raise RolePromotionError(
                f"cannot reclaim {scope!r}: recorded grantor {grantor_key!r} "
                "has no registry row; leave the role in place and re-register "
                "the grantor before retrying"
            )
        if target.status in TERMINAL_STATUSES:
            raise RolePromotionError(
                f"cannot reclaim {scope!r}: recorded grantor {target.name!r} "
                f"is terminal ({target.status}); a role cannot return to a dead row"
            )
        if target.role_scope is not None or target.role_level is not None:
            raise RolePromotionError(
                f"cannot reclaim {scope!r}: grantor {target.name!r} already "
                "holds a role; refusing to replace unrelated authority"
            )
        other = next(
            (
                row
                for row in rows
                if row.name not in {holder_name, target.name}
                and row.status not in TERMINAL_STATUSES
                and _role_rivals(row.role_scope, row.role_level, scope, level)
            ),
            None,
        )
        if other is not None:
            raise RolePromotionError(
                f"cannot reclaim {scope!r}: live row {other.name!r} already "
                f"holds overlapping territory ({other.role_scope!r})"
            )

        returned_by = (
            getattr(holder, "harness_session_id", None)
            or getattr(holder, "cc_session_id", None)
            or getattr(holder, "short_id", None)
            or holder.name
        )
        armed = False
        unarmed_reason = ""
        try:
            from fno.lead.state import arm_lead_manifest

            armed = (
                arm_lead_manifest(
                    scope,
                    getattr(target, "harness_session_id", None) or "",
                    owner_cwd=getattr(target, "cwd", None),
                    role_level=level,
                    role_scope=scope,
                    role_grantor=returned_by,
                    model=getattr(target, "requested_model", None),
                    harness=getattr(target, "harness", None),
                )
                is not None
            )
        except (OSError, ValueError) as exc:
            unarmed_reason = str(exc)
        for index, row in enumerate(rows):
            if row.name == holder_name:
                rows[index] = replace(
                    row,
                    role_level=None,
                    role_scope=None,
                    role_grantor=None,
                )
            elif row.name == target.name:
                rows[index] = replace(
                    row,
                    role_level=level,
                    role_scope=scope,
                    role_grantor=returned_by,
                )
        receipt.update(
            reclaimed=target.name,
            from_holder=holder_name,
            level=level,
            scope=scope,
            grantor=returned_by,
            lead_loop_armed=armed,
        )
        if unarmed_reason:
            receipt["lead_loop_unarmed_reason"] = unarmed_reason
        return rows

    update_registry(_reclaim)
    from fno.lead.state import remove_lead_manifest

    remove_lead_manifest(
        scope,
        owner_cwd=getattr(holder_snapshot, "cwd", None),
        expected_harness_session_id=holder_session,
    )
    return receipt


def promote_existing_session(handle: str, scopes: list[str]) -> dict[str, Any]:
    """Grant a role to one existing row from an attended human shell, or from
    an agent whose own role strictly contains the requested scope.

    Same-scope succession stays on ``spawn --promote``; this function closes the
    human workflow and the in-place re-scope of a live subordinate, where the
    useful target session already exists.

    A row that already holds a role is re-scoped, not refused: the new
    territory replaces the old in the one write below, and the receipt reports
    what was vacated. The live-holder check is the guard that matters here - it
    is what keeps two rows from ruling the same territory.
    """
    try:
        level, scope = resolve_role(scopes)
    except RoleScopeError as exc:
        raise RolePromotionError(str(exc)) from exc

    # Resolved before update_registry, never inside _stamp: this reads the
    # registry itself, and the closure runs under its lock.
    from fno.agents.registry import AgentResolutionError, TERMINAL_STATUSES, resolve_agent, update_registry
    caller = calling_agent_row()
    try:
        resolved_target = resolve_agent(handle).entry
    except AgentResolutionError as exc:
        raise RolePromotionError(
            f"{exc}. `fno agents list` shows every handle you can role."
        ) from exc
    target_name = resolved_target.name
    denial = grant_error(scope, caller, allow_succession=True)
    widen = _widen_answer(scope, caller, target_name) if caller is not None else {}
    if widen.get("widen") is not True and (denial is not None or (caller is not None and target_name == caller.name)):
        raise RolePromotionError(" ".join(filter(None, (denial, widen.get("hint"))))
            or "role self-edit refused: the role-widen answer was unavailable")
    if widen.get("widen") is True and target_name == caller.name and not widen.get("grantor"):
        raise RolePromotionError("role self-edit admitted with no grantor in the answer; update the fno-agents binary (`fno doctor update --rust`)")
    grantor_name = "human" if caller is None else caller.name
    recorded_grantor = widen.get("grantor") or grantor_name
    # `grant_error` blesses an equal scope because SPAWN succession vacates the
    # caller and stamps the successor in one write; this path only stamps the target,
    # so letting it through would leave two live roles and the holder scan
    # below would refuse naming the caller's OWN row. Refuse here, where the
    # remedy is reachable.
    if caller is not None and _same_territory(
        getattr(caller, "role_scope", None), scope
    ):
        raise RolePromotionError(
            f"refusing to role {handle!r}: {scope!r} is your OWN scope, and "
            "this command only stamps the target, so it cannot hand a role over. "
            "Succession runs through `fno agents spawn --promote <scope> "
            "--hand-off` instead, which vacates you and stamps the new holder "
            "in a single registry write, so the scope is never doubly ruled "
            "and never briefly unruled."
        )
    # The authority check ran OUTSIDE the lock, so the grantor's role can move
    # before the stamp. Re-running grant_error under the lock would put graph
    # I/O on the lock, so carry the granted scope and re-assert it under the
    # lock as a plain compare. Fails closed: a grantor whose role moved
    # mid-call cannot grant what it no longer holds.
    granting_scope = None if caller is None else getattr(caller, "role_scope", None)

    receipt: dict[str, Any] = {}
    vacated_manifest_owner = ""
    vacated_owner_cwd = ""

    def _stamp(rows: list) -> list:
        nonlocal vacated_manifest_owner, vacated_owner_cwd
        if caller is not None:
            # The grantor re-asserted under the lock by name AND session, the
            # same pair match the stamp below uses; a rebound grantor name
            # cannot grant what the calling session no longer holds.
            live_caller = _locked_identity(rows, caller)
            if live_caller is not None and live_caller.status in TERMINAL_STATUSES:
                raise RolePromotionError(
                    f"refusing to role {target_name!r}: the grantor's STORED "
                    f"status is {live_caller.status!r}, which is terminal, so "
                    "authority ended before the registry write. Re-register the "
                    "grantor in its live session or use an attended shell."
                )
            live_scope = None if live_caller is None else live_caller.role_scope
            if not _same_territory(live_scope, granting_scope):
                raise RolePromotionError(
                    f"refusing to role {target_name!r}: this session's own role "
                    f"moved from {granting_scope!r} to {live_scope!r} while the "
                    "grant was in flight, so the authority it was checked against "
                    "no longer holds. Re-read your role with `fno agents team`, "
                    "then retry if it still contains the scope."
                )
        target = _locked_identity(rows, resolved_target)
        if target is None:
            raise RolePromotionError(
                f"no agent matching {handle!r}; the target disappeared before the "
                "grant committed. `fno agents list` shows the handles you can role."
            )
        if target.status in TERMINAL_STATUSES:
            # Name the field and a remedy that cannot contradict the refusal:
            # `fno agents list` renders this STORED status beside a
            # freshly-computed `live_status`, and pointing the caller there (as
            # this used to) shows a column saying the row is live.
            raise RolePromotionError(
                f"refusing to role {target.name!r}: its STORED status is "
                f"{target.status!r}, which is terminal. That is a recorded "
                "snapshot, not a live probe, so it can be stale for a session "
                "that is still running. Two ways out:\n"
                "  target is alive    run `fno agents register` IN the target "
                "session (it restamps the row idle in place), then retry\n"
                "  target really died `fno agents reconcile`, then role a live "
                "row instead; `fno agents list` shows stored status beside the "
                "computed live_status, and a disagreement means the stored one "
                "is stale"
            )

        # A row that already holds a role is re-scoped, not refused: the
        # replace() below overwrites the old territory in the same write that
        # stamps the new one, so the vacated scope frees atomically and no
        # reader ever sees two live roles or zero. Captured BEFORE the stamp
        # so the receipt can name what changed hands.
        vacated_scope = target.role_scope
        vacated_level = target.role_level
        vacated_manifest_owner = (
            target.harness_session_id or target.cc_session_id or target.short_id or ""
        )
        vacated_owner_cwd = target.cwd
        if (vacated_level is None) != (vacated_scope is None):
            # Half a role is unstampable by role_validation_error, so no
            # legal writer produces it; overwrite would erase the corruption
            # signal instead of surfacing it.
            raise RolePromotionError(
                f"refusing to role {target.name!r}: it holds half a role "
                f"(level={vacated_level!r}, scope={vacated_scope!r}), which "
                "no legal role produces. fno agents stop the row and "
                "fno agents rm it, then role the re-registered session."
            )

        # An agent grantor's own row legitimately overlaps the delegated scope
        # (grant_error verified a STRICT containment before the write), so the
        # caller is not a second ruler; every other RIVAL live row is. Rivalry
        # is ladder-aware, not bare overlap: a live portfolio over the scope's
        # project is the new lead's team, not a second ruler of it.
        delegating = {target.name} | ({grantor_name} if caller is not None else set())
        holder = next(
            (
                row
                for row in rows
                if row.name not in delegating
                and row.status not in TERMINAL_STATUSES
                and _role_rivals(row.role_scope, row.role_level, scope, level)
            ),
            None,
        )
        if holder is not None:
            raise RolePromotionError(
                f"refusing to role {target.name!r}: scope {scope!r} is already "
                f"held by live row {holder.name!r} (holding "
                f"{holder.role_scope!r}). Three ways out, cheapest "
                "first:\n"
                f"  re-scope the holder   fno agents org promote {holder.name} --scope "
                "<other territory>   (both sessions stay live; retry this "
                "command after)\n"
                "  holder looks dead     fno agents reconcile   (a row whose "
                "harness session is gone flips to orphaned, which frees the "
                "scope)\n"
                f"  end the holder        fno agents stop {holder.name}, then retry"
            )

        try:
            from fno.lead.state import arm_lead_manifest

            manifest_path = arm_lead_manifest(
                scope,
                target.harness_session_id or target.cc_session_id or target.short_id or "",
                owner_cwd=target.cwd,
                role_level=level,
                role_scope=scope,
                role_grantor=recorded_grantor,
                model=getattr(target, "requested_model", None),
                harness=target.harness,
            )
        except (OSError, ValueError) as exc:
            raise RolePromotionError(
                f"refusing to role {target.name!r}: lead manifest arming failed: {exc}"
            ) from exc

        for index, row in enumerate(rows):
            if row.name == target.name:
                rows[index] = replace(
                    row,
                    role_level=level,
                    role_scope=scope,
                    role_grantor=recorded_grantor,
                )
                break
        receipt.update(
            promoted=target.name,
            level=level,
            scope=scope,
            grantor=recorded_grantor,
            vacated_scope=vacated_scope,
            vacated_level=vacated_level,
            lead_loop_armed=manifest_path is not None,
        )
        return rows

    # The persisted rows ARE the lock-window snapshot the strand scan needs:
    # a post-release re-read could see a concurrent grant over the
    # just-freed scope and mislabel that successor as stranded.
    rows_after = update_registry(_stamp)
    # Clear the vacated manifest BEFORE arming missions: a role killed
    # between the registry commit and a slow arm would otherwise leave the
    # leftover on disk, listing as a phantom role.
    if receipt.get("vacated_scope") and not _same_territory(
        receipt["vacated_scope"], scope
    ):
        from fno.lead.state import remove_lead_manifest

        remove_lead_manifest(
            receipt["vacated_scope"],
            owner_cwd=vacated_owner_cwd,
            expected_harness_session_id=vacated_manifest_owner,
        )
    # The recorded team name follows the role (a re-scope used to strand it
    # on the vacated scope; candor landed anonymous twice on 2026-10-04).
    row_after = next((r for r in rows_after if r.name == target_name), None)
    receipt["team_name"] = _carry_team_name(receipt.get("vacated_scope"), scope, row_after, level)
    receipt["missions_armed"] = arm_promoted_missions(scope)
    try:
        receipt["stranded_subordinates"] = _stranded_subordinates(
            receipt["vacated_scope"], scope, target_name, rows_after
        )
    except Exception:
        # Advisory receipt data must never crash a role that committed.
        receipt["stranded_subordinates"] = None
    # The role TYPES the command: the holder learns it terms through raw mail
    # typed as the operator would. Rendered per harness through the one
    # normalizer; an unknown harness keeps the `/fno:` spelling.
    target_row = next((r for r in rows_after if r.name == target_name), None)
    target_harness = getattr(target_row, "harness", None) if target_row else None
    address = target_name
    if target_row is not None:
        address = (
            getattr(target_row, "harness_session_id", None)
            or getattr(target_row, "cc_session_id", None)
            or getattr(target_row, "short_id", None)
            or target_name
        )
    from fno.agents.harness_map import DispatchResolveError, normalize_command

    try:
        command = normalize_command(f"/fno:lead {scope}", target_harness or "")
    except DispatchResolveError:
        command = f"/fno:lead {scope}"
    if caller is not None and target_name == caller.name:
        receipt["term_delivery"] = "skipped: self-edit, this session already terms"
    else:
        receipt["term_delivery"] = _send_term_command(address, command)
    return receipt


def _carry_team_name(vacated_scope: Optional[str], scope: str, row_after, level: int) -> str:
    """Team-name carry behind an in-place grant, via spawn-overlay kind
    ``team-rescope``: a recorded name moves to the landing scope, an unnamed
    team takes the row's people-shaped name. Registry-free (the payload
    names the holder session and level). Advisory: never raises; a store
    refusal is the receipt line naming what did not happen."""
    from fno.agents.spawn_overlay_client import SpawnOverlayUnavailable, spawn_overlay_call

    if row_after is None:
        return "unchanged"
    session = next((s for s in (getattr(row_after, f, None) for f in
                     ("harness_session_id", "cc_session_id", "short_id")) if s), "")
    try:
        answer = spawn_overlay_call({
            "kind": "team-rescope", "old_scope": vacated_scope or "",
            "new_scope": scope, "candidate": row_after.name,
            "holder_session": session, "level": level,
        })
    except SpawnOverlayUnavailable as exc:
        return f"unavailable: {exc}"
    if answer.get("named"):
        return str(answer["named"])
    return "carried" if answer.get("carried") else str(answer.get("reason") or "unchanged")


def _send_term_command(address: str, command: str) -> str:
    """Mail the term command to a freshly promoted holder; never raises.

    Advisory receipt data: a failure is named on the receipt, because a promoted
    session that never receives the command improvises the ritual. `--raw` types
    the payload as the operator would, so a slash arrives as a command.
    """
    import subprocess
    import sys

    try:
        proc = subprocess.run(  # noqa: S603 - fixed argv, no shell
            [sys.executable, "-m", "fno.cli", "agents", "mail",
             "send", address, command, "--raw"],
            capture_output=True,
            text=True,
            timeout=30,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        return f"not delivered ({exc})"
    if proc.returncode == 0 and proc.stdout.strip():
        return proc.stdout.strip().splitlines()[-1]
    detail = (proc.stderr.strip() or proc.stdout.strip() or "no output").splitlines()[-1]
    return f"not delivered (rc={proc.returncode}: {detail})"


def _stranded_subordinates(
    vacated: Optional[str], new_scope: str, target_name: str, rows: list
) -> Optional[list[str]]:
    """Live rows whose role the vacated scope contained and the new one does not.

    Advisory receipt data about a re-scope, decided by Rust (kind ``role-grant``,
    op ``stranded``): ``[]`` when no territory left the new scope, ``None`` when
    the graph could not be read, which is not the same answer as ``[]``.
    Computed by the caller AFTER the registry write returns, over the persisted
    rows, so no graph I/O runs under the lock.
    """
    from fno.agents.spawn_overlay_client import spawn_overlay_call

    by_id = _graph_index()
    scopes = [vacated, new_scope]
    payload_rows = []
    for row in rows:
        scope = row.role_scope if isinstance(row.role_scope, str) else None
        scopes.append(scope)
        payload_rows.append({"name": row.name, "status": str(row.status), "role_scope": scope})
    member_rows = None if by_id is None else _member_rows(scopes, by_id)
    answer = spawn_overlay_call({
        "kind": "role-grant", "op": "stranded", "vacated": vacated, "new_scope": new_scope,
        "target_name": target_name, "rows": payload_rows,
        "projects": _alias_map(scopes, member_rows or []), "entries": member_rows,
    })
    stranded = answer["stranded"]
    return None if stranded is None else [str(name) for name in stranded]


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
