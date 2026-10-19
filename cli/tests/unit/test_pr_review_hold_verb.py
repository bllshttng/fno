"""`fno do pr review-hold`: the verb behind the hold, for callers outside Python.

`check` carries the same exit contract as `coverage-check` - 0 clear, 3 held
with the refusal on stderr, 4 a NAMED instrument failure - because the callers
that cannot import fno read the two the same way.
"""
from __future__ import annotations

import pytest
from typer.testing import CliRunner

from fno.pr import _review_hold
from fno.pr.cli import pr_app


runner = CliRunner()


@pytest.fixture(autouse=True)
def _claims_in_tmp(monkeypatch, tmp_path):
    monkeypatch.setenv("FNO_CLAIMS_ROOT", str(tmp_path))


def _invoke(*args):
    return runner.invoke(pr_app, ["review-hold", *args])


def test_check_is_clear_when_nothing_is_running(monkeypatch, tmp_path):
    monkeypatch.setattr(
        "fno.pr._merge._pr_head_ref_and_oid", lambda pr, repo: ("feature/x-a089", "abc123", "OPEN")
    )
    monkeypatch.setattr(
        _review_hold, "review_hold_refusal", lambda *a, **kw: None
    )
    result = _invoke("check", "42", "--repo", str(tmp_path))
    assert result.exit_code == 0
    assert "no review in flight on feature/x-a089" in result.stdout


def test_check_exits_3_while_a_review_is_running(monkeypatch, tmp_path):
    monkeypatch.setattr(
        "fno.pr._merge._pr_head_ref_and_oid", lambda pr, repo: ("feature/x-a089", "abc123", "OPEN")
    )
    monkeypatch.setattr(
        _review_hold,
        "review_hold_refusal",
        lambda *a, **kw: "review_in_flight: held by reviewer:sess-1",
    )
    result = _invoke("check", "42", "--repo", str(tmp_path))
    assert result.exit_code == 3


def test_check_exits_4_when_the_probe_itself_could_not_run(monkeypatch, tmp_path):
    """Exit 4 is a dead instrument, never a verdict. A caller reading exit 3 as
    "held" must not also read a failed head lookup as one."""
    monkeypatch.setattr("fno.pr._merge._pr_head_ref_and_oid", lambda pr, repo: None)
    result = _invoke("check", "42", "--repo", str(tmp_path))
    assert result.exit_code == 4


def test_acquire_then_release_round_trips(tmp_path):
    from fno.claims.core import claim_status

    key = _review_hold.review_hold_key("feature/x-a089")
    acquired = _invoke(
        "acquire", "--branch", "feature/x-a089", "--head", "abc123",
        "--holder", "reviewer:sess-1", "--verb", "/code-review",
    )
    assert acquired.exit_code == 0
    assert claim_status(key, root=tmp_path)["state"] == "live"

    released = _invoke("release", "--branch", "feature/x-a089")
    assert released.exit_code == 0
    assert "released" in released.stdout
    assert claim_status(key, root=tmp_path)["state"] == "free"

    # Say which happened. A "released" printed over an absent hold is the
    # absence-as-success shape, on the one recovery command an operator has.
    again = _invoke("release", "--branch", "feature/x-a089")
    assert again.exit_code == 0
    assert "no hold on" in again.stdout


def test_acquire_resolves_a_named_pr_and_holds_its_head_ref(monkeypatch, tmp_path):
    """x-b5f6: `acquire <pr>` keys the hold on the PR's own head ref, resolved
    from GitHub, and the resolved pair replaces whatever the caller passed -
    the mis-key that held PR 1709's branch for a review of PR 1713."""
    from fno.claims.core import claim_status

    monkeypatch.setattr(
        "fno.pr._merge._pr_head_ref_and_oid",
        lambda pr, repo, runner=None: ("feature/x-c8e4", "e08b839d515c", "OPEN"),
    )
    result = _invoke(
        "acquire", "1713",
        "--branch", "agents-graphify", "--head", "773b7dd83e34",
        "--holder", "review-session:s1",
    )
    assert result.exit_code == 0
    assert "review-hold: holding feature/x-c8e4 at e08b839d515c" in result.stdout
    hold = claim_status(_review_hold.review_hold_key("feature/x-c8e4"), root=tmp_path)
    assert hold["state"] == "live"
    assert hold["metadata"]["head_sha"] == "e08b839d515c"
    bystander = claim_status(_review_hold.review_hold_key("agents-graphify"), root=tmp_path)
    assert bystander["state"] == "free"


def test_acquire_of_an_unresolvable_pr_takes_no_hold(monkeypatch, tmp_path):
    """No hold rather than a guessed one. The control at the end creates the
    hold in the same root, which proves the refusals read the right directory
    rather than failing to write."""
    import subprocess

    from fno.claims.core import claim_status

    key = _review_hold.review_hold_key("feature/x-c8e4")

    def _timeout(pr, repo, runner=None):
        raise subprocess.TimeoutExpired(cmd="gh", timeout=10)

    for stub in (lambda pr, repo, runner=None: None, _timeout):
        monkeypatch.setattr("fno.pr._merge._pr_head_ref_and_oid", stub)
        result = _invoke("acquire", "1713", "--holder", "review-session:s1")
        assert result.exit_code == 0
        assert "review-hold: not registered on PR 1713" in result.stderr
        assert claim_status(key, root=tmp_path)["state"] == "free"

    monkeypatch.setattr(
        "fno.pr._merge._pr_head_ref_and_oid",
        lambda pr, repo, runner=None: ("feature/x-c8e4", "e08b839d515c", "OPEN"),
    )
    ok = _invoke("acquire", "1713", "--holder", "review-session:s1")
    assert ok.exit_code == 0
    assert claim_status(key, root=tmp_path)["state"] == "live"


def test_a_failed_acquire_still_exits_zero(monkeypatch, tmp_path):
    """Registration must never block a review from starting: an unheld review is
    still covered by the worktree layer, and a review that refuses to start
    because a lockfile write failed is strictly worse."""
    monkeypatch.setattr(_review_hold, "acquire_review_hold", lambda *a, **kw: None)
    result = _invoke(
        "acquire", "--branch", "feature/x-a089", "--head", "abc123", "--holder", "r"
    )
    assert result.exit_code == 0
