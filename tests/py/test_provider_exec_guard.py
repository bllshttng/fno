"""Proof that the provider-exec guard covers tests OUTSIDE cli/tests/agents/.

The old guard lived in ``cli/tests/agents/conftest.py`` and a conftest guards
only its own directory, so 36 test files at or below the ``cli/tests/`` root
reached a spawn seam unguarded (x-ec81). This file therefore sits at the
``cli/tests/`` root ON PURPOSE: its location is the evidence for root
coverage. Moving it under ``agents/`` would prove nothing.
"""
from __future__ import annotations

import shutil
import subprocess
import tempfile
from pathlib import Path

import pytest


def _write_fake_provider(bin_dir: Path, name: str = "claude") -> Path:
    fake = bin_dir / name
    fake.write_text("#!/bin/sh\necho fake-provider\n")
    fake.chmod(0o755)
    return fake


def test_guard_blocks_real_provider_outside_basetemp(monkeypatch):
    """A fake claude that resolves OUTSIDE the pytest tmp tree is a real
    install as far as the discriminator is concerned, so subprocess.run must
    raise before any process starts."""
    bin_dir = Path(tempfile.mkdtemp(prefix="fno-guard-proof-"))
    try:
        _write_fake_provider(bin_dir)
        monkeypatch.setenv("PATH", str(bin_dir))
        with pytest.raises(AssertionError, match="live provider exec blocked"):
            subprocess.run(["claude", "--version"], capture_output=True, text=True)
    finally:
        shutil.rmtree(bin_dir, ignore_errors=True)
