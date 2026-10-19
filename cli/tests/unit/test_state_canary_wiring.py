"""The smoke runner's state-canary bracket, and its refusal semantics.

`scripts/ci/check-state-canary.sh self-test` proves the SCRIPT can go red. This
file proves the RUNNER acts on that: a verify that refuses must turn a run whose
every step passed into a non-zero exit, and a verify that cannot run at all must
refuse rather than fall through to a green.

Every claim is a positive marker. "The runner did not crash" proves nothing
about whether it read the canary, so no test here treats a zero exit as
evidence on its own.
"""

import os

from fno import test_cmd


def _script(root, body="exit 0"):
    path = root / "scripts" / "ci" / "check-state-canary.sh"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(f"#!/usr/bin/env bash\n{body}\n", encoding="utf-8")
    path.chmod(0o755)
    return path


def test_a_missing_script_refuses_on_verify_and_passes_on_plant(tmp_path):
    """A verify that cannot run must never read as a green.

    plant is the other way round: nothing is measured yet, so a missing script
    costs the run nothing and the verify half is where it is caught.
    """
    assert test_cmd._run_state_canary(tmp_path, "verify") == 1
    assert test_cmd._run_state_canary(tmp_path, "plant") == 0


def test_a_refusing_verify_is_relayed_as_a_nonzero_return(tmp_path):
    """The script's exit code is the runner's, not a swallowed advisory."""
    _script(tmp_path, "exit 1")
    assert test_cmd._run_state_canary(tmp_path, "verify") == 1


def test_the_canary_runs_on_the_parent_home_not_the_sandbox(tmp_path, monkeypatch):
    """The sandbox is what the suite may write; the parent HOME is the surface.

    Handing the canary `_smoke_env` would point it at the sandbox, where it
    would pass forever while the real root went unwatched. The receipt is the
    HOME the child actually saw.
    """
    _script(tmp_path, 'echo "$HOME" > "$FNO_STATE_CANARY_SNAPSHOT.home"')
    monkeypatch.setenv("HOME", str(tmp_path / "parent-home"))
    snapshot = test_cmd._state_canary_snapshot()
    assert test_cmd._run_state_canary(tmp_path, "plant") == 0
    seen = open(f"{snapshot}.home", encoding="utf-8").read().strip()
    assert seen == str(tmp_path / "parent-home")
    os.remove(f"{snapshot}.home")
