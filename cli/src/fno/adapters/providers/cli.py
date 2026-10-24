"""Typer sub-app for fno config accounts commands.

Phase 02 of the provider rotation substrate.
Provides: list, show, add, test, use, remove.

Phase 03 will wire in staging.stage(record) inside the `add` command.
"""
from __future__ import annotations

import os
import shutil
import subprocess
import sys
import time as time_module
from pathlib import Path
from typing import TYPE_CHECKING, NamedTuple, Optional, cast

import typer

from fno.adapters.providers import managed
from fno.adapters.providers.dispatch import dispatch_env
from fno.adapters.providers.loader import (
    is_effective_active,
    load_providers,
    load_quota_config,
    load_scope_config,
    mutable_accounts_block,
    save_providers,
)
from fno.adapters.providers.model import (
    ProviderConfigError,
    ProviderNotFoundError,
    ProviderRecord,
    ProviderUnavailableError,
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


def _settings_path_for_scope(scope: str) -> Path:
    """Return the settings.yaml path for the given scope."""
    if scope == "project":
        return _resolve_cwd() / ".fno" / "config.toml"
    else:
        return _resolve_home() / ".fno" / "config.toml"


def _load(scope: str = "global") -> "ProvidersConfig":
    """Load providers config; error on validation failure."""
    from fno.adapters.providers.loader import load_providers
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
def list_providers(
    json_output: bool = typer.Option(
        False, "--json", "-J", help="Emit a JSON array of record rows (Connections UI)."
    ),
) -> None:
    """List all configured accounts, marking the active one with *.

    The human listing always carries an identity column: the ``.active`` stamp
    says who PUT a credential in the slot, not who it serves, so the binding
    verdict and doctor's per-record problems ride the row.
    """
    config = _load()

    def _is_active(record: ProviderRecord) -> bool:
        # One resolver, shared with the dispatch path: for a managed account the
        # meaningful "active" is which one is materialized in that harness's
        # slot, falling back to routing-active otherwise.
        return is_effective_active(record, config)

    # One quota read for the whole listing: the per-record usage age and the
    # DISARMED footer below all read the same config, never a parse per row.
    quota = load_quota_config()

    # Binding reads run only for the human listing.
    needs_binding = not json_output
    identities, findings, shared = {}, [], set()
    if needs_binding:
        now = time_module.time()
        findings = _doctor_findings()
        for record in config.records:
            identities[record.id] = _identity_for(record, config.by_id, now)
        shared = _shared_identity_ids(identities)
    problems = {
        r.id: [f["problem"] for f in findings if f.get("record") == r.id]
        for r in config.records
    }
    # shared-identity rides the problems list: one wire field, both surfaces.
    for rid in shared:
        problems[rid].append("shared-identity")

    if json_output:
        import json as _json

        rows = []
        for record in config.records:
            usage_reading = _usage_age(record.id, ttl=quota.probe_ttl_seconds)
            row = {
                "id": record.id,
                "name": record.name,
                "harness": record.harness,
                "auth": record.auth,
                "priority": record.priority,
                "active": _is_active(record),
                "headroom": _headroom_label(record.id),
                "cred-snapshot": (
                    managed.snapshot_age_label(record.id)
                    if record.auth == "managed"
                    else None
                ),
                "usage_age_s": usage_reading.age_seconds,
                "usage_ttl_seconds": usage_reading.ttl_seconds,
                "usage_stale": usage_reading.stale,
            }
            rows.append(row)
        typer.echo(_json.dumps(rows))
        return

    if not config.records:
        typer.echo(
            "No accounts configured. Run `fno config accounts add` to add one."
        )
        return
    for record in config.records:
        marker = "*" if _is_active(record) else " "
        headroom_col = _headroom_label(record.id)
        line = (
            f"{marker} {record.id}  [{record.harness}] {record.auth}  "
            f"priority={record.priority}  headroom={headroom_col}"
        )
        if record.auth == "managed":
            line += f"  cred-snapshot={managed.snapshot_age_label(record.id)}"
        line += f"  {_usage_age_col(record.id, ttl=quota.probe_ttl_seconds)}"
        off_slot = record.auth == "managed" and not _is_active(record)
        cell = _identity_cell(record, identities.get(record.id), problems[record.id], off_slot)
        line += f"  identity={cell}"
        typer.echo(line)

    # Two flags, two sentences. Recommend `observe` first: it is the reversible
    # half, it changes no dispatch decision, and an operator who only wanted a
    # working meter used to have to accept automatic deferral to get one.
    if not (quota.observe or quota.defer_dispatch):
        typer.echo(
            "\nquota observation is OFF: accounts.quota.observe = false and "
            "accounts.quota.defer_dispatch = false, so dispatch never probes "
            "(the daemon still does). Set accounts.quota.observe = true in "
            "~/.fno/config.toml under [accounts.quota] for a meter that "
            "changes no dispatch decision; add defer_dispatch = true to also "
            "hold a dispatch on a walled account."
        )
    elif not quota.defer_dispatch:
        typer.echo(
            "\nobservation is ON and rotation is DISARMED: "
            "accounts.quota.defer_dispatch = false, so headroom is measured "
            "and recorded but never holds a dispatch."
        )


# ---------------------------------------------------------------------------
# usage (quota-aware dispatch)
# ---------------------------------------------------------------------------


def _headroom_label(provider_id: str) -> str:
    """Compact headroom string with its own provenance for the list column.

    The bare state name sat next to the credential-snapshot age on the same
    line and read as one fact, so an operator took the snapshot age for the
    verdict's provenance. The source (and the observation age when there was
    one) keeps the two claims apart. Fail-open to 'unknown'.
    """
    try:
        from fno.adapters.providers.runtime_state import headroom

        h = headroom(provider_id)
        detail = h.source
        if detail:
            if h.observed_at is not None:
                age = max(0.0, time_module.time() - h.observed_at)
                if age < 60:
                    detail += f" {int(age)}s"
                elif age < 3600:
                    detail += f" {int(age // 60)}m"
                elif age < 86400:
                    detail += f" {int(age // 3600)}h"
                else:
                    detail += f" {int(age // 86400)}d"
            detail = f"({detail})"
        return f"{h.state.name.lower()}{detail}"
    except Exception:  # noqa: BLE001 - a display read must never break `list`
        return "unknown"


class _UsageAge(NamedTuple):
    """The usage-PROBE age, distinct from `snapshot=` (the credential blob
    age). Neither `age_seconds` nor `stale` renders when no probe has ever
    landed for this record - that is the `never` case."""

    age_seconds: Optional[float]
    ttl_seconds: int
    stale: bool


def _usage_age(record_id: str, *, ttl: Optional[int] = None) -> _UsageAge:
    """Read the cached usage snapshot's age regardless of TTL (a stale
    snapshot must still report its age, never vanish - `read_usage`'s
    default TTL treats stale as absent, so bypass it with a large one).
    ``ttl`` is passed by callers that already read the quota config; the
    per-record default re-reads it so standalone calls stay correct."""
    try:
        from fno.adapters.providers.runtime_state import read_usage

        if ttl is None:
            ttl = load_quota_config().probe_ttl_seconds
        snap = read_usage(record_id, ttl_seconds=float("inf"))
        if snap is None:
            return _UsageAge(age_seconds=None, ttl_seconds=ttl, stale=False)
        age = time_module.time() - snap.probed_at
        return _UsageAge(age_seconds=age, ttl_seconds=ttl, stale=age > ttl)
    except Exception:  # noqa: BLE001 - a display read must never break `list`
        return _UsageAge(age_seconds=None, ttl_seconds=300, stale=False)


def _usage_age_col(record_id: str, *, ttl: Optional[int] = None) -> str:
    """The `usage=<age>` column text: `never`, `<age>`, or `<age> (STALE, ttl=<ttl>)`."""
    reading = _usage_age(record_id, ttl=ttl)
    if reading.age_seconds is None:
        return "usage=never"
    label = managed.age_label_from_seconds(reading.age_seconds)
    if reading.stale:
        ttl_label = managed.age_label_from_seconds(reading.ttl_seconds)
        return f"usage={label} (STALE, ttl={ttl_label})"
    return f"usage={label}"


def _identity_cell(record: ProviderRecord, got, problems: list, off_slot: bool = False) -> str:
    """One compact identity token for a list row: matched names the row's own
    record, mismatch names who the credential really serves, anything unproven
    renders ``?<reason>`` and never names an account it did not prove."""
    from fno.adapters.providers.binding import AMBIGUOUS, MATCHED, MISMATCH

    if record.harness != "claude" or record.auth == "api_key":
        cell = "-"
    elif got is None:
        cell = "?no-observation"
    elif got.status == MATCHED:
        cell = got.matched_record or got.requested_record or "?"
    elif got.status == MISMATCH and off_slot:
        cell = "off-slot"  # a managed record not in the slot: no slot login is its own
    elif got.status == MISMATCH:
        served_by = (got.matched_record or got.observed_label
                     or got.observed_principal or "another-account")
        cell = f"!serves {served_by}"
    elif got.status == AMBIGUOUS:
        cell = "?ambiguous"
    else:
        cell = f"?{got.reason or 'unknown'}"
    return cell + "".join(f" !{p}" for p in problems)


def _shared_identity_ids(identities: dict) -> set:
    """Record ids whose observed principal is also observed through a
    different credential root. Managed records share one slot root (their
    ``credential_root`` is None), so a managed pair never trips this; two
    scoped config dirs serving one principal do."""
    by_principal: dict = {}
    for got in identities.values():
        if got is None or got.observed_principal is None:
            continue
        by_principal.setdefault(got.observed_principal, set()).add(got.credential_root)
    return {
        rid for rid, got in identities.items()
        if got is not None and got.observed_principal is not None
        and len(by_principal[got.observed_principal]) > 1
    }


def _fmt_resets_in(resets_at: float | None, now: float) -> str:
    """Render a reset epoch as a relative 'in 40m' / 'reset' string.

    ``None`` is a window the endpoint reported without a reset. It reads
    "reset unknown" and never blank: the window is real and it binds on its
    percentage, and a blank column would read as "nothing to say here".
    """
    if resets_at is None:
        return "reset unknown"
    delta = resets_at - now
    if delta <= 0:
        return "reset"
    mins = int(delta // 60)
    if mins < 60:
        return f"in {mins}m"
    hours = mins // 60
    rem = mins % 60
    return f"in {hours}h{rem:02d}m"


def _identity_for(record, by_id: dict, now: float):
    """The effective-account verdict for one claude record, or None.

    The usage surface calls it for a record that HAS an observation to
    attribute; ``list`` calls it for every claude record, which costs one
    binding read per record (cached; ``list -J`` does no binding work). Safe
    for a record with no observation: the owner answers a typed unknown.
    None means a non-claude record or a read that raised.
    """
    from fno.adapters.providers.binding import resolve_account_binding

    if record.harness != "claude":
        return None
    try:
        return resolve_account_binding(
            record, root=managed.store_root(), by_id=by_id, now=now
        )
    except Exception:  # noqa: BLE001 - a report never fails on an identity read
        return None


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
def window_providers(
    json_output: bool = typer.Option(
        False, "--json", "-J", help="Emit one JSON object keyed by account id."
    ),
    warn_seconds: float = typer.Option(
        None, "--warn-seconds",
        help="Emit provider_window_closing under this many seconds to close "
             "(default 1800). Once per window.",
    ),
) -> None:
    """Where each account's rolling usage window is, without probing anything.

    The gap this fills: ``usage.py`` registers probes for claude and codex only,
    so glm, gemini, openclaw, hermes and every api_key record are UNKNOWN to the
    quota layer - and ``rotation.py`` treats UNKNOWN as never-exhausted. The one
    provider that actually caps here was the one provider nothing could see. The
    operator learned they were at 90 percent, then 99, then capped, from a
    dashboard and a pasted 429.

    Where a probe exists, this prints its headroom verdict. Where none does, it
    prints a PROJECTION from a recorded window open plus ``window_minutes`` on
    the account record: seconds to close and the projected close time.

    A projection is not a utilization reading, and this surface never pretends
    otherwise. It writes no synthetic usage window, so ``headroom()`` keeps
    answering UNKNOWN for a probe-less record and ``fno config accounts list``
    never renders a percentage no probe measured. A record with nothing recorded
    prints ``unknown``, which is the honest answer and not a zero.
    """
    import json as _json
    import time as _time

    from fno.adapters.providers.runtime_state import (
        DEFAULT_WINDOW_WARN_SECONDS,
        headroom,
        mark_window_warned,
        project_window,
    )
    def _emit(event_type: str, data: dict) -> None:
        """Best-effort canonical emit. A warning that cannot be logged is
        still a warning that was printed, so this never raises."""
        try:
            from fno.events import _build, append_event
            from fno.paths import state_dir

            append_event(
                _build(event_type, "cli", data), state_dir() / "events.jsonl"
            )
        except Exception:  # noqa: BLE001 - a read-only report never fails on its log
            pass

    threshold = (
        DEFAULT_WINDOW_WARN_SECONDS if warn_seconds is None else float(warn_seconds)
    )
    config = _load()
    now = _time.time()

    rows: dict[str, dict[str, object]] = {}
    for record in config.records:
        proj = project_window(record.id, now=now)
        verdict = headroom(record.id, now=now)
        rows[record.id] = {
            "harness": record.harness,
            "headroom": verdict.state.value,
            "headroom_resets_at": verdict.resets_at,
            "window_minutes": record.window_minutes,
            "opened_at": proj.opened_at,
            "closes_at": proj.closes_at,
            # Always present, including as null: `done_probe 2` greps for the
            # key, and a key that appears only on the happy path lets a shapeless
            # answer pass the gate.
            "closes_in_s": (
                None if proj.closes_in_s is None else int(proj.closes_in_s)
            ),
        }
        if proj.closes_in_s is not None and proj.closes_in_s <= threshold:
            if mark_window_warned(record.id):
                _emit("provider_window_closing", {
                    "provider_id": record.id,
                    "label": proj.label,
                    "closes_in_s": int(proj.closes_in_s),
                    "closes_at": proj.closes_at,
                })

    if json_output:
        typer.echo(_json.dumps(rows))
        return

    if not config.records:
        typer.echo("No accounts configured.")
        return
    for pid, row in rows.items():
        closes_in = row["closes_in_s"]
        when = (
            "unknown"
            if closes_in is None
            else f"closes {_fmt_resets_in(float(cast(float, row['closes_at'])), now)}"
        )
        typer.echo(
            f"{pid}  [{row['harness']}]  headroom={row['headroom']:<9} {when}"
        )


# ---------------------------------------------------------------------------
# required-bot-check (quota-aware dispatch, US5)
# ---------------------------------------------------------------------------

# Map a required-bot GitHub login (substring match) to the provider CLI kind
# that backs it, so we can look up that provider's headroom. Advisory only: an
# unmappable bot is silently skipped (fail-open).
_REQUIRED_BOT_CLI: dict[str, str] = {
    "codex": "codex",   # chatgpt-codex-connector
    "gemini": "gemini",
    "claude": "claude",
}


def _bot_provider_cli(bot: str) -> Optional[str]:
    low = bot.lower()
    for needle, cli_kind in _REQUIRED_BOT_CLI.items():
        if needle in low:
            return cli_kind
    return None


def required_bot_headroom_check() -> list[dict]:
    """Return one dict per required-bot whose provider is EXHAUSTED.

    Read-only + fail-open (US5): reads the CACHED snapshot only (never
    probes at promise time), unions config.review.github_apps + required_bots,
    maps each bot to a provider record via its CLI kind, and reports those whose
    headroom is EXHAUSTED. Any read failure yields an empty list - a telemetry
    read must never block a promise. Each returned dict emits one
    ``quota_required_bot_exhausted`` decision event as a side effect (AC3-HP).
    """
    try:
        from fno.adapters.providers.runtime_state import HeadroomState, headroom
        from fno.config import load_settings

        settings = load_settings()
        review = settings.review
        bots = list(dict.fromkeys([*(review.github_apps or []), *(review.required_bots or [])]))
        if not bots:
            return []
        config = load_providers(repo_root=_get_repo_root())
        by_cli: dict[str, list[str]] = {}
        for rec in config.records:
            by_cli.setdefault(rec.harness, []).append(rec.id)
    except Exception:  # noqa: BLE001 - a promise-time read must never raise
        return []

    warnings: list[dict] = []
    for bot in bots:
        cli_kind = _bot_provider_cli(bot)
        if cli_kind is None:
            continue
        provider_ids = by_cli.get(cli_kind, [])
        if not provider_ids:
            continue
        # Warn only when EVERY provider of the kind is exhausted: one healthy
        # account (multi-account) keeps the gate un-wedged, so warning on a
        # single exhausted account would be a false positive. Mirrors the
        # lane-routing rule (a kind is exhausted only if all its records are).
        exhausted: list[tuple[str, Optional[float]]] = []
        for provider_id in provider_ids:
            try:
                h = headroom(provider_id)
            except Exception:  # noqa: BLE001
                # An unreadable provider is UNKNOWN, not exhausted - it keeps
                # the kind from counting as fully-exhausted (fail-open).
                exhausted = []
                break
            if h.state is HeadroomState.EXHAUSTED:
                exhausted.append((provider_id, h.resets_at))
            else:
                exhausted = []
                break
        if exhausted and len(exhausted) == len(provider_ids):
            provider_id, retry_at = exhausted[0]
            warnings.append({"bot": bot, "provider": provider_id, "retry_at": retry_at})
            _emit_required_bot_exhausted(bot, provider_id, retry_at)
    return warnings


def _emit_required_bot_exhausted(bot: str, provider: str, retry_at: Optional[float]) -> None:
    """Emit the single decision event for a required-bot exhaustion. Non-fatal."""
    try:
        from fno.events import _build, append_event

        data: dict = {"bot": bot, "provider": provider, "headroom": "exhausted"}
        if retry_at is not None:
            data["retry_at"] = retry_at
        append_event(_build("quota_required_bot_exhausted", "target", data))
    except Exception:  # noqa: BLE001
        pass


@cli.command("required-bot-check")
def required_bot_check_cmd(
    json_output: bool = typer.Option(
        False, "--json", "-J", help="Emit a JSON list of exhausted required-bot providers."
    ),
) -> None:
    """Warn (pre-promise) if a required review bot's provider is out of quota.

    Read-only, advisory, exit 0 always: names each exhausted bot/provider and
    the reset time so a coming review-gate wedge surfaces now instead of hours
    later. Prints nothing when every required bot has headroom (or none are
    configured). The attended <help> tag is raised by the target skill.
    """
    import json as _json

    warnings = required_bot_headroom_check()
    if json_output:
        typer.echo(_json.dumps(warnings))
        return
    if not warnings:
        return
    now = _resolve_time()
    for w in warnings:
        reset = _fmt_resets_in(w["retry_at"], now) if w.get("retry_at") else "unknown"
        typer.echo(
            f"required-bot {w['bot']}: provider {w['provider']} EXHAUSTED, "
            f"review gate will wedge until reset ({reset})"
        )


def _resolve_time() -> float:
    import time as _time

    return _time.time()


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


def _doctor_findings() -> list[dict]:
    """Per-record + per-slot problems in the managed store. Read-only.

    A stale capture, a missing login, an unproven identity, and a tainted
    slot are four separate silent degradations that each surface only as
    ``unknown`` somewhere downstream. This collects them into one answerable
    question so the operator sees the store's real condition instead of a
    symptom. Never mutates; never prints a secret.
    """
    config = _load()
    findings: list[dict] = []

    for record in config.records:
        blob = managed.read_blob(record.id)
        if blob is not None:
            holder = managed.duplicate_credential_holder(blob, exclude_id=record.id)
            if holder is not None:
                findings.append({
                    "record": record.id,
                    "problem": "duplicate-credential",
                    "detail": (
                        f"holds the same credential as '{holder}'; one of them "
                        f"was captured while the other account was signed in"
                    ),
                })

        if (
            record.auth == "managed"
            and record.harness == "claude"
            and record.config_dir is None
            and managed.record_principal(record.id) is None
        ):
            findings.append({
                "record": record.id,
                "problem": "unbound-principal",
                "detail": (
                    "no proven identity is bound, so shared-slot usage cannot be "
                    "attributed to it and reads unknown; sign this account in and "
                    f"run `fno config accounts register {record.id}` to bind it"
                ),
            })

        if record.config_dir is not None:
            from fno.agents.account_env import _login_present

            if not record.config_dir.exists():
                findings.append({
                    "record": record.id,
                    "problem": "missing-config-dir",
                    "detail": f"config_dir {record.config_dir} does not exist",
                })
            elif not _login_present(record.config_dir):
                findings.append({
                    "record": record.id,
                    "problem": "no-login-in-config-dir",
                    "detail": (
                        f"config_dir {record.config_dir} holds no claude login "
                        f"(run: CLAUDE_CONFIG_DIR={record.config_dir} claude /login)"
                    ),
                })

    for harness_kind in sorted({r.harness for r in config.records}):
        try:
            tainted = managed.slot_tainted(harness_kind, managed.store_root())
        except (OSError, managed.ManagedStoreError):
            continue
        if tainted:
            findings.append({
                "record": f"slot:{harness_kind}",
                "problem": "tainted-slot",
                "detail": (
                    "the active stamp was written while sessions were pinned, so "
                    "the slot may hold a credential the stamp does not describe; "
                    "usage stays unknown until identity is proven - repair with "
                    f"`fno config accounts reconcile-slot {harness_kind}`"
                ),
            })

        # Taint watches the door footnote controls; `claude /login` uses the
        # other one and leaves a stamp that is wrong AND untainted, so nothing
        # downstream hesitates. Asking the slot who it actually serves is what
        # turns that into a finding instead of silently wrong billing.
        findings.extend(_slot_identity_findings(harness_kind, config.by_id))

    return findings


def _slot_identity_findings(harness_kind: str, by_id: dict) -> list[dict]:
    """Identity findings for one CLI's shared slot, from the shared binding.

    Free until it can answer. With no stamp, or no principal bound to the
    stamped record, there is nothing to compare.
    """
    from fno.adapters.providers.binding import (
        AMBIGUOUS,
        UNKNOWN_RECEIPT,
        resolve_account_binding,
    )

    if harness_kind != "claude":
        return []
    repair = f"repair with `fno config accounts reconcile-slot {harness_kind}`"
    where = f"slot:{harness_kind}"

    def _finding(problem: str, detail: str) -> list[dict]:
        return [{"record": where, "problem": problem, "detail": detail}]

    try:
        root = managed.store_root()
        stamped = managed.active_slot_id(harness_kind, root)
        bound = managed.identity_key(managed.record_principal(stamped, root)) if stamped else None
    except (OSError, managed.ManagedStoreError):
        return []  # a diagnosis we could not make, not a crash in a read-only command
    if bound is None:
        return []
    got = resolve_account_binding(None, harness=harness_kind, root=root, by_id=by_id)
    if got.status == AMBIGUOUS and got.reason == "ambiguous-slot":
        return _finding("ambiguous-slot", (
            "the slot's stored credentials belong to different accounts, so whichever is "
            "stamped, some reader gets the other; "
            + managed.slot_switch_remedy("the account to keep")
            + f", then {repair}"
        ))
    if got.observed_principal is None:
        # An unreadable slot cannot demonstrate drift, so this used to be quiet.
        return _finding(UNKNOWN_RECEIPT, (
            f"who the live slot serves could not be proven ({got.reason}), so usage "
            f"and launch receipts stay unknown rather than naming '{stamped}'; this "
            f"is not a healthy slot and not a successful switch - {repair}"
        ))
    if got.observed_principal == bound:
        return []
    return _finding("slot-identity-drift", (
        f"the stamp names '{stamped}' but the live slot credential belongs to "
        f"{got.observed_label or got.observed_principal} (an out-of-band "
        f"`{harness_kind} /login`), so usage is being attributed to the wrong "
        f"account - {repair}"
    ))


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
    import json as _json

    findings = _doctor_findings()

    if json_output:
        typer.echo(_json.dumps(findings))
    elif not findings:
        typer.echo("providers doctor: no problems found.")
    else:
        for f in findings:
            typer.echo(f"{f['record']}: {f['problem']}: {f['detail']}")
        typer.echo("")
        n = len(findings)
        typer.echo(f"{n} problem{'' if n == 1 else 's'} found.")

    if findings:
        raise typer.Exit(1)


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
def test_provider(
    provider_id: str = typer.Argument(...),
    smoke: bool = typer.Option(False, "--smoke", help="Attempt a real CLI invocation (costs quota)"),
) -> None:
    """Validate account config: record lookup + binary on PATH + credentials_source exists.

    With --smoke, also attempt a real CLI invocation (not fully implemented in Phase 02).
    """
    config = _load()
    record = config.by_id.get(provider_id)
    if record is None:
        typer.echo(f"error: account '{provider_id}' not found", err=True)
        raise typer.Exit(1)

    # Check CLI binary on PATH
    binary = shutil.which(record.harness)
    if binary is None:
        typer.echo(
            f"error: CLI binary '{record.harness}' is not on PATH",
            err=True,
        )
        raise typer.Exit(1)

    # Check credentials_source for oauth_dir
    if record.auth == "oauth_dir":
        if record.credentials_source is None:
            typer.echo(
                f"error: auth=oauth_dir but credentials_source is not set for '{provider_id}'",
                err=True,
            )
            raise typer.Exit(1)
        if not record.credentials_source.exists():
            typer.echo(
                f"error: credentials_source '{record.credentials_source}' does not exist",
                err=True,
            )
            raise typer.Exit(1)

    if smoke:
        # Resolve dispatch env vars so the subprocess runs under the right
        # credentials, matching how real target invocations work.
        try:
            env_vars = dispatch_env(provider_id, repo_root=_get_repo_root())
        except (ProviderNotFoundError, ProviderUnavailableError) as exc:
            typer.echo(f"smoke: dispatch_env failed for '{provider_id}': {exc}", err=True)
            raise typer.Exit(1)

        try:
            smoke_result = subprocess.run(
                [record.harness, "--help"],
                capture_output=True,
                text=True,
                timeout=10,
                env={**os.environ, **env_vars},
            )
            if smoke_result.returncode != 0:
                typer.echo(
                    f"smoke test: '{record.harness} --help' exited {smoke_result.returncode}",
                    err=True,
                )
                raise typer.Exit(1)
        except (subprocess.TimeoutExpired, FileNotFoundError) as exc:
            typer.echo(f"smoke test error: {exc}", err=True)
            raise typer.Exit(1)

    typer.echo(f"Account '{provider_id}' looks OK.")


# ---------------------------------------------------------------------------
# use
# ---------------------------------------------------------------------------

@cli.command("use")
def use_provider(
    provider_id: str = typer.Argument(...),
    scope: str = typer.Option("project", "--scope", help="project|global"),
) -> None:
    """Set the active account (default scope: project)."""
    # Merged view for the lookup (the pointer may legitimately target a
    # record declared in another layer); the save below writes only the
    # target scope's own records so it cannot freeze the merge.
    repo_root = _get_repo_root()
    try:
        config = load_providers(repo_root=repo_root)
    except ProviderConfigError as exc:
        typer.echo(f"error: {exc}", err=True)
        raise typer.Exit(1)

    if provider_id not in config.by_id:
        typer.echo(f"error: account '{provider_id}' not found", err=True)
        raise typer.Exit(1)

    record = config.by_id[provider_id]

    # A config-dir account lives in its own dir; there is no shared-slot
    # snapshot to materialize, so a daemon-wide `use` switch does not apply. Refuse
    # cleanly instead of letting managed.switch fail with a cryptic no-snapshot
    # error. Use it per-worker via `fno agents spawn --account <id>`.
    if record.config_dir is not None:
        typer.echo(
            f"error: '{provider_id}' is a config-dir account (its own "
            f"{record.config_dir}); it is spawn-only. Use it per-worker with "
            f"`fno agents spawn --account {provider_id}`, not a daemon-wide switch.",
            err=True,
        )
        raise typer.Exit(1)

    # Validate the pointer target BEFORE any side effects: managed.switch
    # materializes credentials into the shared slot irreversibly, and a
    # validation failure after it leaves the slot switched with no routing
    # pointer.
    try:
        scope_records = load_scope_config(scope).records  # type: ignore[arg-type]
    except ProviderConfigError as exc:
        typer.echo(f"error loading existing config: {exc}", err=True)
        raise typer.Exit(1)
    if scope == "global" and provider_id not in {r.id for r in scope_records}:
        # Same rule `config set` enforces: a GLOBAL pointer must name a
        # globally declared record, or every project without that record
        # bricks its loader.
        typer.echo(
            f"error: account '{provider_id}' is not declared in the global "
            "config; declare it there first or set the pointer per project.",
            err=True,
        )
        raise typer.Exit(1)

    # Managed records materialize the account's credentials into the shared
    # slot (capture-before-overwrite + live-pin gate); oauth_dir/api_key records
    # only re-point active routing (spawns pick up the env), as before.
    if record.auth == "managed":
        from fno.agents.events import emit as _emit

        try:
            result = managed.switch(record, by_id=config.by_id, emit_fn=_emit)
        except managed.SwitchDeferred as exc:
            typer.echo(f"switch deferred: {exc}", err=True)
            raise typer.Exit(2)
        except managed.NeedsLogin as exc:
            if not (sys.stdin.isatty() and sys.stdout.isatty()):
                typer.echo(f"{exc} Run in a terminal: fno config accounts use {provider_id}", err=True)
                raise typer.Exit(1)
            if managed.run_vault_login(provider_id) != 0:
                raise typer.Exit(1)
            result = managed.SwitchResult(active=provider_id)
        except managed.ManagedStoreError as exc:
            typer.echo(f"switch failed: {exc}", err=True)
            raise typer.Exit(1)
        except KeyboardInterrupt as exc:
            for note in getattr(exc, "__notes__", ()):
                typer.echo(f"switch interrupted: {note}", err=True)
            raise
        if record.harness == "codex":
            if not result.slot_changed:
                typer.echo(
                    f"Managed Codex account '{result.active}' is already materialized "
                    f"(structural; {result.reason})."
                )
            elif result.verification == "codex-recognized":
                typer.echo(
                    f"Materialized managed Codex account '{result.active}' into the slot; "
                    "Codex recognized the local auth."
                )
            else:
                typer.echo(
                    f"Materialized managed Codex account '{result.active}' into the slot "
                    f"(structural fallback: {result.reason})."
                )
        else:
            typer.echo(f"Materialized managed account '{result.active}' into the slot (verified).")
        if result.pinned_by:
            typer.echo(
                f"warning: swapped under {len(result.pinned_by)} live {record.harness} "
                "session(s); they keep their in-memory auth, and a token refresh "
                "from one may overwrite the slot",
                err=True,
            )

    from fno.adapters.providers.model import ProvidersConfig
    new_config = ProvidersConfig(
        records=scope_records,
        active=provider_id,
    )

    try:
        save_providers(new_config, scope=scope)  # type: ignore[arg-type]
    except OSError as exc:
        # For a managed record the slot + its per-CLI stamp are already switched
        # (that IS the operative state managed dispatch reads); only the routing
        # pointer failed to persist. Say so, so the user retries the save rather
        # than believing the switch did not happen.
        if record.auth == "managed":
            typer.echo(
                f"warning: slot switched to '{provider_id}' but saving the active "
                f"routing pointer failed ({exc}); re-run `fno config accounts use "
                f"{provider_id}` to persist it.",
                err=True,
            )
        else:
            typer.echo(f"error: failed to write settings.yaml: {exc}", err=True)
        raise typer.Exit(1)

    typer.echo(f"Active account set to '{provider_id}' (scope={scope}).")


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
# combos sub-app: add / list / remove / test / use
# Plan B (Spec 4). Combos are named ordered provider lists
# with a rotation strategy (fallback | round_robin). Storage lives under
# ``config.providers.combos.<name>`` in settings.yaml.
# ---------------------------------------------------------------------------

combos_cli = typer.Typer(
    name="combos",
    help="Manage account combos (named ordered lists with rotation strategies).",
)
cli.add_typer(combos_cli, name="combos")


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


def _combos_settings_path(scope: str) -> Path:
    """Resolve settings path for combos commands (mirrors providers scope)."""
    return _settings_path_for_scope(scope)


@combos_cli.command("add")
def combos_add(
    name: str = typer.Argument(..., help="Combo name (must be unique)"),
    strategy: str = typer.Option(
        "fallback",
        "--strategy",
        help="Rotation strategy: fallback | round_robin",
    ),
    sticky_limit: int = typer.Option(
        1,
        "--sticky",
        help="round_robin only: calls per provider before advancing (>=1; clamped)",
    ),
    providers_csv: str = typer.Option(
        ...,
        "--providers",
        help="Comma-separated provider IDs (must exist in config.providers.records)",
    ),
    scope: str = typer.Option("project", "--scope", help="project | global"),
) -> None:
    """Add a new combo. Validates each provider exists; refuses if combo already exists."""
    from fno.adapters.providers.loader import atomic_mutate_settings
    from fno.adapters.providers.rotation import Combo

    providers_list = [p.strip() for p in providers_csv.split(",") if p.strip()]
    if not providers_list:
        typer.echo("error: --providers must list at least one provider id", err=True)
        raise typer.Exit(1)

    # Cross-validate provider IDs against records BEFORE mutation.
    config = _load()
    unknown = [p for p in providers_list if p not in config.by_id]
    if unknown:
        typer.echo(
            f"error: combo {name!r} references unknown provider id(s) "
            f"{unknown!r} (not in config.providers.records)",
            err=True,
        )
        raise typer.Exit(1)

    # Validate strategy/sticky via Combo construction (raises ValueError).
    try:
        Combo(
            name=name,
            strategy=strategy,  # type: ignore[arg-type]
            sticky_limit=sticky_limit,
            providers=tuple(providers_list),
        )
    except ValueError as exc:
        typer.echo(f"error: {exc}", err=True)
        raise typer.Exit(1)

    target = _combos_settings_path(scope)

    def mutator(data: dict) -> dict:
        accounts_section = mutable_accounts_block(data)
        combos_section = accounts_section.setdefault("combos", {})
        if name in combos_section:
            raise ValueError(
                f"combo {name!r} already exists in {scope} settings; "
                "remove it first or use a different name"
            )
        combos_section[name] = {
            "strategy": strategy,
            "sticky_limit": sticky_limit,
            "providers": providers_list,
        }
        return data

    try:
        atomic_mutate_settings(mutator, settings_path=target)
    except ValueError as exc:
        typer.echo(f"error: {exc}", err=True)
        raise typer.Exit(1)
    except OSError as exc:
        typer.echo(f"error: failed to write settings.yaml: {exc}", err=True)
        raise typer.Exit(1)

    typer.echo(
        f"Combo {name!r} added (strategy={strategy}, sticky={sticky_limit}, "
        f"providers={providers_list}, scope={scope})."
    )


@combos_cli.command("list")
def combos_list(
    as_json: bool = typer.Option(False, "--json", "-J", help="Emit JSON instead of a table"),
) -> None:
    """List all configured combos with cursor + member-count info."""
    import json as json_mod

    from fno.adapters.providers.loader import load_active_combo, load_combos
    from fno.adapters.providers.rotation import compute_providers_hash
    from fno.adapters.providers.runtime_state import read_cursor

    repo_root = _get_repo_root()
    try:
        combos = load_combos(repo_root=repo_root)
    except ProviderConfigError as exc:
        typer.echo(f"error: {exc}", err=True)
        raise typer.Exit(1)
    if not combos:
        if as_json:
            typer.echo("[]")
        else:
            typer.echo(
                "No combos configured. Run `fno config accounts combos add` to add one."
            )
        return

    active_combo = load_active_combo(repo_root=repo_root)
    rows = []
    for name, combo in combos.items():
        providers_hash = compute_providers_hash(combo.providers)
        cursor = read_cursor(name, providers_hash)
        rows.append({
            "name": name,
            "strategy": combo.strategy,
            "sticky_limit": combo.sticky_limit,
            "members": list(combo.providers),
            "active": name == active_combo,
            "cursor_index": cursor.cursor_index if cursor else None,
            "consecutive_use_count": cursor.consecutive_use_count if cursor else None,
        })

    if as_json:
        typer.echo(json_mod.dumps(rows, indent=2))
        return
    for row in rows:
        cursor_str = (
            f"idx={row['cursor_index']}/cnt={row['consecutive_use_count']}"
            if row["cursor_index"] is not None
            else "no-cursor"
        )
        typer.echo(
            f"{row['name']}  [{row['strategy']}, sticky={row['sticky_limit']}]  "
            f"members={','.join(cast('list[str]', row['members']))}  {cursor_str}"
        )


@combos_cli.command("remove")
def combos_remove(
    name: str = typer.Argument(...),
    scope: str = typer.Option("project", "--scope", help="project | global"),
) -> None:
    """Remove a combo. If the combo is the active_combo, also clears that field."""
    from fno.adapters.providers.loader import atomic_mutate_settings

    target = _combos_settings_path(scope)
    cleared_active = False

    def mutator(data: dict) -> dict:
        nonlocal cleared_active
        accounts_section = mutable_accounts_block(data)
        combos_section = accounts_section.get("combos", {})
        if name not in combos_section:
            raise ValueError(f"combo {name!r} not found in {scope} settings")
        del combos_section[name]
        if accounts_section.get("active_combo") == name:
            accounts_section["active_combo"] = None
            cleared_active = True
        return data

    try:
        atomic_mutate_settings(mutator, settings_path=target)
    except ValueError as exc:
        typer.echo(f"error: {exc}", err=True)
        raise typer.Exit(1)
    except OSError as exc:
        typer.echo(f"error: failed to write settings.yaml: {exc}", err=True)
        raise typer.Exit(1)

    msg = f"Combo {name!r} removed (scope={scope})."
    if cleared_active:
        msg += " active_combo cleared."
    typer.echo(msg)


@combos_cli.command("test")
def combos_test(
    name: str = typer.Argument(...),
) -> None:
    """Validate a combo: shape, member existence, per-member cooldown state.

    Config-only by design: does NOT issue API calls (smoke-testing every
    member multiplies cost). For an active liveness probe, run
    `fno config accounts test <id> --smoke` per member.
    """
    from fno.adapters.providers.loader import load_combos
    from fno.adapters.providers.runtime_state import read_state

    repo_root = _get_repo_root()
    try:
        combos = load_combos(repo_root=repo_root)
    except ProviderConfigError as exc:
        typer.echo(f"error: {exc}", err=True)
        raise typer.Exit(1)

    if name not in combos:
        typer.echo(f"error: combo {name!r} not found", err=True)
        raise typer.Exit(1)

    combo = combos[name]
    state = read_state()
    # Per-member health.
    cooldowned: list[str] = []
    healthy: list[str] = []
    for pid in combo.providers:
        h = state.provider_health.get(pid)
        if h is not None and h.rate_limited_until is not None and h.rate_limited_until > 0:
            import time as _t
            if h.rate_limited_until > _t.time():
                cooldowned.append(pid)
                continue
        healthy.append(pid)

    typer.echo(f"Combo {name!r}: strategy={combo.strategy}, sticky={combo.sticky_limit}")
    for pid in combo.providers:
        if pid in cooldowned:
            h = state.provider_health[pid]
            typer.echo(f"  {pid}: in_cooldown until {h.rate_limited_until}")
        else:
            typer.echo(f"  {pid}: ok")

    if not cooldowned:
        verdict = "all_healthy"
    elif len(cooldowned) == len(combo.providers):
        verdict = "all_in_cooldown"
    else:
        verdict = "partial_cooldown"
    typer.echo(f"verdict: {verdict}")


@combos_cli.command("use")
def combos_use(
    name: str = typer.Argument(...),
    scope: str = typer.Option("project", "--scope", help="project | global"),
) -> None:
    """Set the active combo (used as default when no --combo flag is passed)."""
    from fno.adapters.providers.loader import (
        atomic_mutate_settings,
        load_combos,
    )

    repo_root = _get_repo_root()
    try:
        combos = load_combos(repo_root=repo_root)
    except ProviderConfigError as exc:
        typer.echo(f"error: {exc}", err=True)
        raise typer.Exit(1)

    if name not in combos:
        typer.echo(f"error: combo {name!r} not found", err=True)
        raise typer.Exit(1)

    target = _combos_settings_path(scope)

    def mutator(data: dict) -> dict:
        accounts_section = mutable_accounts_block(data)
        accounts_section["active_combo"] = name
        return data

    try:
        atomic_mutate_settings(mutator, settings_path=target)
    except OSError as exc:
        typer.echo(f"error: failed to write settings.yaml: {exc}", err=True)
        raise typer.Exit(1)

    typer.echo(f"Active combo set to {name!r} (scope={scope}).")


@combos_cli.command("update")
def combos_update(
    name: str = typer.Argument(..., help="Existing combo to update (must exist)"),
    strategy: Optional[str] = typer.Option(
        None, "--strategy", help="Rotation strategy: fallback | round_robin (unset = keep current)"
    ),
    sticky_limit: Optional[int] = typer.Option(
        None, "--sticky", help="round_robin calls per provider (unset = keep current)"
    ),
    providers_csv: str = typer.Option(
        ..., "--providers", help="Comma-separated provider IDs (the new ordered members)"
    ),
    scope: str = typer.Option("project", "--scope", help="project | global"),
) -> None:
    """Atomically replace an existing combo's members/strategy in one write.

    The atomic alternative to remove+add: a crash mid-pair could strand the
    config with no combo. Validates each member exists and refuses an unknown
    combo. Reordering members changes compute_providers_hash, so the stored
    round-robin cursor is invalidated (read_cursor returns None on the new hash)
    without any explicit reset here.

    ``--strategy``/``--sticky`` default to the combo's CURRENT values when
    omitted, so a pure reorder (the UI's common case) never silently rewrites a
    round_robin combo to fallback/1.
    """
    from fno.adapters.providers.loader import atomic_mutate_settings
    from fno.adapters.providers.rotation import Combo

    providers_list = [p.strip() for p in providers_csv.split(",") if p.strip()]
    if not providers_list:
        typer.echo("error: --providers must list at least one provider id", err=True)
        raise typer.Exit(1)

    config = _load()
    unknown = [p for p in providers_list if p not in config.by_id]
    if unknown:
        typer.echo(
            f"error: combo {name!r} references unknown provider id(s) "
            f"{unknown!r} (not in config.providers.records)",
            err=True,
        )
        raise typer.Exit(1)

    target = _combos_settings_path(scope)
    applied: dict[str, object] = {}

    def mutator(data: dict) -> dict:
        accounts_section = mutable_accounts_block(data)
        combos_section = accounts_section.setdefault("combos", {})
        existing = combos_section.get(name)
        if existing is None:
            raise ValueError(
                f"combo {name!r} not found in {scope} settings; "
                "use `combos add` to create it"
            )
        # Inherit the current strategy/sticky when the flag is omitted, so a
        # reorder preserves round_robin/sticky instead of defaulting them away.
        eff_strategy = strategy if strategy is not None else existing.get("strategy", "fallback")
        eff_sticky = (
            sticky_limit if sticky_limit is not None else existing.get("sticky_limit", 1)
        )
        # Validate the resolved combo (raises ValueError, aborting the write).
        Combo(
            name=name,
            strategy=eff_strategy,  # type: ignore[arg-type]
            sticky_limit=eff_sticky,
            providers=tuple(providers_list),
        )
        combos_section[name] = {
            "strategy": eff_strategy,
            "sticky_limit": eff_sticky,
            "providers": providers_list,
        }
        applied.update(strategy=eff_strategy, sticky_limit=eff_sticky)
        return data

    try:
        atomic_mutate_settings(mutator, settings_path=target)
    except ValueError as exc:
        typer.echo(f"error: {exc}", err=True)
        raise typer.Exit(1)
    except OSError as exc:
        typer.echo(f"error: failed to write settings.yaml: {exc}", err=True)
        raise typer.Exit(1)

    typer.echo(
        f"Combo {name!r} updated (strategy={applied['strategy']}, "
        f"sticky={applied['sticky_limit']}, providers={providers_list}, scope={scope})."
    )
