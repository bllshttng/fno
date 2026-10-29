"""The loader's unknown-key walk: what it names on stderr, and what it honors.

The `fno config doctor` report over the same walk is the native door's
contract, frozen by the parity goldens and the config_doctor unit tests.
"""
from __future__ import annotations

from pathlib import Path

import pytest


@pytest.fixture(autouse=True)
def _isolate(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("FNO_REPO_ROOT", str(tmp_path))
    monkeypatch.delenv("FNO_CONFIG", raising=False)


def _write(path: Path, body: str) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(body, encoding="utf-8")
    return path


def test_the_load_warns_on_stderr_for_an_unknown_key(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The loader names unmodeled keys on stderr. The --json machine read's
    clean stderr is pinned byte-level by the native parity goldens; this
    pins the loud half against the loader itself, no command shell in the path."""
    import contextlib
    import io

    f = _write(
        tmp_path / "config.toml",
        'schema_version = 1\nstate_dir = "%s"\n'
        "[nosuchblock]\nleaf = 1\n" % (tmp_path / ".fno"),
    )
    monkeypatch.setenv("FNO_CONFIG", str(f))
    from fno.config import load_settings

    err = io.StringIO()
    with contextlib.redirect_stderr(err):
        load_settings()
    assert "fno config:" in err.getvalue()
    assert "nosuchblock.leaf" in err.getvalue()


def test_honored_legacy_spelling_is_not_unknown(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    """`lift_retire_grace` lifts the moved retire_grace_s key, so the raw
    spelling is honored, not ignored, and must never report as unknown: a
    load-time warning that fires on a working key is cried wolf. A key the
    repo has retired (the deleted mail_to leaf, per retired-config-leaves.txt)
    is the opposite: dead means dead, and the walker must say so."""
    from fno.config._readback import unknown_key_problems

    honored = _write(tmp_path / "honored.toml", 'schema_version = 1\n[recovery]\nretire_grace_s = 60\n')
    monkeypatch.setenv("FNO_CONFIG", str(honored))
    assert unknown_key_problems() == []

    retired = _write(tmp_path / "retired.toml", 'schema_version = 1\n[recovery]\nwatchdog_mail_to = "bp"\n')
    monkeypatch.setenv("FNO_CONFIG", str(retired))
    assert any("recovery.watchdog_mail_to" in p for p in unknown_key_problems())

    dead = _write(tmp_path / "dead.toml", 'schema_version = 1\n[recovery]\nwatchdog_reap = true\n')
    monkeypatch.setenv("FNO_CONFIG", str(dead))
    assert any("recovery.watchdog_reap" in p for p in unknown_key_problems())

