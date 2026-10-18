"""Tests for the surviving REST readers (`fno.pr._rest`).

The load-bearing guard: the settledness reader issues NO GraphQL call, so a
watching fleet spends the idle core budget instead of the shared per-USER
GraphQL quota. The PR rollup reader itself is the Rust owner now
(crates/fno-agents/src/pr_status.rs); what stays here pins the metadata,
files, PR-listing and reason-classification contracts.
"""

from __future__ import annotations

import json

import pytest

from fno.pr import _quota, _rest
from fno.pr._proc import Result

_PULLS = {
    "html_url": "https://github.com/Owner/Repo/pull/42",
    "state": "open",
    "merged": False,
    "head": {"sha": "abc123def", "ref": "feature/test"},
    "base": {"ref": "main"},
}
_GH_URL = "git@github.com:Owner/Repo.git"


def _runner(*, pulls=_PULLS, check_runs=(), statuses=None, workflow_runs=(), fail=None, calls=None):
    """Dispatch by URL shape: git remote -> slug, gh api -> REST payloads."""

    def r(cmd, cwd=None):
        if calls is not None:
            calls.append(list(cmd))
        url = cmd[-1] if len(cmd) > 1 else ""
        if fail is not None and fail(cmd):
            return Result(1, "", fail(cmd))
        if cmd[:2] == ["git", "remote"]:
            return Result(0, _GH_URL, "")
        if "/pulls/" in url:
            return Result(0, json.dumps(pulls), "")
        if "check-runs" in url:
            return Result(0, json.dumps({"check_runs": list(check_runs)}), "")
        if "actions/runs?" in url:
            return Result(
                0,
                json.dumps(
                    {"total_count": len(list(workflow_runs)), "workflow_runs": list(workflow_runs)}
                ),
                "",
            )
        if url.endswith("/status"):
            return Result(0, json.dumps({"statuses": list(statuses or [])}), "")
        return Result(1, "", "unexpected: " + " ".join(cmd))

    return r


def test_pr_info_uses_one_rest_request_and_returns_positive_metadata():
    calls: list[list[str]] = []
    pulls = {
        "html_url": "https://github.com/Owner/Repo/pull/42",
        "state": "open",
        "merged": False,
        "mergeable": True,
        "head": {"sha": "abc123def", "ref": "feature/rest-info"},
        "base": {"ref": "main"},
        "user": {"login": "alice"},
    }
    info, reason = _rest.fetch_pr_info_rest(
        "42", repo="Owner/Repo", runner=_runner(pulls=pulls, calls=calls)
    )
    assert reason == ""
    assert info == {
        "pr": 42,
        "url": "https://github.com/Owner/Repo/pull/42",
        "body": "",
        "state": "OPEN",
        "head_sha": "abc123def",
        "head_ref": "feature/rest-info",
        "base_ref": "main",
        "mergeable": "MERGEABLE",
        # The REST spelling of mergeStateStatus rides the same payload; the
        # fixture carries none, so the mapped value stays None.
        "merge_state_status": None,
        "merged_at": None,
        "merge_sha": None,
        "author": "alice",
        # Whether GitHub's auto-merge queue owns the PR rides THIS payload, so
        # the armed flag and the head it was read against come from one fetch.
        "auto_merge": None,
    }
    assert calls == [["gh", "api", "repos/Owner/Repo/pulls/42"]]


def test_pr_info_refuses_a_malformed_auto_merge_object():
    pulls = {
        "html_url": "https://github.com/Owner/Repo/pull/42",
        "state": "open",
        "merged": False,
        "mergeable": True,
        "head": {"sha": "abc123def", "ref": "feature/rest-info"},
        "base": {"ref": "main"},
        "auto_merge": "yes",
    }
    info, reason = _rest.fetch_pr_info_rest("42", repo="Owner/Repo", runner=_runner(pulls=pulls))
    assert info is None
    assert "auto_merge" in reason


def test_pr_file_paths_rest_paginates_until_short_page():
    calls: list[list[str]] = []

    def runner(cmd, cwd=None):
        calls.append(list(cmd))
        if cmd[-1].endswith("page=1"):
            return Result(0, json.dumps([{"filename": f"f{i}.py"} for i in range(100)]), "")
        if cmd[-1].endswith("page=2"):
            return Result(0, json.dumps([{"filename": "tail.py"}]), "")
        raise AssertionError(f"unexpected command: {cmd}")

    paths, reason = _rest.fetch_pr_file_paths_rest("42", repo="Owner/Repo", runner=runner)

    assert reason == ""
    assert paths == [f"f{i}.py" for i in range(100)] + ["tail.py"]
    assert [call[-1].endswith("page=1") for call in calls] == [True, False]


def test_pr_file_paths_rest_rejects_a_failed_second_page():
    def runner(cmd, cwd=None):
        if cmd[-1].endswith("page=1"):
            return Result(0, json.dumps([{"filename": f"f{i}.py"} for i in range(100)]), "")
        return Result(1, "", "i/o timeout")

    paths, reason = _rest.fetch_pr_file_paths_rest("42", repo="Owner/Repo", runner=runner)

    assert paths is None
    assert "timeout" in reason


def test_pr_file_paths_rest_fails_closed_at_github_cap():
    def runner(cmd, cwd=None):
        return Result(0, json.dumps([{"filename": "full.py"} for _ in range(100)]), "")

    paths, reason = _rest.fetch_pr_file_paths_rest("42", repo="Owner/Repo", runner=runner)

    assert paths is None
    assert "3,000-file cap" in reason


def test_current_pr_number_uses_rest_not_gh_pr_view():
    calls: list[list[str]] = []

    def runner(cmd, cwd=None):
        calls.append(list(cmd))
        if cmd[:3] == ["git", "branch", "--show-current"]:
            return Result(0, "feature/rest-info\n", "")
        if cmd[:2] == ["gh", "api"]:
            return Result(0, '[{"number":930}]', "")
        return Result(1, "", "unexpected")

    number, reason = _rest.resolve_current_pr_number_rest(repo="Owner/Repo", runner=runner)
    assert (number, reason) == (930, "")
    assert calls == [
        ["git", "branch", "--show-current"],
        [
            "gh",
            "api",
            "repos/Owner/Repo/pulls?state=all&head=Owner:feature/rest-info&per_page=2",
        ],
    ]


_VERBATIM_403 = (
    "gh: API rate limit exceeded for user ID 4994564. If you reach out to "
    "GitHub Support for help, please include the request ID "
    "FAEB:283161:6EF36:99B72:6A8B97DD ... Terms of Service (...) (HTTP 403)"
)


def _rate_limit_runner(core_remaining=None):
    """Answer `gh api rate_limit` with the named core reading.

    None means the instrument itself cannot answer (the endpoint is exempt,
    but the read can still die), which the classifier must read as unknown.
    """

    def r(cmd, cwd=None, timeout=None):
        assert cmd[:3] == ["gh", "api", "rate_limit"], f"unexpected: {cmd}"
        if core_remaining is None:
            return Result(1, "", "instrument unreadable")
        return Result(
            0,
            json.dumps(
                {
                    "resources": {
                        "core": {"remaining": core_remaining, "limit": 5000, "reset": 4102444800},
                        "graphql": {"remaining": 4446, "limit": 5000, "reset": 4102444800},
                    }
                }
            ),
            "",
        )

    return r


def test_verbatim_403_with_healthy_core_reads_secondary_and_carries_the_verdict():
    """The p0 fixture: the measured 403 body says only `API rate limit
    exceeded` (no `secondary` anywhere) while the exempt bucket answers
    4980/5000 - that IS the secondary limit. The reason must carry the verdict
    as data for the cache, and the prose must still say back off."""
    assert "secondary" not in _VERBATIM_403.lower()
    reason = _rest._rest_reason(
        Result(1, "", _VERBATIM_403), runner=_rate_limit_runner(core_remaining=4980)
    )
    assert reason.rate_limit_class == "secondary"
    assert "SECONDARY" in reason
    assert "4980" in reason
    assert "back off" in reason.lower()


def test_drained_core_bucket_classifies_core_and_names_the_reading():
    """Core quota: the same `rate limit` wording but the live bucket reads 0.
    The bucket, not the wording, picks the branch."""
    res = Result(1, "", "gh: API rate limit exceeded (HTTP 403)")
    reason = _rest._rest_reason(res, runner=_rate_limit_runner(core_remaining=0))
    assert reason.rate_limit_class == "core"
    assert "CORE" in reason
    assert "resources.core" in reason


def test_unreadable_bucket_still_fails_toward_back_off():
    """No instrument (no runner passed, or the rate_limit read died): reading
    unknown as CORE sends the fleet to wait for a reset that never comes, so
    the unknown case classifies secondary and tells the caller to back off."""
    no_instrument = _rest._rest_reason(
        Result(1, "", _VERBATIM_403), runner=_rate_limit_runner(core_remaining=None)
    )
    no_runner = _rest._rest_reason(Result(1, "", _VERBATIM_403))
    for reason in (no_instrument, no_runner):
        assert reason.rate_limit_class == "secondary"
        assert "back off" in reason.lower()


# ---- the secondary arm records the refusal in the fleet budget ledger ----


@pytest.fixture(autouse=True)
def quiet_budget(monkeypatch):
    """Keep every classification test off the real fleet ledger: a test that
    classifies a verbatim 403 through the REAL chain would open a live 60s
    backoff on the operator's machine-wide budget. The three door tests below
    restore the real record_refusal from the import-time capture."""
    monkeypatch.setattr("fno.pr._quota.record_refusal", lambda text: None)
    monkeypatch.setattr("fno.pr._quota.admit", lambda argv: None)


_REAL_RECORD_REFUSAL = _quota.record_refusal


def test_a_secondary_classification_records_the_refusal_once(monkeypatch):
    monkeypatch.setattr("fno.pr._quota.record_refusal", _REAL_RECORD_REFUSAL)
    ops = []
    monkeypatch.setattr(
        "fno.pr._quota._gh_budget",
        lambda payload: ops.append(payload.get("op")),
    )
    reason = _rest._rest_reason(
        Result(1, "", _VERBATIM_403), runner=_rate_limit_runner(core_remaining=4980)
    )
    assert reason.rate_limit_class == "secondary"
    assert ops == ["refused"]


def test_the_local_budget_refusal_line_sends_no_refused_op(monkeypatch):
    """The local `gh budget:` line says `rate limit` on purpose, so the
    classifier reads it as secondary - but it carries neither HTTP 403 nor
    HTTP 429, and the marker gate in record_refusal keys on those, so a
    refusal this fleet manufactured is never recorded as GitHub's."""
    ops = []
    monkeypatch.setattr(
        "fno.pr._quota._gh_budget",
        lambda payload: ops.append(payload.get("op")),
    )
    local = (
        "gh budget: fleet GitHub rate limit held locally (budget: 450/450 points"
        " in 60s | backoff 0s left); this command did not reach GitHub."
    )
    reason = _rest._rest_reason(
        Result(75, "", local), runner=_rate_limit_runner(core_remaining=4980)
    )
    assert reason.rate_limit_class == "secondary"
    assert ops == [], "no refused op may reach the ledger for a local line"


def test_transport_failure_names_its_class_and_disclaims_blockers():
    """x-4eac (the 2026-08-19 EOF incident): a transport death is a fact about
    the READ. The reason must say so before a worker polls harder or edits
    content that was never read."""

    class Res:
        stderr = 'Post "https://api.github.com/graphql": unexpected EOF'
        stdout = ""

    reason = _rest._rest_reason(Res())
    assert "TRANSPORT" in reason
    assert "not a verdict about this PR" in reason
    assert "not content" in reason


def test_repo_slug_reason_names_its_failure_class(tmp_path, monkeypatch):
    """A bare None could not tell three different failures apart, and the one
    caller that rendered it said "repo slug unreadable" for all of them.

    That sentence is wrong in subject whenever the SLUG is the readable thing
    and the CWD is what does not exist - the case a caller hits by passing
    `owner/repo` into a parameter that takes a path. Both are bare `str`, so
    nothing static catches it. `isdir` does, before git is spawned (x-51f7).
    """
    monkeypatch.chdir(tmp_path)

    def never_runs(cmd, cwd=None):
        raise AssertionError(f"a non-directory must be refused before any spawn: {cmd}")

    slug, reason = _rest._repo_slug_reason("bllshttng/footnote", never_runs)
    assert slug is None
    assert reason == "no such directory: bllshttng/footnote"

    # A real directory with no origin quotes git's own sentence.
    def no_origin(cmd, cwd=None):
        return Result(1, "", "error: No such remote 'origin'\n")

    slug, reason = _rest._repo_slug_reason(str(tmp_path), no_origin)
    assert slug is None
    assert reason == "error: No such remote 'origin'"

    # A remote that is not GitHub names the url it could not parse.
    def gitlab(cmd, cwd=None):
        return Result(0, "git@gitlab.com:owner/repo.git\n", "")

    slug, reason = _rest._repo_slug_reason(str(tmp_path), gitlab)
    assert slug is None
    assert reason == "origin is not a github remote: git@gitlab.com:owner/repo.git"

    # The success case carries an EMPTY reason, so a caller reads the pair
    # rather than inferring success from a missing sentence.
    def github(cmd, cwd=None):
        return Result(0, _GH_URL, "")

    assert _rest._repo_slug_reason(str(tmp_path), github) == ("owner/repo", "")
    assert _rest._repo_slug(str(tmp_path), github) == "owner/repo"


def test_repo_slug_reason_redacts_credentials_in_the_remote_url():
    """The reason is PUBLISHED: it rides the coverage note into the
    `fno/review-coverage` commit status and into .fno/events.jsonl.

    A CI-shaped clone carries its token in the url's userinfo, and such a
    remote reaches the non-GitHub arm precisely BECAUSE its host is not
    github.com - so the credential is exactly what would get posted.
    """

    def token_remote(cmd, cwd=None):
        return Result(0, "https://x-access-token:ghs_SECRET@ghe.internal/o/r.git\n", "")

    _slug, reason = _rest._repo_slug_reason(".", token_remote)
    assert reason == "origin is not a github remote: https://***@ghe.internal/o/r.git"
    assert "ghs_SECRET" not in reason

    # git's own stderr can quote the url too, so the same redaction guards it.
    def failing_remote(cmd, cwd=None):
        return Result(1, "", "fatal: https://u:p@ghe.internal/o/r.git not found\n")

    _slug, reason = _rest._repo_slug_reason(".", failing_remote)
    assert reason == "fatal: https://***@ghe.internal/o/r.git not found"


def test_published_refusal_hides_the_account_name_in_a_local_path(tmp_path, monkeypatch):
    """The refusal rides the coverage note into a GitHub commit status. It
    redacts a token in one clause, so interpolating a home directory raw in
    the next is an oversight, not a policy."""
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.delenv("USERPROFILE", raising=False)
    reason = _rest._cwd_refusal(str(tmp_path / "code" / "proj"))
    assert reason == "no such directory: ~/code/proj"
    assert str(tmp_path) not in reason
