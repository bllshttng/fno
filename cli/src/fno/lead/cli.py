"""``fno agents lead`` - board reads and session init for the lead loop."""
from __future__ import annotations

import json
import sys
from pathlib import Path
from typing import NoReturn, Optional

import typer

lead_app = typer.Typer(
    name="lead",
    help="The lead's board: what still needs doing, and the session manifest for its loop.",
    no_args_is_help=True,
)


def _refuse(msg: str) -> NoReturn:
    typer.echo(msg, err=True)
    raise typer.Exit(2)


@lead_app.command("init")
def init_cmd(
    scope_values: list[str] = typer.Option(
        ...,
        "--scope",
        help="What this lead was promoted over. Repeat for a set of epics.",
    ),
    harness_session_id: str = typer.Option(
        "", "--harness-session-id", help="The lead's own harness session id."
    ),
    max_iterations: int = typer.Option(
        40, "--max-iterations", help="Iteration ceiling before the loop stops on Budget."
    ),
    respawn_ceiling: int = typer.Option(
        4,
        "--respawn-ceiling",
        help="Lead sessions the walk may respawn before it terminates on Budget.",
    ),
    force: bool = typer.Option(
        False, "--force", "-F", help="Replace an existing manifest."
    ),
) -> None:
    """Write this role scope's manifest, which the lead loop arms read.

    Write-once, like the target manifest; without it the stop hook allows exit
    silently (correct for a session nobody promoted). A successor init needs
    --force only when the predecessor died without stepping_down.
    """
    from fno.lead.state import (
        LeadManifestExists,
        lead_loop_enabled,
        lead_manifest_path,
        write_manifest,
    )

    # The ONE chokepoint for `config.lead.enabled`: every arm (hook shim,
    # loop-check, LeadQueue) arms on the manifest's existence, so gating the
    # manifest gates all three. The version this replaces gated zero paths.
    if not lead_loop_enabled():
        typer.echo(
            "lead: config.lead.enabled is false, so no lead is promoted. "
            "Enable it with `fno config set lead.enabled true`.",
            err=True,
        )
        raise typer.Exit(3)

    # An id-less manifest matches nobody, so it gates every session or none;
    # refuse to write a role that cannot be attributed.
    if not harness_session_id.strip():
        typer.echo(
            "lead: --harness-session-id is required. The stop hook gates the "
            "session the manifest NAMES, so an unattributable manifest roles "
            "nobody and risks holding unrelated sessions open.",
            err=True,
        )
        raise typer.Exit(2)

    # Every spelling of one role must arm the SAME file: canonicalize the set
    # AND resolve aliases, or `--scope a` arms leads/a.md while row-keyed
    # readers resolve leads/alpha.md from row.role_scope - and the
    # unpromoted-row warning stays silent, comparing through the same
    # normalization.
    from fno.agents.role import _canonical_members, canonical_scope, split_scope

    scope = canonical_scope(scope_values)
    members = split_scope(scope)
    if not members:
        typer.echo(
            "lead: --scope needs a role scope: name an epic or a project.",
            err=True,
        )
        raise typer.Exit(2)
    scope = canonical_scope(list(_canonical_members(scope)))
    from fno.rust_binary import call_binary_json
    admit = ["readiness", "--scope", scope, "--session", harness_session_id, "--ensure-goal"]
    error, _ = call_binary_json("loop", admit)
    if error:
        typer.echo(f"lead: refusing role admission for {scope!r}: {error}", err=True)
        raise typer.Exit(2)

    try:
        manifest_path = lead_manifest_path(scope)
    except ValueError as exc:
        # A path-unsafe scope member ('..' or a '/') is a refusal, not a
        # traceback: the safety check lives in lead_manifest_path.
        typer.echo(str(exc), err=True)
        raise typer.Exit(2) from exc

    try:
        fields = write_manifest(
            manifest_path,
            scope=scope,
            harness_session_id=harness_session_id,
            max_iterations=max_iterations,
            respawn_ceiling=respawn_ceiling,
            force=force,
        )
        manifest_path.with_suffix(".cancelled").unlink(missing_ok=True)
    except LeadManifestExists as exc:
        typer.echo(str(exc), err=True)
        raise typer.Exit(1) from exc
    typer.echo(f"lead: manifest written: {manifest_path}")
    typer.echo(f"fno_id: {fields['fno_id']}")
    typer.echo(f"scope:  {fields['scope']}")
    _print_settled_children(scope)
    _warn_unpromoted_row(scope)


def _print_settled_children(scope: str) -> None:
    """Print the role scope's settled children as titles, or nothing.

    A role lands on an epic whose history predates the session, and every
    board read a lead makes filters to open rows, so the done children that
    record what the epic already established are invisible exactly when they
    matter most. Titles only: no search over details, no similarity
    score. One local graph read; nothing prints when none exist or the graph
    cannot be read.
    """
    from fno.agents.role import _graph_index, split_scope

    by_id = _graph_index()
    if by_id is None:
        return
    lines = []
    for member in split_scope(scope):
        for entry in by_id.values():
            if entry.get("parent") != member:
                continue
            if entry.get("status") not in ("done", "superseded"):
                continue
            title = entry.get("title") or entry.get("id") or ""
            lines.append(f"- {title} ({entry.get('id')})")
    if not lines:
        return
    typer.echo()
    typer.echo(
        "Settled findings under this role (done or superseded children; "
        "read before the first check-in, never re-derive):"
    )
    for line in lines:
        typer.echo(line)


def _warn_unpromoted_row(scope: str) -> None:
    """Warn when the manifest is armed but the row carries no role.

    Arming and authority are separate (only `fno agents role` stamps the row),
    but the three row-keyed readers - `lead done`, `lead manifest-path`, the
    post-compact reinject hook - fail CLOSED and QUIETLY with no role, and a
    role over OTHER territory is as absent to them as none. Warn, never
    refuse: the loop arms on the FILE, which works, and warning keeps a
    legitimate arm-then-role ordering usable. An unresolvable row is an
    unanswered question, not a finding.
    """
    from fno.agents.role import (
        AGENT_UNREGISTERED,
        REGISTRY_UNREADABLE,
        calling_agent_row,
        role_scope_matches,
        role_reading,
    )

    try:
        row = calling_agent_row()
    except Exception:  # noqa: BLE001 - a warning never breaks a written manifest
        return
    if row is REGISTRY_UNREADABLE or row is AGENT_UNREGISTERED or row is None:
        typer.echo(
            "lead: warning: manifest armed, but this session resolves to no "
            "registry row, so its role cannot be checked. Run `/fno-me` to "
            "register, then have an attended shell run "
            f"`fno agents org promote <handle> --scope {scope}`.",
            err=True,
        )
        return
    handle = getattr(row, "name", "") or "<handle>"
    reading = role_reading(row)
    if reading is not None and role_scope_matches(reading.get("scope"), scope):
        return

    # Identical consequence for a missing and a mismatched role: the readers
    # key on role_scope, so other territory is as absent as none.
    consequence = (
        "`fno agents lead done` will refuse, `fno agents lead manifest-path` "
        "will exit non-zero without printing a path so the stop hook leaves "
        "LEAD_STATE_FILE unset, and the post-compact lead brief will never "
        "arrive."
    )
    if reading is None:
        typer.echo(
            f"lead: warning: manifest armed for {scope!r}, but this row "
            f"carries NO role, so {consequence} Ask an attended shell to run "
            f"`fno agents org promote {handle} --scope {scope}`.",
            err=True,
        )
        return
    typer.echo(
        f"lead: warning: manifest armed for {scope!r}, but this row's role is "
        f"{reading['label']!r}, which is not that scope. These readers key on "
        f"an EXACT role_scope, so a role over wider territory does not "
        f"satisfy them any more than a role over unrelated territory: "
        f"{consequence} Ask an attended shell to re-scope it with "
        f"`fno agents org promote {handle} --scope {scope}`.",
        err=True,
    )


@lead_app.command("done")
def done_cmd(
    scope: str = typer.Option(
        "", "--scope", help="Role scope to expire. Default: this session's own role."
    ),
) -> None:
    """Expire this role: vacate the row and clear the scope manifest.

    The departure half of the lifecycle: a lead ending its term calls it so a
    successor arms without --force. A lead that dies without it leaves an inert
    manifest (the registry row is authority).
    """
    from dataclasses import replace as _replace

    from fno.agents.role import (
        AGENT_UNREGISTERED,
        REGISTRY_UNREADABLE,
        _canonical_members,
        _derived_level,
        _locked_identity,
        calling_agent_row,
        canonical_scope,
        role_answers_to,
        emit_role_vacated,
    )
    from fno.agents.team import _graph_index, find_presiding_role
    from fno.agents.registry import TERMINAL_STATUSES as _TERMINAL_ROW_STATUSES
    from fno.agents.registry import load_registry, update_registry
    from fno.lead.state import lead_manifest_path, parse_manifest, remove_lead_manifest
    from fno.lead.state import _owner_state_root

    if scope.strip():
        # Rows store the canonical form, so the compares below must not
        # depend on member order or aliases.
        scope = canonical_scope(list(_canonical_members(scope)))
    caller = calling_agent_row()
    if caller is REGISTRY_UNREADABLE or caller is AGENT_UNREGISTERED:
        typer.echo(
            "lead: cannot verify the caller's role: this session carries an "
            "agent identity the registry does not resolve to a row, and "
            "expiring a role requires a holder. Run /fno-me or retry, or "
            "pass --scope from an attended shell.",
            err=True,
        )
        raise typer.Exit(2)

    if caller is None:
        if not scope.strip():
            typer.echo(
                "lead: an attended shell holds no role of its own; pass "
                "--scope <territory> to expire that role.",
                err=True,
            )
            raise typer.Exit(2)
        # One member of a set role is not a role: vacating nothing and
        # clearing no manifest would still print a false expiry receipt.
        try:
            rows = load_registry()
        except Exception as exc:  # noqa: BLE001 - named, never swallowed
            typer.echo(f"lead: role expire failed: {exc}", err=True)
            raise typer.Exit(1) from exc
        live = [row for row in rows if row.status not in _TERMINAL_ROW_STATUSES]
        if not any(row.role_scope == scope for row in live):
            for row in live:
                if role_answers_to(row.role_scope, scope):
                    typer.echo(
                        f"lead: refusing to expire {scope!r}: it is one member of "
                        f"{row.name}'s role over {row.role_scope!r}. Expire the "
                        f"whole role with --scope {row.role_scope}, or narrow it "
                        f"with fno agents org promote {row.name} --scope <the remaining epics>.",
                        err=True,
                    )
                    raise typer.Exit(2)
        # A named scope may still have a LIVE lead; expiring only the manifest
        # would disarm its stop-hook floor while the row reads promoted. The
        # vacate closure below resolves the holder under the lock; no live
        # holder is orphan cleanup and clears the file alone.
        holder_name = ""
    else:
        own = getattr(caller, "role_scope", None)
        if not scope.strip():
            if not own:
                typer.echo(
                    "lead: this session holds no role, so there is nothing "
                    "to expire.",
                    err=True,
                )
                raise typer.Exit(2)
            scope = own
        elif own != scope:
            rows = load_registry()
            mine = [r for r in rows if getattr(r, "role_scope", None) == scope]
            roles = [
                {"holder": r.name, "level": r.role_level, "scope": r.role_scope}
                for r in rows
                if r.status not in _TERMINAL_ROW_STATUSES and r.role_level is not None
            ]
            target_level = next((r.role_level for r in mine if r.role_level is not None), None)
            presider = find_presiding_role(
                scope, target_level or _derived_level(scope), roles, _graph_index()
            )
            has_live = any(r.status not in _TERMINAL_ROW_STATUSES for r in mine)
            member = any(role_answers_to(r.role_scope, scope) and r.role_scope != scope
                         for r in rows)
            if has_live or member or (presider or {}).get("holder") != caller.name:
                typer.echo(
                    f"lead: refusing to expire {scope!r}: this session's role is "
                    f"{own!r}, and an agent expires only its own role. Call "
                    "`fno agents lead done` with no --scope, or use an attended "
                    "shell for another territory.",
                    err=True,
                )
                raise typer.Exit(2)
        holder_name = "" if own != scope else caller.name

    # Snapshot the manifest's OWN session id BEFORE vacating: removal compares
    # it under the manifest lock, so a successor promoted mid-vacate survives.
    # Read from the file, never the row, so a resumed lead expires the manifest
    # it was armed with.
    owner_root = _owner_state_root(getattr(caller, "cwd", None))
    try:
        expired_manifest_session = (
            parse_manifest(
                lead_manifest_path(scope, state_root=owner_root)
            ).get("harness_session_id") or None
        )
    except (OSError, ValueError):
        expired_manifest_session = None

    # Vacate the row BEFORE the file, under the registry lock: a scope that
    # moved to a successor mid-call is refused here instead of disarming the
    # successor's manifest below (same order as the succession path).
    vacated = holder_name is None
    vacated_rows: list = []
    if holder_name is not None:
        attended_named = holder_name == ""

        def _vacate(rows: list) -> list:
            nonlocal vacated, vacated_rows
            if attended_named and caller is not None:
                if any(r.role_scope == scope
                       and r.status not in _TERMINAL_ROW_STATUSES for r in rows):
                    raise ValueError("a live holder promoted mid-expiry")
                return rows
            # The caller's row, matched by name AND session; a rebound name refuses.
            holder_row = None if attended_named else _locked_identity(rows, caller)
            for index, row in enumerate(rows):
                if attended_named:
                    # Attended + named scope: vacate whatever live row holds
                    # it; an orphaned scope clears the manifest alone below.
                    if (
                        row.role_scope == scope
                        and row.status not in _TERMINAL_ROW_STATUSES
                    ):
                        vacated_rows.append(row)
                        rows[index] = _replace(
                            row,
                            role_level=None,
                            role_scope=None,
                            role_grantor=None,
                        )
                        vacated = True
                elif holder_row is not None and row is holder_row and row.role_scope == scope:
                    vacated_rows.append(row)
                    rows[index] = _replace(
                        row, role_level=None, role_scope=None, role_grantor=None
                    )
                    vacated = True
                    break
            return rows

        try:
            update_registry(_vacate)
        except Exception as exc:  # noqa: BLE001 - named, never swallowed
            typer.echo(f"lead: role expire failed: {exc}", err=True)
            raise typer.Exit(1) from exc
        if not vacated and not attended_named:
            typer.echo(
                f"lead: refusing to expire {scope!r}: this row no longer holds "
                "it (the role moved or was already vacated), so the manifest "
                "on disk may belong to a successor. Re-read with "
                "`fno agents team` before expiring anything.",
                err=True,
            )
            raise typer.Exit(1)
        for vacated_row in vacated_rows:
            # The row write is the authority: this fires even if the manifest
            # removal below then fails, and never on the refusal above. One
            # event per vacated row, so a split role records every holder.
            emit_role_vacated(
                scope=scope, level=vacated_row.role_level, holder=vacated_row.name,
                holder_session=vacated_row.harness_session_id,
                grantor=vacated_row.role_grantor,
                cause="expired" if attended_named else "stepped_down",
            )

    # The session-id snapshot guards the successor race under the manifest
    # lock (ownership was proven by the locked vacate above). False means the
    # file is no longer the manifest this expiry targeted.
    if not remove_lead_manifest(
        scope, expected_harness_session_id=expired_manifest_session, state_root=owner_root
    ):
        typer.echo(
            f"lead: row vacated, but the manifest for {scope!r} could not be "
            "removed: the file no longer names the session this expiry "
            "snapshotted, so a successor promoted mid-expiry likely owns it "
            "now. Re-read with `fno agents team` before touching anything.",
            err=True,
        )
        raise typer.Exit(1)
    if not vacated:
        # No live holder: the manifest clear is the whole vacate.
        emit_role_vacated(
            scope=scope, level=None, holder=None,
            holder_session=expired_manifest_session,
            grantor=None, cause="orphan_manifest",
        )
    typer.echo(f"lead: role expired: {scope}")
    typer.echo(
        "row role: vacated; manifest: cleared"
        if vacated
        else "row role: no live holder found; manifest: cleared"
    )


def ledger_cmd(
    out: Optional[Path] = typer.Option(
        None, "--out", help="Write the page here instead of the layout table's lead page."
    ),
) -> None:
    """Render the term ledger page: every role, its territory, its nodes.

    The page assembly is the native ``lead-rundown`` command; this shell resolves
    the team and the paths. Contract: docs/architecture/lead.md.
    """
    from fno.lead.ledger import build_ledger_data, write_ledger

    try:
        path = write_ledger(build_ledger_data(), out)
    except RuntimeError as exc:
        _refuse(f"lead: {exc}")
    # The daemon's team_ledger arm greps this exact prefix
    # (crates/fno-agents/src/rundown.rs run_ledger); keep the two in step.
    typer.echo(f"lead ledger: {path}")


@lead_app.command("board")
def board_cmd(
    last_run: bool = typer.Option(
        False,
        "--last-run",
        help="Ask whether a lead walk terminated recently.",
    ),
    since: str = typer.Option("24h", "--since", help="Window for --last-run (e.g. 24h, 90m, 7d)."),
) -> None:
    """Ask whether a lead walk terminated recently (``--last-run``).

    The board read itself answers at the front door (``fno-agents board
    --user-command``). This leg keeps ``--last-run`` because its predicate,
    ``last_run_is_fresh``, is the one the PR watcher's lead wake reads.
    """
    if not last_run:
        typer.echo(
            "fno inbox board: the board answers through the fno front door; "
            "this leg serves only --last-run.",
            err=True,
        )
        raise typer.Exit(2)
    from fno.lead.state import last_run_is_fresh, parse_window
    from fno.paths import project_events_json

    try:
        window_s = parse_window(since)
    except ValueError as exc:
        typer.echo(str(exc), err=True)
        raise typer.Exit(2) from exc
    fresh = last_run_is_fresh(project_events_json(), since_s=window_s)
    typer.echo(f"last lead walk within {since}: {'yes' if fresh else 'no'}")
    raise typer.Exit(0 if fresh else 1)


@lead_app.command("escalate")
def escalate_cmd(
    scope_arg: str = typer.Argument("", help="Role scope for the verdict read."),
    stalled: str = typer.Option(
        "",
        "--stalled",
        help="Comma-separated board rows nothing is clearing. Omit to escalate the verdict itself.",
    ),
    reason: str = typer.Option(
        "NoProgress", "--reason", "-R", help="The terminal reason that triggered this."
    ),
) -> None:
    """Escalate to the presiding role or operator with work pending (AC4-HP)."""
    from fno.carveout.core import resolve_carveout_root, resolve_session_id
    from fno.lead.escalate import escalate, mail_presiding_lead, resolve_presiding_lead
    from fno.lead.state import term_state
    from fno.paths import resolve_repo_root

    ids = [part.strip() for part in stalled.split(",") if part.strip()]
    try:
        session_id = resolve_session_id(resolve_repo_root())
    except Exception:  # noqa: BLE001 - an unresolvable session never blocks the ask
        session_id = None
    # The caller's own liveness, read not asserted: unknown reads as dead
    # in the renderer, with the reason named.
    try:
        state = term_state(session_id=session_id)
        live, unknown_reason = state.live, state.unknown_reason
    except Exception as exc:  # noqa: BLE001 - escalation must still fire
        live, unknown_reason = None, f"term_state unreadable: {exc}"
    # The term verdict: the native owner resolves everything; a
    # failed read still records, "verdict unreadable".
    summary = None
    verdict_scope = None
    try:
        from fno.lead.history import HistoryUnreadable, verdict_read
        from fno.paths import event_journals

        code, out, err = verdict_read(event_journals(), scope_arg or None, as_json=True)
        if code != 0:
            raise HistoryUnreadable(f"native exit {code}: {(err or out).strip()[:200]}")
        payload = json.loads(out or "{}")
        summary, verdict_scope = payload.get("summary"), payload.get("scope")
    except Exception as exc:  # noqa: BLE001 - escalation never goes silent
        summary = f"unreadable {exc}"
    # A degraded or unknown verdict has no stalled row, so the verdict is the
    # reading. The crate mints this id too (`reading_verdict`) and refuses a
    # converging one.
    if not ids:
        ids = ["reading:verdict:" + (verdict_scope or scope_arg).replace(",", "+")]
    presiding = resolve_presiding_lead(session_id)
    try:
        outcome, qid = escalate(
            ids,
            reason=reason,
            root=resolve_carveout_root(),
            session_id=session_id,
            cwd=Path.cwd(),
            live=live,
            unknown_reason=unknown_reason,
            verdict=summary,
            scope=verdict_scope or None,
        )
    except Exception as exc:  # noqa: BLE001 - named, never swallowed
        typer.echo(f"lead: escalation failed: {exc}", err=True)
        raise typer.Exit(1) from exc
    holder = presiding["holder"] if presiding is not None else None
    mailed = holder is not None and mail_presiding_lead(holder, ids, reason, verdict=summary)
    target = f"lead:{holder}" if mailed else f"operator:{qid}"
    note = f", mailed presiding lead {holder}" if mailed else ""
    typer.echo(f"lead: {outcome}{note} {qid}", err=True)
    typer.echo(target)


@lead_app.command("drain")
def drain_cmd(
    scope: str = typer.Argument(..., help="The role scope to count."),
) -> None:
    """Is the role drained: how many scope nodes are not done or superseded.

    The walk's termination reads this, never a queue: a row with a driver
    leaves the actionable board while its work is unshipped, so the board
    answers assignment and this answers delivery. An unreadable graph exits
    nonzero on purpose - a walk that cannot see the scope must not read the
    failure as drained.
    """
    from fno.graph.store import GraphCorruptError, GraphUnreadableError, StoreUnavailable
    from fno.lead import drain_cache
    from fno.lead.scope import scope_undelivered
    from fno.tracker import active_backend_name
    from fno.tracker.metadata import ExternalMetadataUnavailable
    from fno.tracker.metadata import _graph_store_path, read_entries

    path = _graph_store_path()
    ident = drain_cache.graph_ident(path) if active_backend_name() == "graph" else None
    if ident is not None:
        cached = drain_cache.load(scope, ident)
        if cached is not None:
            typer.echo(
                json.dumps({"scope": scope, "undelivered": cached, "cached": True})
            )
            return
    try:
        entries = read_entries("lead drain", strict=True)
        undelivered = scope_undelivered(scope, entries)
    except (
        ExternalMetadataUnavailable, GraphCorruptError, GraphUnreadableError, StoreUnavailable, ValueError
    ) as exc:
        typer.echo(f"lead: drain for {scope!r} unreadable: {exc}", err=True)
        raise typer.Exit(1) from exc
    # A moved post-read identity describes bytes the count never saw - except
    # first contact: the read itself materializes the db, which moves the
    # identity from the seed-file stat to the store version with no write in
    # between, so that flip is the count's own key.
    post = drain_cache.graph_ident(path)
    materialized = (
        ident is not None and ident[0] == "json" and post is not None and post[0] == "sqlite"
    )
    if post is not None and (post == ident or materialized):
        drain_cache.store(scope, post, undelivered)
    typer.echo(json.dumps({"scope": scope, "undelivered": undelivered}))


agents_lead_app = typer.Typer(
    name="lead",
    help="The lead session manifest and escalation controls.",
    no_args_is_help=True,
)
agents_lead_app.command("init")(init_cmd)
agents_lead_app.command("done")(done_cmd)
agents_lead_app.command("escalate")(escalate_cmd)
agents_lead_app.command("drain")(drain_cmd)
agents_lead_app.command("ledger")(ledger_cmd)


def main() -> None:  # pragma: no cover - console-script shim
    sys.exit(lead_app())
