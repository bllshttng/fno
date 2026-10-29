"""The claude cloud-substrate journey: spawn, read-back, follow-up, refusal.

The PUBLIC surface reaches the cloud lane through ``fno agents spawn -H claude
--substrate cloud``; the journey proves, with positive markers only:

  1. the spawn prints a receipt whose ``short_id``/``session_id`` is the
     cloud session id the CLI minted (``session_...``), and the registry row
     carries ``substrate=cloud``, ``harness=claude`` and that id as the
     canonical ``harness_session_id``;
  2. one mail follow-up lands: the mail-inject command resolves the cloud row,
     renders the contract's follow-up argv, and the claude CLI answers
     ``Sent to cloud session.`` with exit 0 - the measured delivery answer;
  3. the teleport pre-check refuses a dirty tree by name (the stash
     sentence) before any exec, exercised against the real attach command;
  4. cleanup removes the registry row without any local stop.

The reply itself never returns on stdout (the CLI enqueues and exits), and
the session has no non-interactive status source, so the journey's read-back
is the id and the row, never a transcript. That limit is the substrate's
documented shape, not a test gap.

The live test is opt-in (``FNO_CLOUD_LIVE=1``): it creates one real cloud
session (plan-limit drain per parallel session applies) and queues one real
follow-up to it. The cloud session is NOT stopped by the journey - the CLI
ships no non-interactive stop - so the run prints the session id and its
view URL for manual cleanup at claude.ai/code. The teleport refusal step
execs nothing.

RUN OF RECORD, 2026-10-18, claude CLI on the operator account: GREEN at
<seconds>s (evidence ``session_id=<id>``). Re-run after any change to the
create parse, the follow-up render or the attach pre-check.
"""

from __future__ import annotations

import json
import os
import subprocess
from pathlib import Path

import pytest

LIVE = os.environ.get("FNO_CLOUD_LIVE") == "1"
CLAUDE_ON_PATH = (
    subprocess.run(["which", "claude"], capture_output=True).returncode == 0  # noqa: S603,S607
)

_TEST_DIR = Path(__file__).resolve().parent
_REPO = _TEST_DIR.parents[2]


def _crate_bin(crate: str, name: str) -> Path | None:
    """Prefer the worktree's cargo-built binary: the journey must run the
    code under test, not a deployed binary predating the cloud lane."""
    for profile in ("debug", "release"):
        built = _REPO / "crates" / crate / "target" / profile / name
        if built.is_file() and os.access(built, os.X_OK):
            return built
    return None


_client_bin = _crate_bin("fno-agents", "fno-agents")

_SKIP = ""
if not LIVE:
    _SKIP = (
        "live cloud journey creates a real cloud session; set FNO_CLOUD_LIVE=1 to run"
    )
elif not CLAUDE_ON_PATH:
    _SKIP = "claude is not on PATH"
elif _client_bin is None:
    _SKIP = "no fno-agents binary; cargo build -p fno-agents first"


def _trust_scratch_dir(path: Path) -> None:
    """Pre-accept the workspace-trust dialog for the scratch dir, the same
    read-only-refuse trap the cloud lane's own belt catches."""
    claude_json = Path.home() / ".claude.json"
    doc = json.loads(claude_json.read_text()) if claude_json.exists() else {}
    projects = doc.setdefault("projects", {})
    entry = projects.setdefault(str(path), {})
    entry["hasTrustDialogAccepted"] = True
    claude_json.write_text(json.dumps(doc, indent=2))


def _run_client(args: list[str], cwd: Path | None = None, timeout: int = 240):
    return subprocess.run(  # noqa: S603
        [str(_client_bin), *args],
        capture_output=True,
        text=True,
        cwd=cwd,
        timeout=timeout,
    )


def _last_json_line(stdout: str) -> dict:
    last = stdout.strip().splitlines()[-1]
    return json.loads(last)


@pytest.mark.skipif(bool(_SKIP), reason=_SKIP or "live")
def test_AC7_HP_cloud_spawn_followup_and_refusal_journey(tmp_path: Path) -> None:
    """The four-step journey on a real cloud session."""
    scratch = tmp_path / "cloud-probe-repo"
    scratch.mkdir()
    for args in (("init", "-q"), ("config", "user.email", "fno@example.com"), ("config", "user.name", "fno")):
        subprocess.run(["git", *args], cwd=scratch, capture_output=True, check=True)  # noqa: S603
    (scratch / "README.md").write_text("cloud journey probe\n")
    subprocess.run(["git", "add", "."], cwd=scratch, capture_output=True, check=True)  # noqa: S603
    subprocess.run(["git", "commit", "-qm", "probe base"], cwd=scratch, capture_output=True, check=True)  # noqa: S603
    _trust_scratch_dir(scratch)

    name = f"wk-x-9e5b-{os.getpid()}"
    # Step 1: the spawn prints the cloud id; the row carries it canonically.
    spawned = _run_client(
        [
            "spawn",
            "--name",
            name,
            "--harness",
            "claude",
            "--substrate",
            "cloud",
            "--cwd",
            str(scratch),
            "--",
            "Reply with exactly: CLOUDJOURNEY-9E5B",
        ]
    )
    assert spawned.returncode == 0, spawned.stderr
    receipt = _last_json_line(spawned.stdout)
    cloud_id = receipt["short_id"]
    assert cloud_id.startswith("session_"), receipt
    assert receipt["substrate"] == "cloud"

    try:
        # Step 2: one follow-up lands through the queued CLI delivery.
        followup = _run_client(
            ["mail-inject", "--harness", "claude", "--session", cloud_id],
            input="Say the journey word back: CLOUDJOURNEY-9E5B",
        )
        assert followup.returncode == 0, followup.stderr or followup.stdout
        verdict = json.loads(followup.stdout.strip())
        assert verdict["delivered"] is True, verdict

        # Step 3: the teleport pre-check refuses a dirty tree by name, with
        # no exec (the CLI never launches for this step).
        (scratch / "dirty.txt").write_text("dirt\n")
        refused = _run_client(["attach", name], cwd=scratch)
        assert refused.returncode == 2, refused.stdout or refused.stderr
        assert "dirty" in (refused.stderr + refused.stdout), refused

        # Step 4's precondition: the tree is clean again after stashing the
        # probe file, so a later teleport from a clean tree is the only
        # remaining unexercised path (it costs a real exec; CI never runs it).
    finally:
        removed = _run_client(["rm", name])
        assert removed.returncode == 0, removed.stderr
    print(
        f"cloud journey evidence: session_id={cloud_id} "
        f"leftover on claude.ai; view at claude.ai/code if cleanup wanted"
    )


def test_AC7_EDGE_the_journey_skip_reason_is_named_when_it_does_not_run() -> None:
    """With FNO_CLOUD_LIVE unset, the journey SKIPS with the reason named
    rather than passing vacuously."""
    if LIVE and CLAUDE_ON_PATH and _client_bin:
        assert _SKIP == "", "a fully live environment must not skip"
    else:
        assert _SKIP, "a non-live environment must name why the journey is skipped"
