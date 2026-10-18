"""A reader must not fail closed on a registry written by a newer writer.

``~/.fno/agents/registry.json`` is global to every agent on this machine, so a
process running ahead of the deployment raised the on-disk schema and every
deployed reader refused it at once. Mail died fleet-wide with no announcement,
and the symptom surfaced far from the cause.

The shape that fixes it is read forward, delegate safe writes, and say so out loud:

- READ a higher on-disk schema instead of raising, keeping the rows and fields
  this reader understands and ignoring the ones it does not.
- DELEGATE a skewed write to Rust, which merges the raw disk rows and unknown
  fields back before publishing, while honoring the writer compatibility floor.
- ANNOUNCE every degraded read on stderr, naming both versions. A silent
  read-forward makes a partial row indistinguishable from a complete one, so a
  routing or liveness decision taken on a truncated row would leave no trace.
"""

from __future__ import annotations

import json
import uuid
from pathlib import Path

import pytest

from fno.agents import registry as reg


def _write_raw(path: Path, version: int, agents: list[dict]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps({"schema_version": version, "agents": agents}, indent=2),
        encoding="utf-8",
    )


def _row(name: str = "worker-1") -> dict:
    """A row this reader fully understands, as the current schema spells it."""
    from dataclasses import asdict

    entry = reg.AgentEntry(
        name=name,
        cwd="/Users/x/proj",
        log_path="/Users/x/proj/.fno/log",
        harness="claude",
        # One session per name: the table keys rows on it.
        harness_session_id=str(uuid.uuid5(uuid.NAMESPACE_URL, name)),
    )
    return asdict(entry)


# --------------------------------------------------------------------------
# Read forward
# --------------------------------------------------------------------------


def test_unknown_field_from_a_newer_writer_is_ignored_not_fatal(tmp_path: Path) -> None:
    """A field this reader has never heard of must not brick the shared read."""
    path = tmp_path / "registry.json"
    row = _row()
    row["a_field_from_the_future"] = {"nested": True}
    _write_raw(path, reg.SCHEMA_VERSION + 1, [row])

    entries = reg.load_registry(path)

    assert [e.name for e in entries] == ["worker-1"]
    assert not hasattr(entries[0], "a_field_from_the_future")


def test_degraded_read_announces_both_versions(tmp_path: Path, capsys) -> None:
    """Silence is the trap. A partial row must never look like a complete one."""
    path = tmp_path / "registry.json"
    ahead = reg.SCHEMA_VERSION + 1
    _write_raw(path, ahead, [_row()])

    reg.load_registry(path)

    err = capsys.readouterr().err
    assert str(ahead) in err, f"on-disk schema not named on stderr: {err!r}"
    assert str(reg.SCHEMA_VERSION) in err, f"reader schema not named on stderr: {err!r}"


# --------------------------------------------------------------------------
# Read forward covers added VALUES, not only added keys
# --------------------------------------------------------------------------
#
# Tolerating a new key was only half the problem. Widening an existing enum is
# one of the likelier reasons a writer bumps the schema at all (v2 added
# `status`, v4 added `host_mode`), and a row-level refusal took the whole shared
# read down with it -- the exact fleet-wide brick this module now exists to
# prevent, reached by a different door.


def test_an_unknown_enum_value_skips_its_row_not_the_registry(tmp_path: Path) -> None:
    path = tmp_path / "registry.json"
    bad = {**_row("future-worker"), "status": "hibernating"}
    _write_raw(path, reg.SCHEMA_VERSION + 1, [bad, _row("readable-worker")])

    entries = reg.load_registry(path)

    assert [e.name for e in entries] == ["readable-worker"], (
        "one row this fno cannot represent must not hide every other agent"
    )


def test_a_structured_value_skips_its_row_rather_than_raising(tmp_path: Path) -> None:
    """The third door into the same brick, and the one that bypassed the guard.

    `value in KNOWN_STATUSES` tests membership against a frozenset, so a dict or
    list raises `TypeError: unhashable type` rather than `RegistryVersionError`.
    That escaped a handler catching only the latter and took the whole shared
    read down, which is the exact failure read-forward exists to prevent. Callers
    catch `(OSError, RegistryVersionError)`, so it surfaced as a traceback.
    """
    path = tmp_path / "registry.json"
    bad = {**_row("future-worker"), "status": {"state": "live"}}
    _write_raw(path, reg.SCHEMA_VERSION + 1, [bad, _row("readable-worker")])

    entries = reg.load_registry(path)

    assert [e.name for e in entries] == ["readable-worker"]


def test_the_same_row_stays_fatal_at_our_own_schema(tmp_path: Path) -> None:
    """At or below our schema an unknown value is a writer bug, not a version gap."""
    path = tmp_path / "registry.json"
    bad = {**_row(), "status": "hibernating"}
    _write_raw(path, reg.SCHEMA_VERSION, [bad])

    with pytest.raises(reg.RegistryVersionError):
        reg.load_registry(path)


# --------------------------------------------------------------------------
# Skewed writes
# --------------------------------------------------------------------------


def test_newer_schema_write_refuses_when_registry_commit_door_is_unavailable(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A read-forward writer stays byte-safe when its Rust door is unavailable."""
    from fno import rust_binary

    path = tmp_path / "registry.json"
    row = _row()
    row["a_field_from_the_future"] = "keep me"
    _write_raw(path, reg.SCHEMA_VERSION + 1, [row])
    before = path.read_bytes()
    calls: list[tuple[str, dict]] = []

    def unavailable(verb: str, payload: dict, **_kwargs: object) -> dict:
        calls.append((verb, payload))
        raise rust_binary.VerbUnavailable("registry-commit is unavailable")

    monkeypatch.setattr(rust_binary, "verb_call", unavailable)
    with pytest.raises(reg.RegistryVersionError, match="registry-commit"):
        reg.update_registry(lambda entries: entries, path)

    assert [verb for verb, _payload in calls] == ["registry-commit"]
    assert calls[0][1]["path"] == str(path)
    assert path.read_bytes() == before


# --------------------------------------------------------------------------
# Still fail closed on genuine damage
# --------------------------------------------------------------------------


def test_malformed_json_still_raises(tmp_path: Path) -> None:
    """Read-forward covers a version gap, never a torn file."""
    path = tmp_path / "registry.json"
    path.write_text("{not json", encoding="utf-8")

    with pytest.raises(reg.RegistryVersionError):
        reg.load_registry(path)
