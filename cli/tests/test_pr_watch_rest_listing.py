"""REST listing + GraphQL-budget reads behind the pr-watch sweep (x-c12c).

The bulk sweep moved off `gh pr list` (GraphQL, point-cost billed, shared
per-user) onto `gh api repos/<slug>/pulls` (REST, measured free against the
untouched core bucket). AC1 is asserted as a POSITIVE match on the REST argv
shape plus an explicit absence of the old `["pr", "list"]` pair, never
absence alone. Fakes speak the `fno.pr._proc.run` contract (Result objects),
the single runner convention the sweep now carries end to end.
"""
from __future__ import annotations

import json
from datetime import datetime, timezone

from fno.pr._proc import Result


def _ok(stdout: str) -> Result:
    return Result(returncode=0, stdout=stdout, stderr="")


def _page(rows: list[dict]) -> Result:
    return _ok(json.dumps(rows))


def _pr_row(
    number: int,
    state: str = "open",
    merged: bool = False,
    merged_at: str | None = None,
) -> dict:
    row = {"number": number, "state": state, "merged": merged}
    if merged_at is not None:
        row["merged_at"] = merged_at
    return row


def _has_pr_list_pair(cmd: list) -> bool:
    return any(cmd[i : i + 2] == ["pr", "list"] for i in range(len(cmd) - 1))


# ---------------------------------------------------------------------------
# list_prs_rest
# ---------------------------------------------------------------------------


class TestListPrsRest:
    def test_issues_rest_argv_not_gh_pr_list(self):
        """AC1-HP: the listing is a gh api repos/<slug>/pulls read."""
        from fno.pr._rest import list_prs_rest

        calls: list[list] = []

        def runner(cmd, **_kw):
            calls.append(list(cmd))
            return _page([_pr_row(1)])

        rows, reason = list_prs_rest("owner/repo", runner=runner)

        assert reason == ""
        assert rows == [{"number": 1, "state": "OPEN"}]
        assert calls[0][:3] == [
            "gh", "api", "repos/owner/repo/pulls?state=open&per_page=100&page=1",
        ]
        assert not _has_pr_list_pair(calls[0])

    def test_paginates_until_short_page(self):
        """AC2-HP: a full page 1 continues to page 2; a short page stops."""
        from fno.pr._rest import list_prs_rest

        calls: list[list] = []

        def runner(cmd, **_kw):
            calls.append(list(cmd))
            # Anchored: a bare "page=1" also matches inside "per_page=100".
            if "&page=1" in cmd[2]:
                return _page([_pr_row(n) for n in range(100)])
            return _page([_pr_row(101), _pr_row(102, "closed", merged=True)])

        rows, reason = list_prs_rest("owner/repo", runner=runner)

        assert reason == ""
        assert len(calls) == 2
        assert calls[1][2].endswith("page=2")
        numbers = [r["number"] for r in rows]
        assert 1 in numbers and 102 in numbers and len(rows) == 102
        assert next(r for r in rows if r["number"] == 102)["state"] == "MERGED"

    def test_closed_listing_uses_merged_at_to_map_merged(self):
        """Closed list rows expose merge truth through ``merged_at``."""
        from fno.pr._rest import list_prs_rest

        rows, reason = list_prs_rest(
            "owner/repo",
            state="closed",
            runner=lambda *_args, **_kwargs: _page(
                [_pr_row(9, "closed", merged_at="2026-08-24T18:00:00Z")]
            ),
        )

        assert reason == ""
        assert rows == [{"number": 9, "state": "MERGED"}]

    def test_failure_is_loud_none_plus_reason(self):
        from fno.pr._rest import list_prs_rest

        def runner(cmd, **_kw):
            return Result(returncode=1, stdout="", stderr="gh: API rate limit exceeded")

        rows, reason = list_prs_rest("owner/repo", runner=runner)
        assert rows is None
        assert "rate limit" in reason

# ---------------------------------------------------------------------------
# graphql_remaining
# ---------------------------------------------------------------------------


class TestGraphqlRemaining:
    def _payload(self, remaining: int, reset_epoch: int) -> str:
        return json.dumps(
            {"resources": {"graphql": {"remaining": remaining, "reset": reset_epoch},
                           "core": {"remaining": 5000, "reset": reset_epoch}}}
        )

    def test_reads_remaining_and_reset(self):
        from fno.pr._rest import graphql_remaining

        reset_epoch = 1755427200
        remaining, reset_iso = graphql_remaining(
            runner=lambda cmd, **_kw: _ok(self._payload(37, reset_epoch))
        )
        assert remaining == 37
        expected = datetime.fromtimestamp(reset_epoch, tz=timezone.utc).strftime(
            "%Y-%m-%dT%H:%M:%SZ"
        )
        assert reset_iso == expected

    def test_failure_reads_as_unknown_never_zero(self):
        """AC6-EDGE backdrop: an unreadable instrument returns None, not 0."""
        from fno.pr._rest import graphql_remaining

        remaining, reset_iso = graphql_remaining(
            runner=lambda cmd, **_kw: Result(returncode=1, stdout="", stderr="gh: broken")
        )
        assert remaining is None and reset_iso is None

class TestRestReaderHardening:
    def test_sweep_degrades_a_repo_whose_runner_raises_toolmissing(self):
        """launchd PATH rot makes gh vanish; ToolMissing is not OSError, so
        the per-repo handler must name it like any other repo failure."""
        from fno.pr._proc import ToolMissing
        from fno.pr_watch._discover import read_tracked_pr_states

        def runner(cmd, **_kw):
            raise ToolMissing("gh")

        states, failures = read_tracked_pr_states({"owner/repo#1"}, runner=runner)
        assert states == {"owner/repo#1": "UNKNOWN"}
        assert failures == 1

    def test_full_listing_at_the_page_ceiling_warns_loudly(self, caplog):
        """The ceiling is a truncation, not a completion: a listing whose
        every page came back full must say so, the way the old gh pr list
        path logged 'possibly truncated'."""
        import logging as _logging

        from fno.pr._rest import list_prs_rest

        full_page = [_pr_row(i) for i in range(100)]
        calls = {"pages": 0}

        def runner(cmd, timeout=None, **_kw):
            calls["pages"] += 1
            return _page(full_page)

        with caplog.at_level(_logging.WARNING, logger="fno.pr._rest"):
            rows, reason = list_prs_rest(
                "owner/repo", runner=runner, per_page=100, max_pages=2
            )

        assert reason == ""
        assert len(rows) == 200
        assert calls["pages"] == 2
        assert "possibly truncated" in caplog.text
