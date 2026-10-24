"""Typer sub-app for fno config accounts commands.

Phase 02 of the provider rotation substrate.
Provides: list, show, add, test, use, remove.

Phase 03 will wire in staging.stage(record) inside the `add` command.
"""
from __future__ import annotations

import os
from pathlib import Path
from typing import TYPE_CHECKING, NamedTuple, Optional

import typer

from fno.adapters.providers.loader import (
    load_providers,
)
from fno.adapters.providers.model import (
    ProviderConfigError,
)

if TYPE_CHECKING:
    from fno.adapters.providers.model import ProvidersConfig

cli = typer.Typer(name="accounts", help="Manage account records and active selection.")


def _resolve_home(env: dict[str, str] | None = None) -> Path:
    """Return HOME from env override or os.environ."""
    if env and "HOME" in env:
        return Path(env["HOME"])
    return Path(os.environ.get("HOME", str(Path.home())))


def _resolve_cwd(env: dict[str, str] | None = None) -> Path:
    """Return PWD from env override or os.getcwd()."""
    if env and "PWD" in env:
        return Path(env["PWD"])
    return Path(os.environ.get("PWD", os.getcwd()))


def _get_repo_root() -> Path:
    """Derive repo_root from environment (respects test isolation)."""
    return _resolve_cwd()


def _load(scope: str = "global") -> "ProvidersConfig":
    """Load providers config; error on validation failure."""
    repo_root = _get_repo_root()
    try:
        return load_providers(repo_root=repo_root)
    except ProviderConfigError as exc:
        typer.echo(f"error: {exc}", err=True)
        raise typer.Exit(1)


# ---------------------------------------------------------------------------
# list
# ---------------------------------------------------------------------------

@cli.command("list")
def list_forward(
    json_output: bool = typer.Option(
        False, "--json", "-J", help="Emit a JSON array of record rows (Connections UI)."
    ),
) -> None:
    """List all configured accounts, marking the active one with *.

    The human listing always carries an identity column: the ``.active`` stamp
    says who PUT a credential in the slot, not who it serves, so the binding
    verdict and doctor's per-record problems ride the row.
    """
    _forward_accounts("list", *(["--json"] if json_output else []))


# ---------------------------------------------------------------------------
# usage (quota-aware dispatch)
# ---------------------------------------------------------------------------



@cli.command("usage")
def usage_forward(
    refresh: bool = typer.Option(
        False, "--refresh", help="Force a fresh probe (bypass the TTL cache)."
    ),
    json_output: bool = typer.Option(
        False, "--json", "-J", help="Emit a one-line JSON object keyed by provider id."
    ),
) -> None:
    """Show per-account rate-limit windows: used % and reset time."""
    _forward_accounts("usage", *(["--refresh"] if refresh else []), *(["--json"] if json_output else []))


@cli.command("window")
def window_forward(
    json_output: bool = typer.Option(
        False, "--json", "-J", help="Emit one JSON object keyed by account id."
    ),
    warn_seconds: float = typer.Option(
        None, "--warn-seconds",
        help="Emit provider_window_closing under this many seconds to close "
             "(default 1800). Once per window.",
    ),
) -> None:
    """Where each account's rolling usage window is, without probing anything."""
    _forward_accounts(
        "window",
        *(["--json"] if json_output else []),
        *(["--warn-seconds", str(warn_seconds)] if warn_seconds is not None else []),
    )


# ---------------------------------------------------------------------------
# required-bot-check
# ---------------------------------------------------------------------------


@cli.command("required-bot-check")
def required_bot_check_forward(
    json_output: bool = typer.Option(
        False, "--json", "-J", help="Emit a JSON list of exhausted required-bot providers."
    ),
) -> None:
    """Warn (pre-promise) if a required review bot's provider is out of quota."""
    _forward_accounts("required-bot-check", *(["--json"] if json_output else []))


# ---------------------------------------------------------------------------
# show
# ---------------------------------------------------------------------------

@cli.command("show")
def show_forward(
    provider_id: str = typer.Argument(...),
    print_binding: bool = typer.Option(
        False,
        "--print-binding",
        help="Machine mode: the account's LAUNCH BINDING as KEY=VALUE lines "
        "(ACCOUNT_ID, LANE, CLAUDE_CONFIG_DIR when the lane has one). "
        "Deliberately excludes the record's env/credentials: a re-entry "
        "resolver consumes this and must never carry a secret.",
    ),
) -> None:
    """Show all fields of a single account record."""
    _forward_accounts("show", provider_id, *(["--print-binding"] if print_binding else []))


def _forward_accounts(*args: str) -> None:
    """Transport only: `fno-agents spawn-overlay accounts <args>` owns the command."""
    import subprocess

    from fno.rust_binary import resolve_binary

    binary = resolve_binary()
    if binary is None:
        typer.echo("error: the fno-agents binary was not found; set FNO_AGENTS_BIN", err=True)
        raise typer.Exit(127)
    raise typer.Exit(subprocess.run([str(binary), "spawn-overlay", "accounts", *args]).returncode)


# ---------------------------------------------------------------------------
# add
# ---------------------------------------------------------------------------

@cli.command("add")
def add_forward(
    provider_id: str = typer.Argument(..., help="Unique provider id (lowercase alphanumeric + hyphens)"),
    harness_name: str = typer.Option(
        ...,
        "--harness",
        "-H",
        help="claude|codex|opencode|agy|trae|openclaw|hermes",
    ),
    auth: str = typer.Option(..., "--auth", "-a", help="oauth_dir|api_key|managed"),
    credentials_source: Optional[Path] = typer.Option(None, "--credentials-source"),
    env: list[str] = typer.Option([], "--env", help="KEY=VALUE pairs for api_key auth"),
    route: Optional[str] = typer.Option(
        None,
        "--route",
        help="provider/model route used as the api_key credential source",
    ),
    priority: int = typer.Option(100, "--priority", "-p"),
    name: Optional[str] = typer.Option(None, "--name"),
    account_id: Optional[str] = typer.Option(None, "--account-id"),
    scope: str = typer.Option("global", "--scope", "-s", help="global|project"),
    force: bool = typer.Option(False, "--force", "-F", help="Overwrite existing record"),
) -> None:
    """Add a new account record. Refuses to overwrite without --force."""
    argv = [
        "add", provider_id,
        "--harness", harness_name, "--auth", auth,
        "--priority", str(priority), "--scope", scope,
    ]
    if credentials_source is not None:
        argv += ["--credentials-source", str(credentials_source)]
    for pair in env:
        argv += ["--env", pair]
    for flag, value in (("--route", route), ("--name", name), ("--account-id", account_id)):
        if value is not None:
            argv += [flag, value]
    if force:
        argv.append("--force")
    _forward_accounts(*argv)


# ---------------------------------------------------------------------------
# register (managed credential store, US1)
# ---------------------------------------------------------------------------

@cli.command("register")
def register_provider(
    provider_id: str = typer.Argument(..., help="Unique account id (lowercase alphanumeric + hyphens)"),
    harness_name: str = typer.Option("claude", "--harness", "-H", help="claude|codex"),
    priority: int = typer.Option(100, "--priority", "-p"),
    name: Optional[str] = typer.Option(None, "--name"),
    config_dir: Optional[str] = typer.Option(
        None,
        "--config-dir",
        help=(
            "Register a per-account CLAUDE_CONFIG_DIR account instead of "
            "a shared-slot managed snapshot. Point at a full second login in its "
            "own dir (e.g. ~/.claude-alt sharing projects/plugins/settings with "
            "~/.claude via symlinks). This is the verified-correct multi-account "
            "mechanism that bills the right account; `spawn --account <id>` then "
            "injects CLAUDE_CONFIG_DIR. Skips the shared-slot snapshot."
        ),
    ),
    scope: str = typer.Option("global", "--scope", "-s", help="global|project"),
) -> None:
    """Register a managed (shared-slot) or config-dir account.

    Default (no --config-dir): snapshot the CURRENT ~/.claude login into a
    managed account store. Sign into the account first (`claude /login`), then
    run this; it captures the live credential into ``~/.fno/providers/<id>/``
    and writes an ``auth: managed`` record.

    With --config-dir <path>: register an account that lives in its OWN config
    dir (a full second login). No shared-slot snapshot - the dir IS the account.
    """
    argv = ["register", provider_id, "--harness", harness_name, "--priority", str(priority), "--scope", scope]
    for flag, value in (("--name", name), ("--config-dir", config_dir)):
        if value is not None:
            argv += [flag, value]
    _forward_accounts(*argv)


# ---------------------------------------------------------------------------
# pick (launch-time headroom picking): `fno-agents spawn-overlay accounts pick`
# owns it; the function and the command here are transports.
# ---------------------------------------------------------------------------

PICK_EXIT_NO_HEADROOM = 3
PICK_EXIT_NO_CANDIDATE = 4
PICK_EXIT_NOT_ARMED = 5


class PickVerdict(NamedTuple):
    """The picker's answer plus everything needed to explain it.

    ``candidates`` is every considered id with its headroom or the reason it was
    not launchable, so the receipt is readable whether or not the pick succeeded
    - fail-open must not become fail-silent.
    """

    account: Optional[str]
    reason: str
    candidates: list[tuple[str, str]]
    exit_code: int


def pick_account(
    *,
    combo_name: Optional[str] = None,
    exclude: tuple[str, ...] = (),
    if_armed: bool = False,
) -> PickVerdict:
    """Transport only: `fno-agents spawn-overlay accounts pick --json` owns the pick.

    Raises ``CommandUnavailable`` when the binary is missing or answers no
    verdict; both callers treat any exception as "pick unavailable".
    """
    import json
    import subprocess

    from fno.rust_binary import CommandUnavailable, find_dev_binary, resolve_binary

    binary = find_dev_binary() or resolve_binary()
    if binary is None:
        raise CommandUnavailable("the fno-agents binary was not found; set FNO_AGENTS_BIN")
    argv = [str(binary), "spawn-overlay", "accounts", "pick", "--json"]
    if combo_name:
        argv += ["--combo", combo_name]
    for account in exclude:
        argv += ["--exclude", account]
    if if_armed:
        argv.append("--if-armed")
    # The pick probes each candidate's usage over the network.
    try:
        proc = subprocess.run(argv, capture_output=True, text=True, timeout=120)
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise CommandUnavailable(f"fno-agents accounts pick failed: {exc}") from exc
    try:
        out = json.loads(proc.stdout)
        return PickVerdict(
            out["account"],
            out["reason"],
            [(c["id"], c["state"]) for c in out["candidates"]],
            proc.returncode,
        )
    except (ValueError, KeyError, TypeError) as exc:
        raise CommandUnavailable(
            f"fno-agents accounts pick exited {proc.returncode}: {proc.stderr.strip()[:200]}"
        ) from exc


@cli.command("pick")
def pick_forward(
    combo: Optional[str] = typer.Option(
        None, "--combo", help="Combo to pick from (default: config.providers.active_combo)."
    ),
    exclude: list[str] = typer.Option(
        [], "--exclude", help="Account id to skip (repeatable, e.g. one just seen exhausted)."
    ),
    json_output: bool = typer.Option(
        False, "--json", "-J", help="Emit the full verdict as one JSON object."
    ),
    print_env: bool = typer.Option(
        False, "--print-env", help="Emit the picked account's full env overlay instead of the bare id."
    ),
    if_armed: bool = typer.Option(
        False, "--if-armed", help="Decline with exit 5 unless providers.quota.pick_on_launch is true."
    ),
) -> None:
    """Print the account to launch on: the first with headroom footnote can use."""
    argv = ["pick", *(["--combo", combo] if combo else [])]
    for account in exclude:
        argv += ["--exclude", account]
    argv += [flag for flag, on in (("--json", json_output), ("--print-env", print_env),
                                   ("--if-armed", if_armed)) if on]
    _forward_accounts(*argv)


# ---------------------------------------------------------------------------
# doctor (store integrity)
# ---------------------------------------------------------------------------


@cli.command("doctor")
def doctor_providers(
    json_output: bool = typer.Option(
        False, "--json", "-J", help="Emit a JSON array of findings."
    ),
) -> None:
    """Report the managed store's real condition. Exits non-zero on any problem.

    Read-only. Checks each record for a credential another account already
    holds and a config_dir that holds no login;
    then checks each CLI's slot for taint. Usable as a check in a script.
    """
    _forward_accounts("doctor", *(["--json"] if json_output else []))


# ---------------------------------------------------------------------------
# reconcile-slot (the taint's missing clearer, and out-of-band /login adoption)
# ---------------------------------------------------------------------------


@cli.command("reconcile-slot")
def reconcile_slot_cmd(
    harness_name: str = typer.Argument(
        "claude", help="claude (the only CLI with a shared-slot principal endpoint)"
    ),
) -> None:
    """Prove who holds a CLI's shared credential slot, and repair the store to match.

    Deliberately NOT a `clear-taint` command. It reads the live slot credential,
    resolves its principal, and acts only when that principal binds to exactly
    one registered account - then it refreshes that account's snapshot, stamps
    it active, and clears any taint. Anything less than proof (endpoint
    unreachable, unregistered principal, two accounts fingerprinted alike)
    writes nothing and exits non-zero naming which way it failed.

    This is also how a manual `claude /login` gets adopted: that path writes the
    Keychain directly and tells footnote nothing, so the stamp is the state
    known to drift, and the live credential is the only thing that can correct it.
    """
    _forward_accounts("reconcile-slot", harness_name)


# ---------------------------------------------------------------------------
# test
# ---------------------------------------------------------------------------

@cli.command("test")
def test_forward(
    provider_id: str = typer.Argument(...),
    smoke: bool = typer.Option(False, "--smoke", help="Attempt a real CLI invocation (costs quota)"),
) -> None:
    """Validate account config: record lookup + binary on PATH + credentials_source exists."""
    _forward_accounts("test", provider_id, *(["--smoke"] if smoke else []))


# ---------------------------------------------------------------------------
# use
# ---------------------------------------------------------------------------

@cli.command("use")
def use_forward(
    provider_id: str = typer.Argument(...),
    scope: str = typer.Option("project", "--scope", help="project|global"),
) -> None:
    """Set the active account (default scope: project)."""
    _forward_accounts("use", provider_id, "--scope", scope)


# ---------------------------------------------------------------------------
# remove
# ---------------------------------------------------------------------------

@cli.command("remove")
def remove_forward(
    provider_id: str = typer.Argument(...),
    force: bool = typer.Option(False, "--force", "-F", help="Allow removing the active provider"),
    scope: str = typer.Option("global", "--scope", help="global|project"),
) -> None:
    """Remove an account record. Refuses to remove the active account without --force."""
    _forward_accounts("remove", provider_id, "--scope", scope, *(["--force"] if force else []))


# ---------------------------------------------------------------------------
# combos
# ---------------------------------------------------------------------------

@cli.command(
    "combos",
    context_settings={"allow_extra_args": True, "ignore_unknown_options": True, "help_option_names": []},
)
def combos_forward(ctx: typer.Context) -> None:
    """Manage account combos (named ordered lists with rotation strategies)."""
    _forward_accounts("combos", *ctx.args)


benchmarks_cli = typer.Typer(
    name="benchmarks",
    help="Cache + view the OpenRouter coding benchmark snapshot (routing source of truth).",
    no_args_is_help=True,
)
# Hidden, not removed: a cache-refresh for OpenRouter's coding benchmarks is a
# maintenance chore, not something an operator reaches for at the menu. It stays
# fully invocable (`fno config accounts benchmarks refresh`, `fno help config --all`)
# and yields its advertised slot to `pick` / `doctor`, which are.
cli.add_typer(benchmarks_cli, name="benchmarks", hidden=True)


@benchmarks_cli.command("refresh")
def benchmarks_refresh() -> None:
    """Fetch the OpenRouter coding benchmarks and cache them atomically.

    Fails loud (non-zero) on any network/auth error, never leaving a truncated
    snapshot. Meant to run manually at a fortnightly cadence; dispatch-time tier
    resolution reads the cache, never the network.
    """
    from fno.adapters.providers.benchmarks import BenchmarkError, refresh
    from fno.paths import benchmarks_json

    try:
        snapshot = refresh()
    except BenchmarkError as exc:
        typer.echo(f"benchmarks refresh: {exc}", err=True)
        raise typer.Exit(code=1)
    typer.echo(
        f"refreshed {len(snapshot['models'])} models -> {benchmarks_json()}"
    )


@benchmarks_cli.command("show")
def benchmarks_show() -> None:
    """Render the cached benchmark snapshot; warn once if it is stale (>14 days)."""
    from fno.adapters.providers.benchmarks import (
        is_stale,
        load_snapshot,
        reachable,
        staleness_seconds,
    )

    snapshot = load_snapshot()
    if snapshot is None:
        typer.echo(
            "no benchmark snapshot; run `fno config accounts benchmarks refresh`",
            err=True,
        )
        raise typer.Exit(code=1)
    if is_stale(snapshot):
        age = staleness_seconds(snapshot) or 0
        typer.echo(
            f"WARNING: benchmark snapshot is {int(age // 86400)} days old (>14); "
            "run `fno config accounts benchmarks refresh`",
            err=True,
        )
    typer.echo(f"source: {snapshot.get('source')}  fetched_at: {snapshot.get('fetched_at')}")
    for row in snapshot.get("models", []):
        name = row.get("name", "?")
        pct = row.get("coding_percentile")
        mapped = reachable(name)
        route = f"{mapped[0]}:{mapped[1]}" if mapped else "unmapped (not routable)"
        typer.echo(f"  {name:<24} coding={pct!s:<6} -> {route}")
