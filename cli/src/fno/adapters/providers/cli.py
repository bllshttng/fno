"""Typer sub-app for fno config accounts commands.

Phase 02 of the provider rotation substrate.
Provides: list, show, add, test, use, remove.

Phase 03 will wire in staging.stage(record) inside the `add` command.
"""
from __future__ import annotations

import os
import time as time_module
from pathlib import Path
from typing import TYPE_CHECKING, NamedTuple, Optional, cast

import typer

from fno.adapters.providers import managed
from fno.adapters.providers.loader import (
    is_effective_active,
    load_providers,
    load_quota_config,
)
from fno.adapters.providers.model import (
    ProviderConfigError,
    ProviderRecord,
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
