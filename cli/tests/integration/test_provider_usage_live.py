"""Opt-in live positive-marker gate for the native usage command.

The refresh moved to the Rust leg (accounts/usage.rs), so this gate drives
``fno-agents spawn-overlay accounts usage --refresh --json`` end to end. The
probe parsers in fno.adapters.providers.usage stay only as the parity oracle;
their fail-open contract is pinned by the unit tests in
fno/adapters/providers/test_usage.py.
"""
from __future__ import annotations

import os
import time
from pathlib import Path

import pytest


def test_live_provider_usage_has_numeric_usage_and_future_resets(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """AC4/AC6/AC7: live refreshes must produce real reset-bearing windows."""
    if os.environ.get("FNO_LIVE_PROVIDER_USAGE") != "1":
        pytest.skip("set FNO_LIVE_PROVIDER_USAGE=1 to run live provider probes")

    user = os.environ.get("USER", "")
    real_home = next(
        (
            candidate
            for candidate in (
                Path("/Users") / user,
                Path("/home") / user,
                Path("/root"),
            )
            if candidate.is_dir()
        ),
        None,
    )
    assert real_home is not None, "live provider gate could not locate the real HOME"
    monkeypatch.setenv("HOME", str(real_home))
    monkeypatch.setenv("USERPROFILE", str(real_home))

    from fno.adapters.providers.loader import load_providers
    from fno.rust_binary import call_binary_json

    # Avoid the repository's project-local accounts block shadowing the
    # operator's global machine records used by this opt-in test.
    global_only_root = Path("/tmp")
    records = load_providers(repo_root=global_only_root).by_id
    required = ("zai", "readyrule", "makers", "codex-main")
    missing = [account_id for account_id in required if account_id not in records]
    assert not missing, f"live account records missing: {missing}"

    now = time.time()
    observations = {}
    for account_id in required:
        err, rows = call_binary_json(
            "spawn-overlay", ("accounts", "usage", "--refresh", "--json")
        )
        assert err is None, err
        row = rows[account_id]
        assert row.get("windows"), (
            f"{account_id}: live probe did not produce a positive marker "
            f"({row.get('reason')})"
        )
        observations[account_id] = row["windows"]
        for window in row["windows"]:
            assert isinstance(window["used_pct"], (int, float))
            assert not isinstance(window["used_pct"], bool)
            assert 0 <= window["used_pct"] <= 100
            assert isinstance(window["resets_at"], (int, float))
            assert not isinstance(window["resets_at"], bool)
            assert window["resets_at"] > now

    assert any(window["label"] == "5h" for window in observations["zai"])
