"""Unit tests for `fno doctor bundle` thin wrappers.

Each subcommand is a forwarder. Tests verify:
1. Help text renders (top-level + each subcommand).
2. No-subcommand default runs the bundler (scripts/generate-skill-bundles.sh).
3. ``check`` and ``lint`` subcommands forward to their canonical scripts.
4. Exit codes propagate from the canonical script.
5. Missing canonical script -> diagnostic + exit 2.
"""
from __future__ import annotations


from typer.testing import CliRunner

from fno.cli import app


runner = CliRunner()


def test_bundle_check_emits_terminal_verdict_line(monkeypatch):
    """x-6a8e: ``fno doctor bundle check`` prints a terminal verdict line from the same
    code path that sets the exit code, so ``fno doctor bundle check | tail`` cannot mask
    a drift failure behind the pipe's exit. The verdict is the LAST line and
    greppable; the exit code is preserved and is no longer the only signal."""
    import fno.bundle.cli as bc

    # FAIL path: underlying freshness script reports drift (rc=1).
    monkeypatch.setattr(bc, "_forward", lambda command, args: 1)
    result = runner.invoke(app, ["doctor", "bundle", "check"])
    assert result.exit_code == 1
    lines = result.stdout.strip().splitlines()
    assert lines[-1] == "fno doctor bundle check: FAIL (rc=1)", result.stdout

    # PASS path: rc=0.
    monkeypatch.setattr(bc, "_forward", lambda command, args: 0)
    result = runner.invoke(app, ["doctor", "bundle", "check"])
    assert result.exit_code == 0
    assert result.stdout.strip().splitlines()[-1] == "fno doctor bundle check: PASS"


def test_bundle_missing_canonical_script_reports_diagnostic(tmp_path, monkeypatch):
    """When the canonical script can't be found (broken install), the
    wrapper exits 2 with an actionable diagnostic, not a Python traceback."""
    # Point FNO_REPO_ROOT at an empty dir so the canonical scripts are missing.
    fake_root = tmp_path / "fake-repo"
    fake_root.mkdir()
    monkeypatch.setenv("FNO_REPO_ROOT", str(fake_root))

    result = runner.invoke(app, ["doctor", "bundle", "check"])
    assert result.exit_code == 2
    # AC3-ERR: capability-accurate degrade names the footnote plugin and the
    # bare-install gap + an install path, not "is the plugin installed correctly?".
    out = result.stdout + (result.stderr or "")
    assert "footnote plugin" in out
    assert "pip install fno" in out
    assert "--plugin-dir" in out
