"""The review hold is taken and cleared by footnote's own code, not by memory.

`fno do pr merge` is fail-closed against the hold, and the hold works. What
failed on PR 1575 is that nothing took it: the merge fired while the only
non-author review was still running, and eight findings (one HIGH) were
discarded. A hold nobody takes is identical to a hold that does not exist.

The requester verb takes it; the attestation clears it, because a verdict for
this head now exists and merge readiness can see the review.

Nothing here counts a round or gates a merge on origin (laws d-0fa92eb9,
d-777e7d1f). A hold says a review is RUNNING, and that is all.
"""

import json
from pathlib import Path

import pytest

from fno import target_cli


@pytest.fixture()
def taken(monkeypatch):
    calls: list[dict] = []
    monkeypatch.setattr(
        "fno.pr._review_hold.acquire_review_hold",
        lambda branch, **kw: calls.append({"branch": branch, **kw}),
    )
    monkeypatch.setattr("fno.pr._review_hold.review_invocation_refusal", lambda *a, **kw: "")
    monkeypatch.setattr(target_cli, "_git_out", lambda cwd, *args: "feature/x-5ca3")
    return calls


def test_a_sent_review_request_takes_the_hold(taken) -> None:
    target_cli._hold_branch_under_review(
        Path("/repo"),
        head_sha="deadbeef" * 5,
        session_id="sess-1",
        receipt={"outcome": "queued"},
        branch="feature/x-5ca3",
    )

    assert len(taken) == 1
    assert taken[0]["branch"] == "feature/x-5ca3"
    assert taken[0]["head"] == "deadbeef" * 5
    assert taken[0]["holder"] == "review-session:sess-1"


def test_an_unsent_request_takes_nothing(taken) -> None:
    """No review is running, so nothing may claim one is."""
    target_cli._hold_branch_under_review(
        Path("/repo"),
        head_sha="abc",
        session_id="sess-1",
        receipt={"outcome": "refused"},
        branch="feature/x-5ca3",
    )

    assert taken == []


def test_a_refused_invocation_takes_nothing(taken, monkeypatch) -> None:
    """The deadlock this avoids, and why the gate is here rather than skipped.

    A refused invocation runs no review and emits no attestation, so nothing
    would ever release the hold. The merge the refusal is telling the worker to
    take would then be blocked for the full TTL by a review that never started.
    """
    monkeypatch.setattr(
        "fno.pr._review_hold.review_invocation_refusal",
        lambda *a, **kw: "review rounds spent: 2 of 2",
    )

    target_cli._hold_branch_under_review(
        Path("/repo"),
        head_sha="abc",
        session_id="s",
        receipt={"outcome": "queued"},
        branch="feature/x-5ca3",
    )

    assert taken == []


def test_an_empty_branch_takes_nothing_even_when_the_cwd_has_one(taken, monkeypatch) -> None:
    """x-b5f6: the cwd fallback keyed a bystander branch - a review of 1713
    held 1709's. The caller resolves the branch; an unresolved one takes no
    hold rather than a guessed one. The control call in the same root records
    exactly one acquire, which proves the recorder ran."""
    monkeypatch.setattr(target_cli, "_git_out", lambda cwd, *args: "feature/bystander")

    target_cli._hold_branch_under_review(
        Path("/repo"), head_sha="abc", session_id="s", receipt={"outcome": "queued"}, branch=""
    )
    assert taken == []

    target_cli._hold_branch_under_review(
        Path("/repo"),
        head_sha="abc",
        session_id="s",
        receipt={"outcome": "queued"},
        branch="feature/x-5ca3",
    )
    assert len(taken) == 1
    assert taken[0]["branch"] == "feature/x-5ca3"


def test_a_lockfile_failure_never_refuses_the_sent_review(monkeypatch) -> None:
    def _boom(branch, **kw):
        raise OSError("claims root read-only")

    monkeypatch.setattr("fno.pr._review_hold.acquire_review_hold", _boom)
    monkeypatch.setattr("fno.pr._review_hold.review_invocation_refusal", lambda *a, **kw: "")
    monkeypatch.setattr(target_cli, "_git_out", lambda cwd, *args: "feature/x")

    # Returns, does not raise: the review was already sent.
    target_cli._hold_branch_under_review(
        Path("/repo"),
        head_sha="abc",
        session_id="s",
        receipt={"outcome": "queued"},
        branch="feature/x",
    )


def test_the_post_push_form_holds_the_pr_head_ref_without_breaking_the_payload(
    monkeypatch, capsys
) -> None:
    """The hold key and the payload target are two different things.

    Reusing one variable for both broke the post-push form outright: the
    renderer refuses `pr_number` and `branch` together, that refusal was
    caught as a generic failure, and every `request-self-review --pr <n>`
    exited 2. The hold still has to key on the PR's own head ref, because
    the merge guard resolves the branch from GitHub and looks up that key.
    The door now takes the target as flags, so the argv carries `--pr-number`
    and must carry no `--branch`.
    """
    calls: list[list] = []
    held: list[dict] = []

    def _render(argv, payload, *args, **kwargs):
        calls.append(list(argv))
        return {"invocation": "/review high --comment", "harness": "claude"}

    monkeypatch.setattr("fno.rust_binary.verb_call", _render)
    monkeypatch.setattr(target_cli, "_git_out", lambda cwd, *args: "deadbeef" * 5)
    monkeypatch.setattr(
        target_cli,
        "_read_pr_metadata",
        lambda pr, cwd: {
            "number": pr,
            "headRefOid": "deadbeef" * 5,
            "headRefName": "feature/pr-head",
            "baseRefName": "main",
        },
    )
    monkeypatch.setattr(target_cli, "_resolve_self_review_identity", lambda: ("claude", "sess-1"))
    monkeypatch.setattr(target_cli, "_send_self_review_payload", lambda **kw: {"outcome": "queued"})
    monkeypatch.setattr(target_cli, "_hold_branch_under_review", lambda cwd, **kw: held.append(kw))

    target_cli.request_self_review_cmd(pr_number=1584)

    receipt = json.loads(capsys.readouterr().out.strip().splitlines()[-1])
    assert receipt["outcome"] == "queued"
    argv = calls[0]
    assert argv[:3] == ["review-level", "capability", "invocation"]
    assert "--pr-number" in argv and "1584" in argv
    assert "--branch" not in argv
    assert held[0]["branch"] == "feature/pr-head"
