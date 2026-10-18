"""The coverage predicate's two surfaces refuse with ONE sentence.

`fno do pr merge` (through run_merge, recompute=True) and `fno do pr
coverage-check` (through the verb, recompute=False) must print the same
refusal for the same row. Two denials for different reasons is precisely the
defect the shared predicate exists to remove, so these assert the TEXT
character for character, not merely that both refused.

The absence pins are the temptation these tests exist to outlive: a future
author will want to soften a missing or head-mismatched row into a fail-open.
Exit 3 on absence is load-bearing - `fno do pr merge` refuses those rows, so a
hook that waved them through would recreate the divergence on the exact
input where nothing reviewed the PR. Exit 4 stays reserved for a named
instrument failure, reachable here only by forcing the head fetch to fail or
the read to raise, never by an empty read.
"""

import json
from pathlib import Path
from types import SimpleNamespace

import pytest

from fno.pr import _coverage_gate, _merge, _reviews
from fno.pr._proc import Result

from .test_pr_merge import FakeRun, _last_json, enabled  # noqa: F401

HEAD = "aaaa1111bbbb2222"


def _journal(tmp_path):
    """The journal the gate reads: the repo's space via the accessor that owns
    it. A legacy checkout path goes quiet once a reader migrates it, so a
    re-seed that writes .fno/events.jsonl directly lands where nobody looks."""
    from fno.paths import project_log

    journal = project_log("events.jsonl", project_root=tmp_path)
    journal.parent.mkdir(parents=True, exist_ok=True)
    return journal


def _seed_row(
    tmp_path,
    *,
    coverage,
    count,
    head,
    verdicts=None,
    pr=42,
    self_attested=None,
    review_state=None,
):
    """One review_coverage event in the project log the gate reads."""
    (tmp_path / ".fno").mkdir(exist_ok=True)
    from fno.paths import project_log as _pl

    journal = _pl("events.jsonl", project_root=tmp_path)
    journal.parent.mkdir(parents=True, exist_ok=True)
    data = {"pr": pr, "coverage": coverage, "head_sha": head}
    if review_state is not None:
        data["review_state"] = review_state
    if coverage == "covered":
        # A posture-capable producer resolves a rung on every covered row;
        # without the key the gate treats the row as pre-posture evidence and
        # demands a recompute, which is not what these tests are about.
        data["review_posture"] = {
            "posture": "self_review",
            "rank": 3,
            "source": "legacy",
            "posture_satisfied": True,
            "posture_gaps": [],
        }
    if coverage in ("covered", "uncovered"):
        data["reviewed_count"] = count
    if self_attested is not None:
        data["self_attested_count"] = self_attested
    if verdicts is not None:
        data["verdicts"] = verdicts
    journal.write_text(
        json.dumps({"ts": "2026-08-16T03:00:00Z", "type": "review_coverage", "data": data}) + "\n",
        encoding="utf-8",
    )


@pytest.fixture
def live_head(monkeypatch):
    """Both surfaces see the same live PR head and the real events read."""
    monkeypatch.setattr(_merge, "_pr_head_oid", lambda pr, repo: HEAD)
    # Route the merge path through the REAL read (the `enabled` fixture's
    # covered stub would bypass it): the only seams are the head and the verb.
    monkeypatch.setattr(
        _merge,
        "_review_coverage_for_pr",
        lambda pr, repo, head=None: _reviews.review_coverage_for_gate(pr, repo, head),
    )
    # No 3am valve: these tests pin the REFUSAL sentences, and the override
    # would answer COVERED before any of them is built.
    monkeypatch.setattr(_reviews, "_override_label_actor", lambda pr, repo, r: (False, None))
    # The disposition pass scopes the attestation chain by the PR's head
    # branch; a missing probe is an instrument failure (UNANSWERED), so the
    # hermetic fixture answers it.
    monkeypatch.setattr(_merge, "_pr_base_head_refs", lambda pr, cwd: ("main", "feature/x"))


def _merge_refusal(capsys, tmp_path, fake):
    """The refusal sentence the merge verb renders for this row, prefix
    stripped. The coverage gate is decide's now (x-53c5): the test seeds the
    row, lets the gate compose its own line, has the owner hold with it, and
    pins the render."""
    monkeypatch_run = pytest.MonkeyPatch()
    monkeypatch_run.setattr(_merge, "run", fake)
    try:
        _state, refusal, _covered, note = _coverage_gate.coverage_verdict(
            42, str(tmp_path), recompute=False
        )
        line = _coverage_gate.refusal_line(refusal, note)
        detail = f"unreviewed merge refused: {line}" if line else ("unreviewed merge refused")

        def _held(pr_number, repo, **kw):
            return {"outcome": "held", "detail": detail}

        monkeypatch_run.setattr(_merge, "_authorized_merge", _held)
        assert _merge.run_merge(["42"], cwd=str(tmp_path)) == 2
        # A held receipt renders on stdout (err=False in the emit table).
        reason = _merge.reason_after_outcome(_last_json(capsys)["reason"])
    finally:
        monkeypatch_run.undo()
    assert reason.startswith("unreviewed merge refused: ")
    return reason[len("unreviewed merge refused: ") :]


def _verb_refusal(capsys, tmp_path):
    """The first stderr line the verb prints, and its exit state."""
    rc = _coverage_gate.run_coverage_check(42, cwd=str(tmp_path))
    cap = capsys.readouterr()
    return rc, (cap.err.strip().splitlines() or [""])[0]


# ---- the two refusal branches, asserted on the TEXT ----


def test_uncovered_row_refuses_with_one_sentence(
    enabled,  # noqa: F811
    live_head,
    monkeypatch,
    capsys,
    tmp_path,
):
    monkeypatch.setattr(
        _merge, "_code_review_attestation_required", lambda repo, pr_number=0: False
    )
    _seed_row(tmp_path, coverage="uncovered", count=0, head=HEAD)
    fake = FakeRun(toplevel=str(tmp_path))
    merge_line = _merge_refusal(capsys, tmp_path, fake)
    assert "0 reviewed" in merge_line
    rc, verb_line = _verb_refusal(capsys, tmp_path)
    assert rc == 3
    assert verb_line == merge_line, "both surfaces must refuse with one sentence"


# ---- the positive side, at the same head ----


def test_covered_row_passes_both_surfaces(
    enabled,  # noqa: F811
    live_head,
    monkeypatch,
    capsys,
    tmp_path,
):
    monkeypatch.setattr(
        _merge, "_code_review_attestation_required", lambda repo, pr_number=0: False
    )
    _seed_row(
        tmp_path,
        coverage="covered",
        count=1,
        head=HEAD,
        verdicts=[
            {
                "name": "code-review",
                "producer": "local_attestation",
                "verdict": "reviewed",
                "reviewed_sha": HEAD,
                "freshness": "fresh",
            }
        ],
    )
    rc = _coverage_gate.run_coverage_check(42, cwd=str(tmp_path))
    capsys.readouterr()
    assert rc == 0
    fake = FakeRun(gh_merge=Result(0, "Merged pull request", ""), toplevel=str(tmp_path))
    monkeypatch.setattr(_merge, "run", fake)
    assert _merge.run_merge(["42"], cwd=str(tmp_path)) == 0
    assert _last_json(capsys)["outcome"] == "merged"


# ---- the absence pins: refusal, never a shrug ----


def test_no_row_refuses_and_spawns_no_subprocess(
    enabled,  # noqa: F811
    live_head,
    monkeypatch,
    capsys,
    tmp_path,
):
    """Absence is an answer: nothing attested this head. The pure read must
    not fire the Rust producer, which is budgeted far past a hook's reach."""
    monkeypatch.setattr(
        _merge, "_code_review_attestation_required", lambda repo, pr_number=0: False
    )
    fired: list = []
    monkeypatch.setattr(
        _reviews, "_fire_review_coverage_verb", lambda *a, **k: fired.append(a) or (False, "")
    )
    rc, verb_line = _verb_refusal(capsys, tmp_path)
    assert rc == 3
    assert "no review_coverage event for this PR" in verb_line
    assert "searched:" in verb_line, "the refusal names the logs it read"
    assert not fired, "recompute=False spawns no fno-agents subprocess"


def test_row_pinned_to_another_head_refuses(
    enabled,  # noqa: F811
    live_head,
    monkeypatch,
    capsys,
    tmp_path,
):
    monkeypatch.setattr(
        _merge, "_code_review_attestation_required", lambda repo, pr_number=0: False
    )
    _seed_row(tmp_path, coverage="covered", count=2, head="cccc3333dddd4444")
    monkeypatch.setattr(_merge, "_pr_head_oid", lambda pr, repo: "eeee5555ffff6666")
    rc, verb_line = _verb_refusal(capsys, tmp_path)
    assert rc == 3
    assert "cccc3333" in verb_line and "eeee5555" in verb_line
    assert "attestations are head-pinned by design" in verb_line


# ---- exit 4: a named instrument failure, and nothing else ----


def test_exit_four_is_reached_only_by_a_named_instrument_failure(
    enabled,  # noqa: F811
    monkeypatch,
    capsys,
    tmp_path,
):
    """Force the head fetch to die: the verdict is UNANSWERED with a note
    naming the probe. Then, same PR with a live head and an empty log, the
    read returns 3 - no absence route can reach 4."""
    monkeypatch.setattr(_merge, "_pr_head_oid", lambda pr, repo: None)
    rc = _coverage_gate.run_coverage_check(42, cwd=str(tmp_path))
    note = capsys.readouterr().err.strip()
    assert rc == 4
    assert "head fetch" in note, "the note names which probe died"

    monkeypatch.setattr(_merge, "_pr_head_oid", lambda pr, repo: HEAD)
    monkeypatch.setattr(
        _merge, "_code_review_attestation_required", lambda repo, pr_number=0: False
    )
    rc = _coverage_gate.run_coverage_check(42, cwd=str(tmp_path))
    capsys.readouterr()
    assert rc == 3, "an empty read is absence, and absence refuses"


# ---- the 3am release valve on the verb the docs name ----
#
# `docs/best-practices.md` and `docs/troubleshooting.md` both route merges
# through `fno do pr merge` and name the `coverage-override` label as the only
# way past an uncovered head. A valve read anywhere but the shared predicate
# is a valve on one of N reachable paths: the gate refuses first and the
# publisher's override branch is never reached, leaving the raw `gh` path the
# same docs forbid as the only way through.


def test_the_override_label_opens_the_gate_on_an_uncovered_pr(
    enabled,  # noqa: F811
    monkeypatch,
    capsys,
    tmp_path,
):
    """The label answers COVERED where the same row would otherwise refuse."""
    _seed_row(tmp_path, coverage="uncovered", count=0, head=HEAD)
    monkeypatch.setattr(_merge, "_pr_head_oid", lambda pr, repo: HEAD)
    monkeypatch.setattr(_reviews, "_override_label_actor", lambda pr, repo, r: (True, "jane"))
    monkeypatch.setattr(_coverage_gate, "_pr_author_login", lambda pr, repo: "op")
    state, refusal, covered_head, note = _coverage_gate.coverage_verdict(
        42, str(tmp_path), recompute=False
    )
    assert state == _coverage_gate.COVERED
    assert refusal == ""
    # The waiver drops the review, never the TOCTOU pin: `--match-head-commit`
    # must still refuse a push that races the merge.
    assert covered_head == HEAD
    assert note.startswith(_coverage_gate.OVERRIDE_NOTE_PREFIX)
    assert "jane" in note


def test_an_override_applied_by_the_pr_author_is_refused(
    enabled,  # noqa: F811
    monkeypatch,
    capsys,
    tmp_path,
):
    """The author check the approval path always had, on the one bypass
    beside it. An agent that cannot merge can label its own PR; that is
    reject-and-attest wearing a label. The refusal NAMES the author, so an
    operator whose label was refused sees exactly why the valve stayed
    shut."""
    _seed_row(tmp_path, coverage="uncovered", count=0, head=HEAD)
    monkeypatch.setattr(_merge, "_pr_head_oid", lambda pr, repo: HEAD)
    monkeypatch.setattr(
        _merge, "_code_review_attestation_required", lambda repo, pr_number=0: False
    )
    monkeypatch.setattr(
        _reviews, "_override_label_actor", lambda pr, repo, r: (True, "worker-login")
    )
    monkeypatch.setattr(_coverage_gate, "_pr_author_login", lambda pr, repo: "worker-login")
    state, refusal, _head, _note = _coverage_gate.coverage_verdict(
        42, str(tmp_path), recompute=False
    )
    assert state == _coverage_gate.REFUSED
    assert "worker-login" in refusal, "the refusal must name the author: " + refusal
    assert "the PR author" in refusal
    assert "cannot override its own review gate" in refusal


def test_an_override_with_an_unreadable_actor_fails_closed(
    enabled,  # noqa: F811
    monkeypatch,
    capsys,
    tmp_path,
):
    """Label held, actor unreadable: the valve stays shut. An unreadable
    actor is exactly the state a forger produces, and the approval path
    already fails closed on the same ambiguity."""
    _seed_row(tmp_path, coverage="uncovered", count=0, head=HEAD)
    monkeypatch.setattr(_merge, "_pr_head_oid", lambda pr, repo: HEAD)
    monkeypatch.setattr(
        _merge, "_code_review_attestation_required", lambda repo, pr_number=0: False
    )
    monkeypatch.setattr(_reviews, "_override_label_actor", lambda pr, repo, r: (True, None))
    # The author read must never even matter: the actor alone is unreadable.
    monkeypatch.setattr(
        _coverage_gate,
        "_pr_author_login",
        lambda pr, repo: pytest.fail("no author read on an unreadable actor"),
    )
    state, refusal, _head, _note = _coverage_gate.coverage_verdict(
        42, str(tmp_path), recompute=False
    )
    assert state == _coverage_gate.REFUSED
    assert "labelling actor is unreadable" in refusal


def test_an_override_with_an_unreadable_author_fails_closed(
    enabled,  # noqa: F811
    monkeypatch,
    capsys,
    tmp_path,
):
    """Actor readable, author unreadable: the labeller cannot be proven to
    differ from the author, so the valve refuses rather than guess."""
    _seed_row(tmp_path, coverage="uncovered", count=0, head=HEAD)
    monkeypatch.setattr(_merge, "_pr_head_oid", lambda pr, repo: HEAD)
    monkeypatch.setattr(
        _merge, "_code_review_attestation_required", lambda repo, pr_number=0: False
    )
    monkeypatch.setattr(_reviews, "_override_label_actor", lambda pr, repo, r: (True, "jane"))
    monkeypatch.setattr(_coverage_gate, "_pr_author_login", lambda pr, repo: None)
    state, refusal, _head, _note = _coverage_gate.coverage_verdict(
        42, str(tmp_path), recompute=False
    )
    assert state == _coverage_gate.REFUSED
    assert "PR author is unreadable" in refusal


def test_an_unreadable_label_never_opens_the_valve(
    enabled,  # noqa: F811
    monkeypatch,
    capsys,
    tmp_path,
):
    """Fail closed: a label read that dies is not an override.

    The recovery from a wrong refusal is one command. The recovery from a
    merge nobody reviewed is a revert.
    """
    _seed_row(tmp_path, coverage="uncovered", count=0, head=HEAD)
    monkeypatch.setattr(_merge, "_pr_head_oid", lambda pr, repo: HEAD)
    monkeypatch.setattr(
        _merge, "_code_review_attestation_required", lambda repo, pr_number=0: False
    )

    def boom(*_a, **_k):
        raise OSError("gh died")

    monkeypatch.setattr(_reviews, "_override_label_actor", boom)
    rc = _coverage_gate.run_coverage_check(42, cwd=str(tmp_path))
    capsys.readouterr()
    assert rc == 3


# ---- the disposition-complete pass condition (AC5) ----
#
# The specimen is BYTE-PINNED to head 46695fffd (ruling d-fc3b3837): five
# codex findings, five fix commits, five dispositions recorded on the round
# that reviewed the fix delta, a chain tiling base..head. The record that did
# everything right and still could not merge under the clean-only rule MUST
# pass under disposition-complete. The fixture carries its own review
# payloads and never reads the live PR: PR 1179 as it stands today is a
# different record (14 open threads), and a fixture built from it would
# assert COVERED over the exploit itself.

FIXTURE_HEAD = "46695fffd00000000000000000000000000000000"
DISPOSITIONS_FIXTURE = (
    Path(__file__).resolve().parents[1]
    / "fixtures"
    / "review_dispositions"
    / "pr1179-46695fffd.jsonl"
)


def _seed_specimen(tmp_path, *, extra_lines=()):
    (tmp_path / ".fno").mkdir(exist_ok=True)
    text = DISPOSITIONS_FIXTURE.read_text(encoding="utf-8")
    for line in extra_lines:
        text += json.dumps(line) + "\n"
    _journal(tmp_path).write_text(text, encoding="utf-8")


def _specimen_gates(monkeypatch):
    monkeypatch.setattr(_merge, "_review_lane_configured", lambda repo, pr_number=0: True)
    monkeypatch.setattr(_merge, "_pr_head_oid", lambda pr, repo: FIXTURE_HEAD)
    monkeypatch.setattr(
        _merge, "_code_review_attestation_required", lambda repo, pr_number=0: False
    )
    monkeypatch.setattr(_merge, "_pr_base_head_refs", lambda pr, cwd: ("main", "feature/x-8439"))
    monkeypatch.setattr(_reviews, "_override_label_actor", lambda pr, repo, r: (False, None))


def _ac5b_finding():
    return {
        "ts": "2026-08-25T22:28:00Z",
        "type": "review_attestation",
        "source": "hook",
        "data": {
            "reviewer": "code-review",
            "head_sha": "46695fffd00000000000000000000000000000000",
            "verdict": "fail",
            "session_id": "s-1179",
            "branch": "feature/x-8439",
            "reviewed_base_sha": "a4b5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e0f1a2b3",
            "reviewed_head_sha": "46695fffd00000000000000000000000000000000",
            "reviewed_file_count": 43,
            "reviewed_line_count": 2452,
            "findings_blocking": 1,
            "findings_nonblocking": 0,
            "findings": [
                {
                    "category": "correctness",
                    "verdict": None,
                    "blocking": True,
                    "has_required_fields": True,
                    "finding_key": "cli/src/fno/pr/_coverage_gate.py:999:correctness",
                }
            ],
        },
    }


def test_ac5_marker_specimen_is_covered(monkeypatch, tmp_path):
    """AC5-MARKER: the disposition-complete specimen returns literal COVERED.
    The specimen's fixing round attests from a second session, and a fixed
    disposition is terminal once a later round reviewed the fix delta."""
    _specimen_gates(monkeypatch)
    _seed_specimen(tmp_path)
    state, refusal, covered_head, note = _coverage_gate.coverage_verdict(
        1179, str(tmp_path), recompute=False
    )
    assert state == _coverage_gate.COVERED
    assert refusal == ""


def test_ac5b_marker_specimen_plus_one_open_finding_refuses(monkeypatch, tmp_path):
    """AC5b: the same fixture plus one open correctness finding REFUSES, by key.

    The budget is raised here: the specimen's chain carries three attested
    rounds (fail, pass, this finding's fail) and the x-2219 per-PR-total
    counter counts all three, so the shipped max_rounds of 2 would fire the
    discharge arm and the PR would merge, not refuse. This test pins the
    by-key refusal, which needs budget room; the cap arm has its own tests."""
    _specimen_gates(monkeypatch)
    monkeypatch.setattr(_coverage_gate, "resolved_max_rounds", lambda repo: 5)
    _seed_specimen(tmp_path, extra_lines=[_ac5b_finding()])
    state, refusal, covered_head, note = _coverage_gate.coverage_verdict(
        1179, str(tmp_path), recompute=False
    )
    assert state == _coverage_gate.REFUSED
    assert "cli/src/fno/pr/_coverage_gate.py:999:correctness" in refusal


def test_ac5_edge_producer_blocking_count_is_never_the_answer(monkeypatch, tmp_path):
    """AC5-EDGE: findings_blocking: 0 over a CONFIRMED style-tagged finding REFUSES."""
    _specimen_gates(monkeypatch)
    r1 = {
        "ts": "2026-08-25T21:00:00Z",
        "type": "review_attestation",
        "source": "hook",
        "data": {
            "reviewer": "code-review",
            "head_sha": FIXTURE_HEAD,
            "verdict": "pass",
            "session_id": "s-edge",
            "branch": "feature/x-8439",
            "reviewed_base_sha": "17a3b85b1a70a22014f1fc4e04b7aa35a632757f",
            "reviewed_head_sha": FIXTURE_HEAD,
            "reviewed_file_count": 2,
            "reviewed_line_count": 30,
            "findings_blocking": 0,
            "findings_nonblocking": 1,
            "findings": [
                {
                    "category": "style",
                    "verdict": "CONFIRMED",
                    "blocking": False,
                    "has_required_fields": True,
                    "finding_key": "lie.py:1:style",
                },
            ],
        },
    }
    row = {
        "ts": "2026-08-25T21:05:00Z",
        "type": "review_coverage",
        "source": "hook",
        "data": {
            "pr": 1179,
            "coverage": "covered",
            "review_state": "reviewed",
            "reviewed_count": 1,
            "head_sha": FIXTURE_HEAD,
            "verdicts": [
                {
                    "producer": "local_attestation",
                    "name": "code-review",
                    "verdict": "reviewed",
                    "reviewed_sha": FIXTURE_HEAD,
                    "freshness": "fresh",
                    "attestation_origin": "other_session",
                }
            ],
        },
    }
    (tmp_path / ".fno").mkdir(exist_ok=True)
    _journal(tmp_path).write_text(
        "\n".join(json.dumps(e) for e in (r1, row)) + "\n", encoding="utf-8"
    )
    state, refusal, covered_head, note = _coverage_gate.coverage_verdict(
        1179, str(tmp_path), recompute=False
    )
    assert state == _coverage_gate.REFUSED
    assert "lie.py:1:style" in refusal


# ---- the round budget (AC7) ----
#
# The PR-1170 shape: three fail rounds against a budget of two. At the cap
# the review phase is COMPLETE - exit 0, the discharge named on the note,
# and no instruction that asks for another review.


def _ac7_round(ts: str, verdict: str, head: str, dispositions=None):
    data = {
        "reviewer": "code-review",
        "head_sha": head,
        "verdict": verdict,
        "session_id": "s-ac7",
        "attester_session_id": "sess-author",
        "branch": "feature/x-8439",
        "reviewed_base_sha": "a4b5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e0f1a2b3",
        "reviewed_head_sha": head,
        "reviewed_file_count": 3,
        "reviewed_line_count": 40,
        "findings_blocking": 1,
        "findings": [
            {
                "category": "security",
                # CONFIRMED is what makes this finding HARD: the one class
                # the standing operator waiver refuses below the cap. At the
                # cap itself the budget discharges it like any other.
                "verdict": "CONFIRMED",
                "blocking": True,
                "has_required_fields": True,
                "finding_key": "hooks/git-protection.py:302:security",
            }
        ],
    }
    if dispositions is not None:
        data["dispositions"] = dispositions
    return {"ts": ts, "type": "review_attestation", "source": "hook", "data": data}


_AC7_DECLINE = [
    {
        "finding_key": "hooks/git-protection.py:302:security",
        "disposition": "declined",
        "reason": "not applicable here",
    }
]


def _ac7_seed(tmp_path, rounds):
    """A fail-round chain plus the covered self-attested row it tiles to.

    Each round reviews a DIFFERENT head (the loop's non-fix commits move it),
    which is also what keeps the chain dedup honest: identical data rounds
    would collapse to one event."""
    stamps = ["2026-08-25T21:00:00Z", "2026-08-25T21:30:00Z", "2026-08-25T22:00:00Z"]
    heads = [
        "1111111111111111111111111111111111111111",
        "2222222222222222222222222222222222222222",
        FIXTURE_HEAD,
    ]
    lines = []
    for i in range(rounds):
        dispositions = _AC7_DECLINE if i == 1 else None
        lines.append(_ac7_round(stamps[i], "fail", heads[i], dispositions))
    (tmp_path / ".fno").mkdir(exist_ok=True)
    with open(_journal(tmp_path), "w", encoding="utf-8") as fh:
        for line in lines:
            fh.write(json.dumps(line) + "\n")
        fh.write(
            json.dumps(
                {
                    "ts": "2026-08-25T22:01:00Z",
                    "type": "review_coverage",
                    "source": "hook",
                    "data": {
                        "pr": 42,
                        "coverage": "covered",
                        "review_state": "reviewed",
                        "reviewed_count": 1,
                        "self_attested_count": 1,
                        "head_sha": FIXTURE_HEAD,
                        "verdicts": [
                            {
                                "producer": "local_attestation",
                                "name": "code-review",
                                "verdict": "reviewed",
                                "attestation_origin": "self_attested",
                                "reviewed_sha": FIXTURE_HEAD,
                                "freshness": "fresh",
                            }
                        ],
                    },
                }
            )
            + "\n"
        )


def test_ac7_marker_exhausted_decline_discharges_at_the_cap(monkeypatch, tmp_path):
    """Three fail rounds against a budget of two, one decline in the chain:
    the cap discharges the review phase. The decline rides the waiver note
    as a fact, never as a block, and nothing asks for another review."""
    _specimen_gates(monkeypatch)
    _ac7_seed(tmp_path, rounds=3)
    state, refusal, covered_head, note = _coverage_gate.coverage_verdict(
        42, str(tmp_path), recompute=False
    )
    assert state == _coverage_gate.COVERED
    assert refusal == ""
    assert "review budget discharged (3/2 rounds)" in note
    assert "open findings stay in the PR conversation" in note
    assert "run the review verb" not in note


def test_max_rounds_n_means_exactly_n_rounds(monkeypatch, tmp_path):
    """The boundary tracks the CONFIGURED number, not the shipped default.

    Set it to 3 and the third round is the last one. Pinning only
    max_rounds = 2 would leave the cap free to be hardcoded to 2 somewhere
    and still pass.
    """
    max_rounds = 3
    monkeypatch.setattr(_coverage_gate, "resolved_max_rounds", lambda repo: max_rounds)
    seen = {}
    for n in (max_rounds - 1, max_rounds, max_rounds + 1):
        if n < 1:
            continue
        _specimen_gates(monkeypatch)
        _seed_soft_cap(tmp_path, rounds=n)
        state, _refusal, _head, note = _coverage_gate.coverage_verdict(
            42, str(tmp_path), recompute=False
        )
        seen[n] = (state, note)

    if max_rounds - 1 >= 1:
        assert seen[max_rounds - 1][0] == _coverage_gate.REFUSED, (
            f"one short of {max_rounds} must still be under the cap: {seen[max_rounds - 1]}"
        )
    assert seen[max_rounds][0] == _coverage_gate.COVERED, (
        f"round {max_rounds} of {max_rounds} is the last the budget funds: {seen[max_rounds]}"
    )
    assert f"({max_rounds}/{max_rounds}" in seen[max_rounds][1], seen[max_rounds][1]
    assert seen[max_rounds + 1][0] == _coverage_gate.COVERED, seen[max_rounds + 1]


def test_ac7_hp_under_the_cap_refuses_and_says_how_many_remain(monkeypatch, tmp_path, capsys):
    """The same chain UNDER the cap: exit 3 with the budget named, not 5.

    One round of a two-round maximum. This used to seed two rounds and still
    expect a refusal, which only held because `max_rounds` was compared with
    `>` and so meant three. Two means two, so the last fundable round here is
    the next one."""
    _specimen_gates(monkeypatch)
    _ac7_seed(tmp_path, rounds=1)
    rc = _coverage_gate.run_coverage_check(42, cwd=str(tmp_path))
    cap = capsys.readouterr()
    line = (cap.err.strip().splitlines() or [""])[0]
    assert rc == _coverage_gate.REFUSED == 3
    assert "1/2 review rounds used" in line
    assert "the next round is the last the budget funds" in line


# ---- the round cap under operator law ----
#
# The operator's ruling: at the cap the review phase is complete and the PR
# merges on green CI. Open findings stay in the PR conversation - no filing,
# no finding-class exception. One review stays the floor, so an unreviewed
# PR is still uncovered.


def _soft_round(ts: str, head: str, dispositions=None):
    """A fail round whose finding is blocking but NOT hard (unconfirmed)."""
    data = {
        "reviewer": "code-review",
        "head_sha": head,
        "verdict": "fail",
        "session_id": "s-cap",
        "attester_session_id": "sess-author",
        "branch": "feature/x-8439",
        "reviewed_base_sha": "a4b5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e0f1a2b3",
        "reviewed_head_sha": head,
        "reviewed_file_count": 3,
        "reviewed_line_count": 40,
        "findings_blocking": 1,
        "findings": [
            {
                "category": "correctness",
                "verdict": None,
                "blocking": True,
                "has_required_fields": True,
                "finding_key": "cli/src/fno/pr/_merge.py:77:correctness",
            }
        ],
    }
    if dispositions is not None:
        data["dispositions"] = dispositions
    return {"ts": ts, "type": "review_attestation", "source": "hook", "data": data}


def _seed_soft_cap(tmp_path, rounds=3):
    # Generated, not a fixed list: the boundary test walks max_rounds up to 5,
    # and a three-element table silently IndexErrors past three rather than
    # saying the fixture ran out. The LAST round always lands on FIXTURE_HEAD,
    # because the gate reads the newest round at the PR head.
    stamps = [f"2026-08-25T{21 + (i // 2):02d}:{(i % 2) * 30:02d}:00Z" for i in range(rounds)]
    heads = [f"{i + 1}" * 40 for i in range(rounds - 1)] + [FIXTURE_HEAD]
    (tmp_path / ".fno").mkdir(exist_ok=True)
    with open(_journal(tmp_path), "w", encoding="utf-8") as fh:
        for i in range(rounds):
            fh.write(json.dumps(_soft_round(stamps[i], heads[i])) + "\n")
        fh.write(
            json.dumps(
                {
                    "ts": "2026-08-25T22:01:00Z",
                    "type": "review_coverage",
                    "source": "hook",
                    "data": {
                        "pr": 42,
                        "coverage": "covered",
                        "review_state": "reviewed",
                        "reviewed_count": 1,
                        "self_attested_count": 1,
                        "head_sha": FIXTURE_HEAD,
                        "verdicts": [
                            {
                                "producer": "local_attestation",
                                "name": "code-review",
                                "verdict": "reviewed",
                                "attestation_origin": "self_attested",
                                "reviewed_sha": FIXTURE_HEAD,
                                "freshness": "fresh",
                            }
                        ],
                    },
                }
            )
            + "\n"
        )


def test_cap_discharges_the_open_remainder_and_files_nothing(monkeypatch, tmp_path):
    """Operator law: at the cap the review phase is complete and the PR
    merges on green CI. Open findings stay in the PR conversation; the
    backlog receives nothing. The spy is the positive marker: any code path
    that tries to mint a node at the cap fails this test."""
    import fno.pr._proc as _proc

    _specimen_gates(monkeypatch)
    _seed_soft_cap(tmp_path, rounds=3)
    real_run = _proc.run
    attempts = []

    def spy(cmd, **kwargs):
        argv = [str(part) for part in cmd]
        if "backlog" in argv:
            attempts.append(argv)
            raise AssertionError(f"the cap must not file findings: {argv}")
        return real_run(cmd, **kwargs)

    monkeypatch.setattr(_proc, "run", spy)
    state, refusal, _head, note = _coverage_gate.coverage_verdict(
        42, str(tmp_path), recompute=False
    )
    assert state == _coverage_gate.COVERED, refusal
    assert refusal == ""
    assert attempts == []
    assert "review budget discharged (3/2 rounds)" in note
    assert "open findings stay in the PR conversation" in note


# ---- x-aecc: declining must satisfy coverage ----
#
# The pass condition is ANSWERED at this head, never clean at this head. The
# branch's ONLY attestation is a fail whose findings are all terminally
# dispositioned, and the row the Rust producer emits for that chain now reads
# covered/reviewed (its answered-fail local verdict). A pass-chain covering
# proves nothing here - the tests above already pin that arm - so every
# fixture's only attestation is a fail.


def _xaecc_fail_round(findings_keys, dispositioned_keys):
    """One fail round at FIXTURE_HEAD; `dispositioned_keys` get declines."""
    return {
        "ts": "2026-08-26T19:00:00Z",
        "type": "review_attestation",
        "source": "hook",
        "data": {
            "reviewer": "code-review",
            "head_sha": FIXTURE_HEAD,
            "verdict": "fail",
            "session_id": "s-xaecc",
            "attester_session_id": "sess-peer",
            "branch": "feature/x-8439",
            "reviewed_base_sha": "17a3b85b1a70a22014f1fc4e04b7aa35a632757f",
            "reviewed_head_sha": FIXTURE_HEAD,
            "reviewed_file_count": 3,
            "reviewed_line_count": 40,
            "findings_blocking": len(findings_keys),
            "findings": [
                {
                    "category": "correctness",
                    "verdict": None,
                    "blocking": True,
                    "has_required_fields": True,
                    "finding_key": k,
                }
                for k in findings_keys
            ],
            "dispositions": [
                {
                    "finding_key": k,
                    "disposition": "declined",
                    "reason": "not worth the churn",
                }
                for k in dispositioned_keys
            ],
        },
    }


def _xaecc_row(covered: bool):
    """The row the Rust producer emits: covered once the fail is answered,
    uncovered while a finding keeps it non-terminal."""
    verdicts = (
        [
            {
                "producer": "local_attestation",
                "name": "code-review",
                "verdict": "reviewed",
                "attestation_origin": "other_session",
                "reviewed_sha": FIXTURE_HEAD,
                "freshness": "fresh",
            }
        ]
        if covered
        else []
    )
    return {
        "ts": "2026-08-26T19:01:00Z",
        "type": "review_coverage",
        "source": "hook",
        "data": {
            "pr": 42,
            "coverage": "covered" if covered else "uncovered",
            "review_state": "reviewed" if covered else "unreviewed",
            "reviewed_count": 1 if covered else 0,
            "head_sha": FIXTURE_HEAD,
            "verdicts": verdicts,
        },
    }


def _xaecc_seed(tmp_path, attestations, row):
    if not isinstance(attestations, list):
        attestations = [attestations]
    (tmp_path / ".fno").mkdir(exist_ok=True)
    _journal(tmp_path).write_text(
        "\n".join(json.dumps(e) for e in (*attestations, row)) + "\n",
        encoding="utf-8",
    )


def test_xaecc_marker1_fail_only_chain_fully_dispositioned_covers(monkeypatch, tmp_path):
    """MARKER 1: the only attestation is a fail, every finding declined with a
    reason on a corroborated row - literal COVERED, refusal empty."""
    _specimen_gates(monkeypatch)
    _xaecc_seed(
        tmp_path,
        _xaecc_fail_round(
            ["a.py:1:correctness", "b.py:2:correctness"],
            ["a.py:1:correctness", "b.py:2:correctness"],
        ),
        _xaecc_row(covered=True),
    )
    state, refusal, covered_head, note = _coverage_gate.coverage_verdict(
        42, str(tmp_path), recompute=False
    )
    assert state == _coverage_gate.COVERED, refusal
    assert refusal == ""
    # MARKER 3: the covered answer pins the SAME head the declining round
    # attested - no commit and no further review between declining and merge.
    assert covered_head == FIXTURE_HEAD


def test_xaecc_r3_cap_discharged_but_no_local_pass_is_waived_and_named(monkeypatch, tmp_path):
    """The cap discharges and the row is uncovered on the no_local_pass
    conjunct: config REQUIRES a code-review attestation and only a peer ever
    attested.

    This case used to keep a sized refusal. It cannot: past the cap that
    conjunct is unsatisfiable by construction, because the only way to satisfy
    it is a code-review round the spent budget will not fund. That is the
    unpassable-guard shape the round cap exists to end, and the operator's
    ruling is two rounds maximum whatever the shape.

    So the budget discharges - and the FACT is not lost. The waived conjunct
    is named in the receipt, so a merge that happened without the required
    reviewer's attestation stays legible afterward."""
    _specimen_gates(monkeypatch)
    monkeypatch.setattr(_merge, "_code_review_attestation_required", lambda repo, pr_number=0: True)
    stamps = ["2026-08-26T18:00:00Z", "2026-08-26T18:30:00Z", "2026-08-26T19:00:00Z"]
    heads = [
        "1111111111111111111111111111111111111111",
        "2222222222222222222222222222222222222222",
        FIXTURE_HEAD,
    ]
    rounds = []
    for ts, head in zip(stamps, heads):
        r = _xaecc_fail_round(["a.py:1:correctness"], [])
        r["ts"] = ts
        r["data"]["head_sha"] = head
        r["data"]["reviewed_head_sha"] = head
        # The reviewer label is a peer, not code-review: the row carries no
        # code-review local pass, which is the conjunct the waiver names.
        r["data"]["reviewer"] = "peer"
        rounds.append(r)
    _xaecc_seed(tmp_path, rounds, _xaecc_row(covered=False))
    state, refusal, covered_head, note = _coverage_gate.coverage_verdict(
        42, str(tmp_path), recompute=False
    )
    assert state == _coverage_gate.COVERED, f"spent budget must discharge: {refusal}"
    assert not refusal, f"a discharged budget carries no refusal: {refusal}"
    assert covered_head, "a covered verdict must name the head it covers"
    # And the waiver is explicit about WHAT it waived, so the missing
    # required-reviewer attestation is a recorded fact rather than a silence.
    assert "review budget discharged (3/2 rounds)" in note, note
    assert "waived at the cap: uncovered" in note, note
    # The specific fact their sized refusal carried: config demanded a
    # code-review attestation and none exists. Waived, but on the record.
    assert "code-review attestation required by config" in note, note


# --- rounds the attestation chain never saw: the GitHub review axis ---
# The Python half of the shared corpus. The Rust half lives in
# crates/fno-agents/tests/coverage_tiling.rs under the same case names, and
# the two must answer identically: the counter gates the merge on this side
# and the stop hook on that one.

_CONNECTOR = "chatgpt-codex-connector[bot]"
_PR_AUTHOR = "bllshttng"


def _review_object(login, state, commit, submitted_at):
    """One review object in the shape both gates read (gh pr view / REST,
    normalized): author.login, state, commit.oid, submittedAt."""
    return {
        "author": {"login": login},
        "state": state,
        "commit": {"oid": commit},
        "submittedAt": submitted_at,
    }


def test_round_budget_counts_rounds_that_only_github_review_objects_saw():
    """The connector lane: three review rounds, every one ended with
    findings, NO attestation exists anywhere. Each fix moved the head and
    the connector reviewed the new head, so the rounds exist only as three
    distinct reviewed commits. The chain alone answers 0 and the cap cannot
    fire; with the payload it must answer 3."""
    chain: list[dict] = []
    reviews = [
        _review_object(_CONNECTOR, "COMMENTED", "c1", "2026-08-26T11:00:00Z"),
        _review_object(_CONNECTOR, "COMMENTED", "c2", "2026-08-26T13:00:00Z"),
        _review_object(_CONNECTOR, "COMMENTED", "c3", "2026-08-26T15:00:00Z"),
    ]
    assert _coverage_gate.rounds_since_last_pass(chain, reviews=reviews) == 3


def test_round_budget_counts_every_verdict_across_the_pr_life():
    """The operator's ruling (x-2219): max_rounds is a per-PR TOTAL. A pass
    is a round like any verdict and refunds nothing - four attested rounds
    are four, not the reset answer 1."""
    chain = [
        {"verdict": "fail", "ts": "2026-08-26T10:00:00Z"},
        {"verdict": "fail", "ts": "2026-08-26T11:00:00Z"},
        {"verdict": "pass", "ts": "2026-08-26T12:00:00Z"},
        {"verdict": "fail", "ts": "2026-08-26T13:00:00Z"},
    ]
    assert _coverage_gate.rounds_since_last_pass(chain) == 4
    # Drop the pass from the chain: still three rounds.
    no_pass = [chain[0], chain[1], {"verdict": "fail", "ts": "2026-08-26T12:00:00Z"}]
    assert _coverage_gate.rounds_since_last_pass(no_pass) == 3


def test_attestation_chain_dedupes_the_global_mirror_on_invocation_id(monkeypatch, tmp_path):
    """One attestation, two stores, ONE round.

    review_attestation rides GLOBAL_MIRROR_TYPES, and the mirror stamps
    `repo` onto its copy alone, so the project row and the global row carry
    different payloads. Dedup keyed on the whole payload therefore admitted
    both rows and the chain counted every mirrored attestation twice - which
    the per-PR-total budget turns into "one review reads 2/2" and fires the
    cap after a single round. The invocation_id is minted once by the
    producer and lands identically on both rows; it is the dedup key. The
    payload fallback keeps pre-invocation_id rows deduped when the two
    stores genuinely agree."""
    from fno.pr import _reviews

    head = "46695fffd00000000000000000000000000000000"
    base_data = {
        "reviewer": "code-review",
        "head_sha": head,
        "verdict": "fail",
        "session_id": "s-1",
        "attester_session_id": "s-1",
        "branch": "feature/x-8439",
        "reviewed_base_sha": "a4b5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e0f1a2b3",
        "reviewed_head_sha": head,
        "reviewed_file_count": 3,
        "reviewed_line_count": 40,
        "invocation_id": "ri-1",
    }

    def _row(data):
        return (
            json.dumps({"ts": "2026-08-26T10:00:00Z", "type": "review_attestation", "data": data})
            + "\n"
        )

    project = tmp_path / "project-events.jsonl"
    project.write_text(_row(base_data), encoding="utf-8")
    global_log = tmp_path / "global-events.jsonl"
    global_log.write_text(_row({**base_data, "repo": "bllshttng/footnote"}), encoding="utf-8")
    monkeypatch.setattr(
        _reviews,
        "_coverage_logs",
        lambda cwd, project_events: (project, global_log, "bllshttng/footnote"),
    )
    chain = _coverage_gate.attestation_chain(str(tmp_path), head_branch="feature/x-8439", head=head)
    assert len(chain) == 1, "the mirrored copy must dedup, not double-count"
    assert _coverage_gate.rounds_since_last_pass(chain) == 1

    # Fallback: rows with no invocation_id dedup only when the payloads
    # really are identical (the pre-field shape).
    legacy = {k: v for k, v in base_data.items() if k != "invocation_id"}
    project.write_text(_row(legacy), encoding="utf-8")
    global_log.write_text(_row(legacy), encoding="utf-8")
    chain = _coverage_gate.attestation_chain(str(tmp_path), head_branch="feature/x-8439", head=head)
    assert len(chain) == 1, "identical legacy payloads still dedup"

    # Negative control: two distinct invocations stay two chain entries, so
    # the dedup key is not so wide it collapses real attestations.
    project.write_text(_row(base_data), encoding="utf-8")
    second = {**base_data, "invocation_id": "ri-2"}
    global_log.write_text(
        _row({**base_data, "repo": "bllshttng/footnote"})
        + _row({**second, "repo": "bllshttng/footnote"}),
        encoding="utf-8",
    )
    chain = _coverage_gate.attestation_chain(str(tmp_path), head_branch="feature/x-8439", head=head)
    assert len(chain) == 2, "two distinct invocations are two chain entries"
    # Both entries pin the SAME head, so they are ONE round: the unit is a
    # reviewed head, not a verdict row. The dedup key and the round unit are
    # orthogonal, which is exactly what this pair of assertions pins.
    assert _coverage_gate.rounds_since_last_pass(chain) == 1


def test_attestation_chain_keeps_same_invocation_same_head_reruns(monkeypatch, tmp_path):
    """One invocation, one head, ONE second, TWO rounds: kept, and terminal
    for nothing.

    Measured live on feature/x-a3e8: a pass and the later fail that
    superseded it shared invocation_id AND head_sha (the hold mints one id
    per branch, and both rows attested the same commit), so an
    invocation-based key deleted the second row. The ts cannot separate
    them either - most producers stamp second precision, so both rows here
    share one second on purpose. The payload separates them (verdict and
    findings differ). And the same-head re-run must not clear: a fixed
    disposition is terminal only when the disposing round attested a
    DIFFERENT head, else an author clears a CONFIRMED finding by
    re-attesting the head that raised it."""
    from fno.pr import _reviews

    head = "46695fffc00000000000000000000000000000000"
    finding = {
        "finding_key": "f.py:1:correctness",
        "category": "correctness",
        "verdict": "CONFIRMED",
    }
    fail_row = {
        "reviewer": "code-review",
        "head_sha": head,
        "verdict": "fail",
        "session_id": "s-1",
        "attester_session_id": "s-1",
        "branch": "feature/x-77be",
        "invocation_id": "ri-1",
        "findings": [finding],
    }
    pass_row = {
        **fail_row,
        "verdict": "pass",
        "findings": [],
        "dispositions": [
            {
                "finding_key": finding["finding_key"],
                "disposition": "fixed",
                "reason": "re-attested at the same head, which clears nothing",
            }
        ],
    }

    def _row(ts, data):
        return json.dumps({"ts": ts, "type": "review_attestation", "data": data}) + "\n"

    same_second = "2026-09-04T15:53:10Z"
    project = tmp_path / "project-events.jsonl"
    project.write_text(
        _row(same_second, fail_row) + _row(same_second, pass_row),
        encoding="utf-8",
    )
    global_log = tmp_path / "global-events.jsonl"
    global_log.write_text(
        _row(same_second, {**fail_row, "repo": "bllshttng/footnote"})
        + _row(same_second, {**pass_row, "repo": "bllshttng/footnote"}),
        encoding="utf-8",
    )
    monkeypatch.setattr(
        _reviews,
        "_coverage_logs",
        lambda cwd, project_events: (project, global_log, "bllshttng/footnote"),
    )
    chain = _coverage_gate.attestation_chain(str(tmp_path), head_branch="feature/x-77be", head=head)
    assert [(row["verdict"], row["dispositions"]) for row in chain] == [
        ("fail", None),
        ("pass", pass_row["dispositions"]),
    ], "two distinct rounds in one second under one invocation are two entries"
    refusal, _note, nonterminal = _coverage_gate.disposition_refusal(
        chain, cov=None, cwd=str(tmp_path)
    )
    assert nonterminal == ["f.py:1:correctness"], refusal
    assert refusal != "", "a same-head fixed disposition must never read terminal"


def test_past_the_cap_the_spent_budget_discharges_the_obligation(monkeypatch, tmp_path):
    """The Python mirror of the spent-budget receipt: past the cap the
    uncovered refusal must not teach the review verb (that instruction is
    the loop this cap exists to bound) and must name the terminal act. The
    render itself is asserted first so an unrendered refusal cannot pass
    the absence checks."""
    _specimen_gates(monkeypatch)
    _seed_soft_cap(tmp_path, rounds=3)
    # Overwrite the coverage row the seed wrote: this PR is UNCOVERED (the
    # connector lane shape - no verdict ever counted), so after the soft
    # findings are filed the refusal falls through to the spent-budget arm.
    rows = []
    stamps = ["2026-08-25T21:00:00Z", "2026-08-25T21:30:00Z", "2026-08-25T22:00:00Z"]
    heads = [
        "1111111111111111111111111111111111111111",
        "2222222222222222222222222222222222222222",
        FIXTURE_HEAD,
    ]
    for i in range(3):
        rows.append(json.dumps(_soft_round(stamps[i], heads[i])))
    rows.append(
        json.dumps(
            {
                "ts": "2026-08-25T22:01:00Z",
                "type": "review_coverage",
                "source": "hook",
                "data": {
                    "pr": 42,
                    "coverage": "uncovered",
                    "review_state": "unreviewed",
                    "reviewed_count": 0,
                    "self_attested_count": 0,
                    "head_sha": FIXTURE_HEAD,
                    "verdicts": [],
                },
            }
        )
    )
    _journal(tmp_path).write_text("\n".join(rows) + "\n")
    monkeypatch.setattr(_coverage_gate, "_pr_reviews", lambda *a, **k: (None, ""))
    state, refusal, head, note = _coverage_gate.coverage_verdict(42, str(tmp_path), recompute=False)
    # A SPENT budget discharges the review obligation; it does not fail it.
    # `max_rounds = 2` means "this PR gets two rounds, then review is done".
    # The old REFUSED here was a guard nothing could pass: every remedy that
    # could clear it names a review verb, and running one spends a round
    # already spent. That is what produced 12-round PRs against a cap of 2.
    assert state == _coverage_gate.COVERED, f"spent budget must discharge: {refusal}"
    assert not refusal, f"a discharged budget carries no refusal: {refusal}"
    assert head, "a covered verdict must name the head it covers"

    # The waiver is NAMED, never silent: a merge on a spent budget has to be
    # legible afterward, and the note is the only place that record lives.
    assert "review budget discharged (3/2 rounds)" in note, note
    assert "config.review.max_rounds" in note, note
    assert "open findings stay in the PR conversation" in note, note

    # Unchanged, and still the point: past the cap the gate must name NO
    # review verb. That instruction is the loop the cap exists to bound.
    for needle in ("/code-review", "/review", "/fno:review", "review verb"):
        assert needle not in note, f"past-cap receipt names {needle}: {note}"


def test_coverage_verdict_refuses_a_non_directory_at_the_door(monkeypatch, tmp_path):
    """The guard belongs at the gate's entry, not three probes deep.

    Every probe below takes the same cwd, and the head fetch is the first to
    trip on a bad one. It drops its reason and answers None, so the verb used
    to report "pr head fetch failed": true of the probe, silent about the
    argument that broke it. That is the wrong-subject sentence this gate
    exists to stop printing (x-51f7). UNANSWERED, never REFUSED - a gate that
    cannot read its own inputs has not judged the PR.
    """
    monkeypatch.chdir(tmp_path)

    def never_runs(cmd, **kwargs):
        raise AssertionError(f"no probe may run on a bad cwd: {cmd}")

    monkeypatch.setattr("fno.pr._proc.run", never_runs)
    state, refusal, head, note = _coverage_gate.coverage_verdict(
        1264, "bllshttng/footnote", recompute=False
    )
    assert state == _coverage_gate.UNANSWERED
    assert note == "no such directory: bllshttng/footnote"
    assert refusal == "" and head == ""
    assert "pr head fetch failed" not in note


# ---- the operator-law exit: one standing subject, one head-pinned command ----
#
# The gate had two authority surfaces that disagreed: the decisions engine
# recovered a live operator ruling for an exact subject and nothing consumed
# it, while the merge predicate recognized only a non-author approval or the
# `coverage-override` label. These tests hold the join: the deciding list is
# the native decisions door (through the gate's one seam, `law_authority`),
# only `single` is authority, and every other answer keeps the ordinary
# verdict.


def _waive_env(monkeypatch, tmp_path):
    """An attended operator terminal, a hermetic journal, the sandbox index."""

    monkeypatch.setenv("FNO_REPO_ROOT", str(tmp_path))
    monkeypatch.setenv("FNO_EVENTS_PATH", str(_journal(tmp_path)))
    (tmp_path / ".fno").mkdir(parents=True, exist_ok=True)
    from fno.agents import self_stamp

    # No harness identity + a terminal is the one state the superuser lane
    # permits; a resolved session identity is the one it refuses.
    monkeypatch.setattr(
        self_stamp,
        "resolve_self_identity",
        lambda: SimpleNamespace(session_id=None, harness=None),
    )


def _law_law(monkeypatch, scoped="none", standing="none"):
    """Pin the gate's one law seam: what current_law answers per subject."""

    def fake(subject):
        if subject == _coverage_gate.STANDING_WAIVER_SUBJECT:
            if standing == "unknown":
                return "unknown", f"decision probe failed for {subject}: boom"
            return standing, ""
        if scoped == "unknown":
            return "unknown", f"decision probe: conflicting law rows for {subject}"
        return scoped, ""

    monkeypatch.setattr(_coverage_gate, "law_authority", fake)


WAIVE_HEAD = "f" * 40

# The decisions door's per-test answer: flattened rows the gate reads, plus
# the damaged count the door reports. The door-side counting and lifecycle
# derivation are Rust-tested; this stub holds the gate's fold.
_DOOR_ROWS: list[dict] = []
_DOOR_DAMAGED = 0


@pytest.fixture(autouse=True)
def _decisions_door(monkeypatch):
    """Serve the decisions listing from the module's captured rows."""
    global _DOOR_ROWS, _DOOR_DAMAGED
    _DOOR_ROWS = []
    _DOOR_DAMAGED = 0

    def answer(payload):
        assert payload["mode"] == "decisions", payload
        argv = payload.get("argv") or []
        subject = next((a for a in argv if not a.startswith("-")), None)
        rows = [r for r in _DOOR_ROWS if subject is None or r["subject"] == subject]
        return {"decisions": rows, "damaged": _DOOR_DAMAGED}

    monkeypatch.setattr("fno.rust_binary.call_front_json", answer)


def _seed_door_row(subject, decision_id, *, decision, authority_source, ts="2026-08-29T00:00:00Z"):
    """One flattened law-lane row in the door's answer."""
    _DOOR_ROWS.append(
        {
            "decision_id": decision_id,
            "decision": decision,
            "subject": subject,
            "authority_source": authority_source,
            "ts": ts,
            "lane": "law",
            "lifecycle": "live",
        }
    )


@pytest.fixture(autouse=True)
def _sandbox_decision_graph(tmp_path, monkeypatch):
    """Keep this module's decision reads entirely inside the test's tmp.

    The graph-backed reads resolve through paths.graph_json, and the engine's
    index seam derives decisions.jsonl from paths.ledger_json - both must land
    in the SAME per-test directory, or the seeder writes one file while the
    reader reads the worker's shared conftest sandbox, where an earlier
    test's import leaves rows that shadow the fresh seed.
    """
    monkeypatch.setattr("fno.paths.graph_json", lambda: tmp_path / ".decision-index" / "graph.json")
    monkeypatch.setattr("fno.pr._review_hold.resolve_pr_worktree", lambda _pr, repo: repo)
    monkeypatch.setattr(
        "fno.paths.ledger_json", lambda: tmp_path / ".decision-index" / "ledger.json"
    )
    monkeypatch.setattr(
        "fno.paths.decisions_jsonl",
        lambda: tmp_path / ".decision-index" / "decisions.jsonl",
    )


def test_law_authority_folds_the_door_answer_three_ways():
    """The seam is the door's law-lane live answer, not a second deciding
    list: none, single (affirmative value only), damaged -> unknown."""

    def _row(decision):
        return {
            "decision_id": "d-law0001",
            "decision": decision,
            "subject": _coverage_gate.scoped_waiver_subject("acme/widgets", 42, WAIVE_HEAD),
            "authority_source": "operator",
            "ts": "2026-08-29T00:00:00Z",
            "lane": "law",
            "lifecycle": "live",
        }

    subject = _coverage_gate.scoped_waiver_subject("acme/widgets", 42, WAIVE_HEAD)
    _DOOR_ROWS.clear()
    assert _coverage_gate.law_authority(subject) == ("none", "")
    _DOOR_ROWS.append(_row(_coverage_gate.WAIVER_DECISION))
    assert _coverage_gate.law_authority(subject) == ("single", "")
    # Row existence carries no polarity: a denial recorded at the waiver
    # subject is a single law row whose text is not the affirmative value.
    _DOOR_ROWS[0] = _row("coverage waiver DENIED for this head")
    assert _coverage_gate.law_authority(subject) == ("none", "")
    # A single row with NO decision field at all is malformed authority, not
    # a clean no: unknown, with the dead field nameable in the probe.
    row_no_decision = _row(_coverage_gate.WAIVER_DECISION)
    del row_no_decision["decision"]
    _DOOR_ROWS[0] = row_no_decision
    status, probe = _coverage_gate.law_authority(subject)
    assert status == "unknown"
    assert "no decision" in probe, probe
    global _DOOR_DAMAGED
    _DOOR_ROWS[0] = _row(_coverage_gate.WAIVER_DECISION)
    _DOOR_DAMAGED = 1
    status, probe = _coverage_gate.law_authority(subject)
    assert status == "unknown"
    assert "damaged" in probe


def _stub_decide_door(monkeypatch, *, exit_code=0, stdout="d-door0001", stderr=""):
    """The native decide door, stubbed at the subprocess seam. A zero exit
    lands the operator row in the door stub's answer, exactly as the real
    write would make it readable."""

    import subprocess as subprocess_mod

    real_run = subprocess_mod.run

    def run(argv, **kwargs):
        if "decide" not in argv:
            return real_run(argv, **kwargs)
        subject = argv[argv.index("decide") + 1]
        if exit_code == 0:
            _seed_door_row(
                subject,
                stdout.strip(),
                decision=_coverage_gate.WAIVER_DECISION,
                authority_source="operator",
            )
        return SimpleNamespace(returncode=exit_code, stdout=stdout, stderr=stderr)

    monkeypatch.setattr("fno.rust_binary.resolve_front_binary", lambda: "/stub/fno")
    monkeypatch.setattr("subprocess.run", run)


def test_coverage_waive_refuses_an_agent_session_positively(monkeypatch, tmp_path, capsys):
    """A session a harness identifies records nothing: exit nonzero, a
    refusal marker that says who was refused, and no row any gate could read
    as a waiver."""
    _waive_env(monkeypatch, tmp_path)
    _stub_decide_door(
        monkeypatch,
        exit_code=3,
        stderr="decide: refused. agent cl71578 cannot record under superuser authority",
    )
    monkeypatch.setattr(_coverage_gate, "_repo_slug", lambda cwd: "acme/widgets")
    monkeypatch.setattr(_merge, "_pr_head_oid", lambda pr, repo: WAIVE_HEAD)
    rc = _coverage_gate.run_coverage_waive(42, "because", cwd=str(tmp_path))
    cap = capsys.readouterr()
    assert rc == 3
    assert "coverage-waive refused" in cap.err
    assert "cannot record under superuser authority" in cap.err
    subject = _coverage_gate.scoped_waiver_subject("acme/widgets", 42, WAIVE_HEAD)
    assert _coverage_gate.law_authority(subject)[0] == "none"


def test_coverage_waive_refuses_an_unreadable_head(monkeypatch, tmp_path, capsys):
    """No head, no waiver: the failed instrument is named and nothing is
    recorded."""
    _waive_env(monkeypatch, tmp_path)
    monkeypatch.setattr(_coverage_gate, "_repo_slug", lambda cwd: "acme/widgets")
    monkeypatch.setattr(_merge, "_pr_head_oid", lambda pr, repo: None)
    rc = _coverage_gate.run_coverage_waive(42, "because", cwd=str(tmp_path))
    cap = capsys.readouterr()
    assert rc == 4
    assert "pr head fetch failed" in cap.err
    assert cap.out.strip() == ""


def test_recorded_scoped_waiver_covers_only_its_head(monkeypatch, tmp_path, capsys):
    """End to end through the real store: record the waiver at one head, the
    gate covers THAT head with the scoped receipt, and a push refuses again
    because the new head's subject matches nothing."""
    _waive_env(monkeypatch, tmp_path)
    _specimen_gates(monkeypatch)
    _stub_decide_door(monkeypatch)
    monkeypatch.setattr(_merge, "_pr_head_oid", lambda pr, repo: WAIVE_HEAD)
    _seed_row(tmp_path, coverage="uncovered", count=0, head=WAIVE_HEAD)
    monkeypatch.setattr(_coverage_gate, "_repo_slug", lambda cwd: "acme/widgets")
    rc = _coverage_gate.run_coverage_waive(
        42, "operator reviewed this head by hand", cwd=str(tmp_path)
    )
    assert rc == 0
    # The door stub's answer, not the pinned seam: the recorded row is the
    # authority the gate consumed.
    state, refusal, covered_head, note = _coverage_gate.coverage_verdict(
        42, str(tmp_path), recompute=False
    )
    assert state == _coverage_gate.COVERED
    assert covered_head == WAIVE_HEAD
    assert note == (
        _coverage_gate.OVERRIDE_NOTE_PREFIX + f"head-pinned operator waiver at {WAIVE_HEAD[:8]}"
    )
    # A push moves the head; the old subject no longer names it.
    monkeypatch.setattr(_merge, "_pr_head_oid", lambda pr, repo: "e" * 40)
    state, refusal, _head, _note = _coverage_gate.coverage_verdict(
        42, str(tmp_path), recompute=False
    )
    assert state == _coverage_gate.REFUSED
    assert "HEAD is eeeeeeee" in refusal


def _seed_waiver_law_row(
    subject, decision_id, *, decision, authority_source, ts="2026-08-29T00:00:00Z"
):
    """One live law-lane row at an exact subject, in the door stub's answer."""
    _seed_door_row(
        subject, decision_id, decision=decision, authority_source=authority_source, ts=ts
    )


def test_a_chat_attested_waiver_row_is_not_authority(monkeypatch, tmp_path):
    """The law door is open to any harness-descended process, so a
    chat_attested row cannot carry the person-at-a-terminal fact a waiver
    asserts - even when its decision text matches WAIVER_DECISION exactly.
    The gate reads a clean no and the merge verdict stays REFUSED."""
    subject = _coverage_gate.scoped_waiver_subject("acme/widgets", 42, WAIVE_HEAD)
    _seed_waiver_law_row(
        subject,
        "d-agent001",
        decision=_coverage_gate.WAIVER_DECISION,
        authority_source="chat_attested",
    )
    assert _coverage_gate.law_authority(subject) == ("none", "")

    # End to end: the forged-looking row opens nothing.
    _specimen_gates(monkeypatch)
    _seed_row(tmp_path, coverage="uncovered", count=0, head=WAIVE_HEAD)
    monkeypatch.setattr(_merge, "_pr_head_oid", lambda pr, repo: WAIVE_HEAD)
    monkeypatch.setattr(_coverage_gate, "_repo_slug", lambda cwd: "acme/widgets")
    state, refusal, _head, _note = _coverage_gate.coverage_verdict(
        42, str(tmp_path), recompute=False
    )
    assert state == _coverage_gate.REFUSED


def test_standing_law_waives_an_uncovered_head_on_the_real_merge(
    enabled,  # noqa: F811
    live_head,
    monkeypatch,
    capsys,
    tmp_path,
):
    """The standing ruling, green ordinary verdict apart: run_merge reaches
    the merge call and the receipt says which law waived it."""
    monkeypatch.setattr(
        _merge, "_code_review_attestation_required", lambda repo, pr_number=0: False
    )
    _seed_row(tmp_path, coverage="uncovered", count=0, head=HEAD)
    _law_law(monkeypatch, standing="single")
    fake = FakeRun(toplevel=str(tmp_path))
    monkeypatch_run = pytest.MonkeyPatch()
    monkeypatch_run.setattr(_merge, "run", fake)
    try:
        assert _merge.run_merge(["42"], cwd=str(tmp_path)) == 0
        cap = capsys.readouterr()
    finally:
        monkeypatch_run.undo()
    assert "coverage waived: standing operator law" in cap.err


def test_standing_law_does_not_clear_a_hard_finding(monkeypatch, tmp_path, capsys):
    """Standing law is the NARROW waiver: below the cap, an unresolved
    CONFIRMED security finding keeps its refusal and its finding key - and
    no merge call happens. (At the cap the budget discharges before the
    overlay is ever consulted.)"""
    _specimen_gates(monkeypatch)
    _ac7_seed(tmp_path, rounds=1)
    _law_law(monkeypatch, standing="single")
    rc = _coverage_gate.run_coverage_check(42, cwd=str(tmp_path))
    cap = capsys.readouterr()
    line = (cap.err.strip().splitlines() or [""])[0]
    assert rc == _coverage_gate.REFUSED
    assert "hooks/git-protection.py:302:security" in line


def test_unknown_authority_is_unanswered_naming_the_probe(
    enabled,  # noqa: F811
    live_head,
    monkeypatch,
    tmp_path,
):
    """A dead or conflicted decision probe is UNKNOWN authority, never
    absence: when the gate needs the waiver, the answer is UNANSWERED with
    the probe named, not a refusal built on an unread store."""
    monkeypatch.setattr(
        _merge, "_code_review_attestation_required", lambda repo, pr_number=0: False
    )
    _seed_row(tmp_path, coverage="uncovered", count=0, head=HEAD)
    _law_law(monkeypatch, scoped="unknown", standing="unknown")
    monkeypatch.setattr(_coverage_gate, "_repo_slug", lambda cwd: "acme/widgets")
    state, refusal, _head, note = _coverage_gate.coverage_verdict(
        42, str(tmp_path), recompute=False
    )
    assert state == _coverage_gate.UNANSWERED
    assert refusal == ""
    assert "decision probe" in note
    assert note.count("decision probe") == 2


def test_a_missing_code_review_is_not_answered_by_another_bots_stale_row(
    enabled,  # noqa: F811
    live_head,
    monkeypatch,
    capsys,
    tmp_path,
):
    """The load-bearing regression: required code-review never ran at ANY sha.

    Same row as the attestation-refusal test above - covered, counted, one
    github_app verdict - so the failing conjunct is the missing local pass,
    not staleness. The spy answers True to the UNNAMED question and False to
    the named one, so the waiver sentence appears here only if the branch
    asked "does any stale verdict exist". It must ask for `code-review`.
    """
    from fno.pr import _coverage_gate

    asked = []

    def spy(cov, name=None):
        asked.append(name)
        return name is None

    monkeypatch.setattr(_coverage_gate, "_has_stale_verdict", spy)
    monkeypatch.setattr(_merge, "_code_review_attestation_required", lambda repo, pr_number=0: True)
    _seed_row(
        tmp_path,
        coverage="covered",
        count=2,
        head=HEAD,
        verdicts=[{"name": "some-bot", "producer": "github_app", "verdict": "reviewed"}],
    )
    state, refusal, _head, _note = _coverage_gate.coverage_verdict(
        42, str(tmp_path), recompute=False
    )

    assert state == _coverage_gate.REFUSED
    assert "required code-review has no head-pinned local pass attestation" in refusal
    assert "coverage-waive" not in refusal, refusal
    assert asked == ["code-review"], asked


def test_a_decline_at_head_is_not_offered_a_merge_lever(
    enabled,  # noqa: F811
    live_head,
    monkeypatch,
    capsys,
    tmp_path,
):
    """A reviewer that refused AT HEAD is not a head that moved.

    The waiver answers staleness. A decline is a stronger signal than a stale
    row and it is a different one, so appending "this may merge without a
    head-pinned attestation" underneath it reads as a way around the decline.
    The spy answers True to every staleness question, so the sentence appears
    here only if the branch forgot to exclude the refusal.
    """
    from fno.pr import _coverage_gate

    monkeypatch.setattr(_coverage_gate, "_has_stale_verdict", lambda cov, name=None: True)
    monkeypatch.setattr(
        _merge, "_code_review_attestation_required", lambda repo, pr_number=0: False
    )
    # The conjunct is the seam under test. Seeding `review_state` alone does
    # not reach it: the row is reshaped on read, so a refusal with no verdicts
    # normalizes back to plain `uncovered` and the exclusion never runs.
    monkeypatch.setattr(
        _coverage_gate, "covered_conjuncts", lambda cov, head, req: (False, "reviewer_refused")
    )
    _seed_row(tmp_path, coverage="uncovered", count=0, head=HEAD)
    state, refusal, _head, _note = _coverage_gate.coverage_verdict(
        42, str(tmp_path), recompute=False
    )

    assert state == _coverage_gate.REFUSED
    assert "coverage-waive" not in refusal, refusal


# ---- the shared cap computation (cap_verdict) ------------------------------
#
# Every fixture here is CONSTRUCTED. The two live PRs that measured this
# defect are being cleared while this code lands, so no test may source its
# chain from a live PR: a red test that expires the moment somebody fixes
# the specimen is a test that guards nothing. The divergence fixture is the
# load-bearing one - on both live specimens the two axes AGREE, so a
# fixture where they agree proves the helper runs, never which axis it
# reads (a green control aimed at the wrong symbol).


def _cap_chain(rounds, *, first_verdict="fail", dispositions_at=None, category="correctness"):
    """A constructed branch chain: `rounds` attestations, one per head.

    Round 0 raises a CONFIRMED finding of `category`; every later round is
    a findings-free pass unless `dispositions_at` names a round index whose
    pass carries a `fixed` disposition for that finding.
    """
    dispositions_at = dispositions_at if dispositions_at is not None else -1
    lines = []
    for i in range(rounds):
        data = {
            "reviewer": "code-review",
            "head_sha": f"{i:040x}",
            "verdict": first_verdict if i == 0 else "pass",
            "session_id": "s-cap",
            "branch": "feature/x-cap",
            "reviewed_base_sha": "a" * 40,
            "reviewed_head_sha": f"{i:040x}",
            "findings_blocking": 1 if i == 0 else 0,
            "findings": (
                [
                    {
                        "category": category,
                        "verdict": "CONFIRMED",
                        "blocking": True,
                        "has_required_fields": True,
                        "finding_key": f"cli/src/fake.py:779:{category}",
                    }
                ]
                if i == 0
                else []
            ),
        }
        if i == dispositions_at:
            data["dispositions"] = [
                {
                    "finding_key": f"cli/src/fake.py:779:{category}",
                    "disposition": "fixed",
                    "reason": "verified the fix delta",
                }
            ]
        lines.append(
            {
                "ts": f"2026-08-31T2{i:02d}:00:00Z",
                "type": "review_attestation",
                "source": "hook",
                "data": data,
            }
        )
    return lines


def _seed_cap_chain(tmp_path, lines, *, extra_rows=()):
    """Seed the journal the gate reads (the repo's space; a re-seed must land
    where the reader resolves, which a legacy checkout path no longer is)."""
    (tmp_path / ".fno").mkdir(exist_ok=True)
    from fno.paths import project_log as _pl

    journal = _pl("events.jsonl", project_root=tmp_path)
    journal.parent.mkdir(parents=True, exist_ok=True)
    text = "\n".join(json.dumps(e) for e in lines) + "\n"
    for row in extra_rows:
        text += json.dumps(row) + "\n"
    journal.write_text(text, encoding="utf-8")


_CAP_HARD_KEY = "cli/src/fake.py:779:correctness"


def _cap_cov_row():
    return {
        "coverage": "covered",
        "review_state": "reviewed",
        "reviewed_count": 1,
        "head_sha": f"{5:040x}",
        "verdicts": [],
    }


def test_cap_verdict_counts_the_chain_and_names_the_key(tmp_path):
    """One fail raising a CONFIRMED correctness finding, then five
    findings-free passes, at max_rounds 2: six rounds counted, the key
    named. The key is context at the cap, never a blocker."""
    _seed_cap_chain(tmp_path, _cap_chain(6))
    cap = _coverage_gate.cap_verdict(str(tmp_path), f"{5:040x}", "feature/x-cap", _cap_cov_row())
    assert cap.rounds_used == 6
    assert cap.max_rounds == 2
    assert _CAP_HARD_KEY in cap.nonterminal_keys
    assert not hasattr(cap, "impossible")
    assert not hasattr(cap, "hard_keys")


def test_cap_verdict_rounds_read_the_max_of_both_axes(tmp_path):
    """The two-axis budget: 1 events-axis round beside a reviews payload
    naming 5 distinct reviewed commits reads 5 (a bot round IS a round), and
    the same chain without a payload reads the events axis alone."""
    _seed_cap_chain(tmp_path, _cap_chain(1))
    reviews = [{"state": "APPROVED", "commit": {"oid": f"r{i:038x}"}} for i in range(5)]
    cap = _coverage_gate.cap_verdict(
        str(tmp_path), f"{0:040x}", "feature/x-cap", _cap_cov_row(), reviews=reviews
    )
    assert cap.rounds_used == 5
    events_only = _coverage_gate.cap_verdict(
        str(tmp_path), f"{0:040x}", "feature/x-cap", _cap_cov_row()
    )
    assert events_only.rounds_used == 1


def test_cap_verdict_mixed_declared_chain_reads_two(tmp_path):
    """The parity corpus row: an undeclared fail at head A, a declared verify
    pass (review_round 1) at head B, an undeclared fail at head C. The
    declared verify does not double-count the fail it verifies; the trailing
    undeclared round still counts, so both mirrors read 2. The Rust half is
    cap_the_mixed_declared_chain_reads_two_on_both_mirrors."""
    rows = []
    for i, (verdict, declared) in enumerate([("fail", None), ("pass", 1), ("fail", None)]):
        data = {
            "reviewer": "code-review",
            "head_sha": f"{i:040x}",
            "verdict": verdict,
            "session_id": "s-cap",
            "branch": "feature/x-cap",
            "reviewed_base_sha": "a" * 40,
            "reviewed_head_sha": f"{i:040x}",
            "findings_blocking": 1 if verdict == "fail" else 0,
            "findings": (
                [
                    {
                        "category": "correctness",
                        "verdict": "CONFIRMED",
                        "blocking": True,
                        "has_required_fields": True,
                        "finding_key": _CAP_HARD_KEY,
                    }
                ]
                if verdict == "fail"
                else []
            ),
        }
        if declared is not None:
            data["review_round"] = declared
        rows.append(
            {"ts": f"2026-08-31T2{i:02d}:00:00Z", "type": "review_attestation",
             "source": "hook", "data": data}
        )
    _seed_cap_chain(tmp_path, rows)
    cap = _coverage_gate.cap_verdict(
        str(tmp_path), f"{2:040x}", "feature/x-cap", _cap_cov_row()
    )
    assert cap.rounds_used == 2


def test_two_fail_attestations_from_the_authors_own_session_cover(monkeypatch, tmp_path):
    """AC1's core shape: two FAIL rounds, self-attested, on the PR head,
    budget two - COVERED, no refusal, the note naming the discharged budget
    and the operator lever. The specimen that started this node: the lane's
    emit step was refused as self approval, the ledger saw no round, and the
    gate locked a green PR."""
    _specimen_gates(monkeypatch)
    _ac7_seed(tmp_path, rounds=2)
    state, refusal, covered_head, note = _coverage_gate.coverage_verdict(
        42, str(tmp_path), recompute=False
    )
    assert state == _coverage_gate.COVERED
    assert refusal == ""
    assert covered_head == FIXTURE_HEAD
    assert "review budget discharged (2/2 rounds)" in note
    assert "config.review.max_rounds" in note
