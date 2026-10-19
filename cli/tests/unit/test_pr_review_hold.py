"""The in-flight-review guard: a running review must block a merge.

Merge readiness models a review as a RECORDED VERDICT, so a review that is
EXECUTING is invisible to it. These tests pin both layers of the answer: the
registered hold (a TTL claim) and the derived worktree probe that catches a
review nothing registered.
"""
from __future__ import annotations

from pathlib import Path

import pytest

from fno.claims.core import acquire_claim
from fno.claims.types import now_ms
from fno.pr import _review_hold
from fno.pr._proc import Result, ToolMissing
from fno.rust_binary import VerbUnavailable


DEAD_PID = 2**30  # far above any live pid; is_live() reads it as dead


def _fake_git(responses):
    """A ``_proc.run`` stand-in keyed on the first distinguishing argument."""

    def runner(cmd, *, cwd=None, env=None, input_text=None, timeout=None):
        key = next((part for part in cmd if part in responses), None)
        if key is None:
            raise AssertionError(f"unexpected command: {cmd}")
        value = responses[key]
        if isinstance(value, Exception):
            raise value
        return value

    return runner


def _worktree_list(entries):
    """Render ``git worktree list --porcelain`` output for (path, head, branch)."""
    blocks = []
    for path, head, branch in entries:
        block = f"worktree {path}\nHEAD {head}\n"
        block += "detached\n" if branch is None else f"branch refs/heads/{branch}\n"
        blocks.append(block)
    return Result(returncode=0, stdout="\n".join(blocks) + "\n", stderr="")


def _expire_claim(root: Path, key: str) -> None:
    from tests._table_seed import update_claim

    update_claim(key, root=root, expires_at=now_ms() - 1)


NO_WORKTREE = _fake_git({"worktree": _worktree_list([])})

# A listed worktree whose directory is GONE is skipped, not probed (git keeps
# listing it until someone prunes), so every fixture that expects a probe needs
# a path that really exists.
WT_PATH = Path(__file__).resolve().parent
    # A repo-local key (not a global-id prefix; the native routing list lives
    # in crates/fno-agents/src/claims_root.rs): a review hold must live beside
    # the repo whose merge it guards.


def test_pr_worktree_resolution_from_canonical_subdir_uses_the_pr_branch(tmp_path: Path, monkeypatch):
    canonical = tmp_path / "canonical"
    feature = tmp_path / "feature-worktree"
    canonical.mkdir()
    nested = canonical / "nested"
    nested.mkdir()
    (canonical / ".git").mkdir()
    feature.mkdir()
    seen: list[dict] = []

    def verb_call(verb, payload, unavailable=None):
        assert verb == "pr-worktree"
        assert payload == {"cwd": str(nested), "pr": 42}
        seen.append(payload)
        return {"worktree": str(feature)}

    monkeypatch.setattr("fno.rust_binary.verb_call", verb_call)

    resolved = _review_hold.resolve_pr_worktree(42, str(nested))

    assert resolved == str(feature)
    assert seen == [{"cwd": str(nested), "pr": 42}]

    # The verb's exit-3 "no local worktree" answer is an answer (""), never a
    # blocker: a PR pushed with no local worktree carries no worktree hold,
    # and the exit-3 refusal used to propagate as a traceback instead.
    def no_worktree(verb, payload, unavailable=None):
        exc = VerbUnavailable(
            "fno-agents pr-worktree exited 3: pr-worktree: "
            "no local worktree on PR branch fix/some-branch"
        )
        exc.returncode = 3
        raise exc

    monkeypatch.setattr("fno.rust_binary.verb_call", no_worktree)
    assert _review_hold.resolve_pr_worktree(42, str(nested)) == ""

    # Exit 3 WITHOUT that answer (e.g. the gh read failed) still raises: a
    # dead instrument blocks, only an empty enumeration is an answer.
    def gh_failed(verb, payload, unavailable=None):
        exc = VerbUnavailable(
            "fno-agents pr-worktree exited 3: pr-worktree: gh api pulls/42 failed"
        )
        exc.returncode = 3
        raise exc

    monkeypatch.setattr("fno.rust_binary.verb_call", gh_failed)
    with pytest.raises(VerbUnavailable):
        _review_hold.resolve_pr_worktree(42, str(nested))


def test_free_hold_and_no_worktree_is_clear(tmp_path: Path):
    activity = _review_hold.review_activity(
        "feature/x", pr_head="abc123", repo=str(tmp_path), root=tmp_path, runner=NO_WORKTREE
    )
    assert activity.blocked is False
    assert activity.blocker == ""
    assert activity.hold is None
    # AC7: the enumeration RAN and answered. That is what makes an absent hold
    # a clear verdict rather than an unanswered one.
    assert activity.worktree["probed"] is True
    assert activity.worktree["path"] is None


def test_live_hold_blocks_with_review_in_flight(tmp_path: Path):
    acquire_claim(
        _review_hold.review_hold_key("feature/x"),
        "reviewer:sess-1",
        ttl_ms=600_000,
        metadata={"head_sha": "abc123", "verb": "/code-review"},
        root=tmp_path,
    )
    activity = _review_hold.review_activity(
        "feature/x", pr_head="abc123", repo=str(tmp_path), root=tmp_path, runner=NO_WORKTREE
    )
    assert activity.blocked is True
    assert activity.blocker == "review_in_flight"
    assert activity.hold is not None
    assert activity.hold["holder"] == "reviewer:sess-1"
    assert "reviewer:sess-1" in activity.detail


def test_corrupted_hold_blocks_rather_than_assuming_unheld(tmp_path: Path):
    from fno.claims.io import claim_path

    path = claim_path(_review_hold.review_hold_key("feature/x"), root=tmp_path)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("{ this is not a claim", encoding="utf-8")
    activity = _review_hold.review_activity(
        "feature/x", pr_head="abc123", repo=str(tmp_path), root=tmp_path, runner=NO_WORKTREE
    )
    assert activity.blocked is True
    assert activity.blocker == "review_hold_unreadable"
    assert "refusing to assume unheld" in activity.detail


def test_tracked_modifications_block_the_merge(tmp_path: Path):
    runner = _fake_git(
        {
            "worktree": _worktree_list([(str(WT_PATH), "abc123", "feature/x")]),
            "status": Result(returncode=0, stdout=" M cli/src/fno/pr/_merge.py\n", stderr=""),
        }
    )
    activity = _review_hold.review_activity(
        "feature/x", pr_head="abc123", repo=str(tmp_path), root=tmp_path, runner=runner
    )
    assert activity.blocked is True
    assert activity.blocker == "worktree_dirty"
    assert str(WT_PATH) in activity.detail


def test_untracked_scratch_never_blocks(tmp_path: Path):
    """A worker's worktree always carries untracked scratch; it is not evidence."""
    seen: list[list[str]] = []

    def runner(cmd, *, cwd=None, env=None, input_text=None, timeout=None):
        seen.append(list(cmd))
        if "worktree" in cmd:
            return _worktree_list([(str(WT_PATH), "abc123", "feature/x")])
        return Result(returncode=0, stdout="", stderr="")

    activity = _review_hold.review_activity(
        "feature/x", pr_head="abc123", repo=str(tmp_path), root=tmp_path, runner=runner
    )
    assert activity.blocked is False
    status_cmd = next(cmd for cmd in seen if "status" in cmd)
    assert "--untracked-files=no" in status_cmd


def test_worktree_head_mismatch_blocks(tmp_path: Path):
    runner = _fake_git(
        {
            "worktree": _worktree_list([(str(WT_PATH), "localsha", "feature/x")]),
            "status": Result(returncode=0, stdout="", stderr=""),
        }
    )
    activity = _review_hold.review_activity(
        "feature/x", pr_head="abc123", repo=str(tmp_path), root=tmp_path, runner=runner
    )
    assert activity.blocked is True
    assert activity.blocker == "worktree_head_mismatch"
    assert "localsha" in activity.detail and "abc123" in activity.detail


def test_a_dead_probe_blocks_rather_than_clearing(tmp_path: Path):
    runner = _fake_git({"worktree": ToolMissing("git")})
    activity = _review_hold.review_activity(
        "feature/x", pr_head="abc123", repo=str(tmp_path), root=tmp_path, runner=runner
    )
    assert activity.blocked is True
    assert activity.blocker == "worktree_probe_failed"
    assert activity.worktree["probed"] is False


def test_refusal_names_the_clear_command(tmp_path: Path):
    acquire_claim(
        _review_hold.review_hold_key("feature/x"),
        "reviewer:sess-1",
        ttl_ms=600_000,
        root=tmp_path,
    )
    refusal = _review_hold.review_hold_refusal(
        "feature/x", pr_head="abc123", repo=str(tmp_path), root=tmp_path, runner=NO_WORKTREE
    )
    assert refusal is not None
    # A refusal a reader cannot clear in one command is how a guard becomes a
    # wedge, so the command is part of the contract.
    assert "fno do pr review-hold release" in refusal


# ── round 2: what the self-review found ─────────────────────────────────────


def test_a_removed_worktree_is_skipped_not_probed(tmp_path: Path):
    """`git worktree list` keeps listing a removed worktree until someone
    prunes, and `git -C <gone path> status` fails. Reading that as a dead
    instrument held every post-merge retry at exit 2 forever."""
    gone = tmp_path / "archived-worktree"

    def runner(cmd, *, cwd=None, env=None, input_text=None, timeout=None):
        if "worktree" in cmd:
            return _worktree_list([(str(gone), "abc123", "feature/x")])
        raise AssertionError("a worktree that is not on disk must not be probed")

    activity = _review_hold.review_activity(
        "feature/x", pr_head="abc123", repo=str(tmp_path), root=tmp_path, runner=runner
    )
    assert activity.blocked is False
    assert activity.worktree["note"] == "no local worktree on this branch"


def test_release_does_not_require_the_acquiring_holder(tmp_path: Path):
    """The hold is a lane lock, not an ownership assertion. Every release site
    derives its own holder string, and `release_claim` no-ops SILENTLY on a
    mismatch, so a holder-matched release left the hold sitting for its TTL."""
    from fno.claims.core import claim_status

    _review_hold.acquire_review_hold(
        "feature/x", head="abc123", holder="review-session:acquired-by", root=tmp_path
    )
    assert _review_hold.release_review_hold("feature/x", root=tmp_path) is True
    assert claim_status(_review_hold.review_hold_key("feature/x"), root=tmp_path)["state"] == "free"


def test_release_clears_a_corrupted_claim(tmp_path: Path):
    """A corrupted claim has no readable holder, so no release_claim call can
    name one. Leaving it is the worse outcome: every read classifies it
    CORRUPTED, which BLOCKS, forever."""
    from fno.claims.core import claim_status
    from tests._table_seed import update_claim

    key = _review_hold.review_hold_key("feature/x")
    acquire_claim(key, "reviewer:sess-1", root=tmp_path)
    update_claim(key, root=tmp_path, schema_version=999)
    assert claim_status(key, root=tmp_path)["state"] == "corrupted"
    assert _review_hold.release_review_hold("feature/x", root=tmp_path) is True
    assert claim_status(key, root=tmp_path)["state"] == "free"


def test_an_expired_hold_is_deleted_in_the_same_breath_as_its_receipt(
    tmp_path: Path, capsys
):
    """The lapsed lockfile stops blocking HERE but still blocks the stdlib hook,
    which cannot judge expiry and denies on the file's mere presence. One
    crashed reviewer would deny every bare `gh pr merge` in the repo until
    someone noticed."""
    from fno.claims.core import claim_status

    key = _review_hold.review_hold_key("feature/x")
    acquire_claim(key, "reviewer:sess-1", ttl_ms=60_000, pid=DEAD_PID, root=tmp_path)
    _expire_claim(tmp_path, key)

    activity = _review_hold.review_activity(
        "feature/x", pr_head="abc123", repo=str(tmp_path), root=tmp_path, runner=NO_WORKTREE
    )
    assert activity.blocked is False
    assert "expired" in capsys.readouterr().err
    assert claim_status(key, root=tmp_path)["state"] == "free"


# ---- the review-cap invocation gate ----
#
# The two-round cap was enforced at the merge decision only, so rounds three
# through five ran against a saturated, visible counter. The invocation gate
# refuses a NEW hunting round at the spent budget, with the two carveouts the
# attestation law grants: a scoped fix-verification (--verify-fixes is not a
# round), and a rebase delta at or over the interdiff budget.


def _chain_event(head: str, base: str) -> dict:
    return {
        "ts": "2026-09-04T12:00:00Z",
        "head_sha": head,
        "reviewed_base_sha": base,
        "verdict": "pass",
        "review_round": None,
        "findings": [],
        "findings_truncated": False,
        "dispositions": [],
    }


def _spent_chain(last_head: str) -> list[dict]:
    base = "b" * 40
    return [
        _chain_event("a" * 40, base),
        _chain_event(last_head, base),
    ]


def test_spent_cap_refuses_a_new_round_at_the_same_head(monkeypatch, tmp_path: Path) -> None:
    last = "b" * 40
    monkeypatch.setattr(
        _review_hold, "_chain_read",
        lambda branch, head, cwd=None: _door_answer_from_chain(_spent_chain(last)),
    )
    refusal = _review_hold.review_invocation_refusal("feature/x", last, cwd=str(tmp_path))
    assert "the configured round cap is spent" in refusal
    assert "merge on green CI" in refusal
    assert "--verify-fixes" in refusal


def test_verify_fixes_flag_passes_at_the_cap(monkeypatch, tmp_path: Path) -> None:
    last = "b" * 40
    monkeypatch.setattr(
        _review_hold, "_chain_read",
        lambda branch, head, cwd=None: _door_answer_from_chain(_spent_chain(last)),
    )
    flags = ["--verify-fixes"]
    assert _review_hold.review_invocation_refusal("feature/x", last, flags=flags, cwd=str(tmp_path)) == ""


def test_rebase_delta_over_the_budget_passes_at_the_cap(monkeypatch, tmp_path: Path) -> None:
    last = "b" * 40
    monkeypatch.setattr(
        _review_hold, "_chain_read",
        lambda branch, head, cwd=None: _door_answer_from_chain(_spent_chain(last)),
    )
    monkeypatch.setattr(_review_hold, "_interdiff_lines", lambda *a, **k: 250)
    moved = "c" * 40
    assert _review_hold.review_invocation_refusal("feature/x", moved, cwd=str(tmp_path)) == ""


def _fail_event(head: str, base: str) -> dict:
    event = _chain_event(head, base)
    event["verdict"] = "fail"
    return event


def _round_runner(monkeypatch, tmp_path: Path, chain: list[dict]):
    """The `round` action with the chain and both journals pointed at fixtures."""
    from typer.testing import CliRunner

    from fno.pr import pr_app
    import fno.pr._reviews as reviews

    journal = tmp_path / "events.jsonl"
    journal.write_text("")
    monkeypatch.setattr(
        _review_hold, "_chain_read",
        lambda branch, head, cwd=None: _door_answer_from_chain(chain),
    )
    monkeypatch.setattr(
        reviews, "_coverage_logs", lambda cwd=None, project_events=None: (journal, None, None)
    )
    return CliRunner().invoke(
        pr_app,
        ["review-hold", "round", "--branch", "feature/x", "--head", "c" * 40, "--repo", str(tmp_path)],
    )


def test_round_action_prints_the_integer(monkeypatch, tmp_path: Path) -> None:
    r = _round_runner(monkeypatch, tmp_path, [_fail_event("a" * 40, "b" * 40)])
    assert r.exit_code == 0, r.output
    assert r.output.strip() == "1"


def test_round_action_fails_closed_with_no_journal(monkeypatch, tmp_path: Path) -> None:
    """No readable events journal: print nothing, exit non-zero, stamp nothing."""
    import fno.pr._reviews as reviews
    from typer.testing import CliRunner

    from fno.pr import pr_app

    monkeypatch.setattr(
        _review_hold, "_chain_read",
        lambda branch, head, cwd=None: _door_answer_from_chain([]),
    )
    monkeypatch.setattr(
        reviews,
        "_coverage_logs",
        lambda cwd=None, project_events=None: (tmp_path / "absent.jsonl", None, None),
    )
    r = CliRunner().invoke(
        pr_app,
        ["review-hold", "round", "--branch", "feature/x", "--head", "a" * 40, "--repo", str(tmp_path)],
    )
    assert r.exit_code != 0
    assert r.output.strip() == ""


def _fail_event_with_finding(head: str, base: str) -> dict:
    event = _fail_event(head, base)
    event["findings"] = [{"finding_key": "f.py:1:correctness", "blocking": True}]
    return event


def test_advisory_names_the_flag_when_the_head_moved_past_blockers(
    monkeypatch, tmp_path: Path
) -> None:
    chain = [_fail_event_with_finding("a" * 40, "b" * 40)]
    monkeypatch.setattr(
        _review_hold, "_chain_read",
        lambda branch, head, cwd=None: _door_answer_from_chain(chain),
    )
    adv = _review_hold.verify_fixes_advisory("feature/x", "c" * 40, cwd=str(tmp_path))
    assert "--verify-fixes" in adv
    assert "1 blocking finding" in adv
    assert ("a" * 40)[:8] in adv


def test_acquire_below_the_cap_advises_and_still_acquires(
    monkeypatch, tmp_path: Path
) -> None:
    """AC6: the acquire prints the advisory naming --verify-fixes and what it
    costs, then acquires anyway. The advisory never classifies the pass."""
    from typer.testing import CliRunner

    from fno.pr import pr_app

    chain = [_fail_event_with_finding("a" * 40, "b" * 40)]
    monkeypatch.setattr(
        _review_hold, "_chain_read",
        lambda branch, head, cwd=None: _door_answer_from_chain(chain),
    )
    monkeypatch.setenv("FNO_CLAIMS_ROOT", str(tmp_path))
    r = CliRunner().invoke(
        pr_app,
        [
            "review-hold", "acquire",
            "--branch", "feature/x",
            "--head", "c" * 40,
            "--holder", "test",
            "--repo", str(tmp_path),
        ],
    )
    assert r.exit_code == 0, r.output
    assert "holding feature/x" in r.output
    assert "--verify-fixes" in r.output


def _door_answer_from_chain(chain: list) -> dict:
    """The chain op door's answer shape, built from a chain fixture: rounds
    since the last pass (a declared round wins), the last row's verdict
    facts, budget pinned at 2."""
    rounds = 0
    for row in chain:
        declared = row.get("review_round")
        rounds = declared if declared else rounds + 1
    last = None
    if chain:
        row = chain[-1]
        blocking = sum(
            1
            for f in (row.get("findings") or [])
            if isinstance(f, dict) and f.get("blocking") is True
        )
        last = {
            "head_sha": row.get("head_sha", ""),
            "reviewed_base_sha": row.get("reviewed_base_sha", ""),
            "blocking": blocking,
            "findings_truncated": row.get("findings_truncated") is True,
        }
    return {"rounds_used": rounds, "max_rounds": 2, "last": last}
