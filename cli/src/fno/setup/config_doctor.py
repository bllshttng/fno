"""`fno config doctor`: the verb, its reporters and the post-merge oracle.

The verb lives beside its engine: `run_doctor` from fno.setup.doctor is the
setup family's file, so the verb moves with it. The post-merge oracle and
the review-capability reporter ride the same surface. Callers that held
fno.config_cli.post_merge_readiness import from here now.
"""
from __future__ import annotations

from dataclasses import asdict, dataclass
from pathlib import Path
from importlib import import_module
from typing import TYPE_CHECKING, Literal, Optional, Union

import typer

if TYPE_CHECKING:
    from fno.paths import StateFile



# ---------------------------------------------------------------------------
# Post-merge config readiness oracle
#
# One pure, read-only verdict consumed by three callers: `fno config doctor
# --post-merge` (this surface), the /target preflight check, and the deferred
# launchd watcher. The rule lives here once so the three never disagree.
# ---------------------------------------------------------------------------

PostMergeStatus = Literal["ready", "unconfigured", "opted_out", "dormant", "error"]


@dataclass(frozen=True)
class PostMergeVerdict:
    """Whether the /fno:ship pr merged ritual can run for a repo, and why."""

    status: PostMergeStatus
    enabled: bool
    activity: bool
    parking_lot_path: Optional[str] = None
    project_id: Optional[str] = None
    cause: Optional[str] = None  # populated only on status == "error"
    note: Optional[str] = None  # soft advisory (e.g. project.id unset on ready)

    @property
    def is_ready(self) -> bool:
        """The pr-watch tick reads this to decide a merge dispatch (the seam
        `_noop_readiness` already fakes)."""
        return self.status == "ready"

    def to_dict(self) -> dict:
        return dict(asdict(self).items())

    def summary_line(self) -> str:
        """One human-readable line (also appended to bare `fno config doctor`)."""
        if self.status == "ready":
            line = (
                "[doctor] post-merge: ready "
                f"(parking_lot_path={self.parking_lot_path})"
            )
            return line + (f"; note: {self.note}" if self.note else "")
        if self.status == "unconfigured":
            return (
                "[doctor] post-merge: unconfigured - "
                "config.post_merge.parking_lot_path is unset; the /fno:ship pr merged "
                "prose+triage will be skipped. Set it with: fno config setup post-merge"
            )
        if self.status == "opted_out":
            return "[doctor] post-merge: opted_out (config.post_merge.enabled=false)"
        if self.status == "dormant":
            return "[doctor] post-merge: dormant (no fno activity in this repo)"
        if self.status == "error":
            return f"[doctor] post-merge: error - {self.cause}"
        return f"[doctor] post-merge: {self.status}"


def _load_repo_post_merge(repo_root: Path):
    """Parse this repo's `.fno/settings.yaml` and validate ONLY the post_merge
    (+ project) block. Returns ``(PostMergeBlock, project_id)``.

    Reads only the repo-local file (post_merge is a per-repo opt-in; a global
    parking_lot_path must not make every repo look ready). Validating just the
    post_merge block - not the whole SettingsModel - keeps the normal layered
    semantics for UNRELATED keys: a repo that sets e.g. ``config.obsidian.enabled``
    locally while supplying ``config.obsidian.vault`` globally must not be
    reported as a post-merge ``error`` (codex review on PR #511). A missing file
    is defaults; unparseable YAML or an invalid post_merge value RAISES so the
    caller maps it to ``error`` carrying the real cause. project.id is
    scaffold-and-note only, so a bad project block degrades to ``None`` rather
    than erroring the verdict.
    """
    import tomllib

    import yaml

    from fno.config import (
        PostMergeBlock,
        ProjectBlock,
        _deep_merge,
        _unwrap_config_dict,
        _worktree_local_override,
    )

    fno_dir = repo_root / ".fno"

    def _read_flat(toml_path: Path, yaml_path: Path) -> dict:
        """Read config as a FLAT dict, config.toml-first with a READ-ONLY legacy
        settings.yaml fallback. The oracle must never migrate (it is read-only,
        may run against an unmigrated repo, and test_oracle_is_read_only forbids
        writes), so it tolerates both formats without converting. A malformed
        file RAISES so the caller maps it to ``error``, never a false verdict.
        """
        if toml_path.is_file():
            return _unwrap_config_dict(
                tomllib.loads(toml_path.read_text(encoding="utf-8"))
            )
        if yaml_path.is_file():
            parsed = yaml.safe_load(yaml_path.read_text(encoding="utf-8"))
            if parsed is None:
                return {}
            if not isinstance(parsed, dict):
                raise ValueError(f"config is not a mapping: {yaml_path}")
            return _unwrap_config_dict(parsed)
        return {}

    raw: dict = _read_flat(fno_dir / "config.toml", fno_dir / "settings.yaml")

    # Per-worktree local override: layer the allowlisted key (project.id
    # since narrowed the allowlist) so this oracle agrees with
    # load_settings() and `fno config get`. Repo-local only (the local file is
    # never symlinked to canonical), which preserves the "a global
    # parking_lot_path must not make every repo look ready" guard above.
    local_toml = fno_dir / "config.local.toml"
    local_yaml = fno_dir / "settings.local.yaml"
    local_path = local_toml if local_toml.is_file() else local_yaml
    if local_path.is_file() and not local_path.is_symlink():
        try:
            if local_path.suffix == ".toml":
                local_parsed = tomllib.loads(local_path.read_text(encoding="utf-8"))
            else:
                local_parsed = yaml.safe_load(local_path.read_text(encoding="utf-8")) or {}
        except (tomllib.TOMLDecodeError, yaml.YAMLError) as exc:
            # Deliberately non-fatal, UNLIKE `_read_flat` above, which raises so
            # the caller can map it to `error`. This file only supplies the
            # allowlisted worktree-local override (project.id), so a corrupt one
            # must not take down a verdict the repo config can answer on its own.
            # It is logged rather than dropped: silently ignoring it made the
            # oracle look confident about a value it never read.
            import logging

            logging.getLogger(__name__).warning(
                "ignoring unparseable worktree-local override %s: %s", local_path, exc
            )
            local_parsed = {}
        if isinstance(local_parsed, dict):
            override = _worktree_local_override(_unwrap_config_dict(local_parsed))
            if override:
                raw = _deep_merge(raw, override)

    if not raw:
        return PostMergeBlock(), None

    pm_raw = raw.get("post_merge")
    pm_raw = pm_raw if isinstance(pm_raw, dict) else {}
    pm = PostMergeBlock.model_validate(pm_raw)  # raises on an invalid post_merge value

    project_id = None
    proj = raw.get("project")
    if isinstance(proj, dict):
        try:
            project_id = ProjectBlock.model_validate(proj).id or None
        except Exception:  # noqa: BLE001 - project.id is non-blocking; degrade to None
            project_id = None
    return pm, project_id


def _repo_has_fno_activity(repo_root: Path, project_id: Optional[str]) -> bool:
    """True if this repo ships or plans through fno, so the post-merge gap is
    reachable here. Bounded, short-circuits on the first hit, and biases to
    False (dormant) on any unreadable state - a false negative degrades to
    today's silent behavior; a false positive is the nag we are removing.
    """

    # 1. In-flight target session (cheapest: a stat). An imminent merge counts.
    try:
        if (repo_root / ".fno" / "target-state.md").is_file():
            return True
    except OSError:
        pass

    # 2. Global graph holds a node mapping this repo (by project.id or cwd).
    try:
        from fno.graph.store import (
            GraphCorruptError,
            GraphUnreadableError,
            StoreUnavailable,
            read_graph_strict,
        )
        from fno.graph.api import active_backend_name

        if active_backend_name() != "graph":
            return False  # an external backend has no local graph: dormant

        entries = read_graph_strict()
        root_str = str(repo_root.resolve())
        for node in entries:
            if not isinstance(node, dict):
                continue
            if project_id and node.get("project") == project_id:
                return True
            for key in ("_resolved_cwd", "cwd"):
                cwd = node.get(key)
                if isinstance(cwd, str) and cwd and (
                    cwd == root_str or cwd.startswith(root_str + "/")
                ):
                    return True
    except (OSError, ValueError, StoreUnavailable, GraphUnreadableError, GraphCorruptError):
        pass  # bias dormant

    return False


def post_merge_readiness(repo_root: Union[str, Path]) -> PostMergeVerdict:
    """Read-only verdict on post-merge config readiness for ``repo_root``.

    Never writes. Order: settings load -> enabled -> activity -> parking_lot_path.
    A settings-load failure is ``error`` (distinct from ``unconfigured``).
    """
    repo_root = Path(repo_root)
    try:
        pm, project_id = _load_repo_post_merge(repo_root)
    except Exception as exc:  # noqa: BLE001 - surface the real cause, never crash
        return PostMergeVerdict(
            status="error",
            enabled=True,
            activity=False,
            cause=f"{type(exc).__name__}: {exc}",
        )

    enabled = bool(pm.enabled)
    parking = pm.parking_lot_path or None

    if not enabled:
        return PostMergeVerdict(
            status="opted_out",
            enabled=False,
            activity=False,
            parking_lot_path=parking,
            project_id=project_id,
        )

    if not _repo_has_fno_activity(repo_root, project_id):
        return PostMergeVerdict(
            status="dormant",
            enabled=True,
            activity=False,
            parking_lot_path=parking,
            project_id=project_id,
        )

    if not parking:
        return PostMergeVerdict(
            status="unconfigured",
            enabled=True,
            activity=True,
            parking_lot_path=None,
            project_id=project_id,
        )

    note = (
        None
        if project_id
        else "project.id unset - ritual auto-detects; set for clean provenance"
    )
    return PostMergeVerdict(
        status="ready",
        enabled=True,
        activity=True,
        parking_lot_path=parking,
        project_id=project_id,
        note=note,
    )


def _repo_root() -> Path:
    """Git toplevel of the cwd (the repo the oracle reports on), else cwd."""
    import subprocess

    try:
        out = subprocess.run(
            ["git", "rev-parse", "--show-toplevel"],
            capture_output=True,
            text=True,
            timeout=5,
        )
        if out.returncode == 0 and out.stdout.strip():
            return Path(out.stdout.strip())
    except (OSError, subprocess.SubprocessError):
        pass
    return Path.cwd()


def _report_review_capability(json_out: bool = False) -> int:
    """`fno config doctor --review`: the diagnostic twin of the init refusal.

    Reports each configured reviewer as satisfiable, needs-operator, or
    unavailable. Those three stay distinct on purpose: a `human`-kind reviewer
    in an unattended run is correctly unsatisfiable, NOT a misconfiguration, and
    collapsing the two would teach an operator to "fix" a valid config.

    Exit 0 when every reviewer can run here, 1 otherwise. Diagnostic only - it
    never writes, and it never proposes `declare` as the remedy.
    """
    import json as _json

    from dataclasses import asdict

    from fno.config import load_settings, resolvable_reviewers
    from fno.rust_binary import command_call

    review = load_settings().review
    reviewers = list(review.reviewers or [])
    answer = command_call(
        ["review-level", "capability", "reviewers"],
        {
            "reviewers": reviewers,
            "descriptors": {
                name: asdict(d)
                for name, d in resolvable_reviewers(review.reviewer_registry).items()
            },
            "peers": list(review.peers or []),
            "peer_identity": review.peer_identity,
        },
    )
    session = answer["session"]
    verdicts = answer["reviewers"]
    peer_verdicts = answer["local_peers"]
    blocked = [r for r in verdicts if r["blocks_autonomy"]]
    peer_blocked = answer.get("local_peers_refusal") is not None

    # Tier staleness: a band no configured provider can serve, and a tier id
    # no reachability row maps, are both routing holes an operator must see
    # here rather than at dispatch. This table drifted a full model release
    # with nothing detecting it; this line is the tripwire.
    tiers = command_call(["spawn-overlay", "accounts", "benchmarks", "tiers"], {})
    dead_ids = tiers["unreachable_tier_ids"]
    empty_bands = tiers["empty_bands"]

    if json_out:
        typer.echo(
            _json.dumps(
                {
                    "harness": session["harness"],
                    "substrate": session["substrate"],
                    "attended": session["attended"],
                    "provider": session["provider"],
                    "reviewers": [
                        {
                            "name": r["name"],
                            "status": r["status"],
                            "kind": r["kind"],
                            "requires": r["requires"],
                            "asserts": r["asserts"],
                            "invocation": r["invocation"],
                            "reason": r["reason"],
                            "resolved_route": r["name"],
                        }
                        for r in verdicts
                    ],
                    "local_peers": [
                        {"name": r["name"], "status": r["status"], "reason": r["reason"]}
                        for r in peer_verdicts
                    ],
                    "local_peer_gate": (
                        "unavailable"
                        if peer_blocked
                        else "satisfiable"
                        if peer_verdicts
                        else "not-configured"
                    ),
                    "tier_staleness": {
                        "unreachable_tier_ids": dead_ids,
                        "empty_bands": empty_bands,
                    },
                }
            )
        )
        return 1 if blocked or peer_blocked else 0

    typer.echo(f"review gate: {session['describe']} attended={session['attended']}")
    if not reviewers:
        typer.echo("  config.review.reviewers is empty - no local reviewers gate.")
    else:
        for row in verdicts:
            typer.echo(f"  {row['line']}")
    if dead_ids:
        typer.echo(
            "  WARN tier staleness: ids no reachability row serves: "
            + ", ".join(dead_ids)
        )
    for harness, bands in empty_bands.items():
        typer.echo(
            f"  WARN tier staleness: {harness} cannot serve band(s): {', '.join(bands)}"
        )
    if peer_verdicts:
        state = "unavailable" if peer_blocked else "satisfiable"
        typer.echo(f"  local peer gate: {state}")
        for row in peer_verdicts:
            typer.echo(f"    {row['line']}")
    if blocked or peer_blocked:
        typer.echo("")
        typer.echo(
            "This gate is fail-closed. Change config.review.reviewers or "
            "config.review.peers; `declare` is never substituted for you."
        )
    return 1 if blocked or peer_blocked else 0


def _report_gates() -> None:
    """Print the resolved ship gates: each probe with its source, each reviewer
    with the rung it asserts.

    This is where an operator should discover that their guardrail is only
    WITNESSED - a skill gate proves the skill ran at the reviewed commit and
    claims nothing about its verdict - rather than at the stop gate after the
    work is done. Read-only: doctor reports the probes, it never runs them.

    Only the project source is visible here; a plan's own `done_probes` is
    resolved per session against its bound plan doc, which doctor has no
    business guessing at.
    """
    from dataclasses import asdict

    from fno.config import load_settings, resolvable_reviewers
    from fno.rust_binary import command_call

    settings = load_settings()
    probes = list(settings.done_probes or [])
    reviewers = list(settings.review.reviewers or [])
    if not probes and not reviewers:
        return

    typer.echo("")
    typer.echo("ship gates:")
    for cmd in probes:
        typer.echo(f"  probe (project): {cmd}")
    if not probes:
        typer.echo("  probe: none declared in config (a plan may still declare its own)")
    if reviewers:
        answer = command_call(
            ["review-level", "capability", "reviewers"],
            {
                "reviewers": reviewers,
                "descriptors": {
                    name: asdict(d)
                    for name, d in resolvable_reviewers(
                        settings.review.reviewer_registry
                    ).items()
                },
                "peers": [],
                "peer_identity": None,
            },
        )
        for row in answer["reviewers"]:
            rung = row["asserts"] or "unknown"
            typer.echo(f"  reviewer: {row['name']} - asserts {rung}{_RUNG_GLOSS.get(rung, '')}")


_RUNG_GLOSS = {
    "review-evidence": " (a reviewer ran and returned a verdict)",
    "invocation": " (the reviewer ran at the reviewed commit; no claim about its verdict)",
    "self-cert": " (nothing; satisfies the gate on its own say-so)",
}


def _state_root_selector(row: "StateFile") -> str:
    """The key that actually decided this row's root, not the one that could have.

    The table's ``selector`` names the candidates in precedence order. When a
    ``config.paths.*`` override is really in play, ``resolve_source`` says
    WHICH file set it, so the receipt cannot drift from the loader's own merge
    semantics - it is reading the loader's answer rather than re-deriving one.
    """
    from fno.config._readback import source_note

    for token in row.selector.split(","):
        key = token.strip().removeprefix("else ").strip()
        # Drop a trailing parenthetical note. Without this the registry row -
        # the only one that carries one - hands resolve_source a key ending in
        # "(Rust runtime home: FNO_AGENTS_HOME)", gets None, and silently
        # never names its deciding file. A receipt whose whole job is naming
        # the decider must not have one row that structurally cannot.
        key = key.split("(", 1)[0].strip()
        if not key.startswith("config."):
            continue
        note = source_note(key.removeprefix("config."))
        if note is not None:
            return f"{key} ({note})"
    return row.selector


def _report_state_roots() -> None:
    """Name the root each state class actually resolved, and the key that chose it.

    `fno config get review.max_rounds` answered from the canonical root while
    the gate resolver answered from the worktree. The diagnostic tool and the
    runtime disagreed BY CONSTRUCTION and nothing reported the disagreement,
    so the wrong answer stayed a returned value for as long as anyone looked
    at it. Asserting that a resolver RETURNS something proves nothing; this
    prints WHICH root it returned.

    The warning line is the cheapest half and the whole point: whenever the
    cwd-derived project root differs from the ``--git-common-dir`` canonical
    one, the two are named side by side. Read-only.
    """
    from fno.paths import STATE_FILES, resolve_canonical_repo_root, resolve_repo_root

    typer.echo("")
    typer.echo("state roots:")
    typer.echo(f"  {'class':<9}{'file':<17}{'root':<48}selector")
    for row in STATE_FILES:
        if row.resolver is None:
            resolved = "NO RESOLVER - built by hand at every call site"
        else:
            module_name, _, attr = row.resolver.rpartition(".")
            try:
                resolved = str(getattr(import_module(module_name), attr)())
            except Exception as exc:  # noqa: BLE001 - a receipt, not the resolver
                resolved = f"unresolvable ({type(exc).__name__})"
        # One space minimum: a resolved path longer than the column must not
        # run into the selector and read as one token.
        typer.echo(
            f"  {row.root_class:<9}{row.filename:<17}{resolved:<48} "
            f"{_state_root_selector(row)}"
        )

    worktree_root = resolve_repo_root().resolve()
    canonical_root = resolve_canonical_repo_root().resolve()
    if worktree_root != canonical_root:
        typer.echo(
            f"  WARNING: the project root resolved from cwd ({worktree_root}) differs\n"
            f"           from the canonical root ({canonical_root}). Project config is\n"
            "           read from the canonical root, so a key set only in this\n"
            "           worktree's .fno/settings.yaml is NOT what the loader served,\n"
            "           and a gate key set only in canonical reads its shipped default\n"
            "           for any resolver that stays on the worktree root."
        )


def _deprecated_dispatch_file_rows():
    """One walk over the config chain for both deprecated-[dispatch] advisories.

    Yields ``(path, scope_flag, scope, dispatch)`` per parseable file carrying a
    ``[dispatch]`` block, either flat or config-wrapped; ``scope`` is the dict
    the canonical keys live in. Reads the raw candidate chain (not the merged
    model) so each file is named individually - the merged model cannot tell
    them apart, which is exactly the home-vs-project confusion the advisories
    exist to end.
    """
    from fno.config import _candidate_paths, _global_settings_path, _load_raw

    global_dir = _global_settings_path().parent
    for candidate in _candidate_paths():
        if not candidate.is_file():
            continue
        parsed, ok = _load_raw(candidate)
        if not ok:
            continue
        dispatch = parsed.get("dispatch")
        if isinstance(dispatch, dict):
            scope = parsed
        else:
            wrapped = parsed.get("config")
            dispatch = wrapped.get("dispatch") if isinstance(wrapped, dict) else None
            scope = wrapped if isinstance(dispatch, dict) else None
        if not isinstance(dispatch, dict):
            continue
        # Migration commands must target THE SAME FILE the warning names:
        # `fno config set/unset` defaults to the global scope, so a project
        # file needs --local or the operator edits the wrong file.
        scope_flag = "" if candidate.parent == global_dir else " --local"
        yield candidate, scope_flag, scope or {}, dispatch


def _report_deprecated_auto_merge() -> None:
    """Name every config file still setting the deprecated ``dispatch.auto_merge``.

    The migration arm of : the alias keeps old files working for one
    release, and this line tells the operator WHICH file to move.
    """
    for candidate, scope_flag, scope, legacy in _deprecated_dispatch_file_rows():
        if "auto_merge" not in legacy:
            continue
        canonical = scope.get("auto_merge")
        if isinstance(canonical, dict) and "grant" in canonical:
            # Canonical wins in this file (`_alias_am_grant` refuses to fold), so
            # the legacy line is INERT. Never print its fold value as a migration
            # target: telling the operator to `config set auto_merge.grant
            # dispatch` on a file whose canonical grant is "none" would arm
            # unattended merge on a project they just disarmed.
            typer.echo(
                f"warn: {candidate} still sets the deprecated `dispatch.auto_merge`, "
                "but a canonical `auto_merge.grant` in the same file masks it "
                "(the file reads as the canonical value). Remove the legacy line.\n"
                f"      Migrate: fno config unset dispatch.auto_merge{scope_flag}"
            )
            continue
        reads_as = "dispatch" if legacy.get("auto_merge") is True else "none"
        typer.echo(
            f"warn: {candidate} sets the deprecated `dispatch.auto_merge`.\n"
            f"      It reads as `auto_merge.grant = \"{reads_as}\"` for one release.\n"
            f"      Migrate: fno config set auto_merge.grant {reads_as}{scope_flag} && "
            f"fno config unset dispatch.auto_merge{scope_flag}"
        )


def _report_deprecated_active_backlog_mission() -> None:
    """Name every config file still setting the ignored ``active_backlog.mission``.

 retirement: the mission-scoped drain resolves one target per ACTIVE
    mission (an epic with ``mission_active=true`` in the graph), so this key
    selects nothing. Parseable for one release; this line names the live axis.
    """
    from fno.config import _candidate_paths, _global_settings_path, _load_raw

    global_dir = _global_settings_path().parent
    for candidate in _candidate_paths():
        if not candidate.is_file():
            continue
        parsed, ok = _load_raw(candidate)
        if not ok:
            continue
        block = parsed.get("active_backlog")
        if not isinstance(block, dict):
            wrapped = parsed.get("config")
            block = wrapped.get("active_backlog") if isinstance(wrapped, dict) else None
        if not isinstance(block, dict):
            continue
        mission = block.get("mission")
        if not isinstance(mission, str) or not mission.strip():
            continue
        scope_flag = "" if candidate.parent == global_dir else " --local"
        typer.echo(
            f"warn: {candidate} sets `active_backlog.mission`, which the "
            "mission-scoped drain IGNORES (missions are per-epic graph state, "
            "not config).\n"
            "      Live axis: `fno backlog advance --epic <id>` activates a "
            "mission; `--stop` retires it.\n"
            f"      Remove the ignored key: fno config unset active_backlog.mission{scope_flag}"
        )


def _report_deprecated_dispatch_harness() -> None:
    """Name every config file still setting the deprecated ``dispatch.harness``.

    The stage table (``agents.profiles.<command>.provider``) is the home for the
    harness axis; this key reads as the fallback rung beneath it for one
    release. Same contract as :func:`_report_deprecated_auto_merge`: name the
    file, print the exact migration, and when a same-file stage-table provider
    already masks the legacy key, say so instead of printing a migration that
    would re-arm a value the file overrode.
    """
    for candidate, scope_flag, scope, legacy in _deprecated_dispatch_file_rows():
        if "harness" not in legacy:
            continue
        raw_agents = scope.get("agents")
        agents = raw_agents if isinstance(raw_agents, dict) else {}
        raw_profiles = agents.get("profiles")
        profiles = raw_profiles if isinstance(raw_profiles, dict) else {}
        raw_target = profiles.get("target")
        target_profile = raw_target if isinstance(raw_target, dict) else {}
        if "provider" in target_profile:
            typer.echo(
                f"warn: {candidate} still sets the deprecated `dispatch.harness`, "
                "but a stage-table `agents.profiles.target.provider` in the same "
                "file masks it (the file reads as the stage-table value). "
                "Remove the legacy line.\n"
                f"      Migrate: fno config unset dispatch.harness{scope_flag}"
            )
            continue
        reads_as = str(legacy.get("harness") or "").strip() or "claude"
        typer.echo(
            f"warn: {candidate} sets the deprecated `dispatch.harness`.\n"
            "      It reads as the fallback harness rung for one release, below "
            "the stage table.\n"
            f"      Migrate: fno config set agents.profiles.target.provider {reads_as}{scope_flag} && "
            f"fno config unset dispatch.harness{scope_flag}"
        )


def _report_band_routing() -> None:
    """Say when band routing is routing nothing, and why. Silent once any
    command's slot would take a lane; ``model_routing.roles`` is a DIFFERENT
    axis and reads as already on."""
    from fno import route_resolve

    inventory = route_resolve.resolve_inventory()
    read_commands = route_resolve.slot_commands()
    # Silent once any command's slot would take a lane: routing is armed, and
    # the unconfigured commands are a per-command choice, not a dead router.
    empty_commands = [
        command for command in read_commands
        if not str(route_resolve.slot_states(
            command
        ).get("would_take", "")).startswith(f"agents.profiles.{command}.lanes")
    ]
    if len(empty_commands) < len(read_commands):
        return
    settings = None
    try:
        from fno.config import load_settings

        settings = load_settings()
        roles = getattr(getattr(settings, "model_routing", None), "roles", None)
    except Exception:  # noqa: BLE001 - the note is a hint on top of the line
        roles = None
    routing = getattr(settings, "routing", None)
    declared_count = len(getattr(routing, "models", None) or [])
    typer.echo(
        f"band routing inactive: config.routing.models declares {declared_count} "
        "row(s), and no command slot has a lane that resolves: "
        f"{', '.join(empty_commands)}\n"
        f"      The built-in rows keep a tier request answerable: "
        f"{', '.join(sorted(inventory.rows)) or 'none'}\n"
        "      Declare [[routing.models]] rows plus [agents.profiles.<command>] "
        "lanes to activate "
        "(docs/architecture/role-based-model-routing.md)"
        + (
            "\n      Note: model_routing.roles is set, but it routes by ROLE, "
            "not by band."
            if roles
            else ""
        )
    )


def _report_harness_overlays() -> None:
    """Name the (config rung, harness) pairs a scalar cannot serve.

    The spawn seam degrades open on config values by design, so this readout
    is where an operator hears a value will be skipped on some harness.
    Resolution rides the spawn-overlay command (the spawn seam's own rungs);
    silent when every pair maps.
    """
    from fno.agents.harnesses import READABLE_PROVIDERS
    from fno.agents.spawn_defaults import _overlay_payload
    from fno.agents.spawn_overlay_client import (
        SpawnOverlayUnavailable,
        spawn_overlay_call,
    )
    from fno.config import load_settings

    try:
        agents = load_settings().agents
    except Exception:  # noqa: BLE001 - an unreadable config reads as absent
        return
    defaults = agents.defaults
    rows: list = [(v, p) for v, p in (getattr(agents, "profiles", None) or {}).items()]
    rows.append(("", None))
    seen: set = set()
    for command, prof in rows:
        try:
            answer = spawn_overlay_call(
                {
                    "kind": "overlay",
                    "verb": command,
                    "defaults": _overlay_payload(defaults),
                    "profile": _overlay_payload(prof) if prof is not None else None,
                    "harnesses": list(READABLE_PROVIDERS),
                }
            )
        except SpawnOverlayUnavailable as exc:
            typer.echo(f"config doctor: harness-overlay readout unavailable: {exc}")
            return
        if answer.get("refusal"):
            # The command's own guards refused the config (unknown harness
            # key, a ranking field in an overlay); that IS the finding.
            typer.echo(answer["refusal"])
            return
        # The carry tables died with the spawn axes: a config
        # permission_mode/effort value is inert now, and the one honest
        # readout names it instead of probing a mapper that is gone.
        for harness, effective in (answer.get("effective_by_harness") or {}).items():
            for name in ("permission_mode", "effort"):
                entry = effective.get(name)
                if not entry:
                    continue
                value, rung = entry["value"], entry["rung"]
                # One line per unique (rung, field): every harness and command
                # resolves the same scalar beneath it, and the defect is one.
                key = (rung, name, value)
                if key in seen:
                    continue
                seen.add(key)
                typer.echo(
                    f"config.{rung}.{name} = {value!r} is inert on spawn: the "
                    "spawn axis is retired and the lane template owns the "
                    "launch line"
                )


