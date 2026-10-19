"""Tests for `fno doctor event emit` CLI - canonical envelope path.

Covers AC1-HP, AC1-ERR, AC1-UI, AC1-EDGE, AC1-FR from the
events + test hygiene cleanup spec (ab-a1118224).

Routes through fno.events.__init__._build() + append_event() so
the emitted envelope is {ts, type, source, data} (canonical) rather
than the legacy {type, campaign_id, session_id, nonce, ts, payload}
envelope written by events/log.py:emit_event().
"""
from __future__ import annotations

import json
from pathlib import Path

import pytest

from fno.events.store_client import read_committed_lines
from typer.testing import CliRunner

from fno.events.cli import cli as event_cli


@pytest.fixture
def runner() -> CliRunner:
    # Click 8.2+ separates stderr by default; mix_stderr kwarg was removed.
    return CliRunner()


def _write_state(tmp_path: Path, session_id: str = "ses-test-001") -> Path:
    """Write a minimal target-state.md so source auto-detection sees 'target'."""
    state = tmp_path / ".fno" / "target-state.md"
    state.parent.mkdir(parents=True, exist_ok=True)
    state.write_text(
        f"---\nsession_id: {session_id}\nstatus: IN_PROGRESS\n"
        f"provenance_nonce: aaaa1111bbbb2222\n---\n"
    )
    return state


def _events_path(tmp_path: Path) -> Path:
    return tmp_path / ".fno" / "events.jsonl"


# ---------------------------------------------------------------------------
# AC1-HP: Canonical envelope with auto-detected source=target
# ---------------------------------------------------------------------------


def test_ac1_hp_canonical_envelope_target_source(runner: CliRunner, tmp_path: Path) -> None:
    """AC1-HP: state file present -> source auto-detects to 'target'; envelope is canonical."""
    state = _write_state(tmp_path)
    events = _events_path(tmp_path)

    result = runner.invoke(
        event_cli,
        [
            "emit",
            "--type", "phase_transition",
            "--data", json.dumps({
                "gate_bearing": True,
                "gate": "ledger_updated",
                "phase": "register",
                "nonce": "deadbeef" * 4,
                "session_id": "ses-test-001",
            }),
            "--state", str(state),
            "--events", str(events),
        ],
    )

    assert result.exit_code == 0, f"stderr={result.stderr!r} stdout={result.stdout!r}"
    assert read_committed_lines(events)
    lines = read_committed_lines(events)
    assert len(lines) == 1
    event = json.loads(lines[0])

    # Canonical envelope keys (NOT legacy {type, campaign_id, session_id, nonce, ts, payload})
    assert set(event.keys()) == {"ts", "type", "source", "data"}
    assert event["type"] == "phase_transition"
    assert event["source"] == "target"  # auto-detected from state file
    assert event["data"]["gate"] == "ledger_updated"
    assert event["data"]["phase"] == "register"
    assert event["data"]["nonce"] == "deadbeef" * 4
    assert event["data"]["session_id"] == "ses-test-001"


def test_ac1_hp_no_state_file_and_no_source_is_refused(runner: CliRunner, tmp_path: Path) -> None:
    """AC1-HP: missing state file and no --source -> refused, no row written.

    The old behavior silently defaulted to 'test'; the fix requires the
    caller to name a producer instead of guessing one.
    """
    events = _events_path(tmp_path)
    nonexistent_state = tmp_path / ".fno" / "target-state.md"
    # parent dir is created by events.jsonl write, but state file itself must not exist
    events.parent.mkdir(parents=True, exist_ok=True)

    result = runner.invoke(
        event_cli,
        [
            "emit",
            "--type", "lead_checkin",
            "--data", json.dumps({"scope": "x-a792", "change": "no source"}),
            "--state", str(nonexistent_state),
            "--events", str(events),
        ],
    )

    assert result.exit_code != 0
    assert not read_committed_lines(events)
    assert "lead_checkin declares: daemon, hook, loop" in result.stderr


# ---------------------------------------------------------------------------
# AC1-ERR: Unknown event type rejected; no line appended
# ---------------------------------------------------------------------------


def test_ac1_err_unknown_event_type_rejected(runner: CliRunner, tmp_path: Path) -> None:
    """AC1-ERR: unknown --type is non-zero exit; events.jsonl unchanged."""
    state = _write_state(tmp_path)
    events = _events_path(tmp_path)

    result = runner.invoke(
        event_cli,
        [
            "emit",
            "--type", "not_a_real_event",
            "--data", "{}",
            "--state", str(state),
            "--events", str(events),
        ],
    )

    assert result.exit_code != 0
    assert "unknown event type" in result.stderr.lower() or "not_a_real_event" in result.stderr
    assert not read_committed_lines(events)


# ---------------------------------------------------------------------------
# PR #270 Codex P2: non-JSON success path must print a non-empty token
# ---------------------------------------------------------------------------


def test_pr270_codex_non_json_success_prints_nonce_when_present(
    runner: CliRunner, tmp_path: Path
) -> None:
    """Codex P2 (PR #270): shell callers using `$(fno doctor event emit ...)` must not
    receive an empty string on success. When the event type carries a nonce
    (phase_transition, child_promise), the nonce is the token (matches the
    legacy ``emit_event`` contract that shell scripts may depend on)."""
    state = _write_state(tmp_path)
    events = _events_path(tmp_path)
    nonce = "deadbeef" * 4

    result = runner.invoke(
        event_cli,
        [
            "emit",
            "--type", "child_promise",
            "--data", json.dumps({"session_id": "ses-x", "nonce": nonce}),
            "--state", str(state),
            "--events", str(events),
        ],
    )
    assert result.exit_code == 0
    # stdout carries the nonce, not an empty string.
    assert result.output.strip() == nonce


def test_pr270_gemini_default_paths_anchor_to_repo_root(
    runner: CliRunner, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Gemini MEDIUM (PR #270): when --events / --state are not passed, the
    defaults must anchor to the repo root, not to the current working
    directory. Otherwise running `fno doctor event emit` from a subdirectory writes
    events to a per-subdir ``.fno/events.jsonl`` instead of the
    repository's central log."""
    # Stage a fake repo root with a state file at <root>/.fno/.
    repo_root = tmp_path / "repo"
    state = repo_root / ".fno" / "target-state.md"
    state.parent.mkdir(parents=True, exist_ok=True)
    state.write_text(
        "---\nsession_id: ses-anchored\nstatus: IN_PROGRESS\n---\n"
    )

    # Pin resolve_repo_root() to our staged root so the CLI uses the
    # anchored defaults rather than the test runner's actual CWD.
    monkeypatch.setattr(
        "fno.paths.resolve_repo_root",
        lambda: repo_root,
    )

    # Invoke from a subdirectory; do NOT pass --state or --events.
    subdir = repo_root / "deep" / "subdir"
    subdir.mkdir(parents=True, exist_ok=True)
    monkeypatch.chdir(subdir)

    result = runner.invoke(
        event_cli,
        [
            "emit",
            "--type", "child_promise",
            "--data", json.dumps({"session_id": "ses-anchored", "nonce": "a" * 32}),
        ],
    )
    assert result.exit_code == 0, f"stderr={result.stderr!r} stdout={result.output!r}"

    # The event must land in the REPO's one journal (the space journal, keyed
    # on the canonical root), NOT in the subdir's own .fno/.
    from fno.paths import project_log

    anchored_events = project_log("events.jsonl", project_root=repo_root)
    subdir_events = subdir / ".fno" / "events.jsonl"
    assert read_committed_lines(anchored_events), (
        f"event should land in the repo's journal, not a subdir's; "
        f"anchored={anchored_events!r} subdir={subdir_events!r}"
    )
    assert not read_committed_lines(subdir_events), (
        "event leaked to subdir .fno; defaults are not anchored"
    )
    # And source auto-detection found the repo-root state file -> "target".
    event = json.loads(read_committed_lines(anchored_events)[0])
    assert event["source"] == "target"


# ---------------------------------------------------------------------------
# AC1-FR: Schema validation failure surfaces (no event written)
# ---------------------------------------------------------------------------


def test_verification_receipt_is_refused_by_generic_emitter(
    runner: CliRunner, tmp_path: Path
) -> None:
    result = runner.invoke(
        event_cli,
        [
            "emit",
            "--type",
            "verification_receipt",
            "--source",
            "target",
            "--events",
            str(tmp_path / "events.jsonl"),
            "--data",
            "{}",
        ],
    )

    assert result.exit_code == 1
    assert "preflight-owned" in result.output
    assert not read_committed_lines(tmp_path / "events.jsonl")


# ---------------------------------------------------------------------------
# attester stamping: the caller gets no vote on review_attestation identity
# ---------------------------------------------------------------------------


def _attestation_data() -> dict:
    return {
        "reviewer": "code-review",
        "head_sha": "a" * 40,
        "verdict": "pass",
        "session_id": "20260825T000000Z-cl00000-000000",
    }


def test_attestation_stamps_attester_and_witness_from_the_emitting_process(
    runner: CliRunner, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """No caller-supplied attester -> the emit chokepoint stamps the resolved
    identity and its witness. Both directions of the witness are legal values
    on the event; gating on witness is a later operator decision."""
    import fno.harness_identity as hi

    monkeypatch.setattr(hi, "resolve_attester_identity", lambda: ("sess-A", "process"))
    events = _events_path(tmp_path)

    result = runner.invoke(
        event_cli,
        [
            "emit", "--type", "review_attestation",
            "--data", json.dumps(_attestation_data()),
            "--source", "test",
            "--events", str(events),
        ],
    )
    assert result.exit_code == 0, result.stderr
    data = json.loads(read_committed_lines(events)[0])["data"]
    assert data["attester_session_id"] == "sess-A"
    assert data["attester_witness"] == "process"


def test_attestation_refuses_a_supplied_attester_that_disagrees(
    runner: CliRunner, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A --data attester_session_id that disagrees with the resolved identity is
    REFUSED naming both, never silently dropped: the caller gets no vote."""
    import fno.harness_identity as hi

    monkeypatch.setattr(hi, "resolve_attester_identity", lambda: ("sess-true", "process"))
    events = _events_path(tmp_path)
    payload = {**_attestation_data(), "attester_session_id": "sess-forged"}

    result = runner.invoke(
        event_cli,
        [
            "emit", "--type", "review_attestation",
            "--data", json.dumps(payload),
            "--events", str(events),
        ],
    )
    assert result.exit_code == 1
    assert "sess-forged" in result.stderr and "sess-true" in result.stderr
    assert not read_committed_lines(events)


def test_attestation_refuses_the_ancestry_override_shape(
    runner: CliRunner, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The command-line override: an ancestor carries a DIFFERENT value for the
    winning marker. The whole emit is refused naming both ids, and no event is
    written - this is the refusal that stops one env assignment from
    refreshing another session's stale verdict onto a head it never saw."""
    import fno.harness_identity as hi

    def boom():
        raise hi.AttesterIdentityConflict(
            "CODEX_THREAD_ID", "sess-true", "sess-forged"
        )

    monkeypatch.setattr(hi, "resolve_attester_identity", boom)
    events = _events_path(tmp_path)

    result = runner.invoke(
        event_cli,
        [
            "emit", "--type", "review_attestation",
            "--data", json.dumps(_attestation_data()),
            "--events", str(events),
        ],
    )
    assert result.exit_code == 1
    assert "sess-true" in result.stderr and "sess-forged" in result.stderr
    assert not read_committed_lines(events)


# ---------------------------------------------------------------------------
# model stamping: the caller gets no vote on review_attestation model either
# ---------------------------------------------------------------------------


def test_review_attestation_unknown_model_drops_the_callers_claim(
    runner: CliRunner, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """An unresolvable transcript means NOT OBSERVABLE: the caller's env claim
    is dropped, never laundered into the record. Empty is the schema's contract
    for that, so the event carries NO model key."""
    import fno.provenance.observed as observed

    monkeypatch.setattr(
        observed,
        "observed_model_for_session",
        lambda harness, session_id, cwd: {"kind": "no-transcript"},
    )
    events = _events_path(tmp_path)
    payload = {**_attestation_data(), "model": "glm-5.2[1m]"}

    result = runner.invoke(
        event_cli,
        [
            "emit", "--type", "review_attestation",
            "--data", json.dumps(payload),
            "--source", "test",
            "--events", str(events),
        ],
    )
    assert result.exit_code == 0, result.stderr
    data = json.loads(read_committed_lines(events)[0])["data"]
    assert "model" not in data


def test_review_attestation_refuses_a_supplied_model_that_disagrees(
    runner: CliRunner, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A --data model that disagrees with the transcript is REFUSED naming both,
    never silently dropped - the positive marker that the caller gets no vote.
    Assert the non-zero exit AND that no event line was appended, not merely
    that stderr matched."""
    import fno.provenance.observed as observed

    monkeypatch.setattr(
        observed,
        "observed_model_for_session",
        lambda harness, session_id, cwd: {
            "kind": "observed", "model": "claude-opus-5", "samples": 3,
        },
    )
    events = _events_path(tmp_path)
    payload = {**_attestation_data(), "model": "glm-5.2[1m]"}

    result = runner.invoke(
        event_cli,
        [
            "emit", "--type", "review_attestation",
            "--data", json.dumps(payload),
            "--events", str(events),
        ],
    )
    assert result.exit_code == 1
    assert "glm-5.2[1m]" in result.stderr and "claude-opus-5" in result.stderr
    assert not read_committed_lines(events)


# ---------------------------------------------------------------------------
# review_coverage rides the global mirror: a hand emit reaches every reader
# ---------------------------------------------------------------------------


def _coverage_payload() -> dict:
    return {
        "pr": 4242,
        "coverage": "covered",
        "review_state": "reviewed",
        "reviewed_count": 1,
        "verdicts": [
            {
                "name": "code-review",
                "producer": "local_attestation",
                "verdict": "reviewed",
            }
        ],
        "head_sha": "b" * 40,
    }


def test_hand_emitted_review_coverage_reaches_both_logs(
    runner: CliRunner, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A manual coverage emit from a worktree whose journal is not the global
    one lands in BOTH: the project log it named and the machine-global log a
    canonical merge reads. The mirror adds a destination; it moves nothing."""
    global_journal = tmp_path / "global-events.jsonl"
    import fno.paths

    monkeypatch.setattr(fno.paths, "global_events_json", lambda: global_journal)
    worktree_log = tmp_path / "worktree" / ".fno" / "events.jsonl"
    worktree_log.parent.mkdir(parents=True)

    result = runner.invoke(
        event_cli,
        [
            "emit",
            "--type", "review_coverage",
            "--data", json.dumps(_coverage_payload()),
            "--source", "hook",
            "--events", str(worktree_log),
        ],
    )
    assert result.exit_code == 0, result.stderr

    project_rows = [json.loads(ln) for ln in read_committed_lines(worktree_log)]
    assert len(project_rows) == 1
    assert project_rows[0]["type"] == "review_coverage"
    global_rows = [json.loads(ln) for ln in read_committed_lines(global_journal)]
    assert len(global_rows) == 1
    # The mirrored copy carries the repo scoping a cross-project reader needs;
    # the project copy needs none.
    assert "repo" in global_rows[0]["data"]
    assert "repo" not in project_rows[0]["data"]
