"""`fno config doctor` reads the config back the way an operator reads it.

A test that asserts an absence carries a positive marker beside it: an
absence has three explanations and only one of them is the outcome.
"""
from __future__ import annotations

import sys
from pathlib import Path
from typing import Generator

import pytest
from typer.testing import CliRunner

from fno.cli import app

runner = CliRunner()
_ENV = {
    "COLUMNS": "240",
    "NO_COLOR": "1",
    "TERM": "dumb",
    "FNO_SKIP_MIGRATION": "1",
    "FNO_TEST_MODE": "1",
}


@pytest.fixture(autouse=True)
def _isolate(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Generator[None, None, None]:
    monkeypatch.setenv("FNO_REPO_ROOT", str(tmp_path))
    monkeypatch.delenv("FNO_CONFIG", raising=False)
    yield


def _doctor(config: Path, **extra: str):
    env = {**_ENV, "FNO_CONFIG": str(config), **extra}
    return runner.invoke(app, ["config", "doctor"], env=env)


def _write(path: Path, body: str) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(body, encoding="utf-8")
    return path


# --- AC1: an unreadable settings file refuses -----------------------------


def test_a_config_toml_holding_yaml_refuses_and_names_the_parse_error(tmp_path: Path) -> None:
    f = _write(tmp_path / "config.toml", "schema_version: 1\nconfig:\n  state_dir: ~/.fno/\n")
    result = _doctor(f)
    assert result.exit_code == 1, result.output
    assert "[doctor] 1 unreadable settings file(s):" in result.output
    assert str(f) in result.output
    assert "Expected '='" in result.output


# --- AC2: a non-mapping document refuses and names its type ---------------


def test_a_settings_yaml_holding_a_list_names_the_type_it_parsed_to(tmp_path: Path) -> None:
    f = _write(tmp_path / "settings.yaml", "- just\n- a\n- list\n")
    result = _doctor(f)
    assert result.exit_code == 1, result.output
    assert str(f) in result.output
    assert "parsed to a list, not a table" in result.output


# --- AC3 / AC4: unknown keys, named with their file ------------------------


def test_a_typod_section_is_reported_with_the_file_that_holds_it(tmp_path: Path) -> None:
    f = _write(tmp_path / "config.toml", "schema_version = 1\n[reveiw]\ncross_model = true\n")
    result = _doctor(f)
    assert result.exit_code == 1, result.output
    line = next(ln for ln in result.output.splitlines() if "reveiw.cross_model" in ln)
    assert "ignored" in line
    assert str(f) in line


def test_the_wrong_section_pair_names_the_key_the_operator_meant(tmp_path: Path) -> None:
    """AC4: `[agents] max_lanes` reads as a lane cap and sets no lane cap."""
    f = _write(tmp_path / "config.toml", "schema_version = 1\n[agents]\nmax_lanes = 4\n")
    result = _doctor(f)
    assert result.exit_code == 1, result.output
    line = next(ln for ln in result.output.splitlines() if "agents.max_lanes" in ln)
    assert "parallel.max_lanes" in line


# --- AC5: a switch enabled with an empty population ------------------------


def test_cross_model_enabled_with_no_dispatchable_peer_is_named(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    from fno import config_readback as doctor_mod

    f = _write(tmp_path / "config.toml", "schema_version = 1\n[review.cross_model]\nenabled = true\n")
    monkeypatch.setenv("FNO_CONFIG", str(f))
    monkeypatch.setattr(
        "fno.rust_binary.call_binary_json",
        lambda verb, args=(), **_: (None, {"available": ["claude"]}),
    )
    problems = doctor_mod.check_enabled_with_empty_population()
    assert len(problems) == 1, problems
    assert "review.cross_model.enabled is true" in problems[0]
    assert "available reviewer kinds: claude" in problems[0]


# --- AC7: the settings-source line lists contributors, not presences -------


def test_the_source_line_skips_an_unreadable_higher_priority_file(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    repo = tmp_path / "repo"
    project = _write(repo / ".fno" / "config.toml", "not = = toml\n")
    glob = _write(tmp_path / "home" / ".fno" / "config.toml", 'schema_version = 1\nstate_dir = "%s"\n' % (tmp_path / ".fno"))
    monkeypatch.setenv("FNO_REPO_ROOT", str(repo))
    env = {
        **_ENV,
        "FNO_REPO_ROOT": str(repo),
        "FNO_GLOBAL_SETTINGS_PATH": str(glob.with_name("settings.yaml")),
    }
    result = runner.invoke(app, ["config", "doctor"], env=env)
    source = next(ln for ln in result.output.splitlines() if "settings source:" in ln)
    assert str(glob) in source
    assert str(project) not in source


# --- the check that had stopped running ------------------------------------


def test_an_overridden_bad_cap_is_not_a_finding(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The renderer resolves 20 from the toml, so the yaml value nothing reads
    is not the operator's problem. Probing each layer alone reported it."""
    from fno.setup.doctor import check_wip_caps

    yaml_path = _write(tmp_path / "settings.yaml", 'kanban:\n  wip_caps:\n    now: "bad"\n')
    _write(tmp_path / "config.toml", "[kanban.wip_caps]\nnow = 20\n")
    monkeypatch.setenv("FNO_GLOBAL_SETTINGS_PATH", str(yaml_path))
    # Positive control on the same reader: a bad value nothing overrides IS one.
    _write(tmp_path / "config.toml", '[kanban.wip_caps]\nlater = "bad"\n')
    assert check_wip_caps() != []
    _write(tmp_path / "config.toml", "[kanban.wip_caps]\nnow = 20\n")
    assert check_wip_caps() == []


# --- the report stays readable on a real machine ---------------------------


def test_a_dict_keyed_block_is_not_read_as_a_typo(tmp_path: Path) -> None:
    """`agents.profiles.<verb>` keys are operator-chosen names, not field names.

    The walker resolved `dict[str, SpawnProfileBlock]` to its VALUE model and
    then checked the map's own keys against that model's fields, so every real
    profile read as an unknown key. Measured on one machine's live config: 31
    reported keys, 19 of them entries in a `dict[str, Model]` block.
    """
    f = _write(
        tmp_path / "config.toml",
        'schema_version = 1\nstate_dir = "%s"\n'
        '[agents.profiles.blueprint]\nmodel = "opus"\n'
        "[work.workspaces.main]\nprojects = []\n" % (tmp_path / ".fno"),
    )
    result = _doctor(f)
    assert result.exit_code == 0, result.output
    unknown = [ln for ln in result.output.splitlines() if "not a modeled config key" in ln]
    assert unknown == [], unknown


def test_a_json_read_stays_parseable_with_an_unknown_key(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """--json is a machine surface and a reader may capture stderr with
    stdout, so the load warning stays off it entirely (x-dddb). argv carries
    the verb's own flag in a real process, so it is pinned here. Paired
    negative: without --json the warning prints."""
    import json as _json

    f = _write(
        tmp_path / "config.toml",
        'schema_version = 1\nstate_dir = "%s"\n'
        "[nosuchblock]\nleaf = 1\n" % (tmp_path / ".fno"),
    )
    env = {**_ENV, "FNO_CONFIG": str(f)}
    monkeypatch.setattr(
        sys, "argv", ["fno", "config", "get", "backlog.max_open_ideas", "--json"]
    )
    quiet = runner.invoke(
        app, ["config", "get", "backlog.max_open_ideas", "--json"], env=env
    )
    assert quiet.exit_code == 0, quiet.output
    assert _json.loads(quiet.output)["value"] == 25
    assert "fno config:" not in quiet.output

    # A new fingerprint, so the loud half recomputes the load the cache
    # would otherwise reuse from the quiet half.
    f.write_text(f.read_text() + "\n# recompute\n", encoding="utf-8")
    monkeypatch.setattr(sys, "argv", ["fno", "config", "get", "backlog.max_open_ideas"])
    loud = runner.invoke(
        app, ["config", "get", "backlog.max_open_ideas"], env=env
    )
    assert loud.exit_code == 0, loud.output
    assert "fno config:" in loud.output


# --- an out-of-enum value refuses by name -----------------------------------


def test_an_out_of_enum_value_refuses_by_name(tmp_path: Path) -> None:
    """The loader refuses naming file, key, value and legal set; the doctor
    prints that refusal verbatim and never suggests migrate-paths, which
    recreates files, not values."""
    f = _write(
        tmp_path / "config.toml",
        'schema_version = 1\n[recovery.watchdog]\nmode = "on"\n',
    )
    result = _doctor(f)
    assert result.exit_code == 1, result.output
    assert "refusing to load settings" in result.output
    assert str(f) in result.output
    assert "recovery.watchdog.mode = 'on'" in result.output
    assert "'report', 'wake' or 'handoff'" in result.output
    assert "Traceback" not in result.output
    assert "migrate-paths" not in result.output


def test_honored_legacy_spelling_is_not_unknown(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    """`lift_retire_grace` lifts the moved retire_grace_s key, so the raw
    spelling is honored, not ignored, and must never report as unknown: a
    load-time warning that fires on a working key is cried wolf. A key the
    repo has retired (the deleted mail_to leaf, per retired-config-leaves.txt)
    is the opposite: dead means dead, and the walker must say so."""
    from fno.config_readback import check_unknown_keys

    honored = _write(tmp_path / "honored.toml", 'schema_version = 1\n[recovery]\nretire_grace_s = 60\n')
    monkeypatch.setenv("FNO_CONFIG", str(honored))
    assert check_unknown_keys() == []

    retired = _write(tmp_path / "retired.toml", 'schema_version = 1\n[recovery]\nwatchdog_mail_to = "bp"\n')
    monkeypatch.setenv("FNO_CONFIG", str(retired))
    assert any("recovery.watchdog_mail_to" in p for p in check_unknown_keys())

    dead = _write(tmp_path / "dead.toml", 'schema_version = 1\n[recovery]\nwatchdog_reap = true\n')
    monkeypatch.setenv("FNO_CONFIG", str(dead))
    assert any("recovery.watchdog_reap" in p for p in check_unknown_keys())

