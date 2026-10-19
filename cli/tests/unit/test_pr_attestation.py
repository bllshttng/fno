"""`fno do pr attestation retract` - revocation that addresses the event, not
the identity.

Before the attester binding, the only revocation path was emitting a `fail`
under the SAME env markers as the pass, so undoing an impersonation required
performing it a second time. Once the binding refused that, a forged pass
became permanently irretractable. The verb names the revoked pair explicitly
and records the RETRACTING session's own identity.
"""
from __future__ import annotations

import json
import os
import subprocess
from pathlib import Path

import pytest
from typer.testing import CliRunner

from fno.pr.cli import pr_app

HEAD = "ab" * 20


def _pass_line(attester: str = "sess-A", reviewer: str = "code-review") -> str:
    return json.dumps(
        {
            "ts": "2026-08-25T00:00:00Z",
            "type": "review_attestation",
            "source": "target",
            "data": {
                "reviewer": reviewer,
                "head_sha": HEAD,
                "verdict": "pass",
                "session_id": "run-1",
                "attester_session_id": attester,
                "branch": "feature/x",
            },
        }
    )


@pytest.fixture
def runner() -> CliRunner:
    return CliRunner()


@pytest.fixture
def journal(tmp_path: Path) -> Path:
    events = tmp_path / ".fno" / "events.jsonl"
    events.parent.mkdir(parents=True)
    # Seed through the store: the reader answers committed rows, so a raw
    # journal write never reaches the history the verb asserts on.
    from fno.events import append_event

    append_event(json.loads(_pass_line()), events)
    return events


def _invoke(runner: CliRunner, journal: Path, *extra: str):
    return runner.invoke(
        pr_app,
        [
            "attestation",
            "retract",
            "--reviewer", "code-review",
            "--attester", "sess-A",
            "--head", HEAD,
            "--reason", "forged pass",
            "--events", str(journal),
            *extra,
        ],
    )


def test_retract_revokes_the_named_pair_and_records_the_retractor(
    runner: CliRunner, journal: Path, monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """An operator session that is NOT sess-A revokes sess-A's pass: coverage's
    key is the NAMED pair, and the event records the operator's own identity."""
    import fno.harness_identity as hi

    monkeypatch.setattr(hi, "resolve_attester_identity", lambda: ("sess-operator", "process"))
    monkeypatch.chdir(tmp_path)

    result = _invoke(runner, journal)
    assert result.exit_code == 0, result.stderr

    from tests._event_rows import event_rows

    rows = event_rows(journal)
    assert len(rows) == 2
    data = next(r["data"] for r in rows if "retracts_attester" in r["data"])
    assert data["verdict"] == "fail"
    assert data["retracts_attester"] == "sess-A"
    assert data["attester_session_id"] == "sess-operator"
    assert data["attester_witness"] == "process"
    assert data["retraction_reason"] == "forged pass"
    assert data["head_sha"] == HEAD


def test_retract_refuses_when_no_matching_pass_exists(
    runner: CliRunner, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """No pass for the named triple -> non-zero, says so, writes nothing: a
    revocation must never land for a pair that never passed."""
    import fno.harness_identity as hi

    monkeypatch.setattr(hi, "resolve_attester_identity", lambda: ("sess-operator", "process"))
    # find_pass also scans the machine-global journal; point it at an empty
    # file so the refusal is this project log's, not whatever runs on the host.
    monkeypatch.setattr(
        "fno.paths.global_events_json", lambda: tmp_path / "global-events.jsonl"
    )
    events = tmp_path / ".fno" / "events.jsonl"
    events.parent.mkdir(parents=True)
    events.write_text(_pass_line(attester="sess-SOMEONE-ELSE") + "\n")

    result = _invoke(runner, events)
    assert result.exit_code == 1
    assert "no passing attestation" in result.stderr
    assert len(events.read_text().splitlines()) == 1


def test_retract_refuses_the_identity_override_shape(
    runner: CliRunner, journal: Path, monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """A retracting process whose env contradicts its ancestry is refused, same
    as a pass: revocation under a forged identity is the same forgery."""
    import fno.harness_identity as hi

    def boom():
        raise hi.AttesterIdentityConflict("CODEX_THREAD_ID", "sess-true", "sess-forged")

    monkeypatch.setattr(hi, "resolve_attester_identity", boom)
    monkeypatch.chdir(tmp_path)

    result = _invoke(runner, journal)
    assert result.exit_code == 1
    assert "sess-true" in result.stderr and "sess-forged" in result.stderr
    # The refusal emits nothing: the store keeps exactly the seeded pass.
    from tests._event_rows import event_rows

    assert len(event_rows(journal)) == 1


def test_retract_mirrors_the_revocation_to_the_global_log(
    tmp_path, monkeypatch, capsys
):
    """The pass being revoked reached the machine-global log on emit
    (review_attestation rides GLOBAL_MIRROR_TYPES); a retraction appended to
    the project log alone leaves the revoked pass live for every global-log
    reader, which is the forgery the verb exists to undo."""
    import json as _json

    from fno.pr import _attestation

    project = tmp_path / ".fno" / "events.jsonl"
    project.parent.mkdir(parents=True)
    head = "aaaa1111bbbb2222"
    project.write_text(
        _json.dumps(
            {
                "ts": "2026-08-25T00:00:00Z",
                "type": "review_attestation",
                "data": {
                    "reviewer": "code-review",
                    "head_sha": head,
                    "verdict": "pass",
                    "attester_session_id": "sess-author",
                    "branch": "feature/x",
                },
            }
        )
        + "\n",
        encoding="utf-8",
    )
    mirrored_rows: list = []

    def fake_mirror(event, resolved_events, repo_root):
        mirrored_rows.append(event)

    import fno.harness_identity

    monkeypatch.setattr(fno.harness_identity, "resolve_attester_identity", lambda: ("s", "process"))
    monkeypatch.setattr("fno.events.cli.mirror_to_global_log", fake_mirror)
    rc = _attestation.retract(
        "code-review", "sess-author", head, "forged", events=project
    )
    capsys.readouterr()
    assert rc == 0
    assert mirrored_rows, "the retraction must be mirrored like the pass it revokes"
    assert mirrored_rows[0]["data"]["retracts_attester"] == "sess-author"


def test_attestation_from_canonical_emits_in_the_pr_worktree(tmp_path: Path):
    canonical = tmp_path / "canonical"
    feature = tmp_path / "feature-worktree"
    canonical.mkdir()
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=canonical, check=True)
    subprocess.run(["git", "config", "user.email", "t@t.t"], cwd=canonical, check=True)
    subprocess.run(["git", "config", "user.name", "t"], cwd=canonical, check=True)
    subprocess.run(["git", "commit", "-q", "--allow-empty", "-m", "base"], cwd=canonical, check=True)
    base = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=canonical, capture_output=True, text=True, check=True
    ).stdout.strip()
    subprocess.run(
        ["git", "update-ref", "refs/remotes/origin/main", base], cwd=canonical, check=True
    )
    subprocess.run(
        ["git", "worktree", "add", "-q", "-b", "feature/pr-42", str(feature)],
        cwd=canonical,
        check=True,
    )
    (feature / "reviewed.py").write_text("answer = 42\n", encoding="utf-8")
    subprocess.run(["git", "add", "reviewed.py"], cwd=feature, check=True)
    subprocess.run(["git", "commit", "-q", "-m", "reviewed change"], cwd=feature, check=True)

    fake_bin = tmp_path / "bin"
    fake_bin.mkdir()
    fake_agents = fake_bin / "fno-agents"
    fake_agents.write_text(
        "#!/bin/sh\ncat >/dev/null\nprintf '{\"worktree\":\"%s\"}\\n' \"$FNO_WORKTREE\"\n",
        encoding="utf-8",
    )
    fake_agents.chmod(0o755)
    resolver = Path(__file__).parents[3] / "skills/review/scripts/resolve-pr-worktree.sh"
    resolved = subprocess.run(
        ["bash", str(resolver), "42"],
        cwd=canonical,
        env={
            **os.environ,
            "PATH": f"{fake_bin}:{os.environ['PATH']}",
            "FNO_AGENTS_BIN": str(fake_agents),
            "FNO_WORKTREE": str(feature),
        },
        capture_output=True,
        text=True,
        check=False,
    )
    assert resolved.returncode == 0, resolved.stderr
    assert resolved.stdout.strip() == str(feature)
    fno_capture = tmp_path / "fno-calls.txt"
    fake_fno = fake_bin / "fno"
    fake_fno.write_text(
        "#!/bin/sh\nprintf '%s\\n' \"$PWD\" >> \"$FNO_CAPTURE\"\n"
        "case \"$*\" in 'do pr review-hold metadata'*) printf '{}\\n' ;; esac\n"
        "exit 0\n",
        encoding="utf-8",
    )
    fake_fno.chmod(0o755)
    script = Path(__file__).parents[3] / "skills/review/scripts/emit-attestation.sh"
    result = subprocess.run(
        ["bash", str(script), "code-review"],
        cwd=feature,
        env={
            **os.environ,
            "PATH": f"{fake_bin}:{os.environ['PATH']}",
            "FNO": str(fake_fno),
            "FNO_CAPTURE": str(fno_capture),
        },
        capture_output=True,
        text=True,
        check=False,
    )

    assert result.returncode == 0, result.stderr
    assert "branch=feature/pr-42" in result.stderr
    assert fno_capture.read_text(encoding="utf-8").splitlines()
    assert set(fno_capture.read_text(encoding="utf-8").splitlines()) == {str(feature)}
