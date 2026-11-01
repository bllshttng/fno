"""Native event emit contracts at the public command boundary."""
from __future__ import annotations

import json
import sys
from pathlib import Path

import pytest

from fno.events.store_client import read_committed_lines
from tests._native_event_cli import EventRunner


@pytest.fixture
def runtime(tmp_path, monkeypatch):
    binary = tmp_path / "fake-agents"
    binary.write_text(
        f"#!{sys.executable}\n"
        "import json, os, sys\n"
        "args = sys.argv[1:]\n"
        "if args[:2] == ['attest', 'event-context']:\n"
        "    print(os.environ['EVENT_TEST_CONTEXT'])\n"
        "elif args[:1] == ['publish-review']:\n"
        "    with open(os.environ['EVENT_TEST_REVIEW'], 'a') as out: out.write(json.dumps(json.load(sys.stdin)) + '\\n')\n"
        "    print(json.dumps({'receipt': 'bot-review: skipped'}))\n"
        "elif args[:1] == ['machine-mail-send']:\n"
        "    with open(os.environ['EVENT_TEST_MAIL'], 'a') as out: out.write(json.dumps(args) + '\\n')\n"
        "    sys.exit(int(os.environ.get('EVENT_TEST_MAIL_EXIT', '0')))\n"
        "else:\n"
        "    sys.exit(2)\n"
    )
    binary.chmod(0o755)
    context = {
        "session_id": None, "handle": None, "harness": None, "model": None,
        "parent": None, "attester_session_id": "sess-true", "attester_witness": "process",
        "attester_conflict": None, "observed_model": {"kind": "no-transcript"},
        "repo": "github.com/example/repo",
    }
    monkeypatch.setenv("FNO_AGENTS_BIN", str(binary))
    monkeypatch.setenv("FNO_STATE_DIR", str(tmp_path / "global"))
    repo = tmp_path / "repo"
    repo.mkdir()
    import subprocess
    subprocess.run(["git", "init", "-q", str(repo)], check=True)
    subprocess.run(["git", "-C", str(repo), "remote", "add", "origin", "git@github.com:example/repo.git"], check=True)
    monkeypatch.setenv("FNO_REPO_ROOT", str(repo))
    monkeypatch.setenv("EVENT_TEST_MAIL", str(tmp_path / "mail.jsonl"))
    monkeypatch.setenv("EVENT_TEST_REVIEW", str(tmp_path / "reviews.jsonl"))

    def set_context(**updates):
        monkeypatch.setenv("EVENT_TEST_CONTEXT", json.dumps({**context, **updates}))

    set_context()
    return set_context


def emit(path: Path, *args: str):
    return EventRunner().invoke(None, ["emit", "--events", str(path), *args])


def rows(path: Path):
    return [json.loads(line) for line in read_committed_lines(path)]


def attestation(**extra):
    return {"reviewer": "code-review", "head_sha": "a" * 40, "verdict": "pass", "session_id": "session", **extra}


def test_canonical_envelope_detects_source_and_prints_nonce(tmp_path, runtime):
    state = tmp_path / "state.md"
    state.write_text("---\nsession_id: ses-test\n---\n")
    journal = tmp_path / "events.jsonl"
    nonce = "ab" * 16
    result = emit(journal, "-t", "child_promise", "--state", str(state), "-d", json.dumps({"session_id": "ses-test", "nonce": nonce}))
    assert result.exit_code == 0, result.stderr
    assert result.stdout.strip() == nonce
    row = rows(journal)[0]
    assert set(row) == {"ts", "type", "source", "data"}
    assert row["source"] == "target" and row["data"]["nonce"] == nonce


def test_refusals_leave_no_event(tmp_path, runtime):
    journal = tmp_path / "events.jsonl"
    missing = tmp_path / "missing"
    for args, diagnostic in [
        (["-t", "lead_checkin", "--state", str(missing)], "no target session"),
        (["-t", "unknown-kind", "-s", "test"], "unknown event type"),
        (["-t", "verification_receipt", "-s", "target"], "preflight-owned"),
        (["-t", "child_promise", "-s", "test", "-d", "{"], "invalid JSON"),
        (["-t", "child_promise", "-s", "test", "-d", "{}", "--payload", "{}"], "not both"),
    ]:
        result = emit(journal, *args)
        assert result.exit_code == 1 and diagnostic in result.stderr
        assert not rows(journal)


def test_protocol_fields_follow_manifest_and_reject_bad_outcome(tmp_path, runtime):
    state = tmp_path / "state.md"
    state.write_text("---\nsession_id: run-1\ngraph_node_id: node-1\n---\n")
    journal = tmp_path / "events.jsonl"
    result = emit(journal, "-t", "task_started", "-s", "test", "--state", str(state))
    assert result.exit_code == 0, result.stderr
    row = rows(journal)[0]
    assert row["run"] == "run-1" and row["node"] == "node-1" and "from" not in row
    result = emit(journal, "-t", "task_started", "-s", "test", "--state", str(state), "--run", "", "--node", "")
    assert result.exit_code == 0
    assert rows(journal)[-1]["run"] == "run-1" and rows(journal)[-1]["node"] == "node-1"
    other = tmp_path / "other.jsonl"
    result = emit(other, "-t", "task_done", "-s", "test", "--run", "run-1", "--outcome", "PARTIAL")
    assert result.exit_code == 1 and not rows(other)


def test_attester_is_stamped_and_forgery_is_refused(tmp_path, runtime):
    journal = tmp_path / "events.jsonl"
    result = emit(journal, "-t", "review_attestation", "-s", "test", "-d", json.dumps(attestation()))
    assert result.exit_code == 0, result.stderr
    data = rows(journal)[0]["data"]
    assert data["attester_session_id"] == "sess-true" and data["attester_witness"] == "process"
    review = json.loads((tmp_path / "reviews.jsonl").read_text())
    assert review["head_sha"] == "a" * 40 and review["reviewer"] == "code-review"
    assert review["verdict"] == "pass" and review["pr_number"] is None
    other = tmp_path / "forged.jsonl"
    result = emit(other, "-t", "review_attestation", "-s", "test", "-d", json.dumps(attestation(attester_session_id="forged")))
    assert result.exit_code == 1 and "forged" in result.stderr and "sess-true" in result.stderr
    assert not rows(other)
    result = emit(other, "-t", "review_attestation", "-s", "test", "-d", json.dumps(attestation(attester_session_id=123)))
    assert result.exit_code == 1 and "123" in result.stderr and not rows(other)
    runtime(attester_conflict="identity conflict between true and forged")
    result = emit(other, "-t", "review_attestation", "-s", "test", "-d", json.dumps(attestation()))
    assert result.exit_code == 1 and "identity conflict" in result.stderr and not rows(other)


def test_model_is_only_stamped_from_observed_transcript(tmp_path, runtime):
    journal = tmp_path / "events.jsonl"
    result = emit(journal, "-t", "review_attestation", "-s", "test", "-d", json.dumps(attestation(model="claimed")))
    assert result.exit_code == 0 and "model" not in rows(journal)[0]["data"]
    runtime(observed_model={"kind": "observed", "model": "actual", "samples": 1})
    other = tmp_path / "other.jsonl"
    result = emit(other, "-t", "review_attestation", "-s", "test", "-d", json.dumps(attestation(model="claimed")))
    assert result.exit_code == 1 and "claimed" in result.stderr and "actual" in result.stderr
    assert not rows(other)


def test_global_mirror_is_repo_scoped_and_ordinary_event_is_not_mirrored(tmp_path, runtime):
    journal = tmp_path / "project" / "events.jsonl"
    data = {"path": "/tmp/removed", "caller": "test", "claim": "none", "reason": "done"}
    result = emit(journal, "-t", "worktree_removed", "-s", "bash", "-d", json.dumps(data))
    assert result.exit_code == 0, result.stderr
    global_journal = tmp_path / "global" / "events.jsonl"
    assert "repo" not in rows(journal)[0]["data"]
    assert rows(global_journal)[0]["data"]["repo"] == "github.com/example/repo"
    result = emit(journal, "-t", "daemon_started", "-s", "daemon")
    assert result.exit_code == 0 and len(rows(global_journal)) == 1


def test_blocked_push_is_after_commit_and_failure_is_nonfatal(tmp_path, runtime, monkeypatch):
    runtime(parent="parent-1")
    monkeypatch.setenv("EVENT_TEST_MAIL_EXIT", "1")
    journal = tmp_path / "events.jsonl"
    result = emit(journal, "-t", "blocked", "-s", "test", "--run", "run-1", "-d", json.dumps({"reason": "stuck"}))
    assert result.exit_code == 0 and rows(journal)[0]["parent"] == "parent-1"
    mail = json.loads((tmp_path / "mail.jsonl").read_text())
    assert mail[mail.index("--to") + 1] == "parent-1" and "run-1" in mail[-1]
    assert "parent push failed" in result.stderr
    monkeypatch.setenv("EVENT_TEST_MAIL_EXIT", "0")
    success = emit(tmp_path / "success.jsonl", "-t", "blocked", "-s", "test", "--run", "run-2", "-d", json.dumps({"reason": "stuck"}))
    assert success.exit_code == 0
    sent = (tmp_path / "mail.jsonl").read_text().splitlines()
    assert len(sent) == 2 and "run-2" in json.loads(sent[-1])[-1]


def test_defaults_anchor_to_the_declared_repo_from_a_subdirectory(tmp_path, runtime, monkeypatch):
    repo = tmp_path / "repo"
    state = repo / ".fno" / "target-state.md"
    state.parent.mkdir(parents=True)
    state.write_text("---\nsession_id: anchor\n---\n")
    subdir = repo / "deep" / "subdir"
    subdir.mkdir(parents=True)
    monkeypatch.chdir(subdir)
    monkeypatch.setenv("FNO_SPACES_DIR", str(tmp_path / "spaces"))
    result = EventRunner().invoke(None, ["emit", "-t", "child_promise", "-d", json.dumps({"session_id": "anchor", "nonce": "a" * 32})])
    assert result.exit_code == 0, result.stderr
    journal = tmp_path / "spaces" / str(repo.resolve()).replace("/", "-") / "events.jsonl"
    assert rows(journal)[0]["source"] == "target"
    assert not rows(subdir / ".fno" / "events.jsonl")


def test_relative_state_config_mirrors_under_home(tmp_path, runtime, monkeypatch):
    home = tmp_path / "home"
    home.mkdir()
    config = tmp_path / "config.toml"
    config.write_text('state_dir = ".fno-relative/"\n')
    monkeypatch.setenv("HOME", str(home))
    monkeypatch.setenv("FNO_CONFIG", str(config))
    monkeypatch.delenv("FNO_STATE_DIR", raising=False)
    journal = tmp_path / "project" / "events.jsonl"
    data = {"path": "/tmp/removed", "caller": "test", "claim": "none", "reason": "done"}
    result = emit(journal, "-t", "worktree_removed", "-s", "bash", "-d", json.dumps(data))
    assert result.exit_code == 0, result.stderr
    assert rows(home / ".fno" / "events.jsonl")[0]["type"] == "worktree_removed"


def test_worktree_removal_uses_native_front_for_both_destinations(tmp_path, monkeypatch):
    import subprocess

    log = tmp_path / "calls.jsonl"
    front = tmp_path / "fake-front"
    front.write_text(
        f"#!{sys.executable}\nimport json, os, sys\n"
        "with open(os.environ['EVENT_TEST_CALLS'], 'a') as out: out.write(json.dumps(sys.argv[1:]) + '\\n')\n"
    )
    front.chmod(0o755)
    monkeypatch.setenv("FNO_BIN", str(front))
    monkeypatch.setenv("FNO_AGENTS_HOME", str(tmp_path / "agents"))
    monkeypatch.setenv("EVENT_TEST_CALLS", str(log))
    script = Path(__file__).resolve().parents[3] / "scripts/lib/worktree-removal-event.sh"
    result = subprocess.run(
        ["bash", "-c", 'source "$1"; _wt_emit_removal_event "$2" "$3" test free done',
         "test", str(script), str(tmp_path), str(tmp_path / "worktree")],
        capture_output=True, text=True, timeout=30,
    )
    assert result.returncode == 0 and not result.stderr
    calls = [json.loads(line) for line in log.read_text().splitlines()]
    assert len(calls) == 2 and all(call[:3] == ["doctor", "event", "emit"] for call in calls)
    assert "--events" not in calls[0]
    assert calls[1][calls[1].index("--events") + 1] == str(tmp_path / "agents/events.jsonl")


def test_global_mirror_keeps_repo_identity_without_agent_binary(tmp_path, runtime, monkeypatch):
    monkeypatch.setenv("FNO_AGENTS_BIN", str(tmp_path / "missing-agent"))
    journal = tmp_path / "project" / "events.jsonl"
    data = {"path": "/tmp/removed", "caller": "test", "claim": "none", "reason": "done"}
    result = emit(journal, "-t", "worktree_removed", "-s", "bash", "-d", json.dumps(data))
    assert result.exit_code == 0, result.stderr
    assert rows(tmp_path / "global" / "events.jsonl")[0]["data"]["repo"] == "github.com/example/repo"
