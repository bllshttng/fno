"""Shared helpers used by cli.py for intake, blocker validation, and project detection.

Extracted from scripts/roadmap-tasks.py to avoid circular imports between
cli.py and the core graph modules.
"""
from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path
from typing import Collection, Literal, Optional, TypedDict, Union

from fno.graph._constants import (
    PRIORITY_ORDER, _rank_band,
)
from fno import paths as _paths
from fno.graph.statuses import VALID_STATUSES
from fno.graph._reconcile import node_is_open


# Source-field vocabulary for nodes intaked from a plan. Both spellings
# resolve to the same logical source. "adopt" is the legacy spelling
# from PR #184 and earlier; "intake" is the canonical spelling going
# forward. New writers MUST emit "intake"; readers MUST accept either.
# The migration script (cli/scripts/migrate_source_field.py) rewrites
# existing rows opportunistically, but old graph.json files on backups
# may persist indefinitely so the back-compat is permanent.
INTAKE_SOURCE_VALUES: frozenset[Literal["intake", "adopt"]] = frozenset({"intake", "adopt"})


# -- TypedDicts for intake results --

class _IntakeAlready(TypedDict):
    status: Literal["already"]
    id: str
    title: str


class _IntakeReady(TypedDict):
    status: Literal["ready"]
    node_spec: dict


class _IntakeClaim(TypedDict):
    status: Literal["claim"]
    id: str
    title: str
    claim_source: Literal["cli", "frontmatter"]
    node_spec: dict


_IntakeResult = Union[_IntakeAlready, _IntakeReady, _IntakeClaim]


# -- Graph navigation helpers --

def _dashless_variant(node_id: str) -> str:
    """The dash-less spelling of a node id (`x-bbbb` -> `xbbbb`): the minter
    briefly minted dash-less ids, and the graph confirms the id, not the shape.
    """
    return node_id.replace("-", "", 1) if "-" in node_id else node_id


def _find_node(entries: list[dict], node_id: str) -> dict | None:
    # 'ab-' (3) + 8 hex = 11. Anything shorter that starts with 'ab-' is a
    # partial ab-id; route through the fuzzy resolver so callers like
    # `fno backlog done ab-9728` work without typing the full 8 chars.
    # Ambiguity returns None to preserve today's 'not found' contract,
    # but stderr names the candidate IDs so the user can disambiguate
    # rather than seeing a misleading "no such node" from the caller.
    # branch_derived is excluded from the resolved-kinds gate because this
    # helper only takes explicit user input (never a git branch); resolve_id
    # cannot produce branch_derived without a git_branch arg, so the gate
    # is exhaustive for this call shape.
    if node_id.startswith("ab-") and len(node_id) < 11:
        from fno.graph.fuzzy import resolve_id
        match = resolve_id(node_id, entries)
        if match.kind in ("exact", "fuzzy") and match.candidates:
            # candidates[0] is the matched entry; resolve_id already did
            # the iteration so we don't repeat it here.
            return match.candidates[0]
        if match.kind == "ambiguous":
            candidate_ids = ", ".join(
                (e.get("id") or "?") for e in match.candidates
            )
            sys.stderr.write(
                f"[graph] ambiguous prefix '{node_id}' matches: "
                f"{candidate_ids}\n"
            )
        return None
    for e in entries:
        if e.get("id") == node_id:
            return e
    # The dash-less twin, exact-match only: the graph confirms the id, not
    # the spelling. Exact spelling wins first.
    return next(
        (e for e in entries if e.get("id") == _dashless_variant(node_id)), None
    )


def _find_dependents(entries: list[dict], node_id: str) -> list[str]:
    return [
        e["id"]
        for e in entries
        if node_id in e.get("blocked_by", [])
    ]


def _would_create_cycle(
    entries: list[dict], node_id: str, proposed_parent_id: str
) -> bool:
    """True iff setting node_id.parent = proposed_parent_id forms a cycle.

    A cycle exists when node_id appears in proposed_parent_id's ancestor
    chain (i.e. node_id is itself an ancestor of the proposed parent).
    Walks proposed_parent_id upward via the parent field; finite because
    cycle-free graphs terminate when parent is None, and we trip the cycle
    bit if we revisit any seen id along the walk.
    """
    if proposed_parent_id == node_id:
        return True
    if not entries:
        return False
    # Build an id -> entry lookup once. Linear scan per step would make
    # the walk O(N x D); the lookup is O(N + D) and matters once the
    # graph grows to thousands of nodes. Guard against non-dict items
    # and missing string ids so a malformed graph.json can't crash the
    # cycle check.
    id_to_entry: dict[str, dict] = {
        e["id"]: e
        for e in entries
        if isinstance(e, dict) and isinstance(e.get("id"), str)
    }
    seen: set[str] = {node_id}
    current: Optional[str] = proposed_parent_id
    while current:
        if current in seen:
            return True
        seen.add(current)
        ancestor = id_to_entry.get(current)
        if ancestor is None:
            return False
        current = ancestor.get("parent")
    return False


def _would_exceed_epic_depth(
    entries: list[dict], node: dict, parent_node: dict
) -> bool:
    """True iff parenting ``node`` under ``parent_node`` breaks the epic-nesting
    cap (wave 3: mission -> epic -> leaf, two epic levels).

    Fires only when BOTH are epics and either direction would make a third epic
    level: (a) the parent already has an epic ancestor (it is a nested epic, not
    a top-level mission), so ``node`` would be the third level below it; or (b)
    ``node`` already owns an epic subtree, so parenting it under an epic pushes
    its own child epic to the third level. A leaf under any epic, or a childless
    epic under a mission, is allowed. Cycle-safe (seen set / descendants_of).
    """
    if node.get("type") != "epic" or parent_node.get("type") != "epic":
        return False
    id_to_entry = {
        e["id"]: e
        for e in entries
        if isinstance(e, dict) and isinstance(e.get("id"), str)
    }
    # (a) walk UP from the parent: any epic ancestor means the parent is nested.
    seen: set[str] = set()
    current = parent_node.get("parent")
    while current and current not in seen:
        seen.add(current)
        ancestor = id_to_entry.get(current)
        if ancestor is None:
            break
        if ancestor.get("type") == "epic":
            return True
        current = ancestor.get("parent")
    # (b) walk DOWN from node: an epic descendant means node is itself a
    # mission, so nesting it under another epic exceeds the cap.
    nid = node.get("id")
    if nid:
        for desc_id in descendants_of(entries, nid):
            desc = id_to_entry.get(desc_id)
            if desc and desc.get("type") == "epic":
                return True
    return False


def descendants_of(entries: list[dict], parent_id: str) -> set[str]:
    """Return the set of node IDs that are transitive children of ``parent_id``.

    Walks the ``parent`` field downward (epic -> group children -> any
    deeper children) via a children-by-parent index, returning every node
    reachable below ``parent_id``. The parent itself is never included.

    Powers the ``--parent <epic-id>`` epic-scope filter on ``fno backlog
    next``/``ready`` (C2): candidates are restricted to this
    set so a walk drains one epic's subtree feature-by-feature.

    Cycle-safe: a ``seen`` set bounds the BFS so a malformed graph with a
    ``parent`` cycle terminates instead of looping. Non-dict items and
    entries without a string ``id`` are skipped, mirroring the defensive
    posture of ``_would_create_cycle`` and ``make_selection_sort_key``.
    An unknown ``parent_id`` (or one with no children) returns an empty
    set; callers validate node existence separately before filtering.
    """
    children_by_parent: dict[str, list[str]] = {}
    for e in entries:
        if not isinstance(e, dict):
            continue
        node_id = e.get("id")
        pid = e.get("parent")
        # Both keys must be strings: a corrupted row with a dict/list parent
        # would otherwise raise TypeError on the dict insert below.
        if isinstance(node_id, str) and isinstance(pid, str):
            children_by_parent.setdefault(pid, []).append(node_id)

    result: set[str] = set()
    frontier = list(children_by_parent.get(parent_id, []))
    while frontier:
        current = frontier.pop()
        # Never include the scoped parent itself, even if a malformed cycle
        # points a descendant's child chain back to it (contract: parent is
        # excluded). The `in result` guard bounds the BFS against cycles.
        if current in result or current == parent_id:
            continue
        result.add(current)
        frontier.extend(children_by_parent.get(current, []))
    return result


def _graph_sort_key_fn(e: dict) -> tuple:
    return (PRIORITY_ORDER.get(e.get("priority", "p2"), 2), e.get("created_at", ""))


UNSCOPED_LABEL = "(unscoped)"


def _project_key(entry: dict) -> str:
    """Normalize a node's project for swimlane grouping."""
    proj = entry.get("project")
    if isinstance(proj, str) and proj.strip():
        return proj
    return UNSCOPED_LABEL


def _lane_order_key(project: str) -> tuple:
    """Order named project lanes alphabetically and the unscoped lane last."""
    return (project == UNSCOPED_LABEL, project)


_TERMINAL_EPIC_STATUSES = frozenset({"done", "superseded", "deferred"})


def _priority_name(entry: dict) -> str:
    """Return a canonical priority, degrading malformed values to p2."""
    priority = entry.get("priority")
    return priority if isinstance(priority, str) and priority in PRIORITY_ORDER else "p2"


def _sort_text(value: object) -> str:
    """Normalize a graph value into a comparable sort-key string."""
    return value if isinstance(value, str) else ""


def _has_terminal_marker(entry: dict, field: str) -> bool:
    """Match the graph's truthy terminal-marker semantics for malformed rows."""
    return bool(entry.get(field))


def _epics_with_child_progress(id_to_entry: dict[str, dict]) -> frozenset[str]:
    """Epic ids having at least one child that has started or finished.

    Computed ONCE per board render and passed down. It used to be an `any()`
    over every entry inside `_live_epic_for`, which is called from a sort key -
    an O(n) scan per comparison over an O(n) sort, so a 2000-row graph paid
    millions of dict reads to answer a question with one shape.
    """
    progressing: set[str] = set()
    for child in id_to_entry.values():
        if not isinstance(child, dict):
            continue
        parent_id = child.get("parent")
        if not isinstance(parent_id, str) or parent_id in progressing:
            continue
        if (
            child.get("completed_at")
            or child.get("status") in ("done", "in_progress")
            or child.get("session_id")
        ):
            progressing.add(parent_id)
    return frozenset(progressing)


def _live_epic_for(
    node: object,
    id_to_entry: dict[str, dict],
    child_progress_epics: frozenset[str] | None = None,
) -> dict | None:
    """Return the node's live epic parent, or None when it has none."""
    if not isinstance(node, dict):
        return None
    parent_id = node.get("parent")
    epic = id_to_entry.get(parent_id) if isinstance(parent_id, str) else None
    if epic is None or epic.get("type") != "epic":
        return None
    epic_priority = epic.get("priority")
    if not isinstance(epic_priority, str) or epic_priority not in PRIORITY_ORDER:
        return None
    status = epic.get("status")
    if status is not None and (
        not isinstance(status, str) or status not in VALID_STATUSES
    ):
        return None
    created_at = epic.get("created_at")
    if created_at is not None and not isinstance(created_at, str):
        return None
    # ``recompute_statuses`` may derive ``status: done`` from all direct
    # children without stamping the container's own completed_at. Keep that
    # legacy board overlay visible for this derived shape; explicit terminal
    # markers still make the epic terminal.
    explicitly_terminal = (
        _has_terminal_marker(epic, "completed_at")
        or _has_terminal_marker(epic, "superseded_by")
        or _has_terminal_marker(epic, "deferred_at")
    )
    if child_progress_epics is None:
        child_progress_epics = _epics_with_child_progress(id_to_entry)
    child_progress = epic.get("id") in child_progress_epics
    status_terminal = isinstance(status, str) and (
        status in _TERMINAL_EPIC_STATUSES - {"done"}
        or (status == "done" and not child_progress)
    )
    if explicitly_terminal or status_terminal:
        return None
    return epic


def in_progress_epic_ids(
    entries: list[dict],
    live_claimed: Collection[str] = frozenset(),
) -> frozenset[str]:
    """Return live epic ids with a completed or claimed child."""
    id_to_entry = {
        entry["id"]: entry
        for entry in entries
        if isinstance(entry, dict) and isinstance(entry.get("id"), str)
    }
    child_progress_epics = _epics_with_child_progress(id_to_entry)
    result: set[str] = set()
    for child in entries:
        if not isinstance(child, dict):
            continue
        child_id = child.get("id")
        if not (
            child.get("completed_at")
            or child.get("status") in ("done", "in_progress")
            or child.get("session_id")
            or (isinstance(child_id, str) and child_id in live_claimed)
        ):
            continue
        epic = _live_epic_for(child, id_to_entry, child_progress_epics)
        if epic is not None:
            result.add(epic["id"])
    return frozenset(result)


# The encounter-weighting fold that serves selection scoring. It lived in
# graph/demand.py beside the demand read; the demand read is native Rust
# (the grouped dispatcher) and the module deleted, so the scorer moves to
# its surviving owner, the selection key.
PRIORITY_WEIGHT = {"p0": 1, "p1": 2, "p2": 3, "p3": 4}
_DEFAULT_WEIGHT = PRIORITY_WEIGHT["p2"]


#: The one shared operator voter key. A literal here and a check everywhere a
#: kind is tested; the constant is what keeps a third module from re-spelling it.
OPERATOR_VOTER_KIND = "operator"


def voter_key(record: dict) -> str:
    """Return the identity that makes an encounter one-per voter."""
    return str(record.get("voter_key") or record.get("session_id") or "")


def recent_encounter(entry: dict, now, within_days: int) -> bool:
    """True when some encounter was recorded inside the window.

    A vote says the node cost somebody time THEN. Unwindowed it would be a
    permanent exemption from the age drain that any agent could switch on with
    no undo, so this reads the ``created_at`` the record already carries.
    """
    stamps = (
        _parse_ts(r.get("created_at") or r.get("ts"))
        for r in (entry.get("encounters") or [])
        if isinstance(r, dict)
    )
    return any(s is not None and (now - s).days <= within_days for s in stamps)


def encounter_voters(entry: dict) -> set:
    """Distinct voter keys that recorded an encounter with this node.

    Distinct VOTERS, never rows. The write command already refuses a second vote per
    voter, so a duplicate row means the record was written some other way;
    counting rows would let that path inflate the signal. The fallback to
    ``session_id`` keeps encounters written before ``voter_key`` was introduced.
    """
    return {
        voter_key(e)
        for e in (entry.get("encounters") or [])
        if isinstance(e, dict) and voter_key(e)
    }


def operator_voters(entry: dict) -> set:
    """The subset of encounter voters that voted under the operator key."""
    return {
        voter_key(e)
        for e in (entry.get("encounters") or [])
        if isinstance(e, dict)
        and e.get("voter_kind") == OPERATOR_VOTER_KIND
        and voter_key(e)
    }


def divergence_score(entry: dict, effective_priority: str, voters: int | None = None) -> int:
    """Encounter weight against operator priority.

    Higher means the operator is looking at it less than the agents are hitting
    it. A node no session was ever sent to, that sessions keep hitting anyway,
    doubles: it is the loudest row available, and it is the one no other
    instrument reports. ``voters`` lets a caller that already built the set
    pass its size rather than walk ``encounters`` a second time.
    """
    weight = PRIORITY_WEIGHT.get(effective_priority, _DEFAULT_WEIGHT)
    if not entry.get("sessions") and not entry.get("pr_number"):
        weight *= 2
    return (len(encounter_voters(entry)) if voters is None else voters) * weight


#: Age is worth at most 90/100 of a point, less than the smallest one vote can
#: be worth (a p0 vote scores 1), so age never buys a vote. It only breaks ties.
_AGE_CAP_DAYS = 90
_AGE_DIVISOR = 100.0


def _parse_ts(value: object):
    """One ISO reader for both clocks here. An unreadable stamp is no signal."""
    from datetime import datetime, timezone

    try:
        stamp = datetime.fromisoformat(str(value).replace("Z", "+00:00"))
    except (TypeError, ValueError):
        return None
    return stamp if stamp.tzinfo else stamp.replace(tzinfo=timezone.utc)


def importance_score(entry: dict, effective_priority: str, now=None) -> float:
    """Divergence plus age, for the unranked band of the selection key.

    A projection, never stored, and zero for an unvoted node: a score from age
    alone would reorder the backlog on a signal no one recorded. That test runs
    first, so the sort parses no timestamp for the rows that have no vote.
    Difficulty never enters, being a routing axis. Age reads ``touched_at`` and
    falls back to birth; an unparseable stamp is no age signal.
    """
    from datetime import datetime, timezone

    voters = encounter_voters(entry)
    if not voters:
        return 0.0
    divergence = float(divergence_score(entry, effective_priority, len(voters)))
    stamp = _parse_ts(entry.get("touched_at")) or _parse_ts(entry.get("created_at"))
    if stamp is None:
        return divergence
    days = max(0, ((now or datetime.now(timezone.utc)) - stamp).days)
    return divergence + min(days, _AGE_CAP_DAYS) / _AGE_DIVISOR


def make_effective_priority(entries: list[dict]):
    """Build the board-priority projection shared with live-epic ordering.

    A live epic can only promote a child; a child already above its epic keeps
    its own priority. Missing, malformed, and terminal parents are loose-node
    equivalents. The returned value is read-only and never mutates either row.
    """
    id_to_entry = {
        e["id"]: e
        for e in entries
        if isinstance(e, dict) and isinstance(e.get("id"), str)
    }
    child_progress_epics = _epics_with_child_progress(id_to_entry)

    def priority_for(node: object) -> str:
        if not isinstance(node, dict):
            return "p2"
        child_priority = _priority_name(node)
        epic = _live_epic_for(node, id_to_entry, child_progress_epics)
        if epic is None:
            return child_priority
        epic_priority = _priority_name(epic)
        if PRIORITY_ORDER[epic_priority] < PRIORITY_ORDER[child_priority]:
            return epic_priority
        return child_priority

    return priority_for


def make_selection_sort_key(
    entries: list[dict],
    orphans: Optional[frozenset[str]] = None,
    *,
    swimlane: bool = False,
    live_claimed: Collection[str] = frozenset(),
):
    """Build the rank-then-epics-first selection sort key (Locked Decision 7, C3).

    Returns the one key function for sorting board cards and ready candidates.
    ``swimlane=True`` prepends the project lane used by renderers; the remaining
    suffix is byte-identical to selection precedence. The term order and what
    each term means are in ``docs/architecture/backlog-board-ordering.md``. The
    invariants a caller relies on:

    - An operator's pin (band 0) outranks every unranked node, but only among
      the peers it ordered: its lane for a loose node, its live epic's children
      for a child. A pinned child is not the project's next node.
    - With no rank and no encounter anywhere, ordering is byte-for-byte the
      epics-first behavior that predates both terms.
    - An "epic child" is a node whose ``parent`` resolves in ``entries``. A
      ``parent`` naming a missing node is a loose node, never a crash.
    - Everything is precomputed against ``entries`` once, so sorting stays
      O(N log N): every per-node lookup below is a table read.
    """
    id_to_entry: dict[str, dict] = {
        e["id"]: e
        for e in entries
        if isinstance(e, dict) and isinstance(e.get("id"), str)
    }
    # Once per sort, not once per comparison: the key below runs O(n log n)
    # times and this answer does not change while it does.
    child_progress_epics = _epics_with_child_progress(id_to_entry)
    # Fan-out: how many OPEN nodes wait on each id. Within a band the
    # node more work hangs on goes first, AFTER the priority terms: a decision
    # outranks a measurement (a p2 blocking three must not jump a p1 blocking none).
    dependents: dict[str, int] = {}
    for e in entries:
        if not isinstance(e, dict) or not node_is_open(e):
            continue
        for bid in e.get("blocked_by") or []:
            if isinstance(bid, str):
                dependents[bid] = dependents.get(bid, 0) + 1

    def _fanout(node_id: object) -> int:
        return -dependents.get(node_id, 0) if isinstance(node_id, str) else 0
    # Evidence, read once per sort: zero for every unvoted row, so a graph with
    # no encounters keeps its exact previous order.
    from datetime import datetime, timezone

    effective_priority = make_effective_priority(entries)
    scored_at = datetime.now(timezone.utc)

    def _score(node: dict) -> float:
        # Degrade like _rank_band and orphan_ids: a hand-edited `encounters: 3`
        # raising here would take down the whole locked_mutate_graph write.
        try:
            return -importance_score(node, effective_priority(node), scored_at)
        except Exception:  # noqa: BLE001 - ordering signal; never break selection
            return 0.0
    # Board == work order: `next` must demote orphans exactly where the board
    # does, or the board shows one order and the walker works another. Computed
    # here (not passed by every caller) so no call site can forget it; fails
    # open to "no orphans", which reproduces the pre-rollup ordering.
    if orphans is None:
        try:
            from fno.graph.rollup import orphan_ids

            orphans = orphan_ids(entries)
        except Exception:  # noqa: BLE001 - ordering signal; never break selection
            orphans = frozenset()
    epic_in_progress = in_progress_epic_ids(entries, live_claimed)

    def _prio(e: dict) -> int:
        return PRIORITY_ORDER[_priority_name(e)]

    def key(node: object) -> tuple:
        # Curated rank leads: the SAME `_rank_band` the board uses, prepended
        # so a `rank --top` LOOSE node (band 0, ascending rank) is selected
        # ahead of all unranked nodes (band 1), overriding the epics-first
        # heuristic (Locked Decision 1). For a live-epic child the leading
        # band is the EPIC's own: the group's position is decided by the
        # epic, and the child's rank (the `band` term below) orders it only
        # among its siblings under that epic - it can never float the group.
        # Unranked nodes all share the `(1, 0.0)` band, so the epics-first
        # key below decides their order byte-for-byte.
        if not isinstance(node, dict):
            node = {}
        lane = (_lane_order_key(_project_key(node)),) if swimlane else ()
        band = _rank_band(node)
        child_prio = _prio(node)
        node_id = node.get("id")
        child_orphan = isinstance(node_id, str) and node_id in orphans
        child_created = _sort_text(node.get("created_at"))
        pid = node.get("parent")
        epic = _live_epic_for(node, id_to_entry, child_progress_epics)
        if epic is not None:
            in_progress_rank = 0 if pid in epic_in_progress else 1
            return lane + (
                _rank_band(epic),        # EPIC's rank band decides group position
                0,                       # epic-children tier (before loose)
                in_progress_rank,        # in-progress epics first
                _prio(epic),             # highest-priority epic first
                _sort_text(epic.get("created_at")),  # group one epic together
                band,                    # child rank: orders only within its epic
                child_prio,
                _fanout(node_id),    # in-band: after priority, before orphan
                child_orphan,
                _score(node),        # evidence, below every judgement above it
                child_created,
            )
        # Loose node: tier 1. Middle fields mirror the child fields so the
        # tuple stays comparable; tier already separates loose from epic
        # children, so loose nodes only ever compare among themselves and
        # resolve on flat (priority, fan-out, created_at).
        # Decision fields lead: priority, then fan-out, then orphan-last,
        # then created_at. The trailing triple only pads the tuple to the
        # epic branch's arity; tier (index 1) already separates the two, so
        # it is never compared.
        return lane + (
            band, 1, 0, child_prio, _fanout(node_id), child_orphan, _score(node),
            child_created, child_prio, _fanout(node_id), child_created,
        )

    return key


# -- Project detection --

def _git_repo_root() -> str | None:
    # Canonical (main) working tree via the shared resolver: first non-`bare`
    # `git worktree list` record that is a real working tree, robust across
    # normal / bare / separate-git-dir layouts (skips a bare repo and a
    # separate-git-dir gitdir mis-report rather than recording either as the
    # backlog cwd). See paths.resolve_canonical_worktree.
    canonical = _paths.resolve_canonical_worktree()
    if canonical is not None:
        return os.path.normpath(str(canonical))
    # Fallback (helper found no usable working tree, e.g. bare-only or
    # separate-git-dir with no linked checkout): the current checkout via
    # --show-toplevel - a real working tree, never a git dir - else None.
    try:
        top = subprocess.check_output(
            ["git", "rev-parse", "--show-toplevel"],
            stderr=subprocess.DEVNULL, text=True, encoding="utf-8",
        ).strip()
        return os.path.normpath(top) if top else None
    except (subprocess.CalledProcessError, FileNotFoundError):
        return None


def repo_root() -> str:
    """Absolute path of the canonical (main) checkout.

    Resolves through ``_git_repo_root()`` (the main-worktree path from
    ``git worktree list --porcelain``), so it returns the *main* worktree even
    when invoked from a linked worktree; falls back to ``os.getcwd()`` outside
    a git repo.

    This is the path durable artifacts (backlog nodes, think docs,
    blueprints) must record as ``cwd``: they outlive the worktree that
    created them, so a worktree top-level path (``--show-toplevel``) would
    go stale once that worktree is archived. Contrast ephemeral session
    state (``target-state.md``), which is deliberately bound to its worktree.
    """
    return _git_repo_root() or os.getcwd()


def plan_claims(plan_path: str) -> "set[str]":
    """The node ids a plan's frontmatter declares ownership of, as a set.

    Reads both ``claims:`` (a list) and ``node:`` (a single id). 330 of 807 plans
    carry ``node:`` instead of ``claims:``, so reading only ``claims:`` leaves a
    reader blind on roughly 41% of plans and the leniency is invisible.

    The single parser for this fact: graph/cli.py's stamp guard and
    collision.py's self-exclusion default both call here, so a plan resolved
    as claimed by one reader is claimed by every reader.

    Empty means "no claim declared" -- including every unreadable-plan case, since
    ``_read_plan_frontmatter`` returns ``{}`` on all of them. Callers treat
    empty as their no-claim branch, so only a positive claim can act.
    """
    fm = _read_plan_frontmatter(plan_path)
    out: "set[str]" = set()

    claims = fm.get("claims")
    if isinstance(claims, str):
        claims = [claims]
    if isinstance(claims, list):
        out.update(
            c.strip() for c in claims
            if isinstance(c, str) and c.strip() not in ("", "null")
        )

    node = fm.get("node")
    if isinstance(node, str) and node.strip() not in ("", "null"):
        out.add(node.strip())

    return out


def _read_plan_frontmatter(plan_path: str) -> dict:
    """Parse YAML frontmatter from a single-file plan.

    Returns the parsed frontmatter as a dict. Returns ``{}`` on every
    failure mode (missing file, malformed YAML, no frontmatter, missing
    PyYAML) so the caller can use the result without try/except. Malformed
    YAML emits one stderr warning so the user knows their file is broken
    instead of silently routing on stale data; every other failure is
    silent because the caller's chain already handles "no frontmatter
    declared" as a valid state.
    """
    target = Path(plan_path)
    if not target.exists() or not target.is_file():
        return {}

    try:
        text = target.read_text()
    except (OSError, UnicodeDecodeError):
        # UnicodeDecodeError is a ValueError, so letting it escape turns a
        # caller's designed "no frontmatter" branch into an error exit.
        return {}

    lines = text.splitlines()
    # lstrip the UTF-8 BOM: str.strip() keeps it, so a Windows-authored plan
    # whose first line is BOM+'---' would read as no-frontmatter here (and
    # in _has_frontmatter_block), silently un-dating the plan.
    if not lines or lines[0].lstrip("\ufeff").strip() != "---":
        return {}

    end_idx: int | None = None
    for i in range(1, len(lines)):
        if lines[i].strip() == "---":
            end_idx = i
            break
    if end_idx is None:
        return {}

    fm_text = "\n".join(lines[1:end_idx])

    try:
        import yaml
    except ImportError:
        sys.stderr.write(
            "warning: PyYAML missing - plan-frontmatter project resolution disabled\n"
        )
        return {}

    try:
        data = yaml.safe_load(fm_text)
    except yaml.YAMLError as e:
        sys.stderr.write(f"warning: could not parse frontmatter in {target}: {e}\n")
        return {}

    if not isinstance(data, dict):
        return {}
    return data


def _has_frontmatter_block(plan_path: str) -> bool:
    """True when the file OPENS a ``---`` frontmatter block, whatever is in it.

    ``_read_plan_frontmatter`` fails soft to ``{}`` both for a file with no
    block (a valid state callers route on) and for a block it cannot parse.
    The difficulty gate must pass the first and refuse the second, so it
    needs the one bit the ``{}`` return conflates.
    """
    try:
        with open(plan_path, "r", encoding="utf-8", errors="replace") as fh:
            first = fh.readline()
    except OSError:
        return False
    # Same BOM lstrip as _read_plan_frontmatter, so the two readers agree
    # that a BOM+'---' opener IS a frontmatter block.
    return first.lstrip("\ufeff").strip() == "---"


def _list_known_projects() -> set[str]:
    """Return all project names declared across project-local and global settings.yaml.

    Reads the work-map pairs through the settings door (the same fact set the
    node-id knobs use). Returns an empty set on a door failure.
    Used by the post-resolution sanity check that warns when an intake
    routes to a project the orchestrator has never heard of.
    """
    from fno import rust_binary

    receipt = rust_binary.settings_read()
    if receipt is None:
        return set()

    known: set[str] = set()
    for pair in receipt.get("projects") or []:
        if isinstance(pair, list) and len(pair) == 2 and pair[0]:
            known.add(str(pair[0]))

    return known


def _warn_unknown_project(project: str | None, known: set[str] | None = None) -> None:
    """Emit one stderr line if ``project`` is not in any known settings workspace.

    Silent when ``project`` is falsy or when no settings file declares any
    workspace at all (a brand-new install has nothing to compare against).
    Callers in tight loops (e.g. multi-plan intake) can pass a pre-computed
    ``known`` set to avoid re-reading settings.yaml per iteration.
    """
    if not project:
        return
    if known is None:
        known = _list_known_projects()
    if not known:
        return
    if project not in known:
        sys.stderr.write(
            f"warning: project '{project}' is not declared in any settings.yaml "
            f"workspace; work-routing may not happen\n"
        )


def detect_project(entries: list[dict]) -> str | None:
    norm_root = os.path.normpath(repo_root())
    root_with_sep = norm_root.rstrip(os.sep) + os.sep
    fallback_project: str | None = None
    for e in entries:
        entry_cwd = e.get("cwd")
        if not entry_cwd:
            continue
        # expanduser BEFORE normpath so historical entries stored with
        # tilde-form paths (e.g. "~/code/me/chingu") match the absolute
        # repo_root. The writer side (the native create path) stores absolute
        # paths since PR #167, but pre-#167 entries and any direct edits
        # use ~. Without expanduser, the comparison silently never matches
        # and detect_project falls through to the global-scope fallback.
        norm_cwd = os.path.normpath(os.path.expanduser(str(entry_cwd)))
        if norm_cwd == norm_root:
            return e.get("project")
        if fallback_project is None and norm_cwd.startswith(root_with_sep):
            fallback_project = e.get("project")
    return fallback_project


def filter_by_project(entries: list[dict], project: str | None, show_all: bool) -> list[dict]:
    if project:
        return [e for e in entries if e.get("project") == project]
    if show_all:
        return entries
    detected = detect_project(entries)
    if detected:
        return [e for e in entries if e.get("project") == detected]
    return entries


# -- Blocker helpers --

def _parse_blocker_list(values: list[str] | None) -> list[str]:
    if not values:
        return []
    out: list[str] = []
    for v in values:
        if v is None:
            continue
        for token in v.split(","):
            t = token.strip()
            if t:
                out.append(t)
    return out


def _validate_blocker_ids(blockers: list[str], entries: list[dict], task_id: str) -> None:
    import typer
    id_to_entry = {e.get("id"): e for e in entries if isinstance(e.get("id"), str)}
    for bid in blockers:
        if bid not in id_to_entry:
            typer.echo(f"Error: unknown blocker id '{bid}'", err=True)
            raise typer.Exit(code=2)
        if bid == task_id:
            typer.echo(f"Error: node cannot block itself ({task_id})", err=True)
            raise typer.Exit(code=2)
        visited: set[str] = set()
        stack = [bid]
        while stack:
            curr = stack.pop()
            if curr == task_id:
                typer.echo(
                    f"Error: cycle detected - {bid} (transitively) depends on {task_id}",
                    err=True,
                )
                raise typer.Exit(code=2)
            if curr in visited:
                continue
            visited.add(curr)
            curr_entry = id_to_entry.get(curr)
            if curr_entry:
                stack.extend(curr_entry.get("blocked_by", []))


def _warn_similar_nodes(
    node: dict,
    entries: list[dict],
    *,
    intake_hint: bool,
) -> None:
    """Emit a stderr ``dedup:`` receipt naming the closest existing nodes.

    The filing-time duplicate net shared by every birth path (idea/add, single
    intake, multi-intake): scores the just-born ``node`` against all live nodes
    via relatedness.similar_nodes and prints up to three candidates. Warn-only -
    an orchestrator filing fast must be informed, not stopped or prompted.

    ``intake_hint`` adapts the remedy line: the single/multi intake paths can
    re-file with ``--claims`` to consolidate against the top candidate, so they
    name its id; idea/add cannot, so they get the supersede/update remedy only.

    Receipt is stderr-only - stdout stays the machine-readable payload each
    birth command already returns. Scoring/state filtering lives in similar_nodes;
    this helper only formats what it returns.
    """
    from fno.graph.relatedness import similar_nodes
    from fno.graph.store import entries_with_archive

    # Score against the working graph AND archived (shipped) nodes: an archived
    # done node is the answer to a duplicate filing, and the working graph alone
    # misses it (codex P2). Best-effort; an unreadable archive degrades silently.
    scoring_entries = entries_with_archive(entries)
    candidates = similar_nodes(node, scoring_entries)
    if not candidates:
        return
    by_id = {e.get("id"): e for e in scoring_entries if isinstance(e, dict)}
    new_id = node.get("id")
    lines = [f"dedup: {len(candidates)} similar existing node(s) for {new_id}:"]
    for cid, score, _reason in candidates:
        cand = by_id.get(cid, {})
        status = cand.get("status") or "?"
        title = (cand.get("title") or "").strip()
        if len(title) > 60:
            title = title[:60] + "..."
        pr = cand.get("pr_number")
        pr_tok = f"  PR#{pr}" if isinstance(pr, int) and not isinstance(pr, bool) else ""
        lines.append(f'  {cid}  {status:<10}{score:.2f}{pr_tok}  "{title}"')
    top_id = candidates[0][0]
    # The intake paths can re-file with --claims to consolidate against the
    # top candidate, in any node state; idea/add cannot, so they get the
    # supersede/update remedy only.
    if intake_hint:
        lines.append(
            "consolidate: `fno backlog supersede` / `fno backlog update`, or "
            f"re-file with --claims {top_id}"
        )
    else:
        lines.append(
            "consolidate: `fno backlog supersede` / `fno backlog update` the "
            "existing node"
        )
    sys.stderr.write("\n".join(lines) + "\n")
