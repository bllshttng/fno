"""The `fno config` verb shell: the typer app, its lazy mounts, the writer
commands, and the doctor leaf.

The family port moved the read-side verbs (get, history, build-dir,
assert-subagent-budget, status-sinks, active-backlog, the non-codex arms of
plugin install) native to the `fno` front door; the writer stays Python until
the config-writer port, and the doctor verb answers through
fno.setup.config_doctor, the library beside its engine. This module carries
the moved remainder of cli/src/fno/config_cli.py; it deletes when the hub
does.
"""

from __future__ import annotations

import json
import sys
from typing import Any, Literal, Optional

import typer

from fno._lazy_group import make_lazy_group_cls

# Mounted as ``fno config accounts``: accounts ARE config
# (``config.accounts.records``), so the command path mirrors the config path. This
# replaced the top-level ``fno providers``, which was hidden and therefore
# discoverable by nobody; as a `fno config` subcommand it shows up in
# `fno config --help` and gets its own page.
#
# Lazy for the same reason the top-level map is: the sub-app drags in the
# provider loader (~68ms), and `fno config get` is called from shell dozens of
# times per phase. `providers` used to be a top-level LAZY_SUBCOMMANDS entry, so
# mounting it eagerly here would have moved that cost onto every `fno config`.
_LAZY_SUBCOMMANDS: dict[str, tuple[str, str] | tuple[str, str, dict[str, Any]]] = {
    "accounts": ("fno.adapters.providers.cli:cli", "Manage account records."),
    # Folded under config (unit 6). The old top-level spellings stay
    # one-release shims (fno.command_moves); these mounts are the canonical
    # homes. Lazy for the same reason `accounts` is: `fno config get` runs
    # from shell dozens of times per phase and must not pay for their imports.
    # Hidden: the menu cap is 12 and config already curates nine visible
    # commands; `fno help config --all` lists these.
    "paths": ("fno.paths_cli:app", "Path resolution helpers", {"hidden": True}),
    "route": (
        "fno.route_cli:route_app",
        "Provider route lanes: ls / set / unset / env (GLM build lane).",
        {"hidden": True},
    ),
    # The routing-inventory surface under its plan-named spelling: same app as
    # `route`, so `fno config routing init` and `fno config route init` both
    # resolve. Hidden like its sibling.
    "routing": (
        "fno.route_cli:route_app",
        "Declared model-routing inventory: init (sample) / inventory (reach).",
        {"hidden": True},
    ),
    # One local-dev install door for every plugin harness. VISIBLE on
    # purpose and as a lazy mount: this installs the footnote plugin itself,
    # from the filtered stage, in one action per harness (claude|codex|
    # opencode|agy). The codex arm stays on the Python converge engine.
    "plugin": (
        "fno.setup.plugin_install_cli:plugin_app",
        "Install your local checkout of footnote into a harness, for development.",
    ),
}

app = typer.Typer(
    help="Config inspection and diagnostics",
    cls=make_lazy_group_cls(_LAZY_SUBCOMMANDS),
)


@app.command("doctor")
def doctor(
    post_merge: bool = typer.Option(
        False,
        "--post-merge",
        help="Report this repo's post-merge config readiness (read-only).",
    ),
    review: bool = typer.Option(
        False,
        "--review",
        help="Report whether every config.review.reviewers entry can actually "
        "run in this session (read-only). Diagnostic twin of the refusal "
        "`fno do target init` performs.",
    ),
    json_out: bool = typer.Option(
        False,
        "--json",
        "-J",
        help="With --post-merge, emit the verdict as a single JSON object.",
    ),
) -> None:
    """Inspect resolved paths; flag suspicious values. Read-only.

    With ``--post-merge`` (or ``--json``), instead report whether
    ``config.post_merge.parking_lot_path`` is set for this repo - the gate the
    /fno:ship pr merged ritual needs. With ``--review``, report whether every
    configured local reviewer can run in this session. Bare ``fno config
    doctor`` runs the path diagnostic and appends a one-line post-merge
    readiness summary plus the resolved ship gates (each probe with its source,
    each reviewer with the rung it asserts).
    """
    from fno.setup.config_doctor import _report_gates, _report_review_capability, _report_state_roots
    from fno.setup.config_doctor import (
        _report_deprecated_active_backlog_mission,
        _report_deprecated_dispatch_harness,
        _report_deprecated_auto_merge,
        _report_band_routing,
        _report_harness_overlays,
        _repo_root,
        post_merge_readiness,
    )

    if review:
        raise typer.Exit(_report_review_capability(json_out=json_out))

    if post_merge or json_out:
        verdict = post_merge_readiness(_repo_root())
        if json_out:
            typer.echo(json.dumps(verdict.to_dict()))
        else:
            typer.echo(verdict.summary_line())
        raise typer.Exit(0)

    from fno.setup.doctor import run_doctor

    rc = run_doctor()
    # Every report is advisory: one that fails never crashes the diagnostic.
    # The post-merge line runs first so bare doctor shows the gap without the
    # flag (Open Question 1). One wrapper, so the next report is a name here.
    for report in (
        lambda: typer.echo(post_merge_readiness(_repo_root()).summary_line()),
        _report_gates,
        _report_state_roots,
        _report_deprecated_auto_merge,
        _report_deprecated_dispatch_harness,
        _report_deprecated_active_backlog_mission,
        _report_band_routing,
        _report_harness_overlays,
    ):
        try:
            report()
        except Exception:  # noqa: BLE001 - a report, not the diagnostic itself
            pass
    raise typer.Exit(rc)


@app.command("set")
def set_cmd(
    tokens: list[str] = typer.Argument(
        ...,
        help="Either `<key> <value>` (single set; value may contain '=') or "
        "one-or-more `key=value` pairs (atomic multi-key set).",
    ),
    local: bool = typer.Option(
        False,
        "--local/--global",
        "-l/-g",
        help="Write the project-local .fno/config.toml instead of the "
        "per-user global ~/.fno/config.toml (default global).",
    ),
) -> None:
    """Set one or more config keys in config.toml (atomic, schema-validated).

    Two forms:

      fno config set <key> <value>        # single key (value may contain '=')
      fno config set a.b=1 c.d=2 ...       # atomic multi-key set
      fno config set agents.profiles.target.lanes '["codex-luna","zai"]'

    A list takes a JSON array or `a,b`. Each value is coerced to the field's
    type and schema-validated (``config.agents.a2a.turn_ceiling`` must be >= 1),
    then written atomically under one file lock. The multi-key batch is
    all-or-nothing: any invalid value leaves the file unchanged and exits
    non-zero. A key repeated in one call uses the last value.
    """
    from fno.claims.optout_lease import ConfigSetError, set_config_values
    from fno.config.optouts import optout_release_command

    scope = "project" if local else "global"

    # Disambiguate the single-key `<key> <value>` form (so a value may itself
    # contain '=') from the multi-key `key=value` form: exactly two tokens whose
    # first carries no '=' is the legacy single set; otherwise every token must
    # be a key=value pair.
    if len(tokens) == 2 and "=" not in tokens[0]:
        items = [(tokens[0], tokens[1])]
    else:
        items = []
        for tok in tokens:
            if "=" not in tok:
                typer.echo(
                    f"error: expected key=value, got {tok!r}. Use "
                    "`fno config set <key> <value>` for a single key.",
                    file=sys.stderr,
                )
                raise typer.Exit(code=2)
            k, _, v = tok.partition("=")
            items.append((k, v))

    # A dangling accounts.active bricks every loader call (and every accounts
    # command loads first), so the pointer is validated against declared record
    # ids here rather than repaired after the fact. Scope-aware: a GLOBAL
    # pointer must name a globally declared record (a local-only id would
    # brick every other project), while a project pointer may name anything
    # the merged view resolves.
    for k, v in items:
        normalized = k[len("config."):] if k.startswith("config.") else k
        if normalized in ("accounts.active", "providers.active"):
            from fno.adapters.providers.loader import (
                known_account_ids,
                load_scope_config,
            )

            if scope == "global":
                ids = {r.id for r in load_scope_config("global").records}
            else:
                ids = known_account_ids()
            if v not in ids:
                known = ", ".join(sorted(ids)) if ids else "(no records configured)"
                typer.echo(
                    f"error: account {v!r} is not a configured record id "
                    f"(known: {known}); add the record first",
                    file=sys.stderr,
                )
                raise typer.Exit(code=1)

    try:
        results = set_config_values(items, scope=scope)
    except ConfigSetError as exc:
        typer.echo(f"error: {exc}", file=sys.stderr)
        raise typer.Exit(code=exc.exit_code) from exc

    if len(results) == 1:
        r = results[0]
        typer.echo(f"set {r.key} = {r.value} ({r.scope}: {r.path})")
    else:
        for r in results:
            typer.echo(f"set {r.key} = {r.value}")
        # Scope + path printed once (AC2-UI).
        typer.echo(f"({results[0].scope}: {results[0].path})")

    for r in results:
        if r.lease:
            scope_flag = " --local" if r.scope == "project" else ""
            release = optout_release_command(r.key, scope_flag)
            typer.echo(
                f"lease: {r.key} held by {r.lease['holder']}, expires at "
                f"{r.lease['expires_at']}; release: {release}"
            )

    _check_overridden_writes(results)

    # setting pr_watch.enabled couples to the launchd agent so enabled
    # means running. Loud on failure, never reverts config (doctor is the guard).
    for r in results:
        if r.key.endswith("pr_watch.enabled"):
            _couple_pr_watch(bool(r.value))
            break

    # The durable-grant coupling: a write leaving the standing dispatch grant
    # armed implies a live watcher, because recorded receipts are only
    # executable while something ticks. ONE-WAY on purpose: arming the grant
    # activates the agent, but revoking it never unloads - pr_watch.enabled
    # owns that switch and the watcher serves review dispatch beyond grants.
    # Subordinate to autonomy.enabled: the panic switch outranks every
    # spawner, and an autonomous merger must never be brought to life behind
    # its back. Failure is loud and never reverts the config (doctor guards).
    for r in results:
        if r.key.endswith("auto_merge.enabled") or r.key.endswith("auto_merge.grant"):
            _couple_grant_observer()
            break


def _check_overridden_writes(results: list) -> None:
    """Warn on stderr when a write succeeded on disk but a higher-precedence
    configuration layer overrides it.

    Receipt defect fix: local-over-global precedence is correct and stays, but
    `fno config set` must not silently report success on a write that is inert in
    the current project context. Stays silent when the write takes effect or when
    no higher layer overrides it.
    """
    from fno.config import _load_settings_at, load_settings, resolve_source

    # The command just rewrote a config file at the SAME declaration key; the
    # keyed cache would serve the pre-write entry without this clear.
    _load_settings_at.cache_clear()
    root = load_settings()

    from fno.config.writer import resolve_dotted

    for r in results:
        source = resolve_source(r.key)
        if source is None:
            continue
        decider, _ = source
        try:
            is_same_file = decider.resolve() == r.path.resolve()
        except OSError:
            is_same_file = decider == r.path
        if not is_same_file:
            ok, node = resolve_dotted(root, r.key.split("."))
            effective_value = node if ok else None
            if effective_value != r.value:
                target_flag = "--local" if r.scope == "global" else "--global"
                typer.echo(
                    f"warn: {r.key} set in {r.path}, but {decider} overrides it "
                    f"(value in effect: {effective_value}). Target the winning file with {target_flag}.",
                    file=sys.stderr,
                )


def _couple_pr_watch(enabled: bool) -> None:
    """Install+load (or unload) the PR-watch agent to match pr_watch.enabled."""
    try:
        from fno.pr_watch.cli import deactivate_watcher, ensure_watcher_activated
    except Exception as exc:  # noqa: BLE001 - coupling is best-effort
        typer.echo(f"pr-watch coupling unavailable: {exc}", file=sys.stderr)
        return

    if enabled:
        outcome = ensure_watcher_activated()
        if outcome == "activated":
            typer.echo("pr-watch: agent installed and loaded.")
        elif outcome == "already-running":
            typer.echo("pr-watch: agent already running.")
        else:
            # AC1-ERR: activation failed but enable stuck; surface loudly.
            typer.echo(
                f"pr-watch: WARNING enabled but activation failed ({outcome}); "
                "config stays enabled. Run `fno do pr watch install` or check `fno doctor`.",
                file=sys.stderr,
            )
    else:
        outcome = deactivate_watcher()
        if outcome == "unload-failed":
            # Inverse of the found incident: config says disabled but the agent
            # keeps ticking. Surface as loudly as the enable path - doctor's
            # liveness line stays silent once enabled=false, so this is the only
            # signal that the watcher did not actually stop.
            typer.echo(
                "pr-watch: WARNING disable set but launchctl unload failed; the "
                "agent may still be running. Check whether it is "
                "(`launchctl list | grep sh.fno.pr-watcher`), then retry the "
                "unload: `launchctl unload "
                "~/Library/LaunchAgents/sh.fno.pr-watcher.plist` "
                "(or re-run this config set, which re-fires the unload). "
                "`fno do pr watch uninstall` removes the watcher "
                "entirely - nothing measured is lost (the watermark store "
                "survives; `fno do pr watch install` restores it) - but run it "
                "only when removal is the intent, not to silence this warning.",
                file=sys.stderr,
            )
        else:
            typer.echo(f"pr-watch: agent {outcome} (disabled).")


def _couple_grant_observer() -> None:
    """Activate the watcher when a config write leaves the dispatch grant armed.

    The activation half of the grant-implies-observer coupling; revocation is
    deliberately NOT coupled (see the call site). Best-effort and loud: an
    activation failure never reverts the write, it just says so.
    """
    try:
        from fno.config import _load_settings_at, load_settings

        # Same declaration key as the write above; clear the keyed entry.
        _load_settings_at.cache_clear()
        settings = load_settings()
        am = settings.auto_merge
        armed = bool(am.enabled) and str(am.grant or "none") == "dispatch"
        if not armed or not settings.autonomy.enabled:
            return
        from fno.pr_watch.cli import ensure_watcher_activated

        outcome = ensure_watcher_activated()
        if outcome == "activated":
            typer.echo(
                "pr-watch: activated to serve the standing dispatch grant."
            )
        elif outcome == "already-running":
            pass
        else:
            typer.echo(
                f"pr-watch: WARNING dispatch grant armed but activation failed "
                f"({outcome}); config stays. Run `fno do pr watch install` or "
                "check `fno doctor`.",
                file=sys.stderr,
            )
    except Exception as exc:  # noqa: BLE001 - coupling is best-effort
        typer.echo(f"pr-watch grant coupling unavailable: {exc}", file=sys.stderr)


@app.command("unset")
def unset_cmd(
    key: str = typer.Argument(
        ..., help="Dotted config key to remove, e.g. config.auto_merge.enabled"
    ),
    local: bool = typer.Option(
        False,
        "--local/--global",
        "-l/-g",
        help="Remove from the project-local .fno/config.toml instead of the "
        "per-user global ~/.fno/config.toml (default global).",
    ),
) -> None:
    """Remove a config key, reverting it to the model default.

    The undo of ``set``: deletes the dotted key (and prunes any block the
    removal leaves empty), so the value falls back to its schema default. Since
    the revert is non-destructive there is no confirmation. An unknown key exits
    1 and changes nothing; an absent key is a clean no-op (``not set: <key>``).
    Aliased as ``fno config rm``.
    """
    from fno.claims.optout_lease import ConfigSetError, unset_config_value

    scope = "project" if local else "global"
    try:
        result = unset_config_value(key, scope=scope)
    except ConfigSetError as exc:
        typer.echo(f"error: {exc}", file=sys.stderr)
        raise typer.Exit(code=exc.exit_code) from exc

    if not result.present:
        typer.echo(f"not set: {key}")
        raise typer.Exit(0)

    typer.echo(
        f"unset {result.key} (was {result.was}); now defaults to "
        f"{result.default} ({result.scope}: {result.path})"
    )


# `fno config rm` is an alias for `unset` (Claude's Discretion #3).
app.command("rm")(unset_cmd)


# ---------------------------------------------------------------------------
# Native leaves. The read-side verbs answer through the `fno` front door
# (crates/fno/src/config_cli/); these registrations keep the command surface
# byte-identical - the group help page lists the same commands with the same
# descriptions, and `fno help config --all` still finds them. A leaf body
# below runs only when the front door did not claim the argv (a non-UTF-8
# argument is the one shape the lexical classifier forwards), and it refuses
# rather than silently answering nothing.
# ---------------------------------------------------------------------------


def _native_refusal(leaf: str) -> None:
    typer.echo(
        f"fno config {leaf} is answered natively by the fno front door; "
        "this Python registration only carries the help-page entry",
        err=True,
    )
    raise typer.Exit(code=2)


@app.command("get")
def get(
    key: str = typer.Argument(
        ...,
        help="Dotted config key, e.g. config.blueprint.max_prs_per_epic",
    ),
    json_out: bool = typer.Option(
        False,
        "--json",
        "-J",
        help="Emit {key, value, source, overrides} as one JSON object on stdout.",
    ),
) -> None:
    """Print a single resolved config value. Read-only."""
    _native_refusal("get")


@app.command("active-backlog")
def active_backlog() -> None:
    """Print the active-backlog drain reading. Full contract:
    docs/architecture/coordination.md#per-territory-team-cap"""
    _native_refusal("active-backlog")


@app.command("status-sinks")
def status_sinks(
    json_out: bool = typer.Option(
        False, "--json", "-J", help="Emit a JSON list of fanout targets for the daemon."
    ),
) -> None:
    """Resolve which projects the status-fanout supervisor should tick."""
    _native_refusal("status-sinks")


@app.command("assert-subagent-budget", hidden=True)
def assert_subagent_budget(
    width: int = typer.Option(..., "--width", "-w", help="The fan-out width the caller declares."),
    provider: Optional[str] = typer.Option(
        None,
        "--provider",
        "-P",
        help="Provider whose budget applies; defaults to the FNO_ROUTE_PROVIDER stamp.",
    ),
) -> None:
    """Refuse a declared fan-out wider than the provider's subagent budget."""
    _native_refusal("assert-subagent-budget")


@app.command("build-dir", hidden=True)
def build_dir() -> None:
    """Print the CARGO_BUILD_BUILD_DIR value: ``<base>/{workspace-path-hash}``."""
    _native_refusal("build-dir")


@app.command("history", hidden=True)
def history(
    key: Optional[str] = typer.Argument(
        None,
        help="Exact config key or dotted prefix to filter.",
    ),
    limit: int = typer.Option(50, "--limit", min=1, help="Maximum rows to print."),
    json_out: bool = typer.Option(False, "--json", "-J", help="Emit matching rows as JSONL."),
    scope: Literal["global", "project", "all"] = typer.Option(
        "all", "--scope", help="Limit rows by the config scope that was written."
    ),
) -> None:
    """Read config-write receipts from the global and project journals."""
    _native_refusal("history")
