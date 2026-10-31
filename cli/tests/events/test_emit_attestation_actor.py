"""emit-attestation.sh records the attesting actor on review_attestation.

The producer resolves session_id + harness from the live session manifest so a
review_attestation carries WHO certified the diff alongside WHAT. Without it an
author attesting its own diff is indistinguishable from an independent reviewer.
This is the producer-level check; the validator-level accept/reject lives in
test_python_validator.py and the parity corpus.
"""
from __future__ import annotations

import json
import os
import subprocess
from pathlib import Path

from fno.events.store_client import read_committed_lines

_REPO_ROOT = Path(__file__).resolve().parents[3]
_SCRIPT = _REPO_ROOT / "skills" / "review" / "scripts" / "emit-attestation.sh"


def _temp_git_repo(tmp_path: Path, manifest: str | None) -> Path:
    """A throwaway git repo; optionally a session manifest.

    The fixture carries a base commit on origin/main plus a feature commit
    with a real change, because the emitter measures the diff under review
    and refuses a zero-line one - an empty-commit fixture trips that refusal
    on every emit below.
    """
    sub = tmp_path / "repo"
    sub.mkdir()
    for args in (
        ["git", "init", "-q"],
        ["git", "config", "user.email", "t@t.t"],
        ["git", "config", "user.name", "t"],
        ["git", "commit", "-q", "--allow-empty", "-m", "init"],
    ):
        subprocess.run(args, cwd=sub, check=True)
    base = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=sub, check=True, capture_output=True, text=True
    ).stdout.strip()
    subprocess.run(
        ["git", "update-ref", "refs/remotes/origin/main", base], cwd=sub, check=True
    )
    (sub / "a.txt").write_text("body\n")
    subprocess.run(["git", "add", "a.txt"], cwd=sub, check=True)
    subprocess.run(["git", "commit", "-qm", "feature"], cwd=sub, check=True)
    if manifest is not None:
        (sub / ".fno").mkdir()
        (sub / ".fno" / "target-state.md").write_text(manifest)
    return sub


def _events_file(repo: Path) -> Path:
    """The journal the emitter resolves: the repo's space, via the accessor."""
    from fno.paths import project_log

    return project_log("events.jsonl", project_root=repo)


def _last_event(repo: Path) -> dict:
    lines = [
        ln for ln in read_committed_lines(_events_file(repo))
        if ln.strip()
    ]
    return json.loads(lines[-1])


def test_attestation_records_session_and_harness(tmp_path: Path) -> None:
    """A manifest present -> the emitted attestation carries the actor."""
    manifest = (
        "session_id: 20260806T225503Z-cl84104-d4f619\n"
        "harness: claude\n"
    )
    repo = _temp_git_repo(tmp_path, manifest)
    env = {**os.environ, "FNO": os.environ.get("FNO_BIN", "fno")}
    r = subprocess.run(
        ["bash", str(_SCRIPT), "sigma", "pass"],
        cwd=repo, env=env, capture_output=True, text=True,
    )
    assert r.returncode == 0, r.stderr
    ev = _last_event(repo)
    assert ev["type"] == "review_attestation"
    assert ev["data"]["session_id"] == "20260806T225503Z-cl84104-d4f619"
    assert ev["data"]["harness"] == "claude"


def test_attestation_adopts_sidecar_by_harness_session_id(tmp_path: Path) -> None:
    """The sidecar filename key is the manifest harness session id."""
    manifest = (
        "session_id: target-run\n"
        "harness_session_id: harness-run\n"
        "harness: claude\n"
    )
    repo = _temp_git_repo(tmp_path, manifest)
    home = tmp_path / "home"
    sidecar_dir = home / "review-invocations"
    sidecar_dir.mkdir(parents=True)
    expected = "ri-" + "a" * 32
    (sidecar_dir / "harness-run.json").write_text(
        json.dumps({"invocation_id": expected, "target_session_id": "harness-run"}),
        encoding="utf-8",
    )
    env = {**os.environ, "FNO": os.environ.get("FNO_BIN", "fno"), "FNO_HOME": str(home)}
    r = subprocess.run(
        ["bash", str(_SCRIPT), "code-review", "pass"],
        cwd=repo, env=env, capture_output=True, text=True,
    )
    assert r.returncode == 0, r.stderr
    events = [
        json.loads(line)
        for line in read_committed_lines(_events_file(repo))
        if line.strip()
    ]
    started = [
        event for event in events
        if event["type"] == "review_invocation"
        and event["data"].get("stage") == "started"
    ][-1]
    assert started["data"]["invocation_id"] == expected
    assert _last_event(repo)["data"]["invocation_id"] == expected


def _env_without_ambient_identity(**extra: str) -> dict:
    """The subprocess env with every ambient session marker scrubbed, so the
    transcript read deterministically answers not-observable on any machine -
    CI has no markers anyway, but a developer shell runs under a live harness
    session whose transcript WOULD resolve, and the assertion below must not
    depend on where pytest ran."""
    markers = (
        "CODEX_THREAD_ID",
        "CLAUDE_CODE_SESSION_ID",
        "CODEX_SESSION_ID",
        "GEMINI_SESSION_ID",
        "OPENCODE_SESSION_ID",
        "CLAUDE_SESSION_ID",
        "FNO_HARNESS_NAME",
        "FNO_HARNESS_SESSION_ID",
    )
    env = {
        k: v for k, v in os.environ.items()
        if k not in ("ANTHROPIC_MODEL", "ANTHROPIC_BASE_URL") + markers
    }
    env["FNO"] = os.environ.get("FNO_BIN", "fno")
    env.update(extra)
    return env


def test_attestation_drops_the_routed_env_model_claim(tmp_path: Path) -> None:
    """The routed env names the model the session ASKED for, never the one that
    answered - a non-Anthropic name over an unset base URL silently falls back
    to the primary model. The emitter passes no model; the emit chokepoint
    stamps the transcript, so an unresolvable transcript means the env claim is
    DROPPED, not laundered into the record. provider stays an honest env fact."""
    repo = _temp_git_repo(tmp_path, "session_id: s\nharness: claude\n")
    env = _env_without_ambient_identity(
        ANTHROPIC_MODEL="glm-5.2[1m]",
        # Userinfo included on purpose: a base_url may carry a credential, and
        # the event log is durable - neither the path nor the key may land in it.
        ANTHROPIC_BASE_URL="https://sk-secret@api.z.ai/api/anthropic",
    )
    r = subprocess.run(
        ["bash", str(_SCRIPT), "code-review", "pass"],
        cwd=repo, env=env, capture_output=True, text=True,
    )
    assert r.returncode == 0, r.stderr
    data = _last_event(repo)["data"]
    assert "model" not in data
    assert data["provider"] == "api.z.ai"
    assert "sk-secret" not in json.dumps(data) and "sk-secret" not in r.stderr


def test_a_flag_shaped_reviewer_emits_nothing(tmp_path: Path) -> None:
    """`--help` must not write gate evidence.

    `$1` was taken as the reviewer name with no validation, so
    `emit-attestation.sh --help` emitted a real `verdict=pass`
    review_attestation against the current HEAD: ASKING THE SCRIPT HOW TO RUN
    IT stamped the merge gate. Any typo'd or stray flag did the same. No
    reviewer name in any registry begins with `-`, so the whole shape is
    refused rather than the one spelling.
    """
    repo = _temp_git_repo(tmp_path, "session_id: s\nharness: claude\n")
    env = {**os.environ, "FNO": os.environ.get("FNO_BIN", "fno")}

    helped = subprocess.run(
        ["bash", str(_SCRIPT), "--help"],
        cwd=repo, env=env, capture_output=True, text=True,
    )
    assert helped.returncode == 0
    assert "Usage:" in helped.stdout

    flagged = subprocess.run(
        ["bash", str(_SCRIPT), "--bogus"],
        cwd=repo, env=env, capture_output=True, text=True,
    )
    assert flagged.returncode == 2
    assert "not a reviewer name" in flagged.stderr

    # The positive control: neither run wrote a journal, and a REAL reviewer
    # name still does. Without this half, a script that refused everything
    # would pass the two assertions above.
    assert not (repo / ".fno" / "events.jsonl").exists()
    ok = subprocess.run(
        ["bash", str(_SCRIPT), "code-review", "pass"],
        cwd=repo, env=env, capture_output=True, text=True,
    )
    assert ok.returncode == 0, ok.stderr
    assert _last_event(repo)["data"]["reviewer"] == "code-review"
