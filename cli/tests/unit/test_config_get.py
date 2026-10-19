"""Tests for `fno config get <dotted.key>` (ab-e9c81ed3, C1 fallback read).

Lets a skill / LLM caller read a single resolved config value without
re-implementing settings traversal. Used by /blueprint to resolve the
config.blueprint.max_prs_per_epic decomposition ceiling fallback.
"""
from __future__ import annotations

from pathlib import Path

import pytest
from typer.testing import CliRunner


def _write_settings(tmp_path: Path, content: str) -> Path:
    d = tmp_path / ".fno"
    d.mkdir(parents=True, exist_ok=True)
    f = d / "settings.yaml"
    f.write_text(content, encoding="utf-8")
    return f


def _run(args, tmp_path, monkeypatch, settings_content):
    monkeypatch.delenv("FNO_CONFIG", raising=False)
    f = _write_settings(tmp_path, settings_content)
    monkeypatch.setenv("FNO_CONFIG", str(f))

    from fno.cli import app

    return CliRunner().invoke(app, args)


# ---------------------------------------------------------------------------
# A dict[str, Model] hop (x-043f task 1.3): an unknown field inside a known
# loop name still refuses.
# ---------------------------------------------------------------------------


def test_get_unknown_field_inside_known_loop_name(tmp_path, monkeypatch):
    r = _run(
        ["config", "get", "loops.blueprint_judge.nosuchfield"],
        tmp_path, monkeypatch, "schema_version: 1\n",
    )
    assert r.exit_code == 1
    assert "unknown config key" in r.output


# ---------------------------------------------------------------------------
# config.agents.confirm posture knob (ab-27541df5, US4; namespace moved from
# config.dispatch.confirm to config.agents.confirm in ab-f1b0ccd1)
# ---------------------------------------------------------------------------


def test_agents_confirm_invalid_enum_fails_read(tmp_path, monkeypatch):
    """AC4-FR: a typo (`atuo`) fails the read; never silently relaxes to never."""
    r = _run(
        ["config", "get", "config.agents.confirm"],
        tmp_path, monkeypatch,
        "schema_version: 1\nconfig:\n  agents:\n    confirm: atuo\n",
    )
    assert r.exit_code != 0
    assert r.stdout.strip() != "never"


# ---------------------------------------------------------------------------
# Optional leading `config.` prefix (x-8b64 E): `review.required_bots` is
# retried as `config.review.required_bots`. The review gate defaults to that
# key but the shorthand used to error "unknown config key".
# ---------------------------------------------------------------------------

_BOTS_SETTINGS = (
    "schema_version: 1\nconfig:\n  review:\n    required_bots:\n"
    "      - chatgpt-codex-connector\n"
)


def test_get_review_required_bots_shorthand(tmp_path, monkeypatch):
    """`review.required_bots` (no `config.` prefix) resolves."""
    r = _run(
        ["config", "get", "review.required_bots"],
        tmp_path, monkeypatch, _BOTS_SETTINGS,
    )
    assert r.exit_code == 0, r.output
    assert "chatgpt-codex-connector" in r.stdout


# ---------------------------------------------------------------------------
# Source printing (x-4be1): `fno config get` names WHICH FILE decided the
# value. The reported defect: home config set auto_merge.enabled=false, the
# project set true, and `config get` answered True with no indication that the
# project layer had silently overridden the operator kill switch. The rename
# alone does not fix that; the source line does. Value stays ALONE on stdout
# (normalize.sh pipes the whole stream through tr); the source line, including
# an overrides clause exactly when a lower-precedence file set a value the
# merge discarded, is stderr-only.
# ---------------------------------------------------------------------------


def _pin_two_layers(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, project: str, global_: str
) -> None:
    """Project + global config.toml chain (worktree==canonical==tmp project)."""
    monkeypatch.delenv("FNO_CONFIG", raising=False)
    monkeypatch.delenv("FNO_REPO_ROOT", raising=False)
    proj = tmp_path / "proj" / ".fno" / "config.toml"
    proj.parent.mkdir(parents=True, exist_ok=True)
    proj.write_text(project, encoding="utf-8")
    glob = tmp_path / "global-config.toml"
    glob.write_text(global_, encoding="utf-8")

    import fno.paths as paths_mod

    monkeypatch.setattr(paths_mod, "resolve_repo_root", lambda: tmp_path / "proj")
    monkeypatch.setattr(paths_mod, "resolve_canonical_repo_root", lambda: tmp_path / "proj")
    monkeypatch.setenv("FNO_GLOBAL_SETTINGS_PATH", str(glob))


def test_get_prints_deciding_file_and_overridden_file(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The reported defect, end to end: value on stdout; decider + overridden
    home file on stderr."""
    _pin_two_layers(
        tmp_path,
        monkeypatch,
        project="[auto_merge]\nenabled = true\n",
        global_="[auto_merge]\nenabled = false\n",
    )
    from fno.cli import app

    r = CliRunner().invoke(app, ["config", "get", "auto_merge.enabled"])
    assert r.exit_code == 0
    # stdout carries the value and NOTHING else (normalize.sh pipes it whole).
    assert r.stdout.strip() == "True"
    assert "source:" not in r.stdout
    # stderr names the project file as decider and the home file as overridden.
    assert f"source: {(tmp_path / 'proj' / '.fno' / 'config.toml')}" in r.stderr
    assert f"overrides {(tmp_path / 'global-config.toml')}" in r.stderr


def test_get_equal_values_in_both_layers_credit_the_higher_file(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Both files set the same value: the source line names the project file.

    Change-only attribution credited the global file when the project
    restated its value verbatim, and the source line read as "the global
    overrode the local". The decider is the highest-precedence file that sets
    the key, and an equal lower layer is not overridden.
    """
    _pin_two_layers(
        tmp_path,
        monkeypatch,
        project="[auto_merge]\nenabled = false\n",
        global_="[auto_merge]\nenabled = false\n",
    )

    from fno.config import resolve_source

    decided = resolve_source("auto_merge.enabled")
    assert decided is not None
    assert decided[0].resolve() == (tmp_path / "proj" / ".fno" / "config.toml").resolve()
    assert decided[1] == []

    from fno.cli import app

    r = CliRunner().invoke(app, ["config", "get", "auto_merge.enabled"])
    assert r.exit_code == 0
    assert r.stdout.strip() == "False"
    assert f"source: {(tmp_path / 'proj' / '.fno' / 'config.toml')}" in r.stderr
    assert "overrides" not in r.stderr
