"""dispatch_env and resolve_env_value for provider rotation substrate.

Phase 03 of the provider rotation substrate.

Pure functions: no global state, no caching, concurrency-safe by design.
"""
from __future__ import annotations

import os
import subprocess
from pathlib import Path
from typing import Any

from fno.adapters.providers.loader import load_providers
from fno.adapters.providers.model import (
    ProviderNotFoundError,
    ProviderRecord,
    ProviderUnavailableError,
)
from fno.adapters.providers.staging import _default_providers_root, verify_staged
from fno.agents.model_routing import _parse_target, resolve_explicit_route


def resolve_env_value(value: str) -> str:
    """Resolve ${...} references in env values.

    Supported references:
        ${ENV:VAR_NAME}        - read from os.environ
        ${KEYCHAIN:item_name}  - read from macOS keychain via `security`
        ${FILE:/path/to/file}  - read first line of file
        ${literal_value}       - escape; returns "literal_value" verbatim
                                 (detected by absence of ':' after '${')

    Plain strings (no '${' at start) are returned as-is.
    Unresolved references raise ProviderUnavailableError.
    """
    if not value.startswith("${"):
        return value

    # Strip outer ${ ... }
    if not value.endswith("}"):
        return value

    inner = value[2:-1]  # contents between ${ and }

    if ":" not in inner:
        # Escape mechanism: ${literal_value} - no recognised prefix.
        return inner

    prefix, rest = inner.split(":", 1)

    if prefix == "ENV":
        try:
            return os.environ[rest]
        except KeyError:
            raise ProviderUnavailableError(
                f"Environment variable not set: {rest!r} (from reference {value!r})"
            )

    if prefix == "KEYCHAIN":
        cmd = ["security", "find-generic-password", "-w", "-s", rest]
        try:
            result = subprocess.run(
                cmd,
                capture_output=True,
                text=True,
                check=True,
            )
            return result.stdout.strip()
        except subprocess.CalledProcessError as exc:
            raise ProviderUnavailableError(
                f"Keychain item not found: {rest!r} (from reference {value!r})"
            ) from exc

    if prefix == "FILE":
        file_path = Path(rest)
        try:
            first_line = file_path.read_text(encoding="utf-8").splitlines()[0].strip()
            return first_line
        except (OSError, IndexError) as exc:
            raise ProviderUnavailableError(
                f"Cannot read key file: {file_path} (from reference {value!r})"
            ) from exc

    # Unknown prefix - treat as literal (escape mechanism extended).
    return inner


def _env_for_oauth(record: ProviderRecord, root: Path) -> dict[str, str]:
    """Build the env dict for an oauth_dir provider."""
    account_dir = root / record.id
    cli = record.harness
    if cli == "claude":
        return {"CLAUDE_CONFIG_DIR": str(account_dir / ".claude")}
    else:
        # gemini, codex, openclaw, hermes: HOME override.
        return {"HOME": str(account_dir / "home")}


def _env_for_api_key(
    record: ProviderRecord, *, settings: Any = None
) -> dict[str, str]:
    """Build the env dict for an api_key provider; resolve all references.

    Raises ProviderUnavailableError naming the offending key if any value
    cannot be resolved. Never returns a partial dict.
    """
    if record.route:
        parsed = _parse_target(record.route)
        if parsed is None:  # defensive: ProviderRecord validates this at load time
            raise ProviderUnavailableError(
                f"Cannot resolve route {record.route!r} for account {record.id!r}: "
                "malformed provider/model"
            )
        route_env = resolve_explicit_route(parsed[0], parsed[1], settings=settings)
        if route_env is None:
            raise ProviderUnavailableError(
                f"Cannot resolve route {record.route!r} for account {record.id!r}: "
                "route unavailable"
            )
        return dict(route_env)

    assert record.env is not None  # enforced by model validator
    resolved: dict[str, str] = {}
    for key, raw_value in record.env.items():
        try:
            resolved[key] = resolve_env_value(raw_value)
        except ProviderUnavailableError as exc:
            raise ProviderUnavailableError(
                f"Cannot resolve env value for key {key!r}: {exc}"
            ) from exc
    return resolved


def _refuse_identity_mismatch(
    record: ProviderRecord, root: Path, by_id: dict[str, ProviderRecord]
) -> None:
    """The same refusal ``resolve_account_overlay`` makes, from the same binding."""
    from fno.adapters.providers.binding import MISMATCH, resolve_account_binding

    got = resolve_account_binding(record, root=root, by_id=by_id)
    if got.status == MISMATCH:
        raise ProviderUnavailableError(got.receipt)


def dispatch_env(
    provider_id: str,
    repo_root: Path | None = None,
    root: Path | None = None,
) -> dict[str, str]:
    """Return the subprocess env dict for invoking provider_id's CLI.

    Reads settings.yaml via load_providers(), looks up the record, and computes
    the env. For a claude record with a bound principal it also asks the binding
    who the credential root serves, which costs one profile call and a cache
    write. No account record is mutated.

    Raises:
        ProviderNotFoundError: if provider_id not in config.records
        ProviderUnavailableError: if oauth_dir not staged or api_key env
            cannot be resolved (missing keychain entry, env var unset)
    """
    if root is None:
        root = _default_providers_root()
    config = load_providers(repo_root=repo_root)
    by_id = config.by_id

    if provider_id not in by_id:
        raise ProviderNotFoundError(
            f"Provider {provider_id!r} is not configured. "
            "Check config.providers.records in settings.yaml."
        )

    record = by_id[provider_id]

    # A config-dir account lives in its OWN dir, not the shared managed
    # slot. Honor it regardless of auth, else a managed record carrying config_dir
    # would fall through to the `managed -> {}` arm below and dispatch on the
    # ambient default account (silent mis-bill).
    if record.config_dir is not None:
        _refuse_identity_mismatch(record, root, by_id)
        return {"CLAUDE_CONFIG_DIR": str(record.config_dir)}

    if record.auth == "oauth_dir":
        if not verify_staged(record, root=root):
            raise ProviderUnavailableError(
                f"Provider {provider_id!r} is not staged. "
                "Call stage(record) before dispatch_env()."
            )
        return _env_for_oauth(record, root)

    if record.auth == "managed":
        # A managed account materializes into the shared default slot
        # (~/.claude for claude, ~/.codex for codex), so dispatch adds no
        # CLAUDE_CONFIG_DIR/HOME override - the CLI reads the slot directly.
        # So the slot itself has to be asked who it serves.
        _refuse_identity_mismatch(record, root, by_id)
        return {}

    # api_key path
    settings = None
    if record.route and repo_root is not None:
        from fno.config import load_settings_for_repo

        settings = load_settings_for_repo(repo_root)
    return _env_for_api_key(record, settings=settings)

