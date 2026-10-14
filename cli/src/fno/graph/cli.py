"""fno graph CLI - typer subcommands for feature graph management.

Each subcommand delegates to fno.graph.{store,statuses,render,depends}
and preserves identical behavior to scripts/roadmap-tasks.py.

Exit codes:
    0  success
    1  user error (invalid input)
    2  runtime error (bad state, cycle detected)
    3  not found
    4  nothing to intake
"""

from __future__ import annotations

import json
import os
import sys
from datetime import datetime, timezone
from pathlib import Path
from types import SimpleNamespace
from typing import Any, List, Literal, Optional

import typer

from fno.tombstones import tombstone_group_cls
# the external-backend verb classification lives beside its data
from fno.graph._verb_classification import (
    _NO_GRAIN_ON_EXTERNAL_BACKEND,
    classify_backlog_verbs,
)
from fno.graph.api import wire_rows  # noqa: F401 - re-export for lazy importers
from fno.graph.node_builder import (  # noqa: F401 - re-export for lazy importers
    DESCRIPTION_HELP,
    ENCOUNTER_EVIDENCE_HELP,
    ORIGIN_EVIDENCE_HELP,
    RELATED_HELP,
    SOURCE_KIND_HELP,
    SOURCE_NODE_HELP,
    TAG_HELP,
    _build_backlog_node,
    _session_provenance,
)
# The roster renderer lives in its own module: this file is shrink-only and
# the provenance change touches it. The alias keeps the historical name
# importable.
from fno.graph.provenance_view import lifecycle_roster as _lifecycle_roster
from fno.graph.provenance_view import pr_block, render_pr_line
from fno.provenance.registry_liveness import registry_status_index

cli = typer.Typer(
    name="graph",
    help="Feature graph management",
    no_args_is_help=True,
    # Removed verbs under `backlog` refuse by name and say what replaced them,
    # instead of failing with the same message a typo gets.
    cls=tombstone_group_cls("backlog"),
    # The curated menu below is nouns; this line is the answer to "can I take
    # that back". It sits on the group help because that is the surface someone
    # deciding what is possible actually reads - a correction verb nobody can
    # find is, for decision-making purposes, a correction that does not exist.
    epilog=(
        "Corrections: reopen (undo done) | remove (hard delete) | unarchive | "
        "undefer | unqueue | unsupersede | unclaim. All hidden; "
        "`fno help backlog --all` lists every verb."
    ),
)

from fno.graph.worked import cmd_worked as _cmd_worked  # noqa: E402

from fno.graph.triage_forward import register_triage_forward  # noqa: E402
register_triage_forward(cli)
cli.command("worked", hidden=True)(_cmd_worked)


# Nested capture sub-app: `fno backlog capture <verb>`. The capture tier below
# idea nodes (markdown fu-* items, NOT graph nodes). Distinct from
# `fno agents mail`. The retired `inbox` spelling lives in fno.tombstones.
from fno.backlog.capture import cli as _capture_cli  # noqa: E402

cli.add_typer(_capture_cli, name="capture", hidden=True)

# Nested batch sub-app: `fno backlog batch <verb>`. Batch-lane state
# (.fno/batches/<domain>.json) — coalesce same-domain nodes into one PR.
from fno.backlog.batch import cli as _batch_cli  # noqa: E402

cli.add_typer(_batch_cli, name="batch", hidden=True)

# Decision records are node/PR metadata; their backlog leaves (decide,
# decisions, decide-retract, decide-reindex) are the grouped dispatcher's
# native arms since the decide family ported. No Python mount remains.


# Node-lifecycle sub-apps folded under backlog (unit 6 of the  reorg):
# annotate findings, carve-out records, and the retro harvest are all node
# metadata operations. The old top-level spellings stay one-release shims
# (fno.verb_moves); the mounts below are the canonical homes.
from fno.annotate.cli import annotate_app as _annotate_app  # noqa: E402
from fno.carveout.cli import carveout_app as _carveout_app  # noqa: E402
from fno.retro.cli import retro_app as _retro_app  # noqa: E402

cli.add_typer(_annotate_app, name="annotate", hidden=True)
cli.add_typer(_carveout_app, name="carveout", hidden=True)
cli.add_typer(_retro_app, name="retro", hidden=True)


# Selection-time enforcement (): a node another session is actively
# driving holds a live ``node:<id>`` claim and must be skipped so two sessions
# never pick up the same node. The implementation is homed in graph/statuses.py
# (so the board renderers can share it without a cli<->render cycle); re-exported
# under the original module-global name that existing tests monkeypatch.
from fno.graph.statuses import live_claimed_node_ids as _live_claimed_node_ids  # noqa: E402


def _has_unmerged_open_pr(e: dict) -> bool:
    """True when a node already carries a PR but is not yet closed (done) -
    Full contract: docs/architecture/backlog-graph-verb-contracts.md
    """
    if e.get("completed_at"):
        return False  # already done; status derivation bucketed it out of ready
    return bool(e.get("pr_number"))


def _is_batched_member(e: dict) -> bool:
    """True when a node is already committed to an open batch (batch-lane Wave 2).

    A batched member has its atomic commits on a shared batch branch and ships as
    part of the batch PR, not its own. It must NOT be re-selected for dispatch
    (else the daemon would spawn a second worker for work already on the branch).
    The mark is the graph `batch` field, set by `/target batched` via
    `fno backlog update --batch <id>` and cleared (`--batch null`) on abandon so
    the node resurfaces for an individual ship. Mirrors `_has_unmerged_open_pr`:
    an in-flight signal that survives the builder session's PID-claim dying.
    """
    return bool(e.get("batch"))


def _needs_design(e: dict) -> bool:
    """True when a node still needs a design pass before it can be blueprinted.

    Reads the rung rather than `plan_path` presence. The presence check was a
    PROXY for "not ready": with `stub` in no vocabulary, a linked scaffold
    derived `ready`, so withholding the link was the only lever that could say
    "undesigned". `plan_rung` says it directly, which means a child linked to an
    `idea`-rung scaffold is correctly still a design candidate instead of being
    skipped as done.
    """
    from fno.graph.ladder import Rung, plan_rung

    return plan_rung(e) in (Rung.NONE, Rung.IDEA)


def _container_ids(entries: list[dict]) -> set[str]:
    """Ids of nodes that are some other node's ``parent`` - i.e. epics/containers.
    Full contract: docs/architecture/backlog-graph-verb-contracts.md
    """
    return {
        p
        for e in entries
        if isinstance(e, dict)
        and isinstance((p := e.get("parent")), str)
        and e.get("contained_in") != p
    }


@cli.callback()
def _graph_callback(
    ctx: typer.Context,
    json_output: bool = typer.Option(
        False,
        "--json",
        "-J",
        help="Output structured JSON to stdout. Diagnostics go to stderr.",
    ),
) -> None:
    from fno.handoff.output import merge_json_flag

    merge_json_flag(ctx, json_output)


def _graph_path() -> Path:
    """Return the active graph.json path (monkeypatch-friendly)."""
    from fno.graph._constants import GRAPH_JSON

    return GRAPH_JSON


def _display_entries(reader: str, *, strict: bool = False) -> list[dict]:
    """Entries for read-only display/search surfaces (view, find, roadmap,
    relatedness, provenance walks, the status summary).

    These renders visualize the LOCAL store's full records - status, tags,
    details, slug - which the five-field read contract does not carry, so they
    read through the guarded metadata reader: byte-identical on the default
    backend, an honest named refusal under an external selection (an external
    tracker has its own UI; a stale local render is the leak the seam closes).
    Mutation paths keep ``read_graph`` and get the shared external refusal
    from task 4.2 instead.
    """
    from fno.tracker.metadata import ExternalMetadataUnavailable, read_entries

    try:
        return read_entries(reader, strict=strict)
    except ExternalMetadataUnavailable as exc:
        typer.echo(f"fno backlog: {exc}", err=True)
        raise typer.Exit(code=2)


def _safe_stderr_warn(msg: str) -> None:
    """Write ``msg`` to stderr, swallowing a closed/broken stream.

    The post-write dedup fallback runs AFTER the node already committed, so a
    secondary stderr failure (closed fd, broken pipe) must never escape and
    fail a filing whose mutation already landed (codex P2)."""
    try:
        sys.stderr.write(msg)
    except Exception:  # noqa: BLE001 - a dead stderr must not break the filing
        pass


# Distinct from exit 1 ("graph read cleanly, node absent"): the graph itself
# could not be read. click reserves 2 for usage errors, so 3 is the first free
# code. A resolution caller that today treats any non-zero as "absent" keeps
# failing closed; one that cares can tell a wedged graph from a typo.
GRAPH_UNREADABLE_EXIT = 3


def _resolve_entries_or_exit(id: str):
    """Read the graph strictly for a resolution verb.

    Returns the entries on a clean read (populated or empty). On an unreadable
    graph, prints a message that names the read failure and the path -- never
    "No node matching", which would assert the node is absent -- and exits with
    the distinct GRAPH_UNREADABLE_EXIT instead of 1.
    """
    from fno.graph.store import read_graph_strict, GraphUnreadableError

    try:
        return read_graph_strict(_graph_path())
    except GraphUnreadableError as e:
        typer.echo(
            f"Could not read the graph cleanly, so '{id}' cannot be resolved: {e}",
            err=True,
        )
        raise typer.Exit(code=GRAPH_UNREADABLE_EXIT)


def _live_worker(node_id: str) -> Optional[str]:
    """The holder of a live/suspect ``node:<id>`` claim, else None.

    Retained for the decompose surface's adopt guard; the contain verb's
    probe moved to the native leg, which reads the same lockfiles.
    """
    from fno.claims.core import claim_status

    key = f"node:{node_id}"
    try:
        info = claim_status(key)
    except Exception:  # noqa: BLE001 - a status read must never crash the table
        return None
    if info.get("state") in ("live", "suspect"):
        return info.get("holder")
    return None




def _resolve_asserted_id(
    token: str,
    entries: list,
    *,
    flag: str,
    self_id: Optional[str] = None,
) -> str:
    """Resolve a caller-asserted node reference to a canonical id, or refuse.

    The counterpart to ambient capture's degrade-to-null: a caller who names an
    origin or a peer has asserted something, and silently dropping an assertion
    that does not resolve would leave a node looking organically filed. So this
    FAILS CLOSED - non-zero exit, the unresolved
    token named, no write.

    Accepts anything the graph resolver accepts (id, slug, bare hex) rather than
    refusing on shape; passing a slug where an id is expected is the likely
    mistake and the resolver already handles it.
    """
    from fno.graph.fuzzy import resolve_node

    match = resolve_node(token, entries)
    if match.kind != "exact":
        typer.echo(f"Error: {flag} '{token}' does not resolve to a node", err=True)
        raise typer.Exit(code=1)
    resolved = match.candidates[0]["id"]
    if self_id is not None and resolved == self_id:
        typer.echo(f"Error: {flag} cannot reference the node itself ({self_id})", err=True)
        raise typer.Exit(code=1)
    return resolved


def _refuse_create_on_external_backend() -> None:
    """Refuse node-creation verbs on a non-default backend.

    On an external backend, item creation lives in the tracker (GitHub Issues,
    Linear, ...); minting an ab- entry in graph.json would create a phantom item
    the tracker has no record of. Called from EVERY Python creation entry
    point (cmd_decompose, cmd_intake, and cmd_tree; add, idea and the new word
    answer natively and carry their own guard) so the guard is not decorative. A guard on only some reachable
    paths is the pitfall this exists to prevent; the parametrized test exercises
    each path so a future creation verb that bypasses it fails loudly.
    """
    from fno.tracker import active_backend_name

    backend = active_backend_name()
    if backend != "graph":
        typer.echo(
            f"fno backlog: creating work belongs to the {backend} tracker. "
            f"Create the item there; footnote tracks it by its id "
            f"(e.g. /fno:target owner/repo#N).",
            err=True,
        )
        raise typer.Exit(code=1)


def _prompt_difficulty_value(value: str) -> str:
    """``typer.prompt`` value_proc for the difficulty band: re-ask on a bad
    answer instead of crashing. click re-prompts only on ``UsageError``, while
    ``normalize_difficulty`` raises a bare ``ValueError`` that would surface as
    a traceback straight out of the prompt."""
    import click
    from fno.graph._constants import DIFFICULTY_HELP, normalize_difficulty

    try:
        return normalize_difficulty(value) or ""
    except ValueError as exc:
        raise click.UsageError(f"{exc}. {DIFFICULTY_HELP}") from exc


def _validate_priority_or_exit(priority: str, *, blocks_everything: bool = False) -> None:
    """The shared priority gate: PRIORITY_ORDER membership, then the write rule.

    P0 refuses without the blocks-everything acknowledgement; exit 1 is the
    membership miss, exit 2 the write rule."""
    from fno.graph._constants import PRIORITY_ORDER, validate_priority_write

    if priority not in PRIORITY_ORDER:
        typer.echo(
            f"Error: invalid priority '{priority}'. Must be: {', '.join(PRIORITY_ORDER.keys())}",
            err=True,
        )
        raise typer.Exit(code=1)
    try:
        validate_priority_write(priority, blocks_everything=blocks_everything)
    except ValueError as exc:
        typer.echo(f"Error: {exc}", err=True)
        raise typer.Exit(code=2)


def _require_node_id(task_id: str) -> None:
    """The shared node-id gate: one refusal text for every verb that takes one."""
    from fno.graph._constants import has_node_id_prefix

    if has_node_id_prefix(task_id):
        return
    typer.echo(
        f"Error: task_id must be a <prefix>-<4..8 hex> node id, got '{task_id}'", err=True
    )
    raise typer.Exit(code=1)


def _require_nodes(entries: "list[dict]", ids: "list[str]") -> None:
    """The shared missing-node gate for the multi-id verbs."""
    from fno.graph._intake import _find_node

    missing = [tid for tid in ids if _find_node(entries, tid) is None]
    if missing:
        typer.echo(f"Error: feature(s) not found: {', '.join(missing)}", err=True)
        raise typer.Exit(code=1)


# -- decompose (bounded epic -> group child nodes) --


@cli.command("decompose", hidden=True)
def cmd_decompose(
    ctx: typer.Context,
    epic_id: str = typer.Argument(..., help="Epic node ab-ID to decompose into group children"),
    groups: str = typer.Option(
        ...,
        "--groups",
        help=(
            "JSON array of {slug,title,waves,blocked_by_groups[,project][,cwd]"
            "[,adopt]} specs; '@file' or '-' reads stdin. Per-group project/cwd "
            "route a child into another repo; `adopt` re-parents existing nodes "
            "instead of minting. Full schema: "
            "skills/blueprint/references/epic-decomposition.md."
        ),
    ),
    max_prs: Optional[int] = typer.Option(
        None,
        "--max-prs",
        help=(
            "Ceiling on group/PR count; rejects when exceeded. Default: "
            "config.blueprint.max_prs_per_epic, tightened by the epic doc's "
            "`max_children:` frontmatter."
        ),
    ),
    force: bool = typer.Option(
        False,
        "--force",
        "-F",
        help="Allow a re-decomposition that orphans an already-shipped group child node.",
    ),
    plans: str = typer.Option(
        "separate",
        "--plans",
        help=(
            "Only 'separate': a self-contained quick-plan per child (one plan "
            "== one PR == one node). The removed 'fragment' form is still "
            "recognized on existing children for idempotent re-decompose."
        ),
    ),
) -> None:
    """Upsert group child nodes under an epic (atomic + idempotent).

    Each group becomes one child node (parent=epic) bundling 1+ execution waves
    into a single shippable PR, with its own self-contained
    <stem>.group-<slug>.md quick-plan (the only packaging). Re-running with the
    same slugs updates the existing children in place rather than duplicating,
    keyed on the slug - and a child still on the legacy <epic-doc>#group-<slug>
    fragment form is repointed to its separate file. The whole decomposition
    lands in one locked graph mutation, so a bad spec leaves the graph exactly
    as it was (AC1-FR).
    """
    _refuse_create_on_external_backend()
    import sys as _sys
    from fno.graph._constants import mint_node_id, validate_priority_write
    from fno.graph.store import commit_rows_via_store, GraphUnreadableError
    from fno.graph._intake import _find_node, _would_create_cycle
    from fno.graph._decompose import (
        _UNSET,
        DecomposeError,
        canonical_child_plan_path,
        child_plan_path,
        classify_group_dep,
        extract_contract_versions,
        extract_why_digest,
        find_orphans,
        group_child_slug,
        is_group_child,
        is_shipped,
        plan_base,
        resolve_effective_cap,
        epic_strategy_from_doc,
        scaffold_separate_plan,
        separate_plan_path,
        validate_groups,
    )
    from fno.graph._contain import contain_into, refuse_dead_owner
    from fno.graph._intake import _read_plan_frontmatter
    from fno.handoff.output import emit_error, json_mode

    if plans == "fragment":
        emit_error(
            ctx,
            "--plans fragment was removed; 'separate' is now the only packaging "
            "(one plan == one PR == one node). Drop the flag or pass --plans separate.",
        )
        raise typer.Exit(code=1)
    if plans != "separate":
        emit_error(ctx, f"--plans must be 'separate' (got {plans!r})")
        raise typer.Exit(code=1)
    separate = True

    # 1. Read the --groups source ('@file', '-' stdin, or a JSON literal),
    #    keeping read vs parse failures distinct so the message names the cause.
    try:
        if groups == "-":
            raw = _sys.stdin.read()
        elif groups.startswith("@"):
            raw = Path(groups[1:]).expanduser().read_text(encoding="utf-8")
        else:
            raw = groups
    except OSError as e:
        emit_error(ctx, f"could not read --groups file {groups[1:]!r}: {e}")
        raise typer.Exit(code=1)
    try:
        parsed = json.loads(raw)
    except json.JSONDecodeError as e:
        emit_error(ctx, f"--groups is not valid JSON: {e}")
        raise typer.Exit(code=1)

    # 2. Resolve the ceiling. Precedence: a `max_children` in the epic doc's
    #    frontmatter is the author's durable per-epic cap (overrides the config
    #    default upward; an explicit --max-prs may only tighten it). With no
    #    max_children, resolution is byte-identical to before: explicit --max-prs
    #    else config.blueprint.max_prs_per_epic.
    explicit_max_prs = max_prs  # captured before any fallback overwrites the None sentinel

    #    Read the epic's max_children read-only, pre-lock (advisory; the locked
    #    mutator re-resolves the epic). Any read failure -> absent cap -> current
    #    behavior (_read_plan_frontmatter fails safe to {}). A present-but-invalid
    #    value (incl. explicit null) is rejected by resolve_effective_cap below.
    #    _UNSET (not None) marks "no key", so an explicit `max_children: null`
    #    fails closed instead of masquerading as absent.
    max_children: object = _UNSET
    epic_doc_rel: Optional[str] = None
    try:
        epic_node = _find_node(wire_rows(path=_graph_path()), epic_id)
        epic_plan_path = epic_node.get("plan_path") if epic_node else None
        if epic_node is not None and epic_plan_path:
            epic_doc = plan_base(epic_plan_path)
            # Resolve a relative plan_path against the epic's stored cwd, mirroring
            # the in-lock base resolution - reading it against the process cwd would
            # miss the doc when decompose runs elsewhere and silently drop the cap.
            if not os.path.isabs(epic_doc):
                epic_doc = os.path.join(epic_node.get("cwd") or os.getcwd(), epic_doc)
            max_children = _read_plan_frontmatter(epic_doc).get("max_children", _UNSET)
            try:
                epic_doc_rel = os.path.relpath(epic_doc, epic_node.get("cwd") or os.getcwd())
            except ValueError:
                epic_doc_rel = os.path.basename(epic_doc)
    except (DecomposeError, GraphUnreadableError, OSError):
        max_children = _UNSET  # fail-safe: no cap, current behavior

    #    Read config ONLY on the true fallback path (no max_children, no explicit
    #    flag). A valid max_children or an explicit --max-prs makes config
    #    irrelevant, so an unrelated config error must not abort decompose there.
    config_default: Optional[int] = None
    if max_children is _UNSET and explicit_max_prs is None:
        from fno.config import load_settings

        try:
            config_default = load_settings().blueprint.max_prs_per_epic
        except Exception as e:
            emit_error(ctx, f"could not read config.blueprint.max_prs_per_epic: {e}")
            raise typer.Exit(code=1)

    try:
        effective_cap, cap_source = resolve_effective_cap(
            max_children, explicit_max_prs, config_default, epic_doc_rel
        )
    except DecomposeError as e:
        emit_error(ctx, str(e))
        raise typer.Exit(code=e.exit_code)

    # 3. Validate the spec entirely before touching the graph (atomicity).
    try:
        norm = validate_groups(parsed, effective_cap, cap_source, epic_id)
    except DecomposeError as e:
        emit_error(ctx, str(e))
        raise typer.Exit(code=e.exit_code)

    # 3b. Resolve per-group repo routing OUTSIDE the graph lock (settings reads
    #     never happen under the lock, mirroring `update`). A group with an
    #     explicit cwd uses it as-is; a group with only a project derives its
    #     cwd from the work-map and is REFUSED (atomically, before any write) if
    #     that project is unmapped - guessing a cwd would silently record foreign
    #     work under the wrong repo and break spawn-into-project. No project/cwd
    #     -> (None, None) = inherit the epic's repo (the single-repo default).
    from fno.graph._intake import project_root_from_settings

    slug_route: dict[str, tuple[Optional[str], Optional[str]]] = {}
    for grp in norm:
        gproj, gcwd = grp["project"], grp["cwd"]
        if gcwd is not None:
            slug_route[grp["slug"]] = (gproj, os.path.abspath(os.path.expanduser(gcwd)))
        elif gproj is not None:
            root = project_root_from_settings(gproj)
            if root is None:
                emit_error(
                    ctx,
                    f"group {grp['slug']!r} project {gproj!r} is not in any "
                    "settings.yaml work-map; add it there or pass an explicit cwd",
                )
                raise typer.Exit(code=1)
            slug_route[grp["slug"]] = (gproj, root)
        else:
            slug_route[grp["slug"]] = (None, None)

    keep_slugs = {g["slug"] for g in norm}
    results: list[dict] = []
    epic_id_box: list[str] = [epic_id]

    def mutator(graph_entries):
        # Resolve the epic inside the locked snapshot so a corrupt graph
        # surfaces as exit 1 (via locked_mutate_graph) rather than masquerading
        # as "epic not found".
        live_epic = _find_node(graph_entries, epic_id)
        if live_epic is None:
            raise DecomposeError(f"epic node {epic_id} not found", exit_code=3)
        try:
            validate_priority_write(
                live_epic.get("priority", "p2"),
                blocks_everything=bool(live_epic.get("blocks_everything")),
            )
        except ValueError as exc:
            raise DecomposeError(str(exc), exit_code=2) from exc
        epic_resolved_id = live_epic["id"]
        epic_id_box[0] = epic_resolved_id
        base = plan_base(live_epic.get("plan_path"))
        verbatim_base_box[0] = base  # the relative base, for the source_doc seed
        # `base` (verbatim, possibly relative) is the node-identity key used by
        # child_plan_path below - DO NOT mutate it. For the set-expected
        # shell-out only, resolve a relative base against the epic's project
        # root (its stored cwd) so a decompose run from a subdirectory still
        # locates the doc on disk; the writer resolves relative paths against
        # the process cwd, which would otherwise false-"missing" and skip
        # writing the count (reintroducing early graduation).
        if base and not os.path.isabs(base):
            base_box[0] = os.path.join(live_epic.get("cwd") or os.getcwd(), base)
        else:
            base_box[0] = base
        # Stash the epic's cwd (US4) so the post-lock scaffold step can resolve an
        # inherited child's child_root outside the lock (mirrors base_box).
        epic_cwd_box[0] = live_epic.get("cwd")

        # Read the epic doc's pinned interface-contract version(s). The doc is
        # the single source of truth: a `contract`-tier group is eligible only
        # when the doc pins a `## Interface Contract` (G1); with no pin every
        # `contract` request falls back to `hard` (AC2-HP). A missing/unreadable
        # doc -> no pin -> all hard (fail-safe; the downgrade is reported, never
        # silent). Local file read under the lock is trivial (the doc is small).
        pinned_versions: set[int] = set()
        if base_box[0]:
            try:
                pinned_versions = extract_contract_versions(
                    Path(base_box[0]).read_text(encoding="utf-8")
                )
            except (OSError, UnicodeDecodeError):
                # No readable doc -> no pin -> contract falls back to hard. Never
                # hard-fail decompose on a doc-read issue (mirrors the stamp path).
                pinned_versions = set()

        # Refuse to orphan an already-shipped group child unless --force.
        orphans = find_orphans(graph_entries, epic_resolved_id, base, keep_slugs)
        shipped_orphans = [o for o in orphans if is_shipped(o)]
        if shipped_orphans and not force:
            ids = ", ".join(o["id"] for o in shipped_orphans)
            raise DecomposeError(
                f"re-decomposition would orphan already-shipped group node(s): {ids}. "
                "Re-run with --force to proceed, or keep their #group slugs.",
                exit_code=2,
            )

        # Pass 1: resolve each group to an existing or new child node.
        slug_to_id: dict[str, str] = {}
        # Resolved adopt claims, keyed on the node id `_find_node` returns
        # rather than the spelling the spec used. validate_groups can only
        # compare raw strings, and a 4-7 hex `ab-` prefix resolves to the same
        # entry as the full id - so `ab-abcd` in one group and `` in
        # another read as two claims there and are one node here.
        adopt_claim: dict[str, str] = {}
        plan_to_group: list[tuple[dict, dict]] = []  # (node, normalized group)
        for grp in norm:
            frag_path = child_plan_path(base, grp["slug"])
            sep_path = separate_plan_path(base, grp["slug"])
            # Tolerant lookup: identity is the durable group_slug (US2), so
            # a child born unlinked (no plan_path yet) is still found; the legacy
            # plan_path match (fragment or separate form) upserts a pre-field child
            # in place instead of duplicating (idempotent on slug across migration).
            existing = next(
                (
                    e
                    for e in graph_entries
                    if e.get("parent") == epic_resolved_id
                    and (
                        e.get("group_slug") == grp["slug"]
                        or e.get("plan_path") in (frag_path, sep_path)
                    )
                ),
                None,
            )
            route_proj, route_cwd = slug_route[grp["slug"]]
            if existing is not None:
                action = "updated"
                node = existing
                node["group_slug"] = grp["slug"]  # backfill identity on legacy children
                # Preserve a designed child's plan_path; NEVER link an unlinked
                # child here (linking is the inline-fill / fan-out step's job, US2).
                # The one exception is the documented legacy-fragment repoint:
                # a child still on `<doc>#group-<slug>` moves to its separate file
                # (staying linked/ready), never unset.
                if node.get("plan_path") == frag_path:
                    node["plan_path"] = sep_path
                # Re-running with an explicit route reprojects an existing child
                # (e.g. a first pass inherited the epic's repo, a later pass adds
                # per-group routing). No route leaves the child's repo untouched.
                if route_proj is not None:
                    node["project"] = route_proj
                if route_cwd is not None:
                    node["cwd"] = route_cwd
            else:
                action = "created"
                # Born UNLINKED (plan_path=None -> derives `idea`): linking the
                # filled plan is the design-completion signal that flips the child
                # `ready` (US2, Locked Decision 4). group_slug is the durable
                # identity that survives the unlinked window.
                node = _build_backlog_node(
                    title=grp["title"],
                    parent=epic_resolved_id,
                    project=route_proj if route_proj is not None else live_epic.get("project"),
                    cwd=route_cwd if route_cwd is not None else live_epic.get("cwd"),
                    priority=live_epic.get("priority", "p2"),
                    blocks_everything=bool(live_epic.get("blocks_everything")),
                    difficulty=live_epic.get("difficulty"),
                    domain=live_epic.get("domain", "code"),
                    plan_path=None,
                    origin_channel="decompose",
                    origin_evidence=f"parent:{epic_resolved_id}",
                    known_ids={e.get("id") for e in graph_entries},
                )
                node["group_slug"] = grp["slug"]
                node["id"] = mint_node_id({e.get("id") for e in graph_entries})
                # Reuse the parent-setter's cycle detection on this path too
                # (plan Invariants, line 88). A freshly minted id cannot be an
                # ancestor of the epic, so this never trips for new nodes today;
                # it guards future paths that re-parent an existing node.
                if _would_create_cycle(graph_entries, node["id"], epic_resolved_id):
                    raise DecomposeError(
                        f"parenting group {grp['slug']!r} to {epic_resolved_id} would create a cycle",
                        exit_code=2,
                    )
                graph_entries.append(node)
            slug_to_id[grp["slug"]] = node["id"]
            plan_to_group.append((node, grp))

            # Adoption (US2): re-parent each named node under this group
            # child, inside the same locked mutation. Nothing is minted for it
            # and nothing is deleted; membership rides the `parent` pointer.
            adopted: list[str] = []
            # A dead delivery unit cannot own containment; the guard and its
            # remedy text live in _contain (shared with `fno backlog contain`).
            # Gated on a non-empty adopt list so a dead group child with
            # nothing to adopt still updates its title, waves, and blocked_by.
            if grp["adopt"]:
                refuse_dead_owner(node, context=f"group {grp['slug']!r}")
            for adopt_id in grp["adopt"]:
                target = _find_node(graph_entries, adopt_id)
                if target is None:
                    raise DecomposeError(
                        f"group {grp['slug']!r} adopts {adopt_id}, which resolves to no node",
                        exit_code=3,
                    )
                if target["id"] == epic_resolved_id:
                    # validate_groups checks the RAW epic argument, so an
                    # aliasable `ab-` prefix slips past it and would land on the
                    # generic cycle refusal instead of this specific one.
                    raise DecomposeError(
                        f"group {grp['slug']!r} adopt names the epic {epic_resolved_id} itself",
                        exit_code=1,
                    )
                prior_slug = adopt_claim.get(target["id"])
                if prior_slug is not None:
                    raise DecomposeError(
                        f"node {target['id']} is claimed by more than one adopt "
                        f"entry (group {prior_slug!r} and group {grp['slug']!r}); "
                        "two spellings of one id resolve to the same node",
                        exit_code=1,
                    )
                adopt_claim[target["id"]] = grp["slug"]
                # Unscoped on purpose: group_child_slug answers "group child of
                # THIS doc", which misses a legacy child of ANOTHER epic and
                # lets adoption steal it. Also covers self-adoption, since a
                # group naming its own resolved id is a group child by
                # construction.
                if is_group_child(target):
                    owner = group_child_slug(target, base)
                    if owner:
                        whose = f"already the group child for slug {owner!r}"
                    elif target.get("parent") == epic_resolved_id:
                        whose = (
                            "already this epic's group child on a legacy plan "
                            f"path ({target.get('plan_path')}) that no longer "
                            "matches the epic doc"
                        )
                    else:
                        whose = f"already a group child of another epic ({target.get('plan_path')})"
                    raise DecomposeError(
                        f"group {grp['slug']!r} adopts {target['id']}, which is "
                        f"{whose}; demoting a group into a task "
                        "reshapes the epic",
                        exit_code=2,
                    )
                outcome = contain_into(
                    graph_entries,
                    node,
                    target,
                    live_worker=_live_worker,
                    context=f"group {grp['slug']!r}",
                )
                if outcome.warning:
                    uncontained_box[0].append(
                        f"warning: adopted {target['id']} into group "
                        f"{grp['slug']!r} but did NOT mark it contained: it "
                        f"{outcome.warning}, so it is its own delivery unit. It stays "
                        "separately dispatchable, separately costed, and is not "
                        "closed by the group's merge."
                    )
                if outcome.adopted:
                    adopted.append(target["id"])

            results.append(
                {
                    "id": node["id"],
                    "slug": grp["slug"],
                    "waves": grp["waves"],
                    "action": action,
                    "adopted": adopted,
                }
            )

        # Pass 2: set titles + inter-group blocked_by now that all ids exist.
        for (node, grp), r in zip(plan_to_group, results):
            node["title"] = grp["title"]
            # Set details unconditionally so a re-decompose that clears a
            # group's waves does not leave stale wave metadata behind.
            node["details"] = (
                f"Waves {grp['waves']} of epic {epic_resolved_id}" if grp["waves"] else None
            )
            node["blocked_by"] = [slug_to_id[d] for d in grp["blocked_by_groups"]]
            r["blocked_by"] = list(node["blocked_by"])

            # Classify the dependency tier against the doc's pin. Stamp the
            # contract fields ONLY on a `contract` dep; pop them on `hard` so a
            # re-decompose downgrade (contract -> hard) cleans up stale stub
            # metadata and the pure-hard path serializes byte-for-byte unchanged
            # (Invariant). The downgrade reason, if any, is surfaced after the lock.
            dep, stub_against, cversion, downgrade = classify_group_dep(grp, pinned_versions, base)
            if dep == "contract":
                node["dep"] = "contract"
                node["stub_against"] = stub_against
                node["contract_version"] = cversion
                r["dep"] = "contract"
            else:
                node.pop("dep", None)
                node.pop("stub_against", None)
                node.pop("contract_version", None)
                r["dep"] = "hard"
            if downgrade:
                downgrade_box[0].append(downgrade)

        # Surface any unshipped orphans (slug dropped from the spec). They are
        # left in place, not deleted - deleting graph nodes is destructive.
        orphan_box[0] = [o["id"] for o in orphans]

    # Rationale (11 lines): docs/architecture/graph-cli-rationale.md#cmd-decompose-2220
        _unadopted = [
            e
            for e in graph_entries
            if e.get("id") and e.get("parent") == epic_resolved_id and not is_group_child(e)
        ]
        unadopted_box[0] = [e["id"] for e in _unadopted]
        contained_unadopted_box[0] = [
            e["id"] for e in _unadopted if e.get("contained_in") == epic_resolved_id
        ]
        return graph_entries

    orphan_box: list[list[str]] = [[]]
    unadopted_box: list[list[str]] = [[]]
    contained_unadopted_box: list[list[str]] = [[]]
    # Adoptees that were re-parented but deliberately NOT marked contained
    # (they carry their own PR or cost). Same box-then-emit shape as the
    # unadopted warning: collected inside the locked mutator, printed after.
    uncontained_box: list[list[str]] = [[]]
    base_box: list = [None]
    verbatim_base_box: list = [None]
    epic_cwd_box: list = [None]
    downgrade_box: list[list[str]] = [[]]
    try:
        commit_rows_via_store(_graph_path(), mutator)
    except DecomposeError as e:
        emit_error(ctx, str(e))
        raise typer.Exit(code=e.exit_code)

    epic_resolved_id = epic_id_box[0]
    orphan_ids = orphan_box[0]
    unadopted_ids = unadopted_box[0]
    # Deduped: locked_mutate_graph may re-enter the mutator, and this box appends
    # where the sibling boxes assign.
    for _uc in sorted(set(uncontained_box[0])):
        typer.echo(_uc, err=True)
    downgrades = downgrade_box[0]

    # Shared post-mutation graph re-read: 3c reads each child's created_at +
    # plan_path from it, and fan-out 4a reuses it. One read, not two. A read
    # failure degrades to an empty map (scaffold falls back to today's date, the
    # fan-out step is a no-op) rather than wedging the already-committed mutation.
    from fno.graph import api as graph_api

    try:
        by_id = {
            e.get("id"): e
            for e in (
                n.model_dump(by_alias=True)
                for n in graph_api.nodes(include_archived=True, path=_graph_path()).nodes
            )
        }
    except Exception:  # noqa: BLE001 - never wedge the report on a re-read failure
        by_id = {}

    # Rationale (11 lines): docs/architecture/graph-cli-rationale.md#cmd-decompose-2279
    why_digest = ""
    # The epic's own Execution Strategy, read from the SAME doc text as the why
    # digest. `group N` already used these waves to partition the children, so
    # handing each child its slice hands back a partition the epic computed
    # rather than asking a builder to recompute one.
    epic_strategy: dict | None = None
    if separate and base_box[0]:
        try:
            epic_text = Path(base_box[0]).read_text(encoding="utf-8")
            why_digest, why_warn = extract_why_digest(epic_text)
            if why_warn:
                typer.echo(f"warning: {why_warn}", err=True)
            epic_strategy = epic_strategy_from_doc(epic_text)
        except (OSError, UnicodeDecodeError):
            pass

    scaffolded: list[str] = []
    if separate and base_box[0]:
        from fno.graph._intake import repo_root

        source_doc = verbatim_base_box[0] or base_box[0]
        id_by_slug = {r["slug"]: r["id"] for r in results}
        for grp in norm:
            slug = grp["slug"]
            child_id = id_by_slug.get(slug)
            if not child_id:
                continue
            child = by_id.get(child_id)
            # Skip 1: already linked - never spawn a stub beside a filled plan
            # (Locked Decision 6; also grandfathers a repointed legacy fragment).
            if child and child.get("plan_path"):
                continue
            # Skip 2: a legacy `.group-<slug>.md` file exists - grandfather it in
            # place, no rename, no canonical duplicate (Locked Decision 4).
            if Path(separate_plan_path(base_box[0], slug)).exists():
                continue
    # Rationale (8 lines): docs/architecture/graph-cli-rationale.md#cmd-decompose-2326
            child_root = (child.get("cwd") if child else None) or epic_cwd_box[0] or repo_root()
            canonical = Path(
                canonical_child_plan_path(
                    slug,
                    child_id,
                    str(child_root),
                    child.get("created_at") if child else None,
                )
            )
            # Skip 3: canonical already on disk - idempotent re-run.
            if canonical.exists():
                continue
            try:
                canonical.parent.mkdir(parents=True, exist_ok=True)
                # Seed the folded nodes (discovery children adopted into this one
                # PR) as a coverage checklist, so a fresh-context builder sees
                # every commitment the plan must address (task 1.7).
                adopted_nodes = [
                    (aid, (by_id.get(aid) or {}).get("title") or aid)
                    for aid in (grp.get("adopt") or [])
                ]
                canonical.write_text(
                    scaffold_separate_plan(
                        grp,
                        epic_resolved_id,
                        source_doc,
                        why_digest=why_digest,
                        adopted=adopted_nodes,
                        epic_strategy=epic_strategy,
                    ),
                    encoding="utf-8",
                )
                scaffolded.append(str(canonical))
            except OSError as e:
                # Non-fatal: the graph is already the source of truth. Warn loudly
                # so the missing stub is visible, never silently swallowed.
                typer.echo(
                    f"warning: could not scaffold separate plan {canonical}: {e}",
                    err=True,
                )

    # Rationale (18 lines): docs/architecture/graph-cli-rationale.md#cmd-decompose-2375
    fanout: list[dict] = []
    flagged_slugs = {g["slug"] for g in norm if g["needs_think"]}
    slug_by_id = {r["id"]: r["slug"] for r in results}
    created_ids = {r["id"] for r in results if r["action"] == "created"}
    spec_ids = [r["id"] for r in results]
    if spec_ids:
        try:
            from fno.provenance.spawn_think import (
                RunState,
                maybe_spawn_think,
                think_spawn_on_decompose_wave0,
            )

            # Reuse the shared post-mutation re-read from 3c (by_id).
            born_rs = RunState()
            # Force the gate + spawn (over the default-OFF / attended-offer) for the
            # flagged fan-out only; reuses the exact env seams dispatch_conversational
            # uses, so no new maybe_spawn branch.
            forced_env = {
                **os.environ,
                "FNO_THINK_SPAWN": "1",
                "FNO_THINK_SPAWN_ATTENDED": "spawn",
            }
            #  wave-2 lane: opt-in, default OFF. Wave 0 means "no
            # intra-epic blocker", which `compute_waves` already derives (and
            # already projects as `wave:`), so there is nothing new to compute -
            # only a second reason to spawn beside the `needs_think` flag.
            # Restricted to wave 0 deliberately: those children are genuinely
            # independent, which is the only case where handing design to a cold
            # worker beats one warm context writing several coherent siblings.
            wave0_ids: set[str] = set()
            if think_spawn_on_decompose_wave0(
                project_root=Path(epic_cwd_box[0]) if epic_cwd_box[0] else None
            ):
                from fno.plan._project import plan_docs

                wave_by_id = (plan_docs("waves", epic_id=epic_resolved_id) or {}).get("wave_by_id", {})
                wave0_ids = {cid for cid, w in wave_by_id.items() if w == 0}
            for cid in spec_ids:
                child = by_id.get(cid)
                if child is None or not _needs_design(child):
                    continue  # already designed; nothing for a /think to add
                if slug_by_id.get(cid) in flagged_slugs or cid in wave0_ids:
                    # chain_blueprint: the worker must continue /think -> /blueprint
                    # -> link, else the flagged child stays designless/idea forever
                    # (a bare /think never links plan_path). why_digest keeps it
                    # grounded when the transcript is unresolved; project_root scopes
                    # the /think doc to the CHILD's repo (cross-repo routing).
                    child_root = child.get("_resolved_cwd") or child.get("cwd")
                    res = maybe_spawn_think(
                        child,
                        run_state=born_rs,
                        env=forced_env,
                        quiet=json_mode(ctx),
                        chain_blueprint=True,
                        why_digest=why_digest,
                        project_root=Path(child_root) if child_root else None,
                    )
    # Rationale (8 lines): docs/architecture/graph-cli-rationale.md#cmd-decompose-2451
                    lane = "wave0" if cid in wave0_ids else "needs_think"
                    fanout.append(
                        {
                            "id": cid,
                            "decision": res.decision,
                            "reason": res.reason,
                            "lane": lane,
                            "owned": res.decision == "spawned",
                        }
                    )
                    if res.decision != "spawned" and not json_mode(ctx):
                        typer.echo(
                            f"fan-out /think for {cid} did not spawn "
                            f"({res.reason}); it stays yours to inline-fill "
                            f"(or run `/think {cid}` then `/blueprint`)",
                            err=True,
                        )
        except Exception as exc:  # noqa: BLE001 - additive; never wedge the decompose
            # Non-fatal by design (the graph mutation already committed), but
            # NOT silent: an empty `fanout` is indistinguishable from "no
            # children needed designing", so a crash here would read as a clean
            # no-op to both the operator and a --json consumer. Name it and say
            # which children fell back to inline-fill.
            _unowned = [
                c for c in spec_ids if not any(f["id"] == c and f.get("owned") for f in fanout)
            ]
            if not json_mode(ctx):
                typer.echo(
                    f"warning: design fan-out failed ({exc}); "
                    f"{len(_unowned)} child(ren) stay yours to inline-fill"
                    + (f": {', '.join(_unowned)}" if _unowned else ""),
                    err=True,
                )

    # 4b. Report what happened (AC1-UI).
    if json_mode(ctx):
        typer.echo(
            json.dumps(
                {
                    "epic": epic_resolved_id,
                    "groups": results,
                    "orphaned": orphan_ids,
                    "downgrades": downgrades,
                    "packaging": plans,
                    "scaffolded": scaffolded,
                    "fanout": fanout,
                },
                default=str,
            )
        )
    else:
        typer.echo(f"epic: {epic_resolved_id}")
        typer.echo(f"decomposed into {len(results)} group child node(s) (packaging: {plans}):")
        for r in results:
            waves = f" waves {r['waves']}" if r["waves"] else ""
            blk = f" blocked_by={r['blocked_by']}" if r["blocked_by"] else ""
            tier = " dep=contract" if r.get("dep") == "contract" else ""
            marker = r["slug"]
            # Adoption re-parents PRE-EXISTING nodes, so it must reach the human
            # receipt and not only `--json` - same reason the fan-out ownership
            # line below does. Empty stays silent, so a spec with no adopt key
            # prints byte-for-byte what it printed before.
            adopted = f" adopted={r['adopted']}" if r.get("adopted") else ""
            typer.echo(f"  {r['action']}: {r['id']} ({marker}){waves}{blk}{tier}{adopted}")
        for f in scaffolded:
            typer.echo(f"  scaffolded plan: {f}")
        # Ownership must reach the HUMAN receipt, not just `--json`. The
        # blueprint session is told to skip children the fan-out owns
        # (epic-decomposition.md step 7), and step 6 invokes decompose without
        # `--json` - so a contract carried only in the JSON shape is a contract
        # the reader never sees, and AC9-CON's no-double-write property would
        # rest on a field the default invocation does not emit.
        _owned = [fo["id"] for fo in fanout if fo.get("owned")]
        for fo in fanout:
            if fo["decision"] == "spawned":
                typer.echo(f"  fan-out design pass dispatched: {fo['id']}")
        if _owned:
            typer.echo(f"  fan-out OWNS (do NOT inline-fill): {', '.join(_owned)}")
        _unowned_attempts = [fo["id"] for fo in fanout if not fo.get("owned")]
        if _unowned_attempts:
            typer.echo(
                f"  fan-out did NOT claim (inline-fill these): {', '.join(_unowned_attempts)}"
            )
        if orphan_ids:
            typer.echo(
                f"warning: {len(orphan_ids)} group child node(s) no longer in the spec, "
                f"left in place: {', '.join(orphan_ids)}",
                err=True,
            )
        for msg in downgrades:
            typer.echo(f"warning: {msg}", err=True)

    # 4c. Name the epic children no group adopted (US3). Emitted on BOTH
    #     report paths, not just the human one: a --json caller decomposing a
    #     populated epic needs this as much as an operator does, and stderr
    #     never pollutes the JSON on stdout.
    if unadopted_ids:
        _contained_n = len(contained_unadopted_box[0])
        _contained_clause = (
            f"; {_contained_n} of them are contained in the epic itself, which "
            "has no PR, so add them to a group's adopt list or they never close"
            if _contained_n
            else ""
        )
        typer.echo(
            f"warning: {len(unadopted_ids)} epic child(ren) adopted by no group, "
            f"left parented to the epic: {', '.join(unadopted_ids)}. "
            "Add them to a group's `adopt` list to package them into that PR"
            f"{_contained_clause}.",
            err=True,
        )

    # Rationale (9 lines): docs/architecture/graph-cli-rationale.md#cmd-decompose-2571
    base = base_box[0]
    expected_count = len(results)
    if base and expected_count >= 1:
        status, detail = _set_expected_count(base, expected_count)
        if status == "failed":
            typer.echo(
                f"warning: could not record expected_url_count={expected_count} on "
                f"{base}: {detail}. The shared doc will graduate after the FIRST "
                f"group ships unless you run: fno do plan set-expected --plan-path "
                f"{base} --count {expected_count}",
                err=True,
            )
        # status == "skipped": the doc or script is absent (an environment
        # condition that cannot cause early graduation - target can't stamp it
        # either). Proceed silently; the graph mutation already succeeded.

    # Repaint the epic and every child this decompose CREATED so a decomposed
    # epic's children carry correct blocked_by/parent mirrors from birth (US5).
    # Scoped to created children (not already-linked ones): an existing child's
    # hand-filled plan is left untouched here and its drift rides the sweep.
    _project_plans_from_graph([epic_resolved_id, *created_ids])


# -- intake --


def _intake_impl(
    plan_paths: Optional[List[str]] = None,
    from_list: Optional[str] = None,
    roadmap_id: Optional[str] = None,
    title: Optional[str] = None,
    priority: Optional[str] = None,
    deps: Optional[str] = None,
    points: Optional[int] = None,
    project: Optional[str] = None,
    force_new_roadmap: bool = False,
    batch: bool = False,
    dry_run: bool = False,
    claims: Optional[str] = None,
    allow_no_surface: bool = False,
) -> None:
    """Implementation for the intake verb.

    Pulls an existing plan file into the backlog as a new node. Typer-parameter
    defaults are intentionally plain Python values here so the thin command
    wrapper can pass through already-parsed arguments. Kept as a separate
    `_intake_impl` (rather than inlined into `cmd_intake`) so the underlying
    `_intake.py` helpers can be exercised by tests without going through Typer.
    """
    from fno.graph._constants import PRIORITY_ORDER
    from fno.graph.store import commit_rows_via_store
    from fno.graph._intake import (
        _prepare_intake,
        _build_intake_node,
        _refuse_surfaceless_intake,
        _validate_cli_deps,
    )

    # Reject removed --batch flag
    if batch:
        typer.echo(
            "Error: `--batch` was removed. Use multi-path intake instead:\n"
            "  fno backlog intake plans/a.md plans/b.md plans/c.md\n"
            "  fno backlog intake plans/folder/*.md  # shell glob",
            err=True,
        )
        raise typer.Exit(code=1)

    # Build args-like namespace for reuse of shared intake logic
    args = SimpleNamespace(
        roadmap_id=roadmap_id,
        title=title,
        priority=priority,
        deps=deps,
        points=points,
        force_new_roadmap=force_new_roadmap,
        dry_run=dry_run,
        from_list=from_list,
        plan_paths=plan_paths or [],
        project=project,
    )

    if project is not None and (not isinstance(project, str) or not project.strip()):
        typer.echo("Error: --project must be a non-empty string", err=True)
        raise typer.Exit(code=1)

    if args.priority and args.priority not in PRIORITY_ORDER:
        typer.echo(
            f"Error: invalid priority '{args.priority}'. "
            f"Must be: {', '.join(PRIORITY_ORDER.keys())}",
            err=True,
        )
        raise typer.Exit(code=1)

    all_paths = _collect_intake_paths_typer(plan_paths or [], from_list)
    if not all_paths:
        if from_list:
            label = "stdin" if from_list == "-" else from_list
            typer.echo(
                f"Error: --from {label} produced 0 usable paths "
                "(blank lines and '#' comments are skipped).",
                err=True,
            )
        else:
            typer.echo(
                "Error: no plan paths provided. Pass one or more positional "
                "arguments, or use --from FILE (or --from -).",
                err=True,
            )
        raise typer.Exit(code=1)

    if len(all_paths) > 1:
        _do_intake_multi(
            args,
            all_paths,
            roadmap_id=roadmap_id,
            dry_run=dry_run,
            allow_no_surface=allow_no_surface,
        )
        return

    # Single-path flow
    plan_path = all_paths[0]

    cli_deps: list[str] = [d.strip() for d in deps.split(",") if d.strip()] if deps else []

    # Creation path: the same external-backend refusal every birth path
    # carries (an intake mints new nodes).
    _refuse_create_on_external_backend()

    entries = wire_rows(path=_graph_path())

    if roadmap_id and not force_new_roadmap:
        has_roadmap = any(e.get("roadmap_id") == roadmap_id for e in entries)
        if not has_roadmap:
            typer.echo(
                f"unknown roadmap_id: {roadmap_id} "
                "(use /megawalk vision.md to create a roadmap first, "
                "pass --force-new-roadmap, or omit --roadmap-id to intake to the backlog)",
                err=True,
            )
            raise typer.Exit(code=2)

    _validate_cli_deps(cli_deps, entries)

    try:
        prep = _prepare_intake(
            plan_path,
            entries,
            roadmap_id=roadmap_id,
            cli_title=title,
            cli_priority=priority,
            cli_deps=cli_deps,
            cli_points=points,
            cli_project=project,
            cli_claim=claims,
        )
    except ValueError as e:
        typer.echo(f"Error: {e}", err=True)
        raise typer.Exit(code=1)
    if prep["status"] == "already":
        typer.echo(f"already intaked: {prep['id']}")
        return

    # After _prepare_intake validates and before any write, so the refusal
    # also covers the dry-run preview (surface is mandatory at intake).
    _refuse_surfaceless_intake([plan_path], allow_no_surface=allow_no_surface)

    spec = prep["node_spec"]

    if dry_run:
        verb = "claim" if prep["status"] == "claim" else "intake"
        typer.echo(f"{verb.capitalize()} preview (dry-run, no changes):")
        target = f" (claims {prep['id']})" if prep["status"] == "claim" else ""
        typer.echo(f'  would {verb}: "{spec["title"]}"  (plan: {plan_path}){target}')
        if spec["deps"]:
            typer.echo(f"  blocked_by: {', '.join(spec['deps'])}")
        return

    # Emit "not in ledger" warning before mutating
    from fno.graph._intake import _lookup_ledger_entry

    if _lookup_ledger_entry(plan_path) is None:
        typer.echo("plan_path not in ledger.json - intake will continue anyway", err=True)

    if prep["status"] == "claim":
        claim_id = prep["id"]
        claim_source = prep["claim_source"]

        def claim_mutator(es):
            return _apply_claim_in_place(
                es, claim_id, plan_path=plan_path, spec=spec, project=project
            )

        commit_rows_via_store(_graph_path(), claim_mutator)
        typer.echo(
            f'linked plan to {claim_id} via {claim_source}: "{spec["title"]}" - '
            f"take the work lock: fno do target start {claim_id}"
        )
        # Mirror nav fields onto the just-linked plan of the CLAIMED node too -
        # this branch returns early, so the append-path projection never runs.
        # Routed through the converger so parent_slug is injected consistently.
        try:
            from fno.plan._project import project_graph_nodes

            project_graph_nodes(wire_rows(path=_graph_path()), [claim_id])
        except Exception as e:  # noqa: BLE001 - additive; never wedge the claim
            sys.stderr.write(f"warning: post-claim plan projection failed: {e}\n")
        return

    new_id_holder: list[Optional[str]] = [None]

    def mutator(es):
        node = _build_intake_node(spec, es)
        new_id_holder[0] = node["id"]
        es.append(node)
        return es

    try:
        commit_rows_via_store(_graph_path(), mutator)
    except ValueError as exc:
        # Build-time refusals (the difficulty gate, an invalid band) land as
        # a clean one-line error on the single-file lane; the multi lane
        # already caught these per-file to skip, not abort, the batch.
        typer.echo(f"error: intake refused: {exc}", err=True)
        raise typer.Exit(code=1) from exc
    destination = roadmap_id if roadmap_id else "backlog"
    typer.echo(f'intake {new_id_holder[0]} -> {destination}: "{spec["title"]}"')

    try:
        from fno.graph._intake import _warn_unknown_project, _find_node

        post_entries = wire_rows(path=_graph_path())
        node = _find_node(post_entries, new_id_holder[0] or "")
        landed_project = node.get("project") if node else None
        _warn_unknown_project(landed_project)
    except Exception as e:
        # The mutation already committed; a stray failure in the warning
        # path must not surface as if the intake itself failed.
        sys.stderr.write(f"warning: post-intake project check failed: {e}\n")

    # Filing-time dedup net (plan ): warn if the just-born node resembles
    # an existing one across all live states. Own try/except so a dedup failure
    # is reported as itself, not conflated with the project check above.
    try:
        from fno.graph._intake import _find_node, _warn_similar_nodes

        post_entries = wire_rows(path=_graph_path())
        node = _find_node(post_entries, new_id_holder[0] or "")
        if node is not None:
            _warn_similar_nodes(node, post_entries, intake_hint=True)
    except Exception as e:  # noqa: BLE001 - dedup never breaks the intake
        _safe_stderr_warn(f"warning: post-intake dedup check skipped: {e}\n")

    # Mirror the graph-authoritative navigation fields onto the plan doc the
    # node just linked. Non-fatal: a missing/unreadable plan never fails intake.
    # Routed through the converger so parent_slug is injected consistently.
    if new_id_holder[0]:
        try:
            from fno.plan._project import project_graph_nodes

            project_graph_nodes(wire_rows(path=_graph_path()), [new_id_holder[0]])
        except Exception as e:  # noqa: BLE001 - additive; never wedge the intake
            sys.stderr.write(f"warning: post-intake plan projection failed: {e}\n")

    # Born-with-why (v2 A1): route the intaked node through the shared birth hook
    # for uniformity across birth paths. Independent of the project-warning block
    # above so a warn failure never drops the dispatch. Most intake nodes are
    # built by _build_intake_node (no ambient provenance stamp) and self-skip
    # with 'no-origin'; this keeps every birth path consistent. Non-fatal +
    # opt-in (gate-OFF default => complete no-op).
    if new_id_holder[0]:
        try:
            from fno.graph._intake import _find_node
            from fno.provenance.spawn_think import on_node_born

            born_node = _find_node(wire_rows(path=_graph_path()), new_id_holder[0])
            if born_node is not None:
                # The native hook re-reads the durable row from the graph.
                on_node_born(born_node, graph_path=_graph_path())
        except Exception:  # noqa: BLE001 - additive; never wedge the intake
            pass


@cli.command(
    "intake",
    hidden=True,
    epilog="Paired verb: `fno backlog remove <id>` deletes the node this creates.",
)
def cmd_intake(
    plan_paths: Optional[List[str]] = typer.Argument(default=None, help="Plan paths"),
    from_list: Optional[str] = typer.Option(
        None, "--from", help="Read paths from FILE or '-' for stdin"
    ),
    roadmap_id: Optional[str] = typer.Option(None, "--roadmap-id", help="Target roadmap ID"),
    title: Optional[str] = typer.Option(None, "--title", "-t", help="Override derived title"),
    priority: Optional[str] = typer.Option(None, "--priority", "-p", help="p0|p1|p2|p3"),
    deps: Optional[str] = typer.Option(None, help="Comma-separated ab-IDs"),
    points: Optional[int] = typer.Option(None, help="Story point estimate"),
    project: Optional[str] = typer.Option(
        None, "--project", help="Override the project field (beats frontmatter and cwd inference)"
    ),
    force_new_roadmap: bool = typer.Option(False, "--force-new-roadmap"),
    batch: bool = typer.Option(False, "--batch", hidden=True),
    dry_run: bool = typer.Option(False, "--dry-run", "-N"),
    claims: Optional[str] = typer.Option(
        None,
        "--claims",
        help=(
            "ab-XXXXXXXX of an existing node, in any state, that this plan "
            "implements. "
            "Updates the node in place rather than creating a new one. "
            "Beats any frontmatter 'claims:' value."
        ),
    ),
    allow_no_surface: bool = typer.Option(
        False,
        "--allow-no-surface",
        help=(
            "Admit a plan whose '## Files to Modify' parses empty. Such a node "
            "cannot be collision-checked, so lane fill dispatches it fail-open."
        ),
    ),
) -> None:
    """Pull in an existing plan file as a backlog node."""
    _refuse_create_on_external_backend()
    _intake_impl(
        plan_paths=plan_paths,
        from_list=from_list,
        roadmap_id=roadmap_id,
        title=title,
        priority=priority,
        deps=deps,
        points=points,
        project=project,
        force_new_roadmap=force_new_roadmap,
        batch=batch,
        dry_run=dry_run,
        claims=claims,
        allow_no_surface=allow_no_surface,
    )


# -- update --


# -- release / requeue: the queue-return subject lives in fno.backlog.requeue;
# registration stays here on the backlog app. --




# -- next --


def _external_open_status(*, pr_number: Optional[int], plan_path: Optional[str]) -> str:
    """Read-time status for an OPEN external item: in_review > ready > idea.

    A plan-less, PR-less open node is dispatch work nobody has started
    planning yet, not ready work - the same three-way split selection
    filters on. Every external-backend status render (selection, the mux
    snapshot, `backlog get`) must derive from this one function so a node
    dispatch skips never reads as ready anywhere else.
    """
    if pr_number:
        return "in_review"
    return "ready" if plan_path else "idea"


def _read_external_node_and_sidecar(id: str):
    """Exact-id tracker read plus sidecar load, shared by every external-backend
    single-node renderer. Raises the tracker's ``NodeNotFound`` unchanged so
    each caller keeps its own not-found message and exit code."""
    from fno.tracker import get_tracker
    from fno.tracker import sidecar as sidecar_store

    node = get_tracker().read(id)
    sc = sidecar_store.load(id)
    return node, sc


class _ExternalSelectionError(RuntimeError):
    """A tracker or required sidecar read failed during joined selection.

    AC6-ERR: selection fails CLOSED. The message names the failing backend or
    id; the caller exits nonzero and never falls back to the local graph file
    or silently selects a different node.
    """


def _joined_open_candidates() -> list[dict]:
    """The transient joined selection model: the Rust snapshot exactly once.
    Full contract: docs/architecture/backlog-graph-verb-contracts.md
    """
    from fno.tracker import get_tracker

    tracker = get_tracker()
    try:
        entries = tracker._call("snapshot")["entries"]  # type: ignore[attr-defined]
    except Exception as exc:  # noqa: BLE001 - name the backend, fail closed
        raise _ExternalSelectionError(f"tracker {tracker.name!r} snapshot failed: {exc}") from exc
    return [e for e in entries if e.get("state") == "open"]


    # Rationale (8 lines): docs/architecture/graph-cli-rationale.md#joined-open-candidates-4307
EXTERNAL_SELECTION_TTL = "15m"


def _dispatch_node_summary(e) -> dict:
    """The ONE projection a dispatcher sees when it picks work.

    `next` and `ready` both feed autonomous dispatch, so the two summaries
    are one thing spelled twice: the union of every field either surface
    carried, no filtering, no ordering. A key dropped here is dropped from
    every dispatch decision - the silent `dispatch_verb` loss this exists
    to prevent. Consumers read by key, so additions are safe.
    """
    return {
        # slug leads () so a list / clipboard is readable; `id` stays the
        # canonical key right after.
        "slug": e.get("slug"),
        "id": e["id"],
        "title": e.get("title"),
        "priority": e.get("priority"),
        "domain": e.get("domain"),
        "project": e.get("project"),
        "cwd": e.get("cwd"),
        "parent": e.get("parent"),
        "size": e.get("size"),
        "difficulty": e.get("difficulty"),
        # the native lane-fill door's dispatch-time collision gate compares plan file
        # surfaces; without this it has nothing to read.
        "plan_path": e.get("plan_path"),
        # The per-node model pin rides so the active-backlog drain can
        # prefer it over cfg.model.
        "model": e.get("model"),
        # The per-node dispatch overrides must ride so the resolver's
        # verb/brief routing fires for real graph nodes, not only for
        # tests that inject them.
        "dispatch_verb": e.get("dispatch_verb"),
        "dispatch_brief": e.get("dispatch_brief"),
        "mission_id": e.get("mission_id"),
        "mission_wave": e.get("mission_wave"),
        "mission_slug": e.get("mission_slug"),
        "mission_from_msg_id": e.get("mission_from_msg_id"),
        # age and rank ride too, so a dispatcher can order and
        # staleness-check without a second `backlog get` per node.
        "created_at": e.get("created_at"),
        "touched_at": e.get("touched_at"),
        "rank": e.get("rank"),
    }


@cli.command("next")
def cmd_next(
    roadmap_id: Optional[str] = typer.Option(None, "--roadmap-id"),
    parent: Optional[str] = typer.Option(
        None,
        "--parent",
        help="Restrict to transitive children of this epic node (ab-ID).",
    ),
    claim: Optional[str] = typer.Option(None, "--claim", help="Session ID to atomically claim"),
    project: Optional[str] = typer.Option(None, "--project", "-p", help="Filter by project name"),
    all_: bool = typer.Option(False, "--all", "-A", help="Consider all projects"),
    include_ideas: bool = typer.Option(
        False,
        "--ideas",
        "-I",
        "--include-ideas",
        help="Also consider idea-stage rows (plan-less nodes) as claimable.",
    ),
    include_deferred: bool = typer.Option(
        False,
        "--include-deferred",
        help="Also consider deferred rows for explicit re-engagement.",
    ),
    mission: Optional[str] = typer.Option(
        None,
        "--mission",
        help=(
            "Restrict to nodes whose mission_id matches (megatron child walks: "
            "the walk works ONLY the mission's nodes)."
        ),
    ),
) -> None:
    from fno.graph._intake import (
        detect_project,
        descendants_of,
        _find_node,
    )
    from fno.tracker import active_backend_name

    result: list = [None]
    project_filter = project
    _external = active_backend_name() != "graph"

    # The graph-backend answer is native: the door (fno backlog next) owns
    # the prelude, the occupancy, the selection, the reservation, and the
    # receipts. This wheel spelling keeps only the external-backend branch,
    # whose joined tracker candidates the door forwards here from inside its
    # own arm.
    if not _external:
        typer.echo(
            "Error: the selection is served by the native door; "
            "run `fno backlog next`.",
            err=True,
        )
        raise typer.Exit(code=2)

    # One read for the prelude AND selection: the transient joined model
    # (list_open + sidecar join, fail-closed).
    try:
        pre_entries = _joined_open_candidates()
    except _ExternalSelectionError as exc:
        typer.echo(f"Error: {exc}; selection refused", err=True)
        raise typer.Exit(code=1)
    if not project_filter and not all_:
        assert pre_entries is not None  # set under the same condition above
        project_filter = detect_project(pre_entries)
    # Rationale (8 lines): docs/architecture/graph-cli-rationale.md#cmd-next-4424
    parent_target_id: Optional[str] = None
    if parent:
        assert pre_entries is not None  # set when `parent` is truthy above
        target = _find_node(pre_entries, parent)
        if target is None:
            typer.echo(f"Error: no such node '{parent}'", err=True)
            raise typer.Exit(code=1)
        parent_target_id = target["id"]
        if not descendants_of(pre_entries, parent_target_id):
            typer.echo(f"no children under {parent_target_id}", err=True)

    def _select(entries, occupancy):
        from fno.graph._intake import repo_root
        from fno.graph.store import (
            ClaimsUnavailableError,
            ReadyParentMissingError,
            StoreUnavailable,
            ready as store_ready,
        )

        try:
            return store_ready(
                project=project_filter,
                all=all_,
                roadmap_id=roadmap_id,
                mission=mission,
                parent=parent_target_id,
                include_ideas=include_ideas,
                include_deferred=include_deferred,
                repo_root=repo_root(),
                entries=entries,
                occupancy=occupancy,
            )["rows"]
        except StoreUnavailable as exc:
            typer.echo(f"Error: store keeper unavailable; selection refused: {exc}", err=True)
            raise typer.Exit(code=1) from exc
        except ReadyParentMissingError as exc:
            typer.echo(f"Error: {exc}", err=True)
            raise typer.Exit(code=1) from exc
        except ClaimsUnavailableError as exc:
            typer.echo(f"Error: {exc}", err=True)
            raise typer.Exit(code=1) from exc

    from fno.backlog.undispatched import (
        ObserverReadError,
        build_selection_divergence_event,
        prepend_missed_rows,
        read_planned_unclaimed_from_entries,
    )

    # The claim set of the selection that actually ran, for the receipts below.
    selection_claimed: list = [set()]

    def _prepare(entries: list[dict]) -> tuple[set, dict]:
        """Dispatch occupancy plus the observer receipt, read ONCE per selection.

        The claim verdict and the roster read are what this command costs, and
        three layers each used to pay for them. A `--claim` transaction that
        loses to an interleaved writer pays again on its retry, deliberately:
        the entries it selects from are new, so its occupancy must be too.
        """
        try:
            claimed, worked = read_occupancy(entries, _live_claimed_node_ids)
        except OccupancyUnavailable as exc:
            typer.echo(f"Error: {exc}; selection refused", err=True)
            raise typer.Exit(code=1) from exc
        try:
            observer = read_planned_unclaimed_from_entries(
                entries,
                project=None if all_ else project_filter,
                mission=mission,
                roadmap_id=roadmap_id,
                parent=parent_target_id,
                worked=worked,
            )
        except ObserverReadError as exc:
            typer.echo(f"Error: {exc}; selection refused", err=True)
            raise typer.Exit(code=1) from exc
        except Exception as exc:  # noqa: BLE001 - unknown state refuses recovery
            typer.echo(f"Error: observer revalidation failed: {exc}", err=True)
            raise typer.Exit(code=1) from exc
        selection_claimed[0] = claimed
        return claimed | set(worked), observer

    def _with_observer(
        candidates: list[dict],
        source_entries: list[dict],
        occupied: set,
        current_observer: dict,
    ) -> list[dict]:
        by_id = {entry.get("id"): entry for entry in source_entries}
        container_ids = _container_ids(source_entries)
        from fno.rust_binary import verb_call

        guards = verb_call(["backlog", "advance", "--stdin"], {"op": "selection-guards", "entries": source_entries})["guards"]
        safe_rows = []
        for row in current_observer["rows"]:
            entry = by_id.get(row.get("id"))
            if entry is None or row.get("id") in occupied:
                continue
            if entry.get("completed_at") or _has_unmerged_open_pr(entry):
                continue
            if entry.get("id") in container_ids or _is_batched_member(entry):
                continue
            if guards.get(entry.get("id"), {}).get("reason"):
                continue
            safe_rows.append(entry)
        safe_observer = {**current_observer, "rows": safe_rows}
        merged, missed = prepend_missed_rows(candidates, safe_observer)
        if missed:
            scope = f"project={(project_filter if not all_ else '*')}"
            if mission:
                scope += f",mission={mission}"
            if roadmap_id:
                scope += f",roadmap={roadmap_id}"
            for row in missed:
                try:
                    from fno import paths
                    from fno.events import append_event

                    append_event(
                        build_selection_divergence_event(
                            node_id=row["id"],
                            selector_command="fno backlog next",
                            scope=scope,
                            selector_entries_scanned=len(candidates),
                            observer_entries_scanned=current_observer["entries_scanned"],
                        ),
                        paths.project_events_json(),
                    )
                except Exception as exc:  # noqa: BLE001 - receipt is non-gating
                    typer.echo(f"warning: selection divergence event failed: {exc}", err=True)
        return merged

    if claim:
        if pre_entries is not None:
            from fno.claims.cli import _parse_ttl
            from fno.claims.core import ClaimHeldByOther, acquire_claim

            occupied, observer = _prepare(pre_entries)
            candidates = _with_observer(
                _select(pre_entries, occupied), pre_entries, occupied, observer
            )
            for winner in candidates:
                key = f"node:{winner['id']}"
                try:
                    acquire_claim(
                        key,
                        claim,
                        ttl_ms=_parse_ttl(EXTERNAL_SELECTION_TTL),
                    )
                except ClaimHeldByOther:
                    continue
                result[0] = _dispatch_node_summary(winner)
                break
    else:
        assert pre_entries is not None
        entries = pre_entries
        occupied, observer = _prepare(entries)
        candidates = _with_observer(_select(entries, occupied), entries, occupied, observer)
        if candidates:
            result[0] = _dispatch_node_summary(candidates[0])

    # Zero-silent-starvation receipts (G1): explain to stderr why nothing
    # was picked - and, even when a winner WAS picked, which in-scope nodes
    # are stranded under terminal parents. Advisory - stdout stays
    # exactly the node-or-"null" contract `_next_node` parses, so a receipt
    # failure never breaks dispatch. Under an external backend the receipts
    # explain the ACTUAL joined denominator, never the local graph.
    try:
        from fno.rust_binary import verb_call

        recv_entries = pre_entries or []
        scope_ids = (
            descendants_of(recv_entries, parent_target_id)
            if parent_target_id is not None
            else None
        )
        receipts = _starvation_receipts(
            recv_entries,
            project_filter,
            all_,
            scope_ids,
            selection_claimed[0],
            datetime.now(timezone.utc),
            verb_call(["backlog", "advance", "--stdin"], {"op": "staleness"})["days"],
            mission=mission,
            roadmap_id=roadmap_id,
        )
    except Exception as exc:  # noqa: BLE001 - receipts are advisory
        typer.echo(f"warning: starvation receipts failed: {exc}", err=True)
    else:
        if result[0] is None:
            for nid, reason in receipts:
                typer.echo(f"excluded {nid}: {reason}", err=True)
        else:
            # A winner exists: only the strand receipts fire, capped, so a
            # healthy dispatch never drowns in exclusion noise again.
            for line in _stranded_next_receipts(receipts):
                typer.echo(line, err=True)

    typer.echo(json.dumps(result[0], indent=2) if result[0] else "null")


@cli.command("undispatched", hidden=True)
def cmd_undispatched(
    project: Optional[str] = typer.Option(None, "--project", "-p"),
    all_: bool = typer.Option(False, "--all", "-A"),
    roadmap_id: Optional[str] = typer.Option(None, "--roadmap-id"),
    parent: Optional[str] = typer.Option(None, "--parent"),
    mission: Optional[str] = typer.Option(None, "--mission"),
    json_output: bool = typer.Option(False, "--json", "-J"),
) -> None:
    """Name finalized, ready leaf plans with no node claim.

    The graph-backend answer is native: the door (fno backlog undispatched)
    owns the classify, the claim keys, and the worked fold. This wheel
    spelling keeps only the external-backend branch, whose joined tracker
    candidates the door forwards here from inside its own arm.
    """
    del all_, json_output  # the observer is JSON by contract and all-scoped by default
    from fno.backlog.undispatched import ObserverReadError, read_planned_unclaimed_from_entries
    from fno.tracker import active_backend_name

    if active_backend_name() == "graph":
        typer.echo(
            "Error: the observer is served by the native door; "
            "run `fno backlog undispatched`.",
            err=True,
        )
        raise typer.Exit(code=2)

    try:
        receipt = read_planned_unclaimed_from_entries(
            _joined_open_candidates(),
            project=project,
            mission=mission,
            roadmap_id=roadmap_id,
            parent=parent,
        )
    except _ExternalSelectionError as exc:
        typer.echo(f"Error: tracker unreadable: {exc}", err=True)
        raise typer.Exit(code=1) from exc
    except ObserverReadError as exc:
        typer.echo(f"Error: {exc}", err=True)
        raise typer.Exit(code=1) from exc
    typer.echo(json.dumps(receipt, indent=2))


# -- lane-fill --


# -- lane-fill (native door) --


@cli.command("lane-fill", hidden=True)
def cmd_lane_fill(
    max_lanes: Optional[int] = typer.Option(None, "--max"),
    project: Optional[str] = typer.Option(None, "--project", "-p"),
    mission: Optional[str] = typer.Option(None, "--mission"),
    claim: bool = typer.Option(False, "--claim"),
) -> None:
    """The parallel fill answers natively; there is no external body here.

    Unlike next/undispatched this wheel spelling keeps nothing: the tombstone
    is the whole arm, on every backend.
    """
    del max_lanes, project, mission, claim
    typer.echo(
        "Error: the parallel fill is served by the native door; "
        "run `fno backlog lane-fill`.",
        err=True,
    )
    raise typer.Exit(code=2)


# -- join --


# -- lanes --




# -- get --


def _read_time_status_external(
    state: str, pr_number: Optional[int], plan_path: Optional[str]
) -> str:
    """Read-time rung for the external get render - never stored, derived on
    every read from tracker state plus sidecar evidence."""
    if state == "closed":
        return "done"
    return _external_open_status(pr_number=pr_number, plan_path=plan_path)


def _render_external_get(id: str, field: Optional[str]) -> None:
    """`backlog get` under an external backend: exact-id tracker read plus the
    sidecar, joined FOR DISPLAY ONLY (a render, not a stored convenience
    record). Byte-compatibility binds the graph mode above, not this branch."""
    from fno.tracker.types import NodeNotFound

    try:
        node, sc = _read_external_node_and_sidecar(id)
    except NodeNotFound:
        typer.echo(
            f"fno backlog get: no node matches '{id}' "
            "(an external backend resolves exact ids; slug/bare-hex are "
            "footnote-minted)",
            err=True,
        )
        raise typer.Exit(code=1)
    state = str(node.state.value)
    joined: dict = {
        "id": node.id,
        "title": node.title,
        "state": state,
        "status": _read_time_status_external(state, sc.pr_number, sc.plan_path),
        "parent": node.parent,
        "blocked_by": list(node.blocked_by),
    }
    joined.update(sc.model_dump(exclude_unset=True, exclude={"id"}))
    joined["_resolved_cwd"] = sc.cwd

    if field:
        _echo_node_entry(joined, field, False)
        return
    typer.echo(json.dumps(joined, indent=2))


def _echo_node_entry(e: dict, field: Optional[str], grouped: bool) -> None:
    """Render one resolved node: the field / grouped / JSON output ladder."""
    if field:
        value = e.get(field)
        if value is None:
            typer.echo("null")
        elif isinstance(value, (list, dict)):
            typer.echo(json.dumps(value))
        else:
            typer.echo(value)
    else:
        typer.echo(json.dumps(e, indent=2))


@cli.command("get")
def cmd_get(
    ids: List[str] = typer.Argument(
        ..., help="One or more node ab-id, slug, or bare 8-hex (e.g.  | dashless-spawn | ff6f96e0)"
    ),
    field: Optional[str] = typer.Option(None, help="Print only this field"),
    grouped: bool = typer.Option(
        False, "--grouped", help="Render populated fields in human-readable concept groups."
    ),
    strict: bool = typer.Option(
        False,
        "--strict",
        help="Exact-only resolution (id/slug/bare-hex); never fuzzy. The stable surface the /think router seeds a design from - a miss exits 1 so a typo'd token can never silently seed.",
    ),
) -> None:
    from fno.tracker import active_backend_name

    from fno.graph.get_batch import resolve_or_dispatch
    id = resolve_or_dispatch(ids, field=field, grouped=grouped, strict=strict)

    # Pre-rename spelling; shell consumers outside this repo still pass it.
    if field == "_status":
        field = "status"

    # Backend-neutral read: an external tracker resolves the OPAQUE id exactly
    # (never through the local <prefix>-<hex> grammar) and displays the five
    # tracker fields plus the sidecar. Slug/bare-hex tiers are footnote-minted
    # metadata and refuse with the backend named rather than reporting absent.
    if active_backend_name() != "graph":
        _render_external_get(id, field)
        return


# -- project-root (work-map resolution; null-for-unmapped) --


# -- provenance --

# A cycle in source_node_id needs a visited set to terminate; the cap is the
# second belt, and bounds output on a legitimately deep chain. Not configurable:
# a follow-up chain this long is a graph problem, not a display preference.
_SPAWNED_MAX_DEPTH = 10


def _spawned_walk(
    entries: list, root_id: str, *, max_depth: int = _SPAWNED_MAX_DEPTH
) -> "tuple[list, bool, bool]":
    """Walk source_node_id in reverse: what did ``root_id`` produce?

    Breadth-first so depth falls out of the traversal rather than being assigned,
    and so the shortest path to a node is the depth reported for it.

    Returns ``(rows, cycle_detected, truncated)`` where rows are
    ``(depth, entry)``. A cycle TRUNCATES the walk and keeps the descendants
    already found - returning an empty set with a cycle flag would satisfy a
    naive reading of "terminates" while silently discarding the answer.
    """
    from fno.graph.rollup import origin_index

    by_source = origin_index(entries)
    rows: list = []
    seen = {root_id}
    frontier = [root_id]
    cycle = False
    depth = 0
    while frontier and depth < max_depth:
        depth += 1
        nxt: list = []
        for parent_id in frontier:
            for child in sorted(by_source.get(parent_id, []), key=lambda e: e.get("id") or ""):
                child_id = child.get("id")
                if not isinstance(child_id, str):
                    continue
                if child_id in seen:
                    cycle = True
                    continue
                seen.add(child_id)
                rows.append((depth, child))
                nxt.append(child_id)
        frontier = nxt
    # Truncated only if something was actually cut: a chain ending exactly at
    # the cap leaves a non-empty frontier with nothing below it.
    truncated = any(
        child.get("id") not in seen
        for parent_id in frontier
        for child in by_source.get(parent_id, [])
    )
    return rows, cycle, truncated


def _render_external_provenance(id: str, spawned: bool, json_out: bool) -> None:
    """`backlog provenance` under an external backend: exact-id tracker read
    plus the sidecar provenance edges (the AC2 provenance path). Footnote-minted
    extras the sidecar does not carry (``related``) render empty rather than
    from stale local rows."""
    import dataclasses

    from fno.provenance.resolver import resolve_transcript, _DEFAULT_PROJECTS_ROOT
    from fno.tracker import get_tracker
    from fno.tracker import sidecar as sidecar_store
    from fno.tracker.types import NodeNotFound

    try:
        node, sc = _read_external_node_and_sidecar(id)
    except NodeNotFound:
        typer.echo(f"No node matching '{id}' (external backend; exact ids)", err=True)
        raise typer.Exit(code=1)

    def _title_of(nid: Optional[str]) -> Optional[str]:
        if not nid:
            return None
        try:
            return get_tracker().read(nid).title
        except Exception:  # noqa: BLE001 - advisory title; absent is honest
            return None

    birth_result = (
        resolve_transcript(
            sc.source_harness,
            sc.source_session_id,
            sc.source_cwd or sc.cwd,
            projects_root=_DEFAULT_PROJECTS_ROOT,
        )
        if sc.source_session_id
        else None
    )
    spawn_result = (
        resolve_transcript(
            sc.spawned_by_harness,
            sc.spawned_by_session,
            sc.spawned_by_cwd,
            projects_root=_DEFAULT_PROJECTS_ROOT,
        )
        if sc.spawned_by_session
        else None
    )

    # Spawned walk over the sidecar origin index (source_node_id edges),
    # titles joined from the tracker.
    walk_rows: list = []
    if spawned:
        by_source: dict = {}
        for nid, other in sidecar_store.load_all().items():
            if other.source_node_id:
                by_source.setdefault(other.source_node_id, []).append(nid)
        seen = {node.id}
        frontier = [node.id]
        depth = 0
        while frontier and depth < _SPAWNED_MAX_DEPTH:
            depth += 1
            nxt = []
            for parent_id in frontier:
                for child_id in sorted(by_source.get(parent_id, [])):
                    if child_id in seen:
                        continue
                    seen.add(child_id)
                    walk_rows.append((depth, child_id))
                    nxt.append(child_id)
            frontier = nxt

    def _edge(label: str, result) -> dict:
        if result is None:
            return {"edge": label, "session_id": None, "resolved": False}
        d = dataclasses.asdict(result)
        d["edge"] = label
        return d

    if json_out:
        output: dict[str, Any] = {
            "node_id": node.id,
            "title": node.title,
            "edges": [_edge("node_birth", birth_result), _edge("spawn", spawn_result)],
            "pr": pr_block(sc),
            "sessions": sc.sessions,
            "lifecycle": None,  # roster derives from sc.sessions; kept for shape parity
            "source_node_id": sc.source_node_id,
            "source_node_title": _title_of(sc.source_node_id),
            "source_plan_path": sc.source_plan_path,
            "related": [],  # footnote-minted; unavailable under an external backend
        }
        if spawned:
            output["spawned"] = {
                "nodes": [{"depth": d, "id": nid, "title": _title_of(nid)} for d, nid in walk_rows],
                "cycle_detected": False,
                "truncated_at_depth": None,
            }
        typer.echo(json.dumps(output, indent=2))
        return
    typer.echo(f"provenance for {node.id}: {node.title or ''}")
    typer.echo(f"  node_birth: {sc.source_session_id or '(none)'}")
    typer.echo(f"  spawn: {sc.spawned_by_session or '(none)'}")
    typer.echo(render_pr_line(pr_block(sc)))
    typer.echo(f"  sessions: {len(sc.sessions)} row(s)")
    if spawned:
        for d, nid in walk_rows:
            typer.echo(f"  spawned d{d}: {nid} ({_title_of(nid) or ''})")


@cli.command("provenance", hidden=True)
def cmd_provenance(
    id: str = typer.Argument(
        ...,
        help="Node ab-id, slug, or bare 8-hex",
    ),
    spawned: bool = typer.Option(
        False,
        "--spawned",
        help="Also walk the origin edge in reverse: every node this one produced, transitively.",
    ),
    json_out: bool = typer.Option(
        False, "--json", "-J", help="Emit machine-readable JSON instead of human summary"
    ),
) -> None:
    """Show provenance pointers for a node and resolve transcripts where possible.

    Reads two provenance edges stored on the node:

      node-birth edge  source_session_id + source_harness + source_cwd
      spawn edge       spawned_by_session + spawned_by_harness + spawned_by_cwd

    For each edge that carries a session id the resolver is run (claude only;
    codex/gemini/etc. return resolved=False). Read-only: no graph mutation.
    """
    from fno.graph.fuzzy import resolve_node
    from fno.provenance.resolver import resolve_transcript, _DEFAULT_PROJECTS_ROOT
    from fno.tracker import active_backend_name

    if active_backend_name() != "graph":
        _render_external_provenance(id, spawned, json_out)
        return

    # Strict read for the same reason cmd_get uses it: a wedged graph must not
    # read as "No node matching", which asserts the node is absent.
    entries = _resolve_entries_or_exit(id)
    match = resolve_node(id, entries)
    if match.kind != "exact":
        from fno.graph.store import served_store_path
        typer.echo(f"No node matching '{id}' in {served_store_path(_graph_path())}", err=True)
        raise typer.Exit(code=1)

    e = match.candidates[0]
    node_id = e["id"]

    # node-birth edge: resolve against the originating SESSION cwd
    # (source_cwd), NOT the node's durable project `cwd`. Claude transcript dirs
    # are slugged by the session cwd, so a node filed from a worktree resolves
    # only via source_cwd; fall back to `cwd` for legacy pre-source_cwd nodes.
    birth_session = e.get("source_session_id")
    birth_harness = e.get("source_harness")
    birth_cwd = e.get("source_cwd") or e.get("cwd")
    birth_result = None
    if birth_session:
        birth_result = resolve_transcript(
            birth_harness,
            birth_session,
            birth_cwd,
            projects_root=_DEFAULT_PROJECTS_ROOT,
        )

    # spawn edge: uses spawned_by_cwd
    spawn_session = e.get("spawned_by_session")
    spawn_harness = e.get("spawned_by_harness")
    spawn_cwd = e.get("spawned_by_cwd")
    spawn_result = None
    if spawn_session:
        spawn_result = resolve_transcript(
            spawn_harness,
            spawn_session,
            spawn_cwd,
            projects_root=_DEFAULT_PROJECTS_ROOT,
        )

    index = {e.get("id"): e for e in entries if isinstance(e, dict)}

    def _titled(other_id: str) -> str:
        """`<id> (<title>)`, so the line reads without a second lookup."""
        other = index.get(other_id)
        if other is None:
            return f"{other_id} (not in graph)"
        return f"{other_id} ({other.get('title', '')})"

    origin_id = e.get("source_node_id")
    related_ids = e.get("related") or []
    walk_rows, walk_cycle, walk_truncated = (
        _spawned_walk(entries, node_id) if spawned else ([], False, False)
    )

    # Runtime-attempt projection (wave 3): live/suspect/stale/interrupted
    # attempts read off manifests + the claim, alongside the confirmed lifecycle
    # rows below. Never a confirmed `do` row; never a graph mutation.
    from fno.provenance.runtime_attempts import runtime_attempts

    runtime = runtime_attempts(node_id, e)

    # Lifecycle roster (): per-phase start/end/duration + an honest total,
    # computed once for both the JSON and human paths.
    roster_lines, roster_summary = _lifecycle_roster(
        e["sessions"], registry_status_index()
    )

    if json_out:
        import dataclasses

        def _edge(label: str, result) -> dict:
            if result is None:
                return {"edge": label, "session_id": None, "resolved": False}
            d = dataclasses.asdict(result)
            d["edge"] = label
            return d

        output = {
            "node_id": node_id,
            "title": e.get("title"),
            "edges": [
                _edge("node_birth", birth_result),
                _edge("spawn", spawn_result),
            ],
            # Append-only lifecycle provenance in raw append order ().
            # read_graph's defaults guarantee the key, so no fallback guard.
            "pr": pr_block(e),
            "sessions": e["sessions"],
            # Per-phase roster with starts, durations, and an honest total
            # (absent values are null, never 0). See _lifecycle_roster.
            "lifecycle": roster_summary,
            "source_node_id": origin_id,
            "source_node_title": (index.get(origin_id) or {}).get("title"),
            "source_plan_path": e.get("source_plan_path"),
            "related": related_ids,
            "runtime_attempts": runtime,
        }
        if spawned:
            output["spawned"] = {
                "nodes": [
                    {"depth": d, "id": n.get("id"), "title": n.get("title")} for d, n in walk_rows
                ],
                "cycle_detected": walk_cycle,
                "truncated_at_depth": _SPAWNED_MAX_DEPTH if walk_truncated else None,
            }
        typer.echo(json.dumps(output, indent=2))
        return

    # Human-readable summary
    lines = [f"provenance for {node_id}: {e.get('title', '')}"]

    def _fmt_edge(label: str, result, session: Optional[str], harness: Optional[str]) -> None:
        if session is None:
            lines.append(f"  {label}: (none)")
            return
        lines.append(f"  {label}:")
        lines.append(f"    session:  {session}")
        lines.append(f"    harness:  {harness or '(unknown)'}")
        if result is None:
            lines.append("    transcript: (not resolved)")
        elif result.resolved:
            ambig = " [ambiguous match]" if result.ambiguous else ""
            lines.append(f"    transcript: {result.transcript_path}{ambig}")
        else:
            reason = result.reason or "not-found"
            lines.append(f"    transcript: (unresolved - {reason})")

    _fmt_edge("node-birth", birth_result, birth_session, birth_harness)
    # Rendered even when null: an omitted line reads as "this verb does not
    # report origins", which is how the field stayed invisible for a month.
    lines.append(f"  origin: {_titled(origin_id) if origin_id else '(none)'}")
    if e.get("source_plan_path"):
        lines.append(f"    plan: {e['source_plan_path']}")
    _fmt_edge("spawn", spawn_result, spawn_session, spawn_harness)
    lines.append(render_pr_line(pr_block(e)))

    lines.append(f"  related: {'(none)' if not related_ids else ''}".rstrip())
    for rid in related_ids:
        lines.append(f"    {_titled(rid)}")

    if spawned:
        lines.append(f"  spawned: {'(none)' if not walk_rows else ''}".rstrip())
        for depth, n in walk_rows:
            lines.append(f"    {'  ' * (depth - 1)}d{depth} {_titled(n.get('id'))}")
        if walk_cycle:
            lines.append("    note: cycle detected; walk truncated at the repeat")
        if walk_truncated:
            lines.append(f"    note: depth cap {_SPAWNED_MAX_DEPTH} reached; walk truncated")

    # Runtime attempts (wave 3): live/suspect/stale/interrupted attempts
    # projected from manifests + the claim. Rendered BEFORE lifecycle so an
    # operator sees the current/interrupted state first; explicitly labeled
    # unconfirmed so it is never mistaken for a confirmed `do` lifecycle row.
    if runtime:
        lines.append("  runtime:")
        for a in runtime:
            work_bits = []
            if a.get("commits_ahead"):
                work_bits.append(f"{a['commits_ahead']} commits")
            if a.get("pr_number"):
                work_bits.append(f"PR #{a['pr_number']}")
            work = ", ".join(work_bits) or "(no work evidence)"
            lines.append(
                f"    {a['attempt_state']:<11} {a.get('harness', '?')}:{a.get('harness_session_id', '?')}"
            )
            lines.append(f"      run:       {a.get('fno_id', '?')}")
            lines.append(f"      worktree:  {a.get('worktree', '?')}")
            lines.append(f"      claim:     {a.get('claim_state', '?')} pid={a.get('claim_pid')}")
            lines.append(f"      work:      {work}")
            lines.append(f"      lifecycle: {a.get('lifecycle', '?')}")

    # Lifecycle roster (, ): per-phase start/end/duration + an
    # honest total. Distinct from the birth/spawn edges above -- those are
    # single parent pointers; this is the per-phase who-did-what across sessions
    # and harnesses. Every lifecycle phase always renders (not recorded / end
    # only for gaps) so a roster never reads "nobody touched this" by omission.
    lines.append("  lifecycle:")
    lines.extend(roster_lines)

    typer.echo("\n".join(lines))


# Session lifecycle verbs (add/open/close/backfill/reap-open) went native:
# crates/fno-agents/src/backlog/session_cli.rs answers `fno backlog session`
# at the door, and the Python surface no longer mounts a twin.


# -- cost --


# -- remove --


def _expand_valid_ids(task_ids: list[str]) -> list[str]:
    """Expand one-or-many id args; refuse an empty set and non-node ids
    (the shared prologue of every batch-mutating verb)."""

    ids = _expand_id_args(task_ids)
    if not ids:
        typer.echo("Error: at least one task_id is required", err=True)
        raise typer.Exit(code=1)
    for tid in ids:
        _require_node_id(tid)
    return ids


def _expand_id_args(raw_ids: list[str]) -> list[str]:
    """Flatten a list of CLI args into individual node IDs.

    Accepts both space-separated args (``ab-X ab-Y``) and comma-
    separated bundles (``ab-X,ab-Y``) so end-of-day batch triage feels
    natural: ``fno backlog queue ab-X,ab-Y ab-Z`` is valid. Preserves
    first-occurrence order, dedupes ALL repeats (a ``seen`` set drops
    any id already encountered, not just adjacent ones), strips
    whitespace.
    """
    out: list[str] = []
    seen: set[str] = set()
    for raw in raw_ids:
        for part in str(raw).split(","):
            tid = part.strip()
            if not tid:
                continue
            if tid in seen:
                continue
            seen.add(tid)
            out.append(tid)
    return out


@cli.command("queued", hidden=True)
def cmd_queued(
    project: Optional[str] = typer.Option(None, help="Filter by project name"),
    all_: bool = typer.Option(False, "--all", "-A", help="Show all projects"),
) -> None:
    """List nodes the user has queued for action. JSON output, sorted by priority."""
    # The queue read is native: the door (fno backlog queued) serves the
    # read, the project filter, and the sort. A direct wheel spelling has no
    # leg left to run, so it names the door instead of carrying a second
    # implementation.
    typer.echo(
        "Error: the queue read is served by the native door; run `fno backlog queued`.",
        err=True,
    )
    raise typer.Exit(code=2)


@cli.command(
    "queue",
    hidden=True,
    epilog="Paired verb: `fno backlog unqueue <id>...` reverses this (hidden; run its own --help).",
)
def cmd_queue(
    task_ids: List[str] = typer.Argument(
        ...,
        help="Feature IDs (ab-XXXXXXXX). Multiple via space and/or comma: 'ab-X,ab-Y ab-Z'.",
    ),
    reason: Optional[str] = typer.Option(
        None,
        "--reason",
        "-R",
        help="Why these nodes are being queued (applies to all). Free text, surfaced on the card.",
    ),
) -> None:
    """Queue one or more backlog nodes for action. Sets ``queued_at`` + optional ``queued_reason``.

    Atomic across the batch: if any ID is unknown, none of the nodes
    are queued. Same reason applies to every ID in the batch.
    """
    # The queue write is native: the door (fno backlog queue) owns the
    # batch stamp and its atomicity. A direct wheel spelling has no leg left
    # to run, so it names the door instead of carrying a second
    # implementation.
    typer.echo(
        "Error: the queue write is served by the native door; "
        "run `fno backlog queue <ids>`.",
        err=True,
    )
    raise typer.Exit(code=2)


@cli.command("unqueue", hidden=True)
def cmd_unqueue(
    task_ids: List[str] = typer.Argument(
        ...,
        help="Feature IDs (ab-XXXXXXXX). Multiple via space and/or comma: 'ab-X,ab-Y ab-Z'.",
    ),
) -> None:
    """Clear queued state on one or more backlog nodes. Idempotent.

    Atomic across the batch: if any ID is unknown, none are cleared.
    Reports each ID's prior state; warns (non-fatally) for IDs that
    were not actually queued.
    """
    # The unqueue write is native: the door (fno backlog unqueue) owns the
    # batch clear and its atomicity. A direct wheel spelling has no leg left
    # to run, so it names the door instead of carrying a second
    # implementation.
    typer.echo(
        "Error: the unqueue write is served by the native door; "
        "run `fno backlog unqueue <ids>`.",
        err=True,
    )
    raise typer.Exit(code=2)


@cli.command(
    "contain",
    hidden=True,
    epilog="Inverse: `fno backlog update <id> --parent null` un-contains a node - a verb the native binary serves after the python update leg retired.",
)
def cmd_contain(
    ctx: typer.Context,
    owner: str = typer.Argument(..., help="The owning node: contained nodes ship inside its PR."),
    task_ids: List[str] = typer.Argument(
        ...,
        help="Node IDs to fold (ab-XXXXXXXX). Multiple via space and/or comma.",
    ),
) -> None:
    """Fold existing nodes into an owner: they ship inside its PR. No plan needed.

    Stamps contained_in + parent in one locked mutation. Atomic across the
    batch: any refusal stamps nothing. A deferred target is accepted (contain
    first, undefer second, so the node is never armed in between). Containment
    is released by the owner's merge cascade or by moving the node away.
    """
    # The containment write is native: the door (fno backlog contain) owns the
    # guard ladder, the stamps, and the receipts. A direct wheel spelling has
    # no leg left to run, so it names the door instead of carrying a second
    # implementation.
    typer.echo(
        "Error: the containment write is served by the native door; "
        "run `fno backlog contain <owner> <ids>`.",
        err=True,
    )
    raise typer.Exit(code=2)


# -- stuck-epics --




# -- done --


def _project_plans_from_graph(
    node_ids: list[str],
    *,
    mirror_keys_for: tuple[str, frozenset[str]] | None = None,
    force_status_off_terminal_for: str | None = None,
    clear_keys_for: tuple[str, frozenset[str]] | None = None,
) -> None:
    """Project each named node's mirror fields + forward status onto its plan.

    Re-reads the graph so every node carries its recomputed ``status``, then
    delegates to the shared converger. Covers cascade-closed epic parents that
    ``_stamp_and_graduate_plan`` never stamps. Best-effort per node: a missing
    or unreadable plan never fails the mutation.

    ``mirror_keys_for`` pairs the ONE node whose extra keys (``type``,
    ``difficulty``) may be written with those keys, set only where the
    operator supplied the value. It is an id, not a flag: this projection
    repaints ancestors and siblings too, and their values are defaults.
    """
    ids = [i for i in dict.fromkeys(node_ids) if i]
    if not ids:
        return
    try:
        # Vault mirror projection is default-backend machinery (same class as
        # plan sync): guarded metadata read, degrades to a no-op under an
        # external selection rather than painting stale local rows.
        from fno.plan._project import project_graph_nodes
        from fno.tracker.metadata import read_entries

        entries = read_entries("plan.project")
    except Exception as e:  # noqa: BLE001 - additive; never wedge the mutation
        sys.stderr.write(f"warning: plan projection setup failed: {e}\n")
        return
    project_graph_nodes(
        entries,
        ids,
        mirror_keys_for=mirror_keys_for,
        force_status_off_terminal_for=force_status_off_terminal_for,
        clear_keys_for=clear_keys_for,
    )


def _apply_completion_fields(node: dict, *, merge_status: Optional[str] = None) -> None:
    """Set the fields that mark a node done.

    Shared by ``done`` and ``reconcile`` so both close paths stay in
    lockstep. The caller owns the idempotency check (skip when
    ``completed_at`` is already set). ``recompute_statuses`` derives
    ``status: done`` from ``completed_at`` and unblocks dependents.

    ``merge_status`` is passed ONLY by a caller that resolved MERGED from gh,
    so the field keeps meaning "GitHub confirmed this". A ``--force`` close and
    a PR-less epic cascade leave it unset rather than assert a merge.
    """
    # Done dominates deferred per the cascade. Clear any deferred/queued state
    # so the row presents as cleanly done with no ghost fields.
    node["deferred_at"] = None
    node["deferred_reason"] = None
    node.pop("deferred_kind", None)
    node["queued_at"] = None
    node["queued_reason"] = None
    node["completed_at"] = datetime.now(timezone.utc).isoformat()
    if merge_status is not None:
        node["merge_status"] = merge_status


def _auto_closed_note(_entry: dict) -> str:
    """completion_note for a container closed by all-children-complete ().

    Both close paths (_cascade_close_parents, _sweep_close_done_epics) reach
    here holding the parent dict. A plan-having, PR-less container is no
    longer closed at all: the own-plan-work hold stops both close paths, so
    the old UNVERIFIED flag branch is retired with the false close it
    flagged.
    """
    return "auto-closed: all children complete"


def _cascade_close_parents(entries: list[dict], node_id: str) -> list[str]:
    """Close ancestor epics whose children are now all complete .
    Full contract: docs/architecture/backlog-graph-verb-contracts.md
    """
    from fno.graph._reconcile import cascade_close_should_stop
    id_to_entry = {
        e["id"]: e for e in entries if isinstance(e, dict) and isinstance(e.get("id"), str)
    }
    children_by_parent: dict[str, list[dict]] = {}
    for e in entries:
        if isinstance(e, dict) and isinstance(e.get("parent"), str):
            children_by_parent.setdefault(e["parent"], []).append(e)

    closed: list[str] = []
    cur = id_to_entry.get(node_id)
    for _ in range(64):  # depth cap: guards against a malformed parent cycle
        pid = cur.get("parent") if isinstance(cur, dict) else None
        if not isinstance(pid, str):
            break
        parent = id_to_entry.get(pid)
        if parent is None:
            break  # missing ancestor -> stop this branch
        kids = children_by_parent.get(pid) or []
        if cascade_close_should_stop(parent, kids, cur):
            break  # already closed, or a child still open -> stop this branch
        _apply_completion_fields(parent)
        if not parent.get("completion_note"):
            parent["completion_note"] = _auto_closed_note(parent)
        # Deactivate the mission (K1): a kicked-off epic carries
        # mission_active=true for K2's drain loop; its last child landing closes
        # the epic here, so clear the marker in the same mutation. Durable
        # deactivation - the drain never keeps looping a done mission.
        parent.pop("mission_active", None)
        closed.append(pid)
        cur = parent  # cascade up to the grandparent
    return closed


def _echo_freed(freed: list, owner_id: str) -> None:
    """Name the nodes a dying delivery unit just released.

    Silence here is a real gap, not tidiness: the release turns N nodes that
    were invisible to dispatch into autonomously buildable, separately costed
    ones, and a bare remove/supersede receipt gives the operator no way to know
    what the next selection pass will pick up.
    """
    if not freed:
        return
    typer.echo(
        f"Released {len(freed)} contained node(s) from {owner_id}; they are "
        f"dispatchable again: {', '.join(freed)}"
    )


# In graph/strand.py: the terminal-parent strand family (moved with the
# close guards, release twins, and self-heal that share its liveness predicate).
from fno.graph.strand import (  # noqa: E402
    _stranded_next_receipts,
)

# In graph/selection_evidence.py: the occupancy one `backlog next` selection
# reads, and the receipts that explain what it passed over.
from fno.graph.selection_evidence import (  # noqa: E402
    OccupancyUnavailable,
    _starvation_receipts,
    read_occupancy,
)

# In graph/_closures.py: this file is over the source budget; these four ride
# this module's namespace for the tests that import them from here.
from fno.graph._closures import (  # noqa: E402, F401
    _cascade_close_contained as _cascade_close_contained,
    _strandable_contained_ids as _strandable_contained_ids,
    _strandable_epic_ids as _strandable_epic_ids,
    _sweep_close_done_epics as _sweep_close_done_epics,
)


def _status_drift(path: Path) -> dict[str, tuple[str, str]]:
    import copy

    from fno.graph.statuses import recompute_statuses
    from fno.graph.store import _read_json, read_graph_strict

    persisted: dict[str, str] = {}
    for entry in _read_json(path):
        node_id = entry.get("id") if isinstance(entry, dict) else None
        status = entry.get("status") if isinstance(entry, dict) else None
        if isinstance(node_id, str) and isinstance(status, str) and not (status == "blocked" and entry.get("blocked_by")):
            persisted[node_id] = status

    derived: dict[str, str] = {}
    for entry in recompute_statuses(copy.deepcopy(read_graph_strict(path))):
        node_id = entry.get("id") if isinstance(entry, dict) else None
        status = entry.get("status") if isinstance(entry, dict) else None
        if isinstance(node_id, str) and isinstance(status, str):
            derived[node_id] = status
    return {
        node_id: (persisted[node_id], derived[node_id])
        for node_id in sorted(persisted.keys() & derived.keys())
        if persisted[node_id] != derived[node_id]
    }


def _stamp_and_graduate_plan(
    plan_path: str,
    *,
    url: Optional[str] = None,
    session_id: Optional[str] = None,
) -> bool:
    """Best-effort: stamp a plan ``shipped`` (when a ship URL is known) then graduate.
    Full contract: docs/architecture/backlog-graph-verb-contracts.md
    """
    from fno.plan._project import plan_docs

    stamped_shipped = False
    if url:
        sid = session_id or "backlog-close"
        res = plan_docs("stamp", plan_path=plan_path, session_id=sid, urls=[url])
        if res is None or res["exit"]:
            return False
        stamped_shipped = True

    res = plan_docs("graduate", plan_path=plan_path)
    if res is None or res["exit"]:
        # A successful stamp already recorded the ship; report that win even if
        # the graduate spawn failed.
        return stamped_shipped
    return True


# Closed set of outcomes from _set_expected_count, so the call site's
# `status == "failed"` compare is type-checked rather than a free-form string.
SetExpectedStatus = Literal["ok", "skipped", "failed"]


def _set_expected_count(plan_path: str, count: int) -> tuple[SetExpectedStatus, str]:
    """Authoritatively write expected_url_count=count onto a plan's frontmatter.
    Full contract: docs/architecture/backlog-graph-verb-contracts.md
    """
    from fno.plan._project import plan_docs

    res = plan_docs("set_expected", plan_path=plan_path, count=count)
    return (res["status"], res["message"].strip()) if res else ("failed", "graph store unavailable")


# -- gh cross-check helpers (injectable for tests) --
# These module-level callables are replaced by test stubs via monkeypatch.


def _done_gh_query(pr_number, **kwargs):
    """Query gh for PR merge state. Delegates to reconcile's canonical helper."""
    from fno.graph._reconcile import query_pr_merge_state

    return query_pr_merge_state(pr_number, **kwargs)


def _done_gate_pipeline(
    task_id: str,
    node: dict,
    refs: list,
    *,
    force: bool,
    reason: Optional[str],
) -> Optional[str]:
    """The shared rich-completion gates (task 4.1): gh merge evidence,
    the forced-close journal, and the promise gate, with today's exit-code
    contract (3 refused / 4 outage / 5 awaiting merge / 6 promise unmet).
    Both completion front doors (``backlog done`` on either backend,
    the deprecated ``done`` spelling) run this BEFORE any close so neither can bypass the
    gates. Returns the evidencing PR url (None when no evidence).
    """
    from fno.graph._reconcile import (
        render_merge_evidence_failure,
        repo_slug_from_url,
        resolve_merge_evidence,
        resolve_promise_evidence,
    )

    # Usage guard lives in the shared terminal so every front door and every
    # backend gets it: the external dispatch reaches this pipeline before any
    # caller-side guard can fire.
    if force and not reason:
        typer.echo(
            "Error: --force requires --reason TEXT (explain why the cross-check is bypassed)",
            err=True,
        )
        raise typer.Exit(code=2)

    evidence_pr_url: Optional[str] = None
    if refs and not force:
        # There are PR references; require evidence before closing.
        first_pr_number, _ = refs[0]

        # Shared with `done_command` so the two paths cannot drift apart on what
        # counts as evidence.
        evidence = resolve_merge_evidence(refs, cwd=node.get("cwd"), query=_done_gh_query)
        evidence_found = evidence.outcome == "merged"
        if evidence_found:
            evidence_pr_url = evidence.pr_url
        else:
            if evidence.outcome == "awaiting_merge":
                typer.echo(
                    f"awaiting merge: PR #{evidence.open_pr_number} is OPEN, not merged. "
                    f"{task_id} stays in_review and closes on merge "
                    f"(reconcile / merge-triggered advance). "
                    f"Use --force --reason TEXT for an early close."
                    + (f" (note: {evidence.error})" if evidence.error else ""),
                    err=True,
                )
                raise typer.Exit(code=evidence.exit_code)

            if evidence.outcome == "outage":
                typer.echo(render_merge_evidence_failure(task_id, evidence, stays="open"), err=True)
                raise typer.Exit(code=evidence.exit_code)

            # Pure policy refusal - CLOSED-unmerged / UNKNOWN only.
            msg = evidence.reason or f"PR #{first_pr_number}: no merged evidence"
            if evidence.remedy:
                typer.echo(render_merge_evidence_failure(task_id, evidence, stays="open"), err=True)
            else:
                typer.echo(
                    f"Refused: {task_id} cross-check failed: {msg}\n"
                    f"Use --force --reason TEXT to bypass.",
                    err=True,
                )
            # Emit refusal event (best-effort)
            try:
                from fno import events as _evts

                event = _evts.backlog_done_refused(
                    node_id=task_id,
                    pr_number=first_pr_number,
                    reason=msg,
                )
                _evts.append_event(event)
            except Exception:
                pass
            raise typer.Exit(code=evidence.exit_code)

    # -- Step 3: Force path - proceed and journal loudly --
    if force and refs:
        assert reason is not None  # the `--force requires --reason` guard above ensures this
        first_pr_number, first_pr_url = refs[0]
        # A forced close still names a PR; stamp the plan against it so the ship
        # is recorded even when the cross-check was bypassed ().
        evidence_pr_url = first_pr_url
        pr_repo = repo_slug_from_url(first_pr_url)
        # Best-effort: try to read the current PR state for journaling
        try:
            force_pr_state_obj = _done_gh_query(first_pr_number, repo=pr_repo)
            force_pr_state = force_pr_state_obj.state
        except Exception:
            force_pr_state = "UNKNOWN"
        typer.echo(
            f"Warning: force-closing {task_id} (reason: {reason}). "
            f"PR #{first_pr_number} state={force_pr_state}.",
            err=True,
        )
        # Emit forced-close event (best-effort)
        try:
            from fno import events as _evts

            event = _evts.backlog_done_forced(
                node_id=task_id,
                force_reason=reason,
                pr_number=first_pr_number,
                pr_state=force_pr_state,
            )
            _evts.append_event(event)
        except Exception:
            pass
    elif force:
        # --force with no refs: just log (advisory node)
        typer.echo(
            f"Warning: force flag set on advisory node {task_id} (reason: {reason}); no PR refs to check.",
            err=True,
        )

    # -- Step 3b: promise gate () --
    # The merge gate asked "is a PR merged"; this asks "did the plan's declared
    # work all ship". Skipped under --force so a deliberate half-ship stays a
    # journaled line (the backlog_done_forced event above) rather than silence.
    if not force:
        promise = resolve_promise_evidence(node, cwd=node.get("cwd"), query=_done_gh_query)
        if not promise.satisfied:
            typer.echo(promise.reason, err=True)
            raise typer.Exit(code=promise.exit_code)
        if promise.warning:
            typer.echo(f"warning: {promise.warning}", err=True)
    return evidence_pr_url


def _cascade_close_external_parents(tracker, child_id: str) -> list[str]:
    """Close ancestor containers whose children are now ALL closed - the
    external twin of ``_cascade_close_parents``: sibling state from ONE
    list_open, the chain from tracker reads, each close best-effort."""
    closed: list[str] = []
    try:
        open_children: dict[str, list[str]] = {}
        for cand in tracker.list_open():
            if cand.parent:
                open_children.setdefault(cand.parent, []).append(cand.id)
        cur = tracker.read(child_id).parent
        seen: set[str] = set()
        while cur and cur not in seen:
            seen.add(cur)
            if open_children.get(cur):
                break  # still has open children
            try:
                parent_node = tracker.read(cur)
            except Exception:  # noqa: BLE001 - unknown ancestor ends the walk
                break
            if str(parent_node.state.value) == "closed":
                cur = parent_node.parent
                continue
            try:
                tracker.close(cur)
                closed.append(cur)
            except Exception as exc:  # noqa: BLE001 - cascade is best-effort
                typer.echo(f"warning: cascade close failed for {cur}: {exc}", err=True)
                break
            cur = parent_node.parent
    except Exception as exc:  # noqa: BLE001 - cascade never fails the close
        typer.echo(f"warning: cascade evaluation failed: {exc}", err=True)
    return closed


def _done_via_seam(task_id: str, *, skip_stamp: bool, force: bool, reason: Optional[str]) -> None:
    """Rich completion under an external backend (task 4.1, AC7/AC8).

    The same shared gate pipeline runs first; footnote-owned rollups persist
    to the sidecar BEFORE the irreversible close; then
    ``get_tracker().close(task_id)`` runs exactly once and success prints only
    after it returns. A failed external close is loud and retryable - the
    item stays open. Plan stamping and the ancestor cascade ride the same
    seam (tracker parent edges, sidecar plan_path)."""
    from fno.graph._reconcile import node_pr_refs
    from fno.tracker import get_tracker
    from fno.tracker import sidecar as sidecar_store
    from fno.tracker.types import NodeNotFound

    try:
        tnode, sc = _read_external_node_and_sidecar(task_id)
    except NodeNotFound:
        typer.echo(f"Error: feature {task_id} not found", err=True)
        raise typer.Exit(code=1)
    if str(tnode.state.value) == "closed":
        typer.echo(f"{task_id} is already done", err=True)
        return

    tracker = get_tracker()
    row = {
        "id": task_id,
        "title": tnode.title,
        "cwd": sc.cwd,
        "plan_path": sc.plan_path,
        "pr_number": sc.pr_number,
        "pr_url": sc.pr_url,
        "additional_prs": sc.additional_prs,
        "sessions": sc.sessions,
        # The rollup reads containment off the node (a contained child claims
        # no cost); dropping it here would double-count the delivery unit.
        "contained_in": sc.contained_in,
    }
    refs = node_pr_refs(row)
    evidence_pr_url = _done_gate_pipeline(task_id, row, refs, force=force, reason=reason)

    # Footnote-owned rollups BEFORE the close (one physical owner: the sidecar).
    try:
        from fno.done.cli import _rollup_from_ledger

        rollup = _rollup_from_ledger(row)
    except Exception:  # noqa: BLE001 - rollup is fill-only, never blocks
        rollup = {}
    if rollup.get("cost_usd") is not None and sc.cost_usd is None:
        sc.cost_usd = rollup["cost_usd"]
    if rollup.get("cost_sessions") and not sc.cost_sessions:
        sc.cost_sessions = list(rollup["cost_sessions"])
    if evidence_pr_url and sc.pr_url is None:
        sc.pr_url = evidence_pr_url
    sidecar_store.save(sc)

    # The one close. Failure keeps the item open and retryable (AC8-ERR).
    try:
        tracker.close(task_id)
    except Exception as exc:  # noqa: BLE001 - name backend + id, fail loud
        typer.echo(
            f"Error: external close failed for {task_id} on backend "
            f"{tracker.name!r}: {exc}\n"
            "The item stays open; retry once the backend is available.",
            err=True,
        )
        raise typer.Exit(code=1)

    _cascade_close_external_parents(tracker, task_id)
    typer.echo(f"Marked {task_id} done")

    # Closure releases the node claim at the SEAM, right after the one close,
    # so every tracker backend inherits it (github today; the graph backend
    # gets the same release from the store's closure hook, which an external
    # close never reaches). Placing it in one backend's close() would be a
    # guard on one of N reachable implementations.
    from fno.graph.store import release_node_claim_at_closure

    release_node_claim_at_closure(task_id, rung="done")

    if sc.plan_path and not skip_stamp:
        _stamp_and_graduate_plan(sc.plan_path, url=evidence_pr_url, session_id=None)


@cli.command(
    "done",
    epilog="Paired verb: `fno backlog reopen <id> --reason ...` reverses this "
    "(hidden; run its own --help). Related: `fno backlog reconcile` closes nodes "
    "whose PR merged outside the gate (hidden). Correction is reopen, a verb "
    "the native binary serves after the python leg retired.",
)
def cmd_done(
    task_id: Optional[str] = typer.Argument(
        None,
        help="Feature ID (ab-XXXXXXXX), title substring, or omit to auto-detect from the git branch.",
    ),
    skip_stamp: bool = typer.Option(
        False,
        "--skip-stamp",
        help="Skip plan stamp even if plan_path is set",
    ),
    force: bool = typer.Option(
        False,
        "--force",
        "-F",
        help="Bypass gh cross-check. Requires --reason.",
    ),
    reason: Optional[str] = typer.Option(
        None,
        "--reason",
        "-R",
        help="Required when --force is used. Explains why the cross-check is bypassed.",
    ),
    pr: Optional[int] = typer.Option(
        None, "--pr-number", "--pr", "-p", help="PR number (for code-domain completions)."
    ),
    pr_url: Optional[str] = typer.Option(
        None,
        "--pr-url",
        help="PR URL. Derived from the repo when omitted; supply it when the repo slug cannot be resolved.",
    ),
    repo: Optional[str] = typer.Option(
        None,
        "--repo",
        help=(
            "owner/name of the repo the PR lives in, when that differs from the "
            "cwd checkout. Naming the repo makes the stamp an assertion rather "
            "than a cwd derivation, which is also what lets it override a "
            "recorded pr_url naming a different repo ()."
        ),
    ),
    link: Optional[str] = typer.Option(
        None,
        "--link",
        "--url",
        "-l",
        help="Artifact URL (Figma/Canva/Obsidian/any) - sets artifact_url.",
    ),
    note: Optional[str] = typer.Option(
        None, "--note", "-m", help="Free-text completion note - sets completion_note."
    ),
    backfill: bool = typer.Option(
        False,
        "--backfill",
        help=(
            "Run ONLY the ledger-rollup (session_id, cost_usd, cost_sessions, "
            "points). Does not flip status or completed_at. With no TASK_ID, "
            "sweeps every node with status=done."
        ),
    ),
    force_overwrite: bool = typer.Option(
        False,
        "--force-overwrite",
        help="Overwrite existing rollup fields instead of fill-if-null. Use with --backfill for explicit re-reconciliation of stale rollups.",
    ),
) -> None:
    """Mark a node complete.
    Full contract: docs/architecture/backlog-graph-verb-contracts.md
    """
    from fno.graph._constants import has_node_id_prefix

    # The close flags and the completion flags never mixed on either legacy
    # spelling, and the delegation below would silently drop --force. Refuse
    # the combination by name rather than letting a deliberate half-ship
    # request run ungated.
    _close_flags = force or reason is not None or skip_stamp
    _rich_flags = (
        backfill
        or force_overwrite
        or pr is not None
        or pr_url is not None
        or repo is not None
        or link is not None
        or note is not None
    )
    if _close_flags and _rich_flags:
        typer.echo(
            "Error: the close flags (--force/--reason/--skip-stamp) and the "
            "completion flags (--pr/--pr-url/--link/--note/--backfill/"
            "--force-overwrite) are separate paths. A deliberate half-ship "
            "closes on the id alone: `fno backlog done <id> --force --reason ...`",
            err=True,
        )
        raise typer.Exit(code=2)

    # Rationale (8 lines): docs/architecture/graph-cli-rationale.md#cmd-done-8491
    if (
        not _close_flags
        and not _rich_flags
        and task_id is not None
        and not has_node_id_prefix(task_id)
    ):
        from fno.tracker import active_backend_name

        if active_backend_name() == "graph":
            from fno.done.cli import _current_branch
            from fno.graph.fuzzy import resolve_id

            _match = resolve_id(
                task_id,
                wire_rows(path=_graph_path()),
                git_branch=_current_branch(),
            )
            if _match.kind in ("exact", "fuzzy", "branch_derived"):
                task_id = _match.id
            # none/ambiguous fall through to the delegate for its messages.

    # The rich surface delegates (): any ported flag or a missing id
    # (branch auto-detect) routes to the implementation that already owns
    # those paths - and only when NO close flag is set, because the delegate
    # has no --force/--reason/--skip-stamp and would drop them. A node id
    # (given or resolved above) keeps THIS command's gates, stamp, and
    # dependent cascade - the canonical close.
    _delegate = not _close_flags and (
        _rich_flags or task_id is None or not has_node_id_prefix(task_id)
    )
    if _delegate:
        from fno.done.cli import done_command

        done_command(
            query=task_id,
            pr=pr,
            pr_url=pr_url,
            repo=repo,
            link=link,
            note=note,
            backfill=backfill,
            force_overwrite=force_overwrite,
        )
        return
    if task_id is None:
        typer.echo(
            "Error: an explicit node id is required with the close flags "
            "(--force/--reason/--skip-stamp); branch auto-detect applies to "
            "the completion surface only",
            err=True,
        )
        raise typer.Exit(code=2)
    # External backend (task 4.1): the shared gates then exactly one
    # tracker.close. The local <prefix>-<hex> grammar guard does not apply to
    # an opaque external id - resolution is the tracker's exact read.
    from fno.tracker import active_backend_name

    if active_backend_name() != "graph":
        _done_via_seam(task_id, skip_stamp=skip_stamp, force=force, reason=reason)
        return

    # The graph-backend close is native: the door (fno backlog done) serves
    # the close-flag surface, the receipts, and the dependent cascade. A
    # direct wheel-level spelling has no leg left to run, so it names the
    # door instead of carrying a second implementation.
    typer.echo(
        "Error: the graph-backend close is served by the native door; "
        "run `fno backlog done " + (task_id or "<id>") + "`.",
        err=True,
    )
    raise typer.Exit(code=2)


def _canonical_post_close(
    node: dict,
    *,
    task_id: str,
    cascade_closed: list,
    skip_stamp: bool,
    evidence_pr_url: Optional[str],
) -> None:
    """The canonical post-close steps, shared by every close path (PR 1200).

    Stamp+graduate the plan, project the closed node and any cascade-closed
    epic parents, and fire the retro trigger. The rich completion surface
    (fno.done.cli) runs the SAME steps so closing depth never forks on which
    flags or spelling named the node.
    """
    plan_path = node.get("plan_path")
    if plan_path and not skip_stamp:
        # Stamp the plan shipped (against the evidencing PR) THEN graduate, so a
        # plan that never went through target's ship gate still records the ship
        # rather than getting a graduate no-op ().
        _stamp_and_graduate_plan(
            plan_path,
            url=evidence_pr_url,
            session_id=node.get("session_id"),
        )

    # Project the closed node + any cascade-closed epic parents onto their plans
    # (forward-only, stamps done_at) AFTER the stamp above, so the primary plan's
    # shipped_at is written before its done_at (never done-before-shipped). The
    # primary is already `done` here, so this is a no-op on it and its real job
    # is the cascade-closed epic parents that _stamp_and_graduate_plan skips.
    # --skip-stamp suppresses ALL plan writes, projection included.
    if not skip_stamp:
        _project_plans_from_graph([task_id, *cascade_closed])


# -- reconcile (close merged-PR drift) --
# `done` and `reopen` are a deliberate gate inversion (`done` refuses when no
# referenced PR is merged, `reopen` refuses when one IS); both verbs are
# native now, owned by the door's lifecycle module.


@cli.command("reconcile-findings", hidden=True)
def cmd_reconcile_findings(
    apply: bool = typer.Option(
        False,
        "--apply",
        help="Close the addressed nodes (default: dry-run, mutate nothing).",
    ),
) -> None:
    """Close phantom retro-triage nodes a later commit already addressed.

    Retro files a node from a reviewer comment; on an autonomously-merged,
    bot-reviewed PR the fix often lands after the comment without the thread
    being resolved or replied to, so the node is filed for work already done
    (). This re-runs the harvest addressed-detection against each open
    retro node's source PR and closes the ones now addressed - the
    reconciliation counterpart to the harvest-side suppression. Dry-run by
    default; ``--apply`` closes via ``fno backlog done --note``. A PR whose
    review state can't be read is skipped, never closed on uncertainty.
    """
    import subprocess

    from fno.retro.reconcile_findings import scan_addressed_findings

    entries = wire_rows(path=_graph_path())
    warnings: list = []
    findings = scan_addressed_findings(entries, warnings=warnings)
    for w in warnings:
        typer.echo(w, err=True)

    if not findings:
        typer.echo("reconcile-findings: no addressed phantom retro nodes found")
        return

    for f in findings:
        typer.echo(f"{f.node_id}  PR #{f.pr_number}  comment {f.comment_id}  ({f.signal})")

    if not apply:
        typer.echo(f"\n{len(findings)} node(s) would close. Re-run with --apply to close them.")
        return

    closed = 0
    for f in findings:
        reason = (
            f"addressed on PR #{f.pr_number} ({f.signal}); retro reconcile-findings "
            f"re-check - fix landed without the thread being resolved/replied"
        )
        proc = subprocess.run(["fno", "backlog", "done", f.node_id, "--note", reason])
        if proc.returncode == 0:
            closed += 1
        else:
            typer.echo(
                f"reconcile-findings: close of {f.node_id} failed (rc={proc.returncode})",
                err=True,
            )
    typer.echo(f"reconcile-findings: closed {closed}/{len(findings)} node(s)")


# -- reprioritize --




# -- Internal helpers for intake / update (avoid circular imports) --


def _apply_claim_in_place(es, claim_id: str, *, plan_path: str, spec: dict, project: Optional[str]):
    """Bind a claimed node to the plan in place, in any state: attach the
    plan, merge the doc-declared fields, promote idea -> ready.

    The single-plan claim lane's mutator body, shared with the multi lane so
    a claim lands identically from either surface. A second copy here would
    drift, and drift on this path mints duplicate nodes.
    """
    from fno.graph._intake import (
        DEFAULT_NODE_TYPE,
        _find_node,
        _read_plan_frontmatter,
        _would_exceed_epic_depth,
        normalize_type,
        resolve_node_project_and_cwd,
    )

    # Intake has TWO lanes; the create lane reads `type` doc->graph in
    # _build_intake_node, so this one must too or the flow is a guard on
    # one of N paths. Same rule as priority below: the doc only speaks
    # when it declares a real, non-default value.
    frontmatter = _read_plan_frontmatter(plan_path) or {}
    claimed_type = normalize_type(frontmatter.get("type"))
    from fno.graph._constants import normalize_difficulty

    raw_difficulty = frontmatter.get("difficulty")
    if raw_difficulty is None and frontmatter.get("model_tier") is not None:
        # Same loss-signal as the intake lane (): the claim reads the
        # canonical key only, so a plan still spelling model_tier must hear
        # the band was dropped rather than wonder why the node has none.
        typer.echo(
            f"warning: {plan_path}: frontmatter model_tier is retired and no "
            "longer read; set difficulty: low|medium|high to carry the band",
            err=True,
        )
    for entry in es:
        if entry.get("id") != claim_id:
            continue
        entry["plan_path"] = plan_path
        entry["title"] = spec["title"]
        if raw_difficulty is not None:
            try:
                revised_difficulty = normalize_difficulty(raw_difficulty)
            except (ValueError, AttributeError, TypeError):
                # AttributeError joins ValueError: a non-string difficulty
                # dies inside strip()/lower() before the band check; warn and
                # leave the band unchanged rather than traceback mid-claim.
                typer.echo(
                    f"warning: invalid difficulty {raw_difficulty!r} "
                    "(expected one of: low, medium, high); "
                    "difficulty left unchanged",
                    err=True,
                )
            else:
                # The one canonical-write shape, shared with the native
                # update verb; the claim records every canonical write (the filed-versus-
                # revised delta counts confirmations too).
                from fno.graph._constants import write_canonical_difficulty

                write_canonical_difficulty(
                    entry,
                    revised_difficulty,
                    "blueprint",
                    datetime.now(timezone.utc).isoformat(),
                    history_on="always",
                )
        if (
            claimed_type != DEFAULT_NODE_TYPE
            and entry.get("type") != claimed_type
        ):
            # `add` and `update` both refuse a write that would make a
            # third epic level; a doc-frontmatter lane that skips the cap
            # would be the decorative guard this whole change is about.
            # It SKIPS rather than refuses, unlike those two: there the
            # operator typed `--type` and deserves a hard error, here the
            # doc is advisory and the claim itself is still valid.
            parent_node = _find_node(es, entry["parent"]) if entry.get("parent") else None
            if (
                claimed_type == "epic"
                and parent_node is not None
                and _would_exceed_epic_depth(es, {**entry, "type": "epic"}, parent_node)
            ):
                typer.echo(
                    f"warning: plan declares type: epic but {claim_id} sits "
                    f"under {parent_node['id']}; promoting it would exceed "
                    f"the epic-nesting cap - type left as "
                    f"{entry.get('type')!r}",
                    err=True,
                )
            else:
                entry["type"] = claimed_type
        if spec["deps"]:
            merged = list(dict.fromkeys([*entry.get("blocked_by", []), *spec["deps"]]))
            entry["blocked_by"] = merged
        # Only override priority if the plan supplied a non-default one.
        if spec.get("priority") and spec["priority"] != "p2":
            entry["priority"] = spec["priority"]
        if spec.get("priority") == "p0" and spec.get("blocks_everything"):
            entry["blocks_everything"] = True
        if spec.get("points") is not None:
            entry["points"] = spec["points"]
        # Backfill project/cwd when the node was created via
        # `fno backlog new` (no plan path -> no auto-scope) and is
        # now being claimed by a plan that lives in a project repo.
        # Only fills nulls; never overwrites existing values.
        if entry.get("project") is None or entry.get("cwd") is None:
            resolved_project, resolved_cwd, _ = resolve_node_project_and_cwd(
                plan_path,
                project,
                es,
            )
            if entry.get("project") is None and resolved_project:
                entry["project"] = resolved_project
            if entry.get("cwd") is None and resolved_cwd:
                entry["cwd"] = resolved_cwd
        break
    return es


def _collect_intake_paths_typer(plan_paths: list[str], from_list: Optional[str]) -> list[str]:
    """Build the path list for intake from positional args + --from."""
    paths: list[str] = []
    if from_list:
        if from_list == "-":
            import sys

            raw = sys.stdin.read()
        else:
            try:
                raw = Path(from_list).read_text()
            except OSError as e:
                typer.echo(f"Error: --from {from_list}: {e}", err=True)
                raise typer.Exit(code=1)
        for line in raw.splitlines():
            s = line.strip()
            if not s or s.startswith("#"):
                continue
            paths.append(s)
    for p in plan_paths or []:
        if "," in p and not os.path.exists(p):
            for part in p.split(","):
                part = part.strip()
                if part:
                    paths.append(part)
        else:
            paths.append(p)
    return paths


def _do_intake_multi(
    args,
    all_paths: list[str],
    *,
    roadmap_id,
    dry_run,
    allow_no_surface: bool = False,
) -> None:
    """Multi-path intake flow delegating to intake helpers."""
    from fno.graph.store import commit_rows_via_store
    from fno.graph._intake import (
        _prepare_intake,
        _build_intake_node,
        _refuse_surfaceless_intake,
        _validate_cli_deps,
    )

    cli_deps: list[str] = (
        [d.strip() for d in args.deps.split(",") if d.strip()] if args.deps else []
    )
    cli_project = getattr(args, "project", None)
    _refuse_create_on_external_backend()
    _validate_cli_deps(cli_deps, wire_rows(path=_graph_path()))

    resolved: list[dict] = []
    for raw in all_paths:
        if not os.path.exists(raw):
            resolved.append({"path": raw, "files": [], "status": "missing"})
            continue
        resolved.append({"path": raw, "files": [raw], "status": "ready"})

    concrete_files = [f for r in resolved if r["status"] == "ready" for f in r["files"]]
    if not concrete_files:
        for r in resolved:
            if r["status"] == "missing":
                typer.echo(f"warning: not found, skipped: {r['path']}", err=True)
        typer.echo(
            f"Error: nothing to intake (0 of {len(all_paths)} paths resolved)",
            err=True,
        )
        raise typer.Exit(code=4)

    preview_entries = wire_rows(path=_graph_path())
    # Rationale (8 lines): docs/architecture/graph-cli-rationale.md#do-intake-multi-11781
    from fno.graph.store import plan_path_owner_conflict

    def _unbound(f: str) -> bool:
        # plan_path_owner_conflict compares normpath-only, so a CLI-relative
        # spelling never matches a graph-stored absolute plan_path. Probe the
        # absolute spelling too - the mutator's per-file verdicts have the
        # same view, and a false refusal blames a file this run was a no-op
        # for (the reviewer's divergent-spelling batch case).
        spellings = [f]
        target = Path(f).expanduser()
        if not target.is_absolute():
            spellings.append(str(target.resolve()))
        return all(plan_path_owner_conflict(preview_entries, None, s) is None for s in spellings)

    _refuse_surfaceless_intake(
        [f for f in concrete_files if _unbound(f)],
        allow_no_surface=allow_no_surface,
    )

    if roadmap_id and not args.force_new_roadmap:
        has_roadmap = any(e.get("roadmap_id") == roadmap_id for e in preview_entries)
        if not has_roadmap:
            typer.echo(
                f"unknown roadmap_id: {roadmap_id} "
                "(use /megawalk vision.md to create a roadmap first, "
                "pass --force-new-roadmap, or omit --roadmap-id to intake to the backlog)",
                err=True,
            )
            raise typer.Exit(code=2)

    if dry_run:
        typer.echo(f"Multi-intake preview (dry-run, no changes): {len(all_paths)} paths:")
        would = 0
        for r in resolved:
            if r["status"] == "missing":
                typer.echo(f"  warning: not found, skipped: {r['path']}")
                continue
            for f in r["files"]:
                # Validate-then-preview, mirroring the single-plan dry run: a
                # plan the real run would refuse (bad priority, a cross-roadmap
                # owner conflict) or skip as already-intaked must preview as
                # exactly that, never as would-intake.
                try:
                    prep = _prepare_intake(
                        f,
                        preview_entries,
                        roadmap_id=roadmap_id,
                        cli_title=args.title,
                        cli_priority=args.priority,
                        cli_deps=cli_deps,
                        cli_points=args.points,
                        cli_project=cli_project,
                    )
                except ValueError as exc:
                    typer.echo(f"  error: would skip {f}: {exc}", err=True)
                    continue
                if prep["status"] == "already":
                    typer.echo(f'  already intaked {prep["id"]}: "{prep["title"]}"  ({f})')
                    continue
                spec = prep["node_spec"]
                if prep["status"] == "claim":
                    typer.echo(
                        f'  would claim: "{spec["title"]}"  (plan: {f})  (claims {prep["id"]})'
                    )
                    _apply_claim_in_place(
                        preview_entries, prep["id"], plan_path=f,
                        spec=spec, project=cli_project,
                    )
                    continue
                # Grow the preview graph the way the real mutator grows its
                # entries - the BUILT node, so a later duplicate of this plan
                # previews the outcome the real run would produce (already
                # intaked by this batch), not a second would-intake. Building
                # here also previews build-time refusals (invalid difficulty)
                # as would-skip, the same outcome the real run now lands.
                try:
                    preview_entries.append(_build_intake_node(spec, preview_entries))
                except ValueError as exc:
                    typer.echo(f"  error: would skip {f}: {exc}", err=True)
                    continue
                typer.echo(f'  would intake: "{spec["title"]}"  (plan: {f})')
                would += 1
        typer.echo(f"{would} plans would be intaked. Run without --dry-run to apply.")
        return

    typer.echo(f"Multi-intake {len(concrete_files)} plans:")
    tallies = {"intaked": 0, "claimed": 0, "already": 0, "invalid": 0}
    landed_projects: set[str] = set()
    new_ids: list[str] = []
    claimed_ids: list[str] = []

    def mutator(es):
        for r in resolved:
            if r["status"] != "ready":
                typer.echo(f"  warning: not found, skipped: {r['path']}")
                continue
            for f in r["files"]:
                # A refusal names ONE file: skip it with a clean per-file error
                # so the batch lands, exactly like the single-plan surface that
                # catches the same ValueError. The try covers BOTH raise sites -
                # _prepare_intake (bad priority, a cross-roadmap owner conflict)
                # and _build_intake_node (invalid difficulty frontmatter).
                # Uncaught, either aborted the whole mutator with a traceback
                # from inside the lock and persisted nothing.
                try:
                    prep = _prepare_intake(
                        f,
                        es,
                        roadmap_id=roadmap_id,
                        cli_title=args.title,
                        cli_priority=args.priority,
                        cli_deps=cli_deps,
                        cli_points=args.points,
                        cli_project=cli_project,
                    )
                except ValueError as exc:
                    tallies["invalid"] += 1
                    typer.echo(f"  error: skipped {f}: {exc}", err=True)
                    continue
                if prep["status"] == "already":
                    tallies["already"] += 1
                    typer.echo(f'  already intaked {prep["id"]}: "{prep["title"]}"  ({f})')
                    continue
                if prep["status"] == "claim":
                    # The claim lands on the existing idea node in place, the
                    # same helper the single-plan lane uses - never a fresh
                    # node beside an unclaimed idea (the duplicate the claim
                    # mechanism exists to prevent).
                    _apply_claim_in_place(
                        es,
                        prep["id"],
                        plan_path=f,
                        spec=prep["node_spec"],
                        project=cli_project,
                    )
                    tallies["claimed"] += 1
                    claimed_ids.append(prep["id"])
                    typer.echo(
                        f'  claim {prep["id"]} via {prep["claim_source"]}: "{prep["title"]}"  ({f})'
                    )
                    continue
                try:
                    node = _build_intake_node(prep["node_spec"], es)
                except ValueError as exc:
                    tallies["invalid"] += 1
                    typer.echo(f"  error: skipped {f}: {exc}", err=True)
                    continue
                es.append(node)
                tallies["intaked"] += 1
                new_ids.append(node["id"])
                typer.echo(f'  intake {node["id"]}: "{node["title"]}"  ({f})')
                if isinstance(node.get("project"), str):
                    landed_projects.add(node["project"])
        return es

    commit_rows_via_store(_graph_path(), mutator)

    # A claimed node gets the same nav-field projection the single-plan claim
    # path runs; additive and guarded so it never wedges the batch.
    if claimed_ids:
        try:
            from fno.plan._project import project_graph_nodes

            project_graph_nodes(wire_rows(path=_graph_path()), claimed_ids)
        except Exception as e:  # noqa: BLE001
            _safe_stderr_warn(f"warning: post-claim projection skipped: {e}\n")

    # Filing-time dedup net (plan ): per just-born node, warn if it
    # resembles an existing one. Fresh post-write read; non-fatal.
    try:
        from fno.graph._intake import _find_node, _warn_similar_nodes

        post_entries = wire_rows(path=_graph_path())
        for nid in new_ids:
            node = _find_node(post_entries, nid)
            if node is not None:
                _warn_similar_nodes(node, post_entries, intake_hint=True)
    except Exception as e:  # noqa: BLE001 - dedup never breaks the batch
        _safe_stderr_warn(f"warning: post-intake dedup check skipped: {e}\n")

    from fno.graph._intake import _warn_unknown_project, _list_known_projects

    known = _list_known_projects()
    for proj in sorted(landed_projects):
        _warn_unknown_project(proj, known=known)

    missing = sum(1 for r in resolved if r["status"] == "missing")
    typer.echo(
        f"\n{tallies['intaked']} newly intaked, "
        f"{tallies['claimed']} claimed, "
        f"{tallies['already']} already intaked, "
        f"{missing} skipped." + (f" {tallies['invalid']} refused." if tallies["invalid"] else "")
    )
    if tallies["intaked"] + tallies["claimed"] + tallies["already"] == 0:
        raise typer.Exit(code=4)


# collisions sub-app: file-overlap detection between plans
# ---------------------------------------------------------------------------

# ---------------------------------------------------------------------------
# supersede: record a proposed replacement until its merged PR proves coverage
# ---------------------------------------------------------------------------


_SUPERSEDE_EXAMPLE = (
    "fno backlog supersede <new> --replaces <old> "
    '--cause "<what the old node was for>" --surface <path/it/owned>'
)


@cli.command("supersede", hidden=True)
def cmd_supersede(
    new_id: str = typer.Argument(..., help="The new node ID that replaces the old"),
    replaces: str = typer.Option(..., "--replaces", help="The old node ID being superseded"),
    cause: Optional[str] = typer.Option(
        None, "--cause", help="REQUIRED. Inherited cause being replaced"
    ),
    surface: list[str] = typer.Option(
        [], "--surface", help="REQUIRED, repeatable. Repo-relative cause surface"
    ),
    reason: Optional[str] = typer.Option(None, "--reason", "-R", help="Optional human rationale"),
    force: bool = typer.Option(
        False,
        "--force",
        "-F",
        help="Supersede even if the target still has live children (orphaning them)",
    ),
) -> None:
    """Record that ``new_id`` proposes to replace ``replaces``.

    Sets the compatibility edge plus a structured evidence record on the old
    node. The edge terminals the old row's status immediately ; the
    record stays unverified until a merged PR covers every declared surface,
    which reconcile reports as receipts. Refuses if ``replaces`` still has
    live children unless ``--force`` is given; under ``--force`` the live
    children's ``parent`` is cleared so they stay dispatchable instead of
    stranding under a dead unit. Reverse with ``unsupersede``.
    """
    # The supersede write is native: the door (fno backlog supersede) owns the
    # guard ladder, the edge + record, both child releases, and the plan
    # projection. A direct wheel spelling has no leg left to run, so it names
    # the door instead of carrying a second implementation.
    typer.echo(
        "Error: the supersede write is served by the native door; "
        "run `fno backlog supersede <new> --replaces <old> ...`.",
        err=True,
    )
    raise typer.Exit(code=2)


# ---------------------------------------------------------------------------
# unsupersede: reverse a supersede (the death transition was reversible only
# by hand-editing graph.json before this; cmd_undefer clears deferred_at but
# leaves superseded_by set, so a superseded node stayed superseded)
# ---------------------------------------------------------------------------


def _relevant_exec_scope(root: str, by_id: dict) -> set[str]:
    """The node set the execution graph is compiled over, order-independent.

    A fixpoint (not a single pass) so the result never depends on graph.json
    row order: root, its transitive ``blocked_by`` ancestors, the transitive
    dependents that name any in-scope node as a blocker, and the verifier /
    evidence-producer nodes referenced by in-scope nodes (so verification and
    data edges are not silently dropped for lack of a blocked_by link).
    """
    scope: set[str] = set()
    if root in by_id:
        scope.add(root)
        stack = [root]
        while stack:  # up: transitive blocked_by ancestors
            for dep in by_id[stack.pop()].get("blocked_by") or []:
                if dep in by_id and dep not in scope:
                    scope.add(dep)
                    stack.append(dep)

    changed = True
    while changed:
        changed = False
        for nid, e in by_id.items():
            if nid in scope:
                # verifier + evidence producers referenced by an in-scope node
                v = str(e.get("verifier") or "").strip()
                if v and v in by_id and v not in scope:
                    scope.add(v)
                    changed = True
                for ev in e.get("requires_evidence") or []:
                    for src, se in by_id.items():
                        if src not in scope and ev in (se.get("produces_evidence") or []):
                            scope.add(src)
                            changed = True
                continue
            # down: a dependent of anything already in scope
            if any(dep in scope for dep in (e.get("blocked_by") or [])):
                scope.add(nid)
                changed = True
    return scope


def _exec_liveness(state: str) -> str:
    """Map a claim_status state to the ExecNode liveness enum."""
    return {
        "live": "live",
        "suspect": "unknown",
        "stale": "unknown",
        "corrupted": "unknown",
        "free": "",
    }.get(state, "")


# -- the external-backend verb classification (the sets are imported at the
# top of this module from _verb_classification.py, beside the data)


def _refuse_tracker_owned_on_external_backend(label: str) -> None:
    """The shared external-backend refusal for a tracker-owned backlog verb.

    One guard at every reachable tracker-owned entry point (the wrapper below
    installs it on the registered callback), firing before any graph read or
    write. The message names the verb and the backend."""
    from fno.tracker import active_backend_name

    backend = active_backend_name()
    if backend != "graph":
        typer.echo(
            f"fno backlog {label}: this verb owns graph state; under the "
            f"{backend} tracker backend it is refused. Track the item in the "
            f"tracker by its id.",
            err=True,
        )
        if label in _NO_GRAIN_ON_EXTERNAL_BACKEND:
            raise typer.Exit(code=TASK_NO_GRAIN_EXIT)
        raise typer.Exit(code=1)


def iter_backlog_registry():
    """The (group-label, typer-app) pairs carrying every backlog verb.
    The ONE structural list: the verb classifier, the census, and the
    classification tests all walk it; register a new sub-app beside its
    add_typer call.
    """
    return [
        (None, cli),
        ("capture", _capture_cli),
        ("batch", _batch_cli),
    ]


from fno.graph import note_cli  # noqa: E402,F401

# The node-lifecycle reversal verbs (defer, undefer, retract, unsupersede)
# answer natively from the grouped dispatcher; the Python registration
# retired with their bodies.

# The root loader runs this before Click builds the tree.
_fno_pre_dispatch = classify_backlog_verbs
