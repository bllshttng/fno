"""Unit tests for the composed ``--settings`` sandbox payload (x-6ac1).

There is ONE ``--settings`` flag, so the sandbox write policy must compose
into the same file the route/auth floor already rides: a second file would
silently drop the first. Widening ``_write_settings_env_file`` to a full
payload keeps content addressing (a per-worker sandbox block naturally yields
a distinct digest) and the 0600 tmp-then-``os.replace`` mechanics.

The parity rule under test: a spawn with no write policy produces the SAME
settings path and argv as before this feature existed, and a route + policy
spawn keeps the route's credential keys byte-identical to a route-only spawn.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from fno.paths_testing import use_tmpdir

SANDBOX = {
    "enabled": True,
    "filesystem": {"allowWrite": ["/wt/src/a.py", "/wt/.git/"]},
}


def test_policy_without_route_still_writes_the_scrub_floor(tmp_path, monkeypatch):
    """A sandbox-only spawn still owes the auth floor: it is the one channel
    that survives the daemon fork, sandbox or not. No overlay and no policy
    writes nothing at all."""
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents.account_env import SCRUB_AUTH_VARS
    from fno.agents.model_routing import route_settings_path_for

    path = route_settings_path_for(None, None, env={}, sandbox=SANDBOX)
    assert path is not None
    payload = json.loads(Path(path).read_text())
    assert payload["sandbox"] == SANDBOX
    assert all(payload["env"][var] == "" for var in SCRUB_AUTH_VARS)

    assert route_settings_path_for(None, None, env={}, sandbox=None) is None


def test_load_sandbox_write_policy_reads_the_sandbox_block(tmp_path, monkeypatch):
    """The spawn CLI flag takes a policy FILE and fails closed on a bad one."""
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents.model_routing import load_sandbox_write_policy

    good = tmp_path / "policy.json"
    good.write_text(json.dumps({"band": "high", "sandbox": SANDBOX}))
    assert load_sandbox_write_policy(good) == SANDBOX

    for bad_payload in ('{"band": "high"}', "not json"):
        bad = tmp_path / "bad.json"
        bad.write_text(bad_payload)
        with pytest.raises(SystemExit):
            load_sandbox_write_policy(bad)
    with pytest.raises(SystemExit):
        load_sandbox_write_policy(tmp_path / "missing.json")
