"""Hermetic isolation for the tests living under ``cli/src/fno/``.

This is a SEPARATE pytest root from ``cli/tests/``, which is the trap: a scrub
applied over there protects nothing here. A targeted ``pytest cli/src/fno/...``
reaches target_cli.py and the carveout/done/log readers with the live session's
markers still set, and resolves the identity of the session running the tests.

Both trees apply the SAME native env (the ``test-run env`` door on the
``fno-agents`` binary), so they cannot drift on what counts as ambient - which
is the failure that produced three specimens through three different channels
on 2026-08-11.

Applied at module load rather than as a fixture, because a fixture is too late
for anything that reads a marker at import time.

Tests that intentionally exercise the global-fallback path (e.g.
``test_global_active_combo_falls_back_when_no_project_override``) opt out
per-test with ``monkeypatch.delenv("FNO_GLOBAL_SETTINGS_PATH", raising=False)``
before redirecting ``HOME``; that still works, since the door pins
``FNO_GLOBAL_SETTINGS_PATH`` in ``os.environ`` exactly as the retired
Python ``neutralise`` did.
"""
from __future__ import annotations

import os
import tempfile
from pathlib import Path

import pytest

from fno.test_env_door import native_env

_SANDBOX = tempfile.mkdtemp(prefix="fno-src-test-sandbox-")
_hermetic_env = native_env("neutralise", sandbox=Path(_SANDBOX))
os.environ.clear()
os.environ.update(_hermetic_env)


@pytest.fixture(autouse=True, scope="session")
def _config_search_ceiling(tmp_path_factory: pytest.TempPathFactory):
    """Widen the config ceiling to include the pytest basetemp.

    Both trees need this and for the same reason, so both carry it: tests here
    write project-local and global settings files under ``tmp_path``, and the
    ceiling the door sets covers only the sandbox. Adding it to one tree
    and not the other is how three tests in this file went red while the
    cli/tests tree stayed green - the same one-of-N-paths shape this whole
    change exists to remove.
    """
    basetemp = str(tmp_path_factory.getbasetemp())
    previous = os.environ.get("FNO_CONFIG_SEARCH_ROOT", "")
    os.environ["FNO_CONFIG_SEARCH_ROOT"] = os.pathsep.join(
        [basetemp, previous] if previous else [basetemp]
    )
    yield
    os.environ["FNO_CONFIG_SEARCH_ROOT"] = previous


def pytest_sessionfinish(session, exitstatus) -> None:  # noqa: ANN001
    import shutil

    shutil.rmtree(_SANDBOX, ignore_errors=True)
    # Sweep AFTER the rmtree: the only ordering that catches the keepers
    # that were serving the just-deleted sandbox graphs. Native door, same
    # reason the env is a door: one sweep, never a second implementation.
    # FNO_AGENTS_BIN outranks PATH (the claim-door contract): the smoke
    # pytest legs run with the dev build scrubbed and the pin alive.
    # FNO_AGENTS_FRONT sits between them: the CI job's declared build, which
    # a gate step beside the checkout has no PATH install for.
    import os
    import subprocess

    pinned = os.environ.get("FNO_AGENTS_BIN") or ""
    front = os.environ.get("FNO_AGENTS_FRONT") or ""
    binary: str | None
    if pinned and Path(pinned).is_file():
        binary = pinned
    elif front and Path(front).is_file():
        binary = front
    else:
        binary = shutil.which("fno-agents")
    if binary is not None:
        subprocess.run([binary, "test-run", "sweep", "15.0"], check=False)


@pytest.fixture(autouse=True)
def _in_memory_hold_verdict(tmp_path, monkeypatch):
    from fno.tests.hold_verdict_adapter import install

    install(tmp_path, monkeypatch)
