"""Commit-bound verification receipt validation and reduction."""
from __future__ import annotations

import json
import datetime as dt
from pathlib import Path

import pytest

from fno.events import validate
from fno.pr._preflight import (
    check_verification_evidence,
    hosted_ci_decision,
    local_verification_required,
    next_verification_generation,
    verification_decision,
)


SHA = "a" * 40
OTHER_SHA = "b" * 40
PREFLIGHT_SCOPE = [
    "smoke",
    "rustfmt:fno-agents",
    "rustfmt:fno",
    "cargo-test:fno-agents-unit",
    "cargo-test:fno-agents-e2e",
    "cargo-test:fno-unit",
    "cargo-test:fno-e2e",
    "squads-leak-guard:fno",
]


def receipt(
    *,
    ts: str = "2026-07-26T01:02:04Z",
    candidate_sha: str = SHA,
    mode: str = "full",
    result: str = "passed",
    steps_expected: int = 8,
    steps_executed: int = 8,
    generation: int | float = 1,
    scope: list[str] | None = None,
) -> dict:
    return {
        "ts": ts,
        "type": "verification_receipt",
        "source": "target",
        "data": {
            "candidate_sha": candidate_sha,
            "command": ["scripts/ci/preflight.sh", "--force"],
            "environment": {
                "host": "builder-1",
                "platform": "Darwin-arm64",
                "runner": "scripts/ci/preflight.sh",
            },
            "scope": PREFLIGHT_SCOPE if scope is None else scope,
            "started_at": "2026-07-26T01:00:00Z",
            "finished_at": "2026-07-26T01:02:03Z",
            "mode": mode,
            "result": result,
            "producer": {"kind": "preflight", "id": "builder-1:42"},
            "generation": generation,
            "steps_expected": steps_expected,
            "steps_executed": steps_executed,
        },
    }


def write(path: Path, *events: dict) -> None:
    path.write_text("".join(json.dumps(e) + "\n" for e in events))


def test_full_receipt_validates_and_satisfies_exact_candidate(tmp_path: Path) -> None:
    event = receipt()
    validate(event)
    journal = tmp_path / "events.jsonl"
    write(journal, event)

    decision = verification_decision(SHA, [journal])

    assert decision["satisfied"] is True
    assert decision["mode"] == "full"
    assert decision["result"] == "passed"
    assert decision["receipt"]["data"]["candidate_sha"] == SHA


def test_non_full_modes_never_satisfy(tmp_path: Path) -> None:
    mode = "subset"
    event = receipt(mode=mode)
    validate(event)
    journal = tmp_path / "events.jsonl"
    write(journal, event)

    decision = verification_decision(SHA, [journal])

    assert decision["satisfied"] is False
    assert decision["mode"] == mode
    assert decision["result"] == event["data"]["result"]


@pytest.mark.parametrize(
    "mutate",
    [
        lambda event: event.update(source="test"),
        lambda event: event["data"].update(producer={"kind": "untrusted", "id": "x"}),
        lambda event: event["data"].update(command=["true"]),
        lambda event: event["data"].update(scope=[f"fake-{i}" for i in range(8)]),
    ],
)
def test_shape_valid_but_untrusted_receipt_cannot_satisfy_ship_gate(
    mutate, tmp_path: Path
) -> None:
    event = receipt()
    mutate(event)
    validate(event)
    journal = tmp_path / "events.jsonl"
    write(journal, event)

    decision = verification_decision(SHA, [journal])

    assert decision["result"] == "passed"
    assert decision["satisfied"] is False


def test_wrong_sha_is_stale_not_absent(tmp_path: Path) -> None:
    journal = tmp_path / "events.jsonl"
    write(journal, receipt(candidate_sha=OTHER_SHA))

    decision = verification_decision(SHA, [journal])

    assert decision["satisfied"] is False
    assert decision["result"] == "stale"
    assert decision["receipt"]["data"]["candidate_sha"] == OTHER_SHA


def test_corrupt_coverage_fails_closed_even_with_valid_full_pass(tmp_path: Path) -> None:
    journal = tmp_path / "mixed.jsonl"
    journal.write_text("not json\n" + json.dumps(receipt()) + "\n")

    decision = verification_decision(SHA, [journal])

    assert decision["mode"] == "full"
    assert decision["result"] == "passed"
    assert decision["coverage"]["complete"] is False
    assert decision["satisfied"] is False


def test_mirror_cannot_originate_satisfaction(tmp_path: Path) -> None:
    canonical = tmp_path / "global.jsonl"
    mirror = tmp_path / "delivery.jsonl"
    write(canonical, receipt(candidate_sha=OTHER_SHA))
    write(mirror, receipt())

    decision = verification_decision(SHA, [canonical, mirror])

    assert decision["satisfied"] is False
    assert decision["coverage"]["canonical_required"] is True
    assert decision["result"] in {"unavailable", "stale"}


def test_equal_timestamp_conflict_is_unavailable_not_lexical_green(tmp_path: Path) -> None:
    journal = tmp_path / "events.jsonl"
    write(journal, receipt(result="failed"), receipt(result="passed"))

    decision = verification_decision(SHA, [journal])

    assert decision["satisfied"] is False
    assert decision["result"] == "unavailable"
    assert decision["coverage"]["conflicting_latest"] == 2


def test_generation_supersedes_timestamp_after_clock_rollback(tmp_path: Path) -> None:
    journal = tmp_path / "events.jsonl"
    write(
        journal,
        receipt(ts="2026-07-26T03:00:00Z", result="passed", generation=1),
        receipt(ts="2026-07-26T02:00:00Z", result="failed", generation=2),
    )

    decision = verification_decision(SHA, [journal])

    assert decision["satisfied"] is False
    assert decision["result"] == "failed"
    assert decision["receipt"]["data"]["generation"] == 2


def test_mirror_ahead_cannot_supersede_canonical_pending(tmp_path: Path) -> None:
    canonical = tmp_path / "global.jsonl"
    mirror = tmp_path / "delivery.jsonl"
    write(
        canonical,
        receipt(generation=4),
        receipt(
            mode="void",
            result="pending",
            generation=5,
            scope=["preflight-execution"],
            steps_expected=1,
            steps_executed=0,
        ),
    )
    write(mirror, receipt(generation=100))

    decision = verification_decision(SHA, [canonical, mirror])

    assert decision["satisfied"] is False
    assert decision["result"] == "unavailable"
    assert decision["coverage"]["mirror_ahead"] is True


def test_next_generation_refuses_mirror_ahead_of_canonical(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    canonical = tmp_path / "global.jsonl"
    mirror = tmp_path / "delivery.jsonl"
    write(canonical, receipt(generation=4))
    write(mirror, receipt(generation=100))
    monkeypatch.setattr(
        "fno.pr._preflight.verification_event_paths",
        lambda **_kwargs: ([canonical, mirror], []),
    )

    with pytest.raises(ValueError, match="mirror generation exceeds"):
        next_verification_generation(cwd=str(tmp_path), candidate_sha=SHA)


def test_next_generation_fails_closed_on_uncertain_journal(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    corrupt = tmp_path / "events.jsonl"
    corrupt.write_text("not json\n")
    monkeypatch.setattr(
        "fno.pr._preflight.verification_event_paths",
        lambda **_kwargs: ([corrupt], []),
    )

    with pytest.raises(ValueError, match="malformed"):
        next_verification_generation(cwd=str(tmp_path), candidate_sha=SHA)


@pytest.mark.parametrize(
    "body",
    [
        '{"entries": null}\n',
        '{"entries": [{"root_path": "relative-root"}]}\n',
    ],
)
def test_present_structurally_invalid_ledger_fails_closed(
    body: str, monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    ledger = tmp_path / "ledger.json"
    ledger.write_text(body)
    monkeypatch.setattr("fno.paths.ledger_json", lambda: ledger)

    with pytest.raises(ValueError, match="ledger"):
        next_verification_generation(cwd=str(tmp_path), candidate_sha=SHA)


def test_unavailable_salvage_scan_fails_closed(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    ledger = tmp_path / "ledger.json"
    ledger.write_text(
        json.dumps(
            {
                "entries": [
                    {
                        "canonical_root_path": str(tmp_path / "canonical"),
                    }
                ]
            }
        )
    )
    monkeypatch.setattr("fno.paths.ledger_json", lambda: ledger)
    monkeypatch.setattr(
        "fno.pr._preflight.os.scandir",
        lambda _path: (_ for _ in ()).throw(PermissionError("denied")),
    )

    with pytest.raises(ValueError, match="salvage journal discovery failed"):
        next_verification_generation(cwd=str(tmp_path), candidate_sha=SHA)


def test_future_pass_cannot_shadow_later_appended_failure(tmp_path: Path) -> None:
    now = dt.datetime.now(dt.timezone.utc)
    future = (now + dt.timedelta(minutes=4)).isoformat().replace("+00:00", "Z")
    current = now.isoformat().replace("+00:00", "Z")
    journal = tmp_path / "events.jsonl"
    write(journal, receipt(ts=future), receipt(ts=current, result="failed"))

    decision = verification_decision(SHA, [journal])

    assert decision["satisfied"] is False
    assert decision["result"] == "failed"
    assert decision["coverage"]["malformed_lines"] == 1


def test_delivery_journal_discovery_failure_fails_closed(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    journal = tmp_path / "events.jsonl"
    write(journal, receipt())
    monkeypatch.setattr(
        "fno.pr._preflight.verification_event_paths",
        lambda **_kwargs: ([journal], ["delivery journal discovery failed"]),
    )

    decision = check_verification_evidence(cwd=str(tmp_path), candidate_sha=SHA)

    assert decision["result"] == "passed"
    assert decision["coverage"]["complete"] is False
    assert decision["coverage"]["discovery_errors"] == [
        "delivery journal discovery failed"
    ]
    assert decision["satisfied"] is False


def _opt_in_preflight(tmp_path: Path) -> None:
    """Pin `preflight.required = true` so these tests exercise the GATE.

    The stock default is opt-in (a code diff reads `policy-opt-in` before any
    git work), so without this pin every assertion below would be vacuous.
    """
    (tmp_path / ".fno").mkdir(exist_ok=True)
    (tmp_path / ".fno" / "config.toml").write_text("[preflight]\nrequired = true\n")


def test_local_verification_policy_preserves_explicit_exemptions(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    runner = tmp_path / "scripts" / "ci" / "preflight.sh"
    runner.parent.mkdir(parents=True)
    runner.write_text("#!/bin/sh\n")
    runner.chmod(0o755)
    _opt_in_preflight(tmp_path)

    def docs_git(args, *_args, **_kwargs):
        output = "100755 blob abc\tscripts/ci/preflight.sh\n" if args[0] == "ls-tree" else "docs/readme.md\n"
        return type("Result", (), {"returncode": 0, "stdout": output})()

    monkeypatch.setattr("fno.pr._preflight._git", docs_git)
    assert local_verification_required(cwd=str(tmp_path)) == (False, "docs-only")
    assert local_verification_required(
        cwd=str(tmp_path), env={"FNO_SKIP_PREFLIGHT": "1"}
    ) == (False, "explicit-skip")

    def code_git(args, *_args, **_kwargs):
        output = "100755 blob abc\tscripts/ci/preflight.sh\n" if args[0] == "ls-tree" else "cli/code.py\n"
        return type("Result", (), {"returncode": 0, "stdout": output})()

    monkeypatch.setattr("fno.pr._preflight._git", code_git)
    assert local_verification_required(cwd=str(tmp_path)) == (True, "required")


def test_base_configured_runner_removal_requires_verification(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    _opt_in_preflight(tmp_path)
    monkeypatch.setattr(
        "fno.pr._preflight._git",
        lambda *_args, **_kwargs: type(
            "Result",
            (),
            {
                "returncode": 0,
                "stdout": "100755 blob abc\tscripts/ci/preflight.sh\n",
            },
        )(),
    )

    assert local_verification_required(cwd=str(tmp_path)) == (True, "runner-removed")


@pytest.mark.parametrize(
    ("declared_none", "workflow_state", "observed_sha", "checks", "result", "satisfied"),
    [
        (True, "absent", None, [], "not_configured", True),
        (False, "absent", None, [], "pending", False),
        (False, "present", OTHER_SHA, [], "stale", False),
        (
            False,
            "present",
            SHA,
            [{"name": "ci", "status": "COMPLETED", "conclusion": "FAILURE"}],
            "failed",
            False,
        ),
        (
            False,
            "present",
            SHA,
            [{"name": "ci", "status": "COMPLETED", "conclusion": "SUCCESS"}],
            "passed",
            True,
        ),
    ],
)
def test_hosted_ci_states_remain_distinct(
    declared_none: bool,
    workflow_state: str,
    observed_sha: str | None,
    checks: list[dict],
    result: str,
    satisfied: bool,
) -> None:
    decision = hosted_ci_decision(
        declared_none=declared_none,
        workflow_state=workflow_state,
        candidate_sha=SHA,
        observed_sha=observed_sha,
        checks=checks,
    )

    assert decision["result"] == result
    assert decision["satisfied"] is satisfied


# ---- rebase-equivalent evidence: a receipt survives a rebase ------------------


def _git_ok(repo: Path, *args: str) -> str:
    import subprocess

    proc = subprocess.run(
        ["git", "-C", str(repo), *args], capture_output=True, text=True
    )
    assert proc.returncode == 0, proc.stderr
    return proc.stdout.strip()


def _commit(repo: Path, message: str, fname: str, content: str) -> str:
    (repo / fname).write_text(content)
    _git_ok(repo, "add", fname)
    _git_ok(
        repo,
        "-c",
        "user.email=t@example.com",
        "-c",
        "user.name=t",
        "commit",
        "-q",
        "-m",
        message,
    )
    return _git_ok(repo, "rev-parse", "HEAD")


def _rebase_fixture(tmp_path: Path) -> tuple[Path, Path, str]:
    """A feature commit rebased onto a newer main, plus its old-SHA receipt."""
    repo = tmp_path / "repo"
    repo.mkdir()
    _git_ok(repo, "init", "-q")
    _git_ok(repo, "symbolic-ref", "HEAD", "refs/heads/main")
    _git_ok(repo, "config", "user.email", "t@example.com")
    _git_ok(repo, "config", "user.name", "t")
    _commit(repo, "base", "base.txt", "base\n")
    _git_ok(repo, "checkout", "-q", "-b", "feature")
    sha_a = _commit(repo, "a", "a.txt", "a\n")
    journal = tmp_path / "events.jsonl"
    write(journal, receipt(candidate_sha=sha_a))
    _git_ok(repo, "checkout", "-q", "main")
    _commit(repo, "sibling", "sibling.txt", "sibling\n")
    # patch identity is measured against origin/main, like the real check
    _git_ok(
        repo,
        "update-ref",
        "refs/remotes/origin/main",
        _git_ok(repo, "rev-parse", "HEAD"),
    )
    _git_ok(repo, "checkout", "-q", "feature")
    _git_ok(repo, "rebase", "main")
    return repo, journal, sha_a


def test_rebase_equivalent_receipt_satisfies_only_with_the_flag(
    tmp_path: Path,
) -> None:
    repo, journal, sha_a = _rebase_fixture(tmp_path)

    strict = check_verification_evidence(cwd=str(repo), event_paths=[journal])

    assert strict["satisfied"] is False
    assert strict["result"] == "stale"

    equivalent = check_verification_evidence(
        cwd=str(repo), event_paths=[journal], allow_equivalent=True
    )

    assert equivalent["satisfied"] is True
    assert equivalent["result"] == "equivalent"
    assert equivalent["coverage"]["matched_sha"] == sha_a
    assert equivalent["coverage"]["equivalence"] == "patch-id-verbatim"


def test_equivalence_refused_after_a_whitespace_only_edit(tmp_path: Path) -> None:
    """The default ``patch-id --stable`` strips whitespace, so the identity
    keys on ``--verbatim`` ids: a re-indent that changes Python semantics
    must not borrow the old receipt. The edit amends the rebased commit so
    HEAD stays a single patch-equal commit; adding a second commit would
    refuse on commit count alone and stay green under ``--stable``."""
    repo, journal, _sha_a = _rebase_fixture(tmp_path)
    (repo / "a.txt").write_text("   a\n")
    _git_ok(repo, "add", "a.txt")
    _git_ok(repo, "commit", "-q", "--amend", "-m", "re-indent")

    decision = check_verification_evidence(
        cwd=str(repo), event_paths=[journal], allow_equivalent=True
    )

    assert decision["satisfied"] is False
    assert decision["result"] == "stale"


def test_equivalence_never_rescues_a_failed_receipt_for_head(
    tmp_path: Path,
) -> None:
    """A fresh red for HEAD outranks an older green for a patch-equal
    ancestor: the same patches can fail against a newer main."""
    repo, journal, sha_a = _rebase_fixture(tmp_path)
    head = _git_ok(repo, "rev-parse", "HEAD")
    write(
        journal,
        receipt(candidate_sha=sha_a, ts="2026-07-26T01:02:04Z"),
        receipt(candidate_sha=head, ts="2026-07-26T02:02:04Z", result="failed"),
    )

    decision = check_verification_evidence(
        cwd=str(repo), event_paths=[journal], allow_equivalent=True
    )

    assert decision["satisfied"] is False
    assert decision["result"] == "failed"


def test_equivalence_blocked_by_incomplete_journal_coverage(
    tmp_path: Path,
) -> None:
    """The strict path refuses on malformed journal lines; the equivalence
    path must not relax that fail-closed rule."""
    repo, journal, sha_a = _rebase_fixture(tmp_path)
    journal.write_text(
        json.dumps(receipt(candidate_sha=sha_a)) + "\nnot-json\n",
        encoding="utf-8",
    )

    decision = check_verification_evidence(
        cwd=str(repo), event_paths=[journal], allow_equivalent=True
    )

    assert decision["satisfied"] is False
    assert decision["coverage"]["complete"] is False
