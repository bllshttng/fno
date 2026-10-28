"""The `fno config` typer shell: the app, its lazy mounts, and the doctor leaf.

Every other leaf answers natively on the `fno` front door
(crates/fno/src/config_cli/), the writer (set, unset, rm) included; the
registrations below keep the help pages and forward. The doctor's engine
lives in the setup family (fno.setup.config_doctor), so this module stays
top-level and unmapped by the layer scan, like its config_cli.py ancestor.
"""

from __future__ import annotations

import json
from collections.abc import Callable
from pathlib import Path
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
    # `config route` / `config routing` (the provider route lanes and the
    # declared-inventory reads) answer natively: the front classifies both
    # spellings onto route-slot's config-route mode, so no Python mount
    # remains.
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
    # Typed as a list, not a bare tuple: mypy's join over the mixed lambda +
    # imported helpers collapsed to an untyped callable and strict mode read
    # the call itself as untyped.
    reports: list[Callable[[], None]] = [
        lambda: typer.echo(post_merge_readiness(_repo_root()).summary_line()),
        _report_gates,
        _report_state_roots,
        _report_deprecated_auto_merge,
        _report_deprecated_dispatch_harness,
        _report_deprecated_active_backlog_mission,
        _report_band_routing,
        _report_harness_overlays,
    ]
    for report in reports:
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
    _native_leaf("set", ["config", "set", *(["--local"] if local else []), "--", *tokens])


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
    _native_leaf("unset", ["config", "unset", *(["--local"] if local else []), "--", key])


# `fno config rm` is an alias for `unset` (Claude's Discretion #3).
app.command("rm")(unset_cmd)


# ---------------------------------------------------------------------------
# Native leaves. The read-side commands answer through the `fno` front door
# (crates/fno/src/config_cli/); these registrations keep the command surface
# byte-identical - the group help page lists the same commands with the same
# descriptions, and `fno help config --all` still finds them. A leaf body
# below runs only when the front door did not claim the argv (a non-UTF-8
# argument is the one shape the lexical classifier forwards, and a wheel-only
# environment has no compiled front on PATH at all). The body forwards to the
# front door built in this checkout when one exists - the worktree manager's
# wt_config and the hooks read config through exactly that wheel-only path -
# and refuses only where no front door exists to forward to.
# ---------------------------------------------------------------------------


def _front_door() -> Optional[Path]:
    """The compiled front door built in THIS checkout, newest first.

    A bare site-packages install has no checkout and no build, and stays on
    the refusal; a dev checkout and the CI smoke lanes build debug beside the
    source, so the forwarding path is the one they take.
    """
    here = Path(__file__).resolve()
    try:
        repo_root = here.parents[3]
    except IndexError:  # installed shallower than a dev checkout
        return None
    if not (repo_root / "crates" / "fno").is_dir():
        return None
    from fno.rust_binary import newest_runnable

    return newest_runnable(
        [
            repo_root / "crates" / "fno" / "target" / kind / "fno"
            for kind in ("release", "debug")
        ]
    )


def _native_leaf(leaf: str, argv: list[str]) -> None:
    """Forward the leaf's argv verbatim to the compiled front door: stdout,
    stderr and the exit code relay byte-exact, which is what keeps the wheel
    path inside the parity goldens' byte contract."""
    binary = _front_door()
    if binary is None:
        typer.echo(
            f"fno config {leaf} is answered natively by the fno front door; "
            "this Python registration carries the help page. Build the front "
            "door to answer here: (cd crates/fno && cargo build --bin fno)",
            err=True,
        )
        raise typer.Exit(code=2)
    import subprocess

    raise typer.Exit(code=subprocess.run([str(binary), *argv]).returncode)


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
    argv = ["config", "get", key]
    if json_out:
        argv.append("--json")
    _native_leaf("get", argv)


@app.command("active-backlog")
def active_backlog() -> None:
    """Print the active-backlog drain reading. Full contract:
    docs/architecture/coordination.md#per-territory-team-cap"""
    _native_leaf("active-backlog", ["config", "active-backlog"])


@app.command("status-sinks")
def status_sinks(
    json_out: bool = typer.Option(
        False, "--json", "-J", help="Emit a JSON list of fanout targets for the daemon."
    ),
) -> None:
    """Resolve which projects the status-fanout supervisor should tick."""
    argv = ["config", "status-sinks"]
    if json_out:
        argv.append("--json")
    _native_leaf("status-sinks", argv)


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
    argv = ["config", "assert-subagent-budget", "--width", str(width)]
    if provider is not None:
        argv.extend(["--provider", provider])
    _native_leaf("assert-subagent-budget", argv)


@app.command("build-dir", hidden=True)
def build_dir() -> None:
    """Print the CARGO_BUILD_BUILD_DIR value: ``<base>/{workspace-path-hash}``."""
    _native_leaf("build-dir", ["config", "build-dir"])


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
    argv = ["config", "history"]
    if key is not None:
        argv.append(key)
    argv.extend(["--limit", str(limit), "--scope", scope])
    if json_out:
        argv.append("--json")
    _native_leaf("history", argv)
