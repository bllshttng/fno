"""Policy tests for opt-in local preflight.

`local_verification_required` is the ONE function that decides whether a full
local `scripts/ci/preflight.sh` receipt is required; every Python lane
(`fno do pr evidence-required`, the worker ship lane, the batch lane) routes
through it, and the bash ship path asks it via the CLI. These tests pin the
returned reason string, not just the boolean, because the reason is the
receipt three blocked workers needed on 2026-08-19.
"""
from __future__ import annotations

from pathlib import Path

import pytest

from fno.pr._preflight import local_verification_required


def _opt_in_preflight(tmp_path: Path) -> None:
    """Pin the project config to `preflight.required = true` for tmp_path."""
    (tmp_path / ".fno").mkdir(exist_ok=True)
    (tmp_path / ".fno" / "config.toml").write_text("[preflight]\nrequired = true\n")


def _fake_git(monkeypatch: pytest.MonkeyPatch, changed: str) -> None:
    """ls-tree sees the runner in the base; diff names `changed`."""

    def fake(args, *_args, **_kwargs):
        output = (
            "100755 blob abc\tscripts/ci/preflight.sh\n"
            if args[0] == "ls-tree"
            else f"{changed}\n"
        )
        return type("Result", (), {"returncode": 0, "stdout": output})()

    monkeypatch.setattr("fno.pr._preflight._git", fake)


def _make_runner(tmp_path: Path) -> None:
    runner = tmp_path / "scripts" / "ci" / "preflight.sh"
    runner.parent.mkdir(parents=True, exist_ok=True)
    runner.write_text("#!/bin/sh\n")
    runner.chmod(0o755)


def test_stock_config_returns_policy_opt_in_before_any_git_work(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """AC1: a stock config never requires the rehearsal, reason pinned."""
    _make_runner(tmp_path)

    def unreachable(*_args, **_kwargs):  # pragma: no cover - assertion fires first
        raise AssertionError("policy line must fire before any git subprocess work")

    monkeypatch.setattr("fno.pr._preflight._git", unreachable)
    assert local_verification_required(cwd=str(tmp_path)) == (False, "policy-opt-in")


def test_opted_in_project_requires_on_a_code_diff(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """AC2: preflight.required = true restores (True, "required")."""
    _make_runner(tmp_path)
    _opt_in_preflight(tmp_path)
    _fake_git(monkeypatch, "cli/code.py")
    assert local_verification_required(cwd=str(tmp_path)) == (True, "required")


def test_explicit_skip_env_still_wins_over_opt_in(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """AC5: FNO_SKIP_PREFLIGHT=1 beats preflight.required = true."""
    _opt_in_preflight(tmp_path)
    assert local_verification_required(
        cwd=str(tmp_path), env={"FNO_SKIP_PREFLIGHT": "1"}
    ) == (False, "explicit-skip")


def test_malformed_config_fails_open(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    """AC4: a gate no one can evaluate must not stop a green commit."""
    _make_runner(tmp_path)
    (tmp_path / ".fno").mkdir(exist_ok=True)
    (tmp_path / ".fno" / "config.toml").write_text("[preflight\nrequired = true\n")
    assert local_verification_required(cwd=str(tmp_path)) == (False, "policy-opt-in")
