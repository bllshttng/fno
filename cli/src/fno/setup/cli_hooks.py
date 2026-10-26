"""Read-only view of footnote's user-level CLI hooks.

The installers live in the Rust front door (``fno config setup cli-hooks``,
crates/fno/src/cli_hooks.rs). Two Python readers remain: ``fno doctor`` calls
``inspect_codex_hooks`` to report the Codex hook layers, and the context census
reads ``SESSION_START_CONTEXT_CARRIERS``.
"""
from __future__ import annotations

import json
import tomllib
from dataclasses import dataclass
from pathlib import Path
from typing import Any

# footnote's SessionStart entry script; every footnote hook command names it.
_HOOK_SUFFIX = "hooks/session-start.sh"


@dataclass(frozen=True)
class SessionStartContextCarrier:
    """Static carrier metadata consumed by the read-only context census."""

    harness: str
    script: str
    entry_states: tuple[str, ...]


SESSION_START_CONTEXT_CARRIERS = (
    SessionStartContextCarrier(
        harness="codex",
        script=_HOOK_SUFFIX,
        entry_states=("startup", "resume", "clear"),
    ),
    SessionStartContextCarrier(
        harness="gemini",
        script=_HOOK_SUFFIX,
        entry_states=("startup", "resume", "clear"),
    ),
)


@dataclass(frozen=True)
class CodexHookDiagnostics:
    """Read-only view of Codex's current user-level hook layers."""

    config_path: Path
    hooks_json_path: Path
    state: str  # neither | toml-only | json-only | both | malformed
    toml_commands: tuple[str, ...] = ()
    json_commands: tuple[str, ...] = ()
    toml_footnote_commands: tuple[str, ...] = ()
    json_footnote_commands: tuple[str, ...] = ()
    toml_foreign_commands: tuple[str, ...] = ()
    json_foreign_commands: tuple[str, ...] = ()
    toml_footnote_state_keys: tuple[str, ...] = ()
    toml_footnote_state_recorded: tuple[bool, ...] = ()
    errors: tuple[str, ...] = ()

    @property
    def has_toml_hooks(self) -> bool:
        return bool(self.toml_commands)

    @property
    def has_json_hooks(self) -> bool:
        return bool(self.json_commands)

    @property
    def all_toml_footnote_hooks_verified(self) -> bool:
        # A stored trusted_hash is only an approval record. Until this module
        # recomputes and compares Codex's exact local hash contract, it cannot
        # truthfully claim the command is verified.
        return False


def _load_json_object(path: Path) -> dict[str, Any]:
    """Load a JSON file whose root must be an object."""
    text = path.read_text(encoding="utf-8")
    data = json.loads(text)
    if not isinstance(data, dict):
        raise json.JSONDecodeError("expected a JSON object", text, 0)
    return data


def _is_footnote_codex_command(command: str) -> bool:
    """Classify only footnote-owned Codex SessionStart commands."""
    if _HOOK_SUFFIX not in command:
        return False
    return (
        "FNO_PLATFORM=codex" in command
        or "/footnote/hooks/session-start.sh" in command
    )


def _all_hook_commands(data: Any, *, source: str) -> tuple[str, ...]:
    """Extract commands from every hook event array, excluding trust state."""
    if not isinstance(data, dict):
        raise ValueError(f"{source} root is not an object")
    hooks = data.get("hooks", {}) or {}
    if not isinstance(hooks, dict):
        raise ValueError(f"{source} `hooks` is not an object")

    commands: list[str] = []
    for event, groups in hooks.items():
        if event == "state":
            continue
        if groups is None:
            continue
        if not isinstance(groups, list):
            raise ValueError(f"{source} `hooks.{event}` is not an array")
        for group in groups:
            if not isinstance(group, dict):
                raise ValueError(f"{source} {event} group is not an object")
            entries = group.get("hooks", []) or []
            if not isinstance(entries, list):
                raise ValueError(f"{source} {event} `hooks` is not an array")
            for hook in entries:
                if not isinstance(hook, dict):
                    raise ValueError(f"{source} {event} hook is not an object")
                command = hook.get("command")
                if command is not None and not isinstance(command, str):
                    raise ValueError(f"{source} {event} command is not a string")
                if command:
                    commands.append(command)
    return tuple(commands)


def _session_start_commands(data: Any, *, source: str) -> tuple[str, ...]:
    """Extract commands from the shared TOML/JSON SessionStart shape."""
    return tuple(
        command
        for _group_index, _hook_index, command in _session_start_hook_records(
            data, source=source
        )
    )


def _session_start_hook_records(
    data: Any, *, source: str
) -> tuple[tuple[int, int, str], ...]:
    """Extract ``(group index, hook index, command)`` SessionStart records."""
    if not isinstance(data, dict):
        raise ValueError(f"{source} root is not an object")
    hooks = data.get("hooks", {})
    if hooks is None:
        hooks = {}
    if not isinstance(hooks, dict):
        raise ValueError(f"{source} `hooks` is not an object")
    groups = hooks.get("SessionStart", [])
    if groups is None:
        groups = []
    if not isinstance(groups, list):
        raise ValueError(f"{source} `hooks.SessionStart` is not an array")

    records: list[tuple[int, int, str]] = []
    for group_index, group in enumerate(groups):
        if not isinstance(group, dict):
            raise ValueError(f"{source} SessionStart group is not an object")
        entries = group.get("hooks", [])
        if entries is None:
            entries = []
        if not isinstance(entries, list):
            raise ValueError(f"{source} SessionStart `hooks` is not an array")
        for hook_index, hook in enumerate(entries):
            if not isinstance(hook, dict):
                raise ValueError(f"{source} SessionStart hook is not an object")
            command = hook.get("command")
            if command is not None and not isinstance(command, str):
                raise ValueError(f"{source} SessionStart command is not a string")
            if command:
                records.append((group_index, hook_index, command))
    return tuple(records)


def _codex_hook_trust(
    data: dict[str, Any],
    *,
    config_path: Path,
    records: tuple[tuple[int, int, str], ...],
) -> tuple[tuple[str, ...], tuple[bool, ...]]:
    """Return Codex state keys and trust-entry presence for footnote hooks."""
    hooks = data.get("hooks", {}) or {}
    state = hooks.get("state", {}) or {}
    if not isinstance(state, dict):
        raise ValueError("config.toml `hooks.state` is not an object")

    path = config_path.expanduser().absolute()
    keys = tuple(
        f"{path}:session_start:{group_index}:{hook_index}"
        for group_index, hook_index, command in records
        if _is_footnote_codex_command(command)
    )
    return keys, tuple(key in state for key in keys)


def inspect_codex_hooks(
    *, config_path: Path, hooks_json_path: Path
) -> CodexHookDiagnostics:
    """Inspect Codex TOML and legacy JSON hook layers without mutating either.

    Syntax, schema, and read failures are returned in ``errors`` rather than
    escaping. A malformed layer makes the aggregate state ``malformed`` while
    commands from a healthy layer remain available to the caller.
    """
    toml_commands: tuple[str, ...] = ()
    json_commands: tuple[str, ...] = ()
    toml_session_commands: tuple[str, ...] = ()
    json_session_commands: tuple[str, ...] = ()
    toml_footnote_state_keys: tuple[str, ...] = ()
    toml_footnote_state_recorded: tuple[bool, ...] = ()
    errors: list[str] = []

    if config_path.exists():
        try:
            parsed_toml = tomllib.loads(config_path.read_text(encoding="utf-8"))
            toml_records = _session_start_hook_records(parsed_toml, source="config.toml")
            toml_session_commands = tuple(command for _, _, command in toml_records)
            toml_commands = _all_hook_commands(parsed_toml, source="config.toml")
            toml_footnote_state_keys, toml_footnote_state_recorded = _codex_hook_trust(
                parsed_toml,
                config_path=config_path,
                records=toml_records,
            )
        except (OSError, UnicodeError, tomllib.TOMLDecodeError, ValueError) as exc:
            errors.append(f"{config_path}: {exc}")

    if hooks_json_path.exists():
        try:
            parsed_json = _load_json_object(hooks_json_path)
            json_session_commands = _session_start_commands(
                parsed_json, source="hooks.json"
            )
            json_commands = _all_hook_commands(parsed_json, source="hooks.json")
        except (OSError, UnicodeError, json.JSONDecodeError, ValueError) as exc:
            errors.append(f"{hooks_json_path}: {exc}")

    if errors:
        state = "malformed"
    elif toml_commands and json_commands:
        state = "both"
    elif toml_commands:
        state = "toml-only"
    elif json_commands:
        state = "json-only"
    else:
        state = "neither"

    toml_footnote = tuple(
        c for c in toml_session_commands if _is_footnote_codex_command(c)
    )
    json_footnote = tuple(
        c for c in json_session_commands if _is_footnote_codex_command(c)
    )
    return CodexHookDiagnostics(
        config_path=config_path,
        hooks_json_path=hooks_json_path,
        state=state,
        toml_commands=toml_commands,
        json_commands=json_commands,
        toml_footnote_commands=toml_footnote,
        json_footnote_commands=json_footnote,
        toml_foreign_commands=tuple(c for c in toml_commands if c not in toml_footnote),
        json_foreign_commands=tuple(c for c in json_commands if c not in json_footnote),
        toml_footnote_state_keys=toml_footnote_state_keys,
        toml_footnote_state_recorded=toml_footnote_state_recorded,
        errors=tuple(errors),
    )
