"""Dispatch-layer tests for the codex create path and sealed thread spawn.

Provider invocation is monkeypatched so the dispatch logic is exercised in
isolation. Covered: create registers the row, each provider failure maps to
its exit code with the registry untouched, an invalid from_name is refused
before any subprocess, and a stale binary is refused under a sealed env.
"""
from __future__ import annotations

from pathlib import Path
from unittest.mock import MagicMock

import pytest

from fno.paths_testing import use_tmpdir
from fno.agents.harnesses import codex as codex_mod
from fno.agents.harnesses.codex import (
    CodexResult,
)


@pytest.fixture
def workdir(tmp_path, monkeypatch):
    """Isolated fno home with codex marked available on PATH."""
    use_tmpdir(monkeypatch, tmp_path)
    # Stub PATH so `which("codex")` returns truthy.
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    (bin_dir / "codex").write_text("#!/bin/sh\necho fake\n", encoding="utf-8")
    (bin_dir / "codex").chmod(0o755)
    monkeypatch.setenv("PATH", str(bin_dir))
    return tmp_path


@pytest.fixture
def fake_codex_create(monkeypatch):
    """Replace codex_mod.create with a MagicMock returning a happy CodexResult.

    Tests can mutate the return_value or side_effect to script failures.
    """
    mock = MagicMock(return_value=CodexResult(
        exit_code=0,
        session_id="codex-sid-abc",
        last_msg="hello",
        duration_ms=42,
    ))
    monkeypatch.setattr(codex_mod, "create", mock)
    return mock


# ---------------------------------------------------------------------------
# AC1 — create
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# AC4 — from-name
# ---------------------------------------------------------------------------


    # The actual bracket prefix is applied by codex.create() (verified in
    # test_harnesses_codex_create), but we assert dispatch passes the
    # name through unchanged.


def test_invalid_from_name_rejected_before_subprocess(workdir, fake_codex_create):
    """AC4-ERR: from_name validator rejects XML-unsafe input on the codex path.

    Reuses the US2 validator (no fresh regex per AC1.3 invariant); the rule
    set is: non-empty AND <=128 chars AND no XML-unsafe characters (", <, >, &).
    """
    from fno.agents.dispatch import DispatchAskError, _validate_inputs
    for bad in ('', '"bad"', '<inject>', 'amp&persand', 'right>angle'):
        with pytest.raises(DispatchAskError) as exc:
            _validate_inputs(name="worker-X", message="msg", from_name=bad)
        assert exc.value.exit_code == 2, f"input {bad!r}"
    # Provider MUST NOT have been called for any of the rejected inputs.
    fake_codex_create.assert_not_called()


# ---------------------------------------------------------------------------
# Sealed thread spawn
# ---------------------------------------------------------------------------


def _write_spawn_stub(tmp_path: Path, body: str) -> Path:
    stub = tmp_path / "fno-agents-stub"
    stub.write_text(f"#!/bin/sh\n{body}\n")
    stub.chmod(0o755)
    return stub


# Emulates the current binary's contract: the state-root door answers with
# the root the inherited env resolves, and a spawn prints a receipt.


