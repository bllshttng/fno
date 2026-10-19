"""Executor-routing parsers: ``fno.executor._surface`` and ``fno.executor._locked``.

Mis-routing a frontend changeset to ``do`` (or a backend one to
``impeccable``) is a real behavioral bug, so each distinct routing branch keeps
one fixture row here.
"""
from __future__ import annotations

import subprocess
import sys
from pathlib import Path

import pytest

from fno.executor import _locked, _surface

_REPO_ROOT = Path(__file__).resolve().parents[3]


def _module_env() -> dict:
    import os

    env = dict(os.environ)
    src = str(_REPO_ROOT / "cli" / "src")
    env["PYTHONPATH"] = src + (os.pathsep + env["PYTHONPATH"] if env.get("PYTHONPATH") else "")
    return env


# ---------------------------------------------------------------------------
# Surface inference (_surface): one row per distinct branch
# ---------------------------------------------------------------------------

# (stdin, expected impeccable|do). Each entry is a newline-joined file list.
_SURFACE_FIXTURES: list[tuple[str, str]] = [
    ("src/Widget.jsx", "impeccable"),            # frontend extension
    ("components/Header.ts", "impeccable"),      # components/ dir
    ("pkg/routes/x.go", "impeccable"),           # routes/ dir
    ("src/styles/main.css", "impeccable"),       # styles/ dir
    ("app/main.py", "do"),                       # Python module root, NOT frontend
    ("src/Widget.vue", "do"),                    # .vue is NOT a locked surface
    ("", "do"),
    ("src/utils/format.ts\ncli/src/loop.py\nsrc/components/Foo.tsx\n", "impeccable"),  # any frontend wins
    # shell `components/*` matches the empty string; the port must too
    ("components/", "impeccable"),
]


@pytest.mark.parametrize("stdin,expected", _SURFACE_FIXTURES)
def test_surface_python_matches_expected(stdin: str, expected: str) -> None:
    """The Python module routes each fixture to the documented value."""
    paths = [ln for ln in stdin.split("\n") if ln != ""]
    matched = _surface.any_frontend_surface(paths)
    assert ("impeccable" if matched else "do") == expected


def test_surface_has_ui_flag() -> None:
    """--has-ui echoes true/false (the infer-has-ui.sh contract)."""
    for stdin, expected in [("src/components/Foo.tsx\n", "true"), ("app/main.py\n", "false"), ("", "false")]:
        py = subprocess.run(
            [sys.executable, "-m", "fno.executor._surface", "--has-ui"],
            input=stdin,
            capture_output=True,
            text=True,
            cwd=_REPO_ROOT,
            env={**_module_env()},
        )
        assert py.stdout.strip() == expected


# ---------------------------------------------------------------------------
# Locked-decision parser (_locked): one row per distinct branch
# ---------------------------------------------------------------------------

_LOCKED_FIXTURES: list[tuple[str, str]] = [
    (
        "## Locked Decisions (DO NOT revisit)\n\n"
        "1. **State machine**: explicit per-screen.\n"
        "2. **Executor routing**: plan-level `executor: impeccable` (auto-detected).\n"
        "   Rationale: frontend-only feature.\n",
        "impeccable",
    ),
    # the deleted parser's `do` is spelled `tdd` now
    ("## Locked Decisions\n\n1. **Executor routing**: plan-level `executor: do` (cli-flag).\n", "tdd"),
    ("# A design doc with no Locked Decisions section at all.\n\nJust prose.\n", ""),
    # casing and bold variations
    ("## Locked Decisions\n\n1. **executor ROUTING**: plan-level `Executor: Impeccable`\n", "impeccable"),
    # multiple entries: last wins
    (
        "## Locked Decisions\n\n"
        "1. **Executor routing**: plan-level `executor: do` (auto-detected).\n"
        "2. **Other thing**: foo.\n"
        "3. **Executor routing**: plan-level `executor: impeccable` (user-confirmed).\n",
        "impeccable",
    ),
    # unknown values rejected
    ("## Locked Decisions\n\n1. **Executor routing**: plan-level `executor: archer`.\n", ""),
    # section scoping: executor mention OUTSIDE Locked Decisions must NOT match
    (
        "# Some Doc\n\n"
        "The operator dispatches an `executor: impeccable` in some cases.\n\n"
        "## Locked Decisions\n\n"
        "1. **Auth model**: cookie-based.\n",
        "",
    ),
    # multi-line continuation buffer
    (
        "## Locked Decisions\n\n"
        "1. **Executor routing**:\n"
        "   plan-level `executor: impeccable`\n"
        "   because it is a frontend feature.\n",
        "impeccable",
    ),
]


@pytest.mark.parametrize("doc,expected", _LOCKED_FIXTURES)
def test_locked_python_matches_expected(doc: str, expected: str) -> None:
    assert _locked.parse_locked_executor(doc) == expected


# ── parse_locked_model (sibling of parse_locked_executor) ──────────────────────

def test_parse_locked_model_extracts_from_locked_decisions() -> None:
    doc = (
        "# Plan\n\n## Architecture\nWe discuss the model resolver here.\n\n"
        "## Locked Decisions\n1. **Model**: `fable` (user-confirmed)\n"
        "2. Executor: do\n"
    )
    assert _locked.parse_locked_model(doc) == "fable"


def test_parse_locked_model_ignores_prose_outside_section() -> None:
    # A bare `model:` outside Locked Decisions is prose, not a lock.
    doc = "## Overview\nmodel: fable\n\n## Locked Decisions\nExecutor: do\n"
    assert _locked.parse_locked_model(doc) == ""


def test_parse_locked_model_rejects_multitoken_and_metachars() -> None:
    # A spaced value must be REJECTED, not truncated to the first token (which
    # would transcribe a wrong-but-valid pin onto the node).
    assert _locked.parse_locked_model("## Locked Decisions\nModel: opus 4.8\n") == ""
    # A glob/shell metacharacter is out of the shell-safe charset -> rejected.
    assert _locked.parse_locked_model("## Locked Decisions\nModel: fo*\n") == ""


def test_locked_model_cli_key_flag_and_backward_compat() -> None:
    doc = "## Locked Decisions\nModel: fable\n"
    model_out = subprocess.run(
        [sys.executable, "-m", "fno.executor._locked", "--key", "model"],
        input=doc, capture_output=True, text=True, cwd=_REPO_ROOT, env={**_module_env()},
    )
    assert model_out.stdout.strip() == "fable"
    # No flag -> the executor parser (backward compatible): a Model line is
    # invisible to it, so it emits nothing. This proves --key routes the parser
    # rather than the default silently changing.
    exec_out = subprocess.run(
        [sys.executable, "-m", "fno.executor._locked"],
        input=doc, capture_output=True, text=True, cwd=_REPO_ROOT, env={**_module_env()},
    )
    assert exec_out.stdout.strip() == ""
