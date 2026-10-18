from __future__ import annotations

import json

import pytest

from fno.pr import _internal_gh, _quota
from fno.pr._proc import Result


PULL = {
    "number": 930,
    "html_url": "https://github.com/o/r/pull/930",
    "state": "open",
    "merged": False,
    "mergeable": True,
    "mergeable_state": "behind",
    "head": {"sha": "abc123", "ref": "feature/test"},
    "base": {"ref": "main"},
}


def _runner(calls: list[list[str]]):
    def run(cmd, **kwargs):
        calls.append(list(cmd))
        endpoint = cmd[-1]
        if endpoint == "repos/o/r/pulls/930":
            return Result(0, json.dumps(PULL), "")
        if "check-runs" in endpoint:
            return Result(
                0,
                json.dumps(
                    {
                        "total_count": 1,
                        "check_runs": [
                            {
                                "name": "unit",
                                "status": "completed",
                                "conclusion": "success",
                                "started_at": "2026-08-18T00:00:00Z",
                            }
                        ],
                    }
                ),
                "",
            )
        if "actions/runs?" in endpoint:
            return Result(0, '{"total_count":0,"workflow_runs":[]}', "")
        if endpoint.endswith("/status"):
            return Result(0, '{"statuses":[]}', "")
        if cmd[-2:] == ["api", "rate_limit"]:
            return Result(
                0,
                '{"resources":{"graphql":{"remaining":5000,"reset":1787072400}}}',
                "",
            )
        return Result(0, '{"reviews":[]}', "")

    return run


def test_the_stop_gate_read_with_merge_state_status_still_routes_rest(monkeypatch):
    # loop-check's pr view asks for mergeStateStatus beside the metadata set.
    # A field set outside _METADATA_FIELDS falls to the GraphQL executor,
    # where a coverage-purpose executable refuses it as
    # "coverage reserve accepts review-coverage reads only" -- the stop gate
    # of every session then fails its fingerprint read. The REST lane must
    # carry the field so the read never leaves the translator.
    calls: list[list[str]] = []
    monkeypatch.setattr(
        _internal_gh._rest, "_slug_or_reason", lambda cwd, runner, repo=None: ("o/r", "")
    )
    result = _internal_gh.execute(
        "coverage",
        [
            "pr",
            "view",
            "930",
            "--json",
            "state,number,headRefName,headRefOid,mergeable,mergeStateStatus,baseRefName,author",
        ],
        runner=_runner(calls),
        real_gh="/real/gh",
    )
    assert result.returncode == 0
    payload = json.loads(result.stdout)
    assert payload["mergeStateStatus"] == "behind"
    assert payload["headRefOid"] == "abc123"
    assert calls == [["/real/gh", "api", "repos/o/r/pulls/930"]]


def test_coverage_review_read_composes_rest_evidence(monkeypatch):
    calls: list[list[str]] = []
    clean_body = (
        "Codex Review: Didn’t find any major issues. Bravo. "
        "Reviewed commit: abc123"
    )

    def runner(cmd, **kwargs):
        calls.append(list(cmd))
        endpoint = cmd[-1]
        if endpoint == "repos/o/r/pulls/930":
            return Result(0, json.dumps(PULL), "")
        if endpoint == "repos/o/r/pulls/930/reviews?per_page=100&page=1":
            return Result(0, "[]", "")
        if endpoint == "repos/o/r/issues/930/comments?per_page=100&page=1":
            return Result(
                0,
                json.dumps(
                    [
                        {
                            "user": {"login": "chatgpt-codex-connector[bot]"},
                            "created_at": "2026-08-17T00:00:00Z",
                            "body": clean_body,
                        }
                    ]
                ),
                "",
            )
        raise AssertionError(cmd)

    monkeypatch.setattr(
        _internal_gh._rest, "_slug_or_reason", lambda cwd, runner, repo=None: ("o/r", "")
    )
    monkeypatch.setattr(
        _internal_gh._quota,
        "execute_graphql",
        lambda *a, **k: pytest.fail("coverage review reads must not use GraphQL"),
    )
    result = _internal_gh.execute(
        "coverage",
        [
            "pr",
            "view",
            "930",
            "--json",
            "reviews,comments,headRefOid,baseRefName",
        ],
        runner=runner,
        real_gh="/real/gh",
    )
    assert result.returncode == 0
    payload = json.loads(result.stdout)
    assert payload["headRefOid"] == "abc123"
    assert payload["baseRefName"] == "main"
    assert payload["reviews"] == []
    assert payload["comments"] == [
        {
            "author": {"login": "chatgpt-codex-connector[bot]"},
            "createdAt": "2026-08-17T00:00:00Z",
            "body": clean_body,
        }
    ]
    assert all("graphql" not in " ".join(call) for call in calls)


def test_a_held_budget_stops_the_coverage_read_after_two_runner_calls(monkeypatch):
    # The REST runner consults the fleet budget per request. A hold on the
    # third consult stops the read: two gh reads went out, the comments page
    # never does, and the stderr carries the gh budget: marker the Rust
    # adapter wrapper classifies.
    calls: list[list[str]] = []

    def admit(argv):
        if len(calls) >= 2:
            return (
                "gh budget: fleet GitHub rate limit held locally "
                "(budget: 2/1 points in 60s)"
            )
        return None

    def runner(cmd, **kwargs):
        calls.append(list(cmd))
        endpoint = cmd[-1]
        if endpoint == "repos/o/r/pulls/930":
            return Result(0, json.dumps(PULL), "")
        if endpoint == "repos/o/r/pulls/930/reviews?per_page=100&page=1":
            return Result(0, "[]", "")
        raise AssertionError(cmd)

    monkeypatch.setattr(_internal_gh._quota, "admit", admit)
    monkeypatch.setattr(
        _internal_gh._rest, "_slug_or_reason", lambda cwd, runner, repo=None: ("o/r", "")
    )
    result = _internal_gh.execute(
        "coverage",
        ["pr", "view", "930", "--json", "reviews,comments"],
        runner=runner,
        real_gh="/real/gh",
    )
    assert result.returncode == 1
    assert "gh budget:" in result.stderr
    assert calls == [
        ["/real/gh", "api", "repos/o/r/pulls/930"],
        ["/real/gh", "api", "repos/o/r/pulls/930/reviews?per_page=100&page=1"],
    ]


def test_coverage_review_read_fails_on_malformed_collection(monkeypatch):
    def runner(cmd, **kwargs):
        endpoint = cmd[-1]
        if endpoint == "repos/o/r/pulls/930":
            return Result(0, json.dumps(PULL), "")
        if "/reviews?" in endpoint:
            return Result(0, "{}", "")
        return Result(0, "[]", "")

    monkeypatch.setattr(
        _internal_gh._rest, "_slug_or_reason", lambda cwd, runner, repo=None: ("o/r", "")
    )
    result = _internal_gh.execute(
        "coverage",
        ["pr", "view", "930", "--json", "reviews,comments"],
        runner=runner,
        real_gh="/real/gh",
    )
    assert result.returncode == 1
    assert "reviews" in result.stderr
    assert "JSON array" in result.stderr


def test_ambiguous_pr_selectors_are_rejected():
    number, reason = _internal_gh._pr_number(
        ["pr", "view", "42", "43", "--json", "state"], cwd=None, runner=lambda *a: None
    )
    assert number is None
    assert "ambiguous" in reason


def test_execute_turns_a_proxy_identity_failure_into_a_result(monkeypatch):
    def broken():
        raise _quota.ProxyIdentityError("config load failed")

    monkeypatch.setattr(_quota, "resolve_real_gh", broken)
    result = _internal_gh.execute("discretionary", ["auth", "status"])
    assert result.returncode == 2
    assert "cannot identify its own install directory" in result.stderr


