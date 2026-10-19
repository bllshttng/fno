"""Characterization tests for the _merge.py port (ab-d4c98550, US1/AC1/AC5).

Mocks gh/git at the _proc.run seam so the guard-rejection, classification,
and worktree-recovery branches are exercised deterministically (they are hard
to reproduce against a live PR). Pins the JSON-line schema, the exit codes,
and the stdout-vs-stderr routing the bash used.
"""
from __future__ import annotations
from tests.fixtures.graph_seed import seed_graph

import json

import pytest

from fno.config import AutoMergeBlock
from fno.pr import _merge
from fno.pr._proc import Result
from fno.graph.store import read_graph_strict

# Captured at import, before conftest's autouse hermetic stub replaces the
# attribute: the test that exercises the door itself needs the real one.
_REAL_AUTHORIZED_MERGE = _merge._authorized_merge


class FakeRun:
    """Dispatch canned Results by command, recording every call.

    gh shapes are exhaustive by intent: a gh command no branch matches RAISES
    echoing the argv, so the next gh call added to production code fails loud
    here instead of silently passing a fabricated Result through (an unmatched
    `gh pr view --json number,body,url,state,mergedAt,files` once fell through
    to the view_url tail and a fail-closed reader blamed the PR body).
    git and bash stay permissive; incidental git reads are noise, not the trap.
    """

    def __init__(
        self,
        *,
        gh_merge: Result | None = None,
        merged_at: str = "null",
        view_url: str = "https://example/pr",
        toplevel: str | None = None,
        behind_by: int = 0,
        view_fails: bool = False,
        auto_merge_request: str = "null",
        head_ref: str = "feature/x",
        head_repo: str = "owner/repo",
        base_repo: str = "owner/repo",
        checks: dict | None = None,
        base_move_files: list[str] | None = None,
        pr_files: list[str] | None = None,
        compare_truncated: bool = False,
    ) -> None:
        self.gh_merge = gh_merge or Result(0, "", "")
        self.view_fails = view_fails
        self.merged_at = merged_at
        self.view_url = view_url
        self.toplevel = toplevel
        self.behind_by = behind_by
        self.auto_merge_request = auto_merge_request
        self.head_ref = head_ref
        self.head_repo = head_repo
        self.base_repo = base_repo
        # The overlap hold's probes: the reverse compare's file list
        # and the PR's own changed paths. compare_truncated forces the
        # fail-closed miss branch without building a 300-file payload.
        self.base_move_files = base_move_files
        self.pr_files = pr_files
        self.compare_truncated = compare_truncated
        self.cwd_calls: list[str | None] = []
        # require_checks_pass is enforced in-process now (x-9d11), so the
        # default fake serves a GREEN rollup; tests exercising refusal paths
        # pass their own.
        self.checks = checks if checks is not None else {
            "state": "OPEN",
            "headRefOid": "deadbeefcafe",
            "statusCheckRollup": [
                {"name": "c0", "status": "COMPLETED", "conclusion": "SUCCESS"}
            ],
        }
        self.calls: list[list[str]] = []

    def __call__(self, cmd, *, cwd=None, env=None, input_text=None, timeout=None):
        cmd = list(cmd)
        self.calls.append(cmd)
        self.cwd_calls.append(cwd)
        tool = cmd[0]
        if tool == "git":
            if cmd[1:3] == ["rev-parse", "--show-toplevel"]:
                return Result(0, (self.toplevel or cwd or "") + "\n", "")
            if cmd[1:4] == ["remote", "get-url", "origin"]:
                return Result(0, "git@github.com:owner/repo.git\n", "")
            return Result(0, "", "")
        if tool == "gh":
            if cmd[1:3] == ["pr", "merge"]:
                return self.gh_merge
            if cmd[1:3] == ["pr", "view"]:
                if any("statusCheckRollup" in a for a in cmd):
                    return Result(0, json.dumps(self.checks) + "\n", "")
                if cmd[-1] == "headRefOid":
                    # The coverage gate's head fetch: a merge first pins the
                    # head coverage would describe. Served from the same
                    # default the rollup read carries, so a test faking a
                    # moved head overrides `checks` and both reads agree.
                    return Result(
                        0,
                        json.dumps({"headRefOid": self.checks.get("headRefOid", "deadbeefcafe")}) + "\n",
                        "",
                    )
                if "mergedAt" in cmd:
                    if self.view_fails:
                        return Result(1, "", "gh: could not reach api.github.com")
                    return Result(0, self.merged_at + "\n", "")
                if "autoMergeRequest" in cmd:
                    # Models `-q .autoMergeRequest.enabled`: "true" armed,
                    # "false"/"null" not (jq prints null, not empty).
                    return Result(0, self.auto_merge_request + "\n", "")
                if "headRefName" in cmd[-1] and "headRepository" in cmd[-1]:
                    # x-9d11 fork guard: jq renders "branch\theadRepo\tbaseRepo"
                    # (tab separators; an empty head repo = deleted fork).
                    return Result(
                        0,
                        f"{self.head_ref}\t{self.head_repo}\t{self.base_repo}\n",
                        "",
                    )
                if cmd[-1] == ".headRefName":
                    return Result(0, self.head_ref + "\n", "")
                if "baseRefName,headRefName" in cmd:
                    return Result(
                        0,
                        json.dumps({"baseRefName": "main", "headRefName": self.head_ref}) + "\n",
                        "",
                    )
                if cmd[-1] == ".url":
                    # _pr_url's fetch (`--json url -q .url`), the one shape the
                    # old permissive tail served deliberately.
                    return Result(0, self.view_url + "\n", "")
                raise AssertionError(
                    f"FakeRun: no canned response for gh pr view (add one, never a fall-through): {cmd!r}"
                )
            if cmd[1] == "api":
                endpoint = cmd[-1]
                if any(
                    a.startswith("repos/")
                    and a.split("?", 1)[0].endswith("/files")
                    for a in cmd
                ):
                    # The PR-side changed-path read (paginated REST, one page
                    # of file objects); None serves an empty diff, which
                    # proves no overlap rather than a miss.
                    return Result(
                        0,
                        json.dumps(
                            [{"filename": name} for name in (self.pr_files or [])]
                        ),
                        "",
                    )
                if (
                    "/pulls/" in endpoint
                    and "/comments" not in endpoint
                    and endpoint.rsplit("/", 1)[-1].isdigit()
                ):
                    if "--jq" in cmd:
                        return Result(
                            0,
                            f"{self.head_ref}\t{self.head_repo}\t{self.base_repo}\n",
                            "",
                        )
                    return Result(
                        0,
                        json.dumps(
                            {
                                "number": int(endpoint.rsplit("/", 1)[-1]),
                                "state": str(self.checks.get("state") or "OPEN").lower(),
                                "merged": self.checks.get("state") == "MERGED",
                                "mergeable": True,
                                "head": {
                                    "sha": self.checks.get("headRefOid", "deadbeefcafe"),
                                    "ref": self.head_ref,
                                    "repo": (
                                        {"full_name": self.head_repo}
                                        if self.head_repo
                                        else None
                                    ),
                                },
                                "base": {
                                    "ref": "main",
                                    "repo": {"full_name": self.base_repo},
                                },
                            }
                        )
                        + "\n",
                        "",
                    )
                if "/check-runs?" in endpoint:
                    rows = []
                    for check in self.checks.get("statusCheckRollup") or []:
                        if not check.get("name"):
                            continue
                        rows.append(
                            {
                                "name": check.get("name"),
                                "status": str(check.get("status") or "completed").lower(),
                                "conclusion": str(check.get("conclusion") or "").lower(),
                                "started_at": check.get("startedAt") or "2026-08-18T00:00:00Z",
                            }
                        )
                    return Result(
                        0,
                        json.dumps({"total_count": len(rows), "check_runs": rows}) + "\n",
                        "",
                    )
                if "/actions/runs?" in endpoint:
                    return Result(0, json.dumps({"total_count": 0, "workflow_runs": []}), "")
                if endpoint.endswith("/status"):
                    rows = [
                        {
                            "context": check.get("context"),
                            "state": check.get("state"),
                            "created_at": check.get("createdAt") or "2026-08-18T00:00:00Z",
                        }
                        for check in self.checks.get("statusCheckRollup") or []
                        if check.get("context")
                    ]
                    return Result(0, json.dumps({"statuses": rows}) + "\n", "")
                if len(cmd) > 2 and "/compare/" in cmd[2]:
                    if any("truncated" in a for a in cmd):
                        # The overlap probe's reverse compare: one
                        # payload carries the truncation flag and the names.
                        return Result(
                            0,
                            json.dumps(
                                {
                                    "truncated": self.compare_truncated,
                                    "names": self.base_move_files or [],
                                }
                            )
                            + "\n",
                            "",
                        )
                    return Result(0, f"{self.behind_by}\n", "")
                if "DELETE" in cmd and "/git/refs/heads/" in cmd[-1]:
                    # The post-merge branch delete (gh api against the verified
                    # base repo, never `git push origin`).
                    return Result(0, "", "")
                if "PUT" in cmd and any(a.endswith("/merge") for a in cmd):
                    # The server-side REST fallback merge (worktree-held
                    # branch); only api.ok is read, same bytes api_ok served.
                    return Result(0, "", "")
                raise AssertionError(
                    f"FakeRun: no canned response for gh api (add one, never a fall-through): {cmd!r}"
                )
            raise AssertionError(
                f"FakeRun: no canned response for gh (add one, never a fall-through): {cmd!r}"
            )
        if tool == "bash":
            return Result(0, "", "")
        return Result(0, "", "")


@pytest.fixture
def enabled(monkeypatch, tmp_path):
    from functools import partial
    from fno.claims import core as claims

    monkeypatch.setattr(claims, "acquire_claim", partial(claims.acquire_claim, root=tmp_path))
    monkeypatch.setattr(claims, "release_claim", partial(claims.release_claim, root=tmp_path))
    monkeypatch.setattr(
        "fno.pr._review_hold.resolve_pr_worktree", lambda _pr, repo: repo
    )
    monkeypatch.setattr(_merge, "_load_auto_merge", lambda _repo: AutoMergeBlock(enabled=True))
    real_which = _merge.shutil.which
    monkeypatch.setattr(_merge.shutil, "which", lambda tool: "/usr/bin/gh" if tool == "gh" else real_which(tool))
    # The posture floor (merge step 1b) reads live settings; hermetic tests
    # resolve to the shipped default rung (self_review), never the operator's
    # real config, and never through the real loader (some tests break the
    # paths layer underneath it).
    from fno.config import ReviewBlock as _ReviewBlock, SettingsModel as _Settings

    monkeypatch.setattr(
        "fno.config.load_settings_for_repo",
        lambda _p: _Settings(review=_ReviewBlock()),
    )
    # Hermetic merge-lock: route the LD#9 serialization claim (and the lane
    # probe) to a tmp claims root so tests never touch the repo's .fno/claims
    # or contend with a real in-flight merge.
    monkeypatch.setenv("FNO_CLAIMS_ROOT", str(tmp_path))
    # The coverage gate is the native op door now: conftest's hermetic fake
    # answers it covered with no covered head (the fresh-install no-lane
    # answer), so merge-behavior tests proceed past the guard and the
    # --match-head-commit pin stays undisturbed; the pin has its own test.
    # Same hermeticity for the flake hold: no merge case here is about rerun
    # recovery, so the probe is UNAVAILABLE (flake None, hold skipped); the
    # rerun tests override with their own answer.
    monkeypatch.setattr(
        "fno.rust_binary.verb_call",
        _door_stub(None),
    )
    # The graph_json hermeticity pin this fixture used to carry is closed at
    # the reader now: the merge gate reads its hold receipt through the
    # authorized-merge door this suite stubs above.


def _door_stub(answer, verdict=None):
    """A verb_call fake answering the status-rerun ask (None = the probe is
    unavailable, the fail-open old default) and, when `verdict` is given, the
    native coverage gate's verdict door; every other ask rides the real
    transport."""
    import fno.rust_binary as rust_binary

    real = rust_binary.verb_call

    def _fake(verb, payload, unavailable=None, **kw):
        if (
            verdict is not None
            and verb == ["review-coverage", "op"]
            and isinstance(payload, dict)
            and payload.get("op") == "verdict"
        ):
            return verdict
        if isinstance(payload, dict) and payload.get("op") == "status-rerun":
            if answer is None:
                raise (unavailable or rust_binary.VerbUnavailable)(
                    "status-rerun unavailable in tests"
                )
            return answer
        return real(verb, payload, unavailable=unavailable, **kw)

    return _fake



@pytest.fixture(autouse=True)
def _stub_pr_worktree_resolution(monkeypatch):
    """Keep merge tests hermetic; the resolver has its own real-worktree test."""
    monkeypatch.setattr(
        "fno.pr._review_hold.resolve_pr_worktree", lambda _pr, repo: repo
    )


def _last_json(capsys, *, stream="out") -> dict:
    cap = capsys.readouterr()
    text = cap.out if stream == "out" else cap.err
    return json.loads(text.strip().splitlines()[-1])


def test_invalid_pr_number_exits_1_with_failed_json_on_stderr(capsys):
    assert _merge.run_merge(["0"]) == 1
    obj = _last_json(capsys, stream="err")
    assert obj["outcome"] == "failed"
    assert "invalid pr number" in obj["reason"]


def test_gh_missing_exits_127(monkeypatch, capsys, tmp_path):
    monkeypatch.setattr(_merge, "_load_auto_merge", lambda _repo: AutoMergeBlock(enabled=True))
    # Same hermeticity as the `enabled` fixture: a populated per-worker
    # sandbox graph makes the hold lookup's closure fetch fail-closed (exit 2)
    # before the gh-missing check this test exists to pin ever runs.
    monkeypatch.setattr("fno.paths.graph_json", lambda: tmp_path / "graph.json")
    monkeypatch.setattr(_merge.shutil, "which", lambda _x: None)
    # Isolate from an ambient target-state.md: run_merge with no cwd reads
    # `auto_merge_approved` from the caller's repo state, so a suite run inside
    # an active /target worktree (whose manifest carries a per-run no-merge)
    # bails on that field before the gh check. A fresh tmp cwd has no state.
    assert _merge.run_merge(["42"], cwd=str(tmp_path)) == 127
    obj = _last_json(capsys, stream="err")
    assert obj["outcome"] == "failed"
    assert obj["reason"] == "gh CLI not installed"


# ---- classification ----


def test_merge_immediate_exit_0_uses_pr_worktree(enabled, monkeypatch, capsys, tmp_path):
    feature = tmp_path / "feature-worktree"
    (feature / ".fno").mkdir(parents=True)
    monkeypatch.setattr(
        "fno.pr._review_hold.resolve_pr_worktree",
        lambda pr, repo: str(feature),
    )
    fake = FakeRun(gh_merge=Result(0, "Merged pull request", ""), toplevel=str(tmp_path))
    monkeypatch.setattr(_merge, "run", fake)
    assert _merge.run_merge(["42"], cwd=str(tmp_path)) == 0
    obj = _last_json(capsys)
    assert obj["outcome"] == "merged"
    assert obj["strategy"] == "merge"
    assert "invoker" not in obj
    assert str(feature) in fake.cwd_calls



# ---- rerun-recovery flake hold ----


def _flake_recovered(monkeypatch, failed=None):
    monkeypatch.setattr(
        "fno.rust_binary.verb_call",
        _door_stub({"recovered": True, "failed": failed or ["smoke-pytest (7)"]}),
    )


def test_accept_flake_merges_and_journals_the_acceptance(
    enabled, monkeypatch, capsys, tmp_path
):
    monkeypatch.setattr("fno.paths.graph_json", lambda: tmp_path / "graph.json")
    fake = FakeRun(gh_merge=Result(0, "Merged pull request", ""), toplevel=str(tmp_path))
    monkeypatch.setattr(_merge, "run", fake)
    monkeypatch.setattr(
        _merge,
        "_pr_head_ref_and_oid",
        lambda pr, repo, runner=None: ("feature/x", "abc123", "OPEN"),
    )
    _capture_posture(monkeypatch, outcome="merged")
    _flake_recovered(monkeypatch)
    events = []
    monkeypatch.setattr("fno.events.append_event", lambda ev: events.append(ev))

    assert _merge.run_merge(["42", "--accept-flake"], cwd=str(tmp_path)) == 0
    cap = capsys.readouterr()
    receipt = json.loads(cap.out.strip().splitlines()[-1])
    assert receipt["outcome"] == "merged"
    assert "flake override accepted" in cap.err
    flake_events = [e for e in events if e.get("type") == "merge_flake_accepted"]
    assert len(flake_events) == 1
    data = flake_events[0]["data"]
    assert data["pr"] == 42
    assert data["head"] == "abc123"
    assert data["failed_checks"] == ["smoke-pytest (7)"]


def _write_manifest(tmp_path, body: str) -> None:
    (tmp_path / ".fno").mkdir(exist_ok=True)
    (tmp_path / ".fno" / "target-state.md").write_text(body, encoding="utf-8")


def _owner_answers(monkeypatch, outcome, detail):
    """Make the authorized-merge owner answer one receipt, decision included."""

    def _authorized(pr_number, repo, *, effect, approved, source, **kwargs):
        return {"outcome": outcome, "detail": detail}

    monkeypatch.setattr(_merge, "_authorized_merge", _authorized)


def _capture_posture(monkeypatch, outcome="merged"):
    """Record the posture this verb hands the authorized-merge owner."""
    seen: dict = {}

    def _authorized(pr_number, repo, *, effect, approved, source, **kwargs):
        seen["effect"] = effect
        seen["approved"] = approved
        seen["source"] = source
        seen.update(kwargs)
        if kwargs.get("decide_only"):
            return {"outcome": "authorized", "detail": "abc123"}
        return {"outcome": outcome, "detail": "abc123"}

    monkeypatch.setattr(_merge, "_authorized_merge", _authorized)
    return seen


# The posture fold itself - which grant beats which refusal, and the wording of
# each refusal - belongs to the authorized-merge owner, and is under test in
# crates/fno-agents/src/authorized_merge.rs. What this verb still owns is
# READING the manifest and handing the right posture down, so that is what these
# pin: a wrong posture here would make the owner decide correctly about the
# wrong run.


@pytest.mark.parametrize(
    "manifest,expected_approved,expected_source",
    [
        pytest.param(
            "session_id: s1\nauto_merge_approved: false\nauto_merge_source: flag-no-merge\n",
            False,
            "flag-no-merge",
            id="per-run refusal names its source",
        ),
        pytest.param(
            "session_id: s1\nauto_merge_approved: true\nauto_merge_source: config\n",
            True,
            "config",
            id="config-mirrored grant stays a config grant",
        ),
        pytest.param(
            "session_id: s1\n",
            None,
            "",
            id="absent field: the live config decides on its own",
        ),
    ],
)
def test_the_manifest_posture_reaches_the_owner_verbatim(
    enabled, monkeypatch, tmp_path, manifest, expected_approved, expected_source
):
    _write_manifest(tmp_path, manifest)
    monkeypatch.setattr(_merge, "run", FakeRun(toplevel=str(tmp_path)))
    seen = _capture_posture(monkeypatch)
    _merge.run_merge(["42"], cwd=str(tmp_path))
    assert seen["effect"] == "merge"
    assert seen["approved"] is expected_approved
    assert seen["source"] == expected_source


def test_the_owners_refusal_renders_as_a_skip_with_its_own_reason(
    enabled, monkeypatch, capsys, tmp_path
):
    """The refusal text is the owner's, and it reaches the operator intact - a
    refusal that names no sanctioned override is the one workers improvised
    past, twice inside sixty seconds."""

    def _authorized(pr_number, repo, *, effect, approved, source, **kwargs):
        return {
            "outcome": "refused",
            "detail": (
                "auto_merge disabled (live config resolves auto_merge.enabled=false); "
                "sanctioned override (operator levers): `fno config set "
                "auto_merge.enabled true`, or start the run with TARGET_AUTO_MERGE=1"
            ),
        }

    monkeypatch.setattr(_merge, "_authorized_merge", _authorized)
    monkeypatch.setattr(_merge, "run", FakeRun(toplevel=str(tmp_path)))
    assert _merge.run_merge(["42"], cwd=str(tmp_path)) == 2
    obj = _last_json(capsys)
    assert obj["outcome"] == "skipped"
    assert "sanctioned override" in obj["reason"]
    assert "fno config set auto_merge.enabled true" in obj["reason"]
    assert "TARGET_AUTO_MERGE=1" in obj["reason"]


def test_a_landed_merge_with_a_cleanup_failure_reports_partial(
    enabled, monkeypatch, capsys, tmp_path
):
    """Primary specimen (PR #742). The server-side merge landed and a step
    around it did not - a local branch delete a worktree holds, a base-branch
    checkout, a remote delete. The owner reports that in ``cleanup_failure``;
    this verb must render it as partial with the reason visible, never as
    failed, or an autonomous caller retries a merge that already happened.
    """
    (tmp_path / ".fno").mkdir()
    cleanup_failure = (
        "gh merge exited non-zero after the server-side merge: "
        "cannot delete branch 'feature/x-beb7' used by worktree"
    )

    def _authorized(pr_number, repo, *, effect, approved, source, **kwargs):
        if kwargs.get("decide_only"):
            return {"outcome": "authorized", "detail": "abc123"}
        return {
            "outcome": "merged",
            "detail": "abc123",
            "note": "merged server-side",
            "cleanup_failure": cleanup_failure,
        }

    monkeypatch.setattr(_merge, "_authorized_merge", _authorized)
    monkeypatch.setattr(_merge, "run", FakeRun(toplevel=str(tmp_path)))
    assert _merge.run_merge(["742"], cwd=str(tmp_path)) == 3
    obj = _last_json(capsys)
    assert obj["outcome"] == "partial"
    assert obj["cleanup"].startswith("failed")
    assert "cleanup failed" in obj["reason"]
    assert "cannot delete branch" in obj["reason"]


def test_a_worktree_fallback_merge_is_merged_not_partial(
    enabled, monkeypatch, capsys, tmp_path
):
    """The regression the two fields exist to prevent. ``note`` says HOW the
    merge landed, and the REST recovery for a worktree-held branch WORKED.
    Rendered as a cleanup failure it made every worktree-first merge - which
    is every run in this repo - report partial at exit 3.
    """
    (tmp_path / ".fno").mkdir()

    def _authorized(pr_number, repo, *, effect, approved, source, **kwargs):
        if kwargs.get("decide_only"):
            return {"outcome": "authorized", "detail": "abc123"}
        return {
            "outcome": "merged",
            "detail": "abc123",
            "note": "merged server-side (worktree fallback)",
        }

    monkeypatch.setattr(_merge, "_authorized_merge", _authorized)
    monkeypatch.setattr(_merge, "run", FakeRun(toplevel=str(tmp_path)))
    assert _merge.run_merge(["742"], cwd=str(tmp_path)) == 0
    obj = _last_json(capsys)
    assert obj["outcome"] == "merged"
    assert obj["reason"] == "merged server-side (worktree fallback)"
    assert not obj.get("cleanup")


# ---- post-merge followups ----


def test_post_merge_sentinels_written(enabled, monkeypatch, tmp_path):
    (tmp_path / ".fno").mkdir()
    fake = FakeRun(gh_merge=Result(0, "Merged", ""), toplevel=str(tmp_path))
    monkeypatch.setattr(_merge, "run", fake)
    _merge.run_merge(["42"], cwd=str(tmp_path))
    mem = tmp_path / ".fno" / ".memory-pass-pending"
    triage = tmp_path / ".fno" / ".triage-pending"
    assert mem.read_text().strip() == "42"
    sentinel = json.loads(triage.read_text())
    assert sentinel["pr_number"] == 42
    assert sentinel["mode"] == "interactive"
    assert sentinel["pr_url"] == "https://example/pr"


def test_session_satisfied_emitted_when_state_present(enabled, monkeypatch, tmp_path):
    """The row lands in the RESOLVED journal, not beside the checkout.

    This test used to assert `<checkout>/.fno/events.jsonl`. That file is a
    plain file the post-merge worktree reap deletes, so a row written there is
    as unauditable as no row at all. The destination was never noticed because
    three early returns meant the emit had produced no row in any journal.
    """
    fno_dir = tmp_path / ".fno"
    fno_dir.mkdir()
    (fno_dir / "target-state.md").write_text(
        '---\nsession_id: "20260613T000000Z-1-abc"\n---\n'
    )
    events = tmp_path / "space" / "events.jsonl"
    events.parent.mkdir()
    monkeypatch.setattr("fno.paths.project_events_json", lambda *a, **kw: events)
    fake = FakeRun(gh_merge=Result(0, "Merged", ""), toplevel=str(tmp_path))
    monkeypatch.setattr(_merge, "run", fake)
    _merge.run_merge(["42"], cwd=str(tmp_path))
    from tests._event_rows import event_rows

    line = event_rows(events)[-1]
    assert line["type"] == "session_satisfied"
    assert line["data"]["source"] == "pr_merge"
    assert line["data"]["session_id"] == "20260613T000000Z-1-abc"


def _lock_key():
    from fno.paths import resolve_canonical_repo_root

    return f"merge:{resolve_canonical_repo_root()}"


def test_merge_lock_held_by_peer_exits_2_held(enabled, monkeypatch, capsys, tmp_path):
    from fno.claims.core import acquire_claim

    acquire_claim(_lock_key(), "pr-merge:peer", reason="test peer merge")
    monkeypatch.setattr(_merge, "_MERGE_LOCK_WAIT_S", 0)
    fake = FakeRun(gh_merge=Result(0, "Merged pull request", ""), toplevel=str(tmp_path))
    monkeypatch.setattr(_merge, "run", fake)
    assert _merge.run_merge(["42"], cwd=str(tmp_path)) == 2
    obj = _last_json(capsys)
    assert obj["outcome"] == "held"
    assert "merge serialized" in obj["reason"]
    # the gh merge was never attempted while a peer holds the lock
    assert not any(c[1:3] == ["pr", "merge"] for c in fake.calls)



def test_manifest_reader_ignores_keys_inside_a_multiline_input_scalar(
    tmp_path,
):
    """A `harness:` line inside the target's free-text argument is not the
    run's harness. The Rust manifest parser already skips input-scalar lines
    for exactly this forgery (parse_manifest_identity); the Python reader
    agrees, so the two gates cannot disagree on one manifest."""
    state = tmp_path / ".fno"
    state.mkdir(exist_ok=True)
    manifest = str(state / "target-state.md")
    # The scalar body carries a forged harness line before the real one.
    (state / "target-state.md").write_text(
        "---\n"
        'input: "/target fix the floor\nharness: gemini\nsee docs"\n'
        "harness: agy\n"
        "---\n",
        encoding="utf-8",
    )
    assert _merge._read_state_field(manifest, "harness") == "agy"
    # With no real harness line, the forged one is still not attribution:
    # the reader answers an empty string, not a harness name.
    (state / "target-state.md").write_text(
        "---\n"
        'input: "/target fix the floor\nharness: gemini\nsee docs"\n'
        "cross_project: false\n"
        "---\n",
        encoding="utf-8",
    )
    assert _merge._read_state_field(manifest, "harness") == ""
    # The lone-quote shape: `input: "` OPENS the scalar (the Rust parser's
    # len >= 2 guard), so the forged line is still scalar body, not a field.
    (state / "target-state.md").write_text(
        "---\n" 'input: "\n' "harness: gemini\n" 'see docs"\n' "---\n",
        encoding="utf-8",
    )
    assert _merge._read_state_field(manifest, "harness") == ""



def test_merge_lock_released_when_merge_body_raises(enabled, monkeypatch, tmp_path, capsys):
    # Regression pin: an exception thrown through the lock's yield must still
    # release the claim (the original except-then-yield shape leaked it).
    from fno.claims.core import acquire_claim

    def _boom(*_a, **_k):
        raise RuntimeError("merge body exploded")

    monkeypatch.setattr(_merge, "_do_merge", _boom)
    fake = FakeRun(gh_merge=Result(0, "Merged pull request", ""), toplevel=str(tmp_path))
    monkeypatch.setattr(_merge, "run", fake)
    with pytest.raises(RuntimeError, match="merge body exploded"):
        _merge.run_merge(["42"], cwd=str(tmp_path))
    # the lock was released on the way out, not leaked
    acquire_claim(_lock_key(), "pr-merge:next", reason="post-raise probe")


# ---------------------------------------------------------------------------
# Repo without auto-merge enabled (x-3218 side-fix)
# ---------------------------------------------------------------------------


def _rollup(*conclusions, head="deadbeefcafe"):
    return {
        "state": "OPEN",
        "headRefOid": head,
        "statusCheckRollup": [
            {"name": f"c{i}", "status": "COMPLETED", "conclusion": c}
            for i, c in enumerate(conclusions)
        ],
    }


class _AutoMergeRejectingRun(FakeRun):
    """Serves a rollup (default green) and models a racing head at merge time.

    Name predates x-9d11, when gh rejected ``--auto``; the verb no longer sends
    it, so this fake now exists for the checks-verdict fixtures and the
    head-moved discrimination below."""

    def __init__(self, *, rollup=None, head_moved=False, live_head="pushed9999", **kw):
        super().__init__(**kw)
        self.merge_cmds: list[list[str]] = []
        self.rollup = rollup if rollup is not None else _rollup("SUCCESS", "SUCCESS")
        self.checks = self.rollup
        self.head_moved = head_moved
        # The head as it exists server-side at merge time. head_moved=True means
        # someone pushed after the rollup read, so this differs from the rollup's.
        self.live_head = live_head

    def __call__(self, cmd, *, cwd=None, env=None, input_text=None, timeout=None):
        cmd = list(cmd)
        if cmd[:3] == ["gh", "pr", "merge"]:
            self.merge_cmds.append(cmd)
            if "--auto" in cmd:
                return Result(
                    1,
                    "",
                    "GraphQL: Auto merge is not allowed for this repository "
                    "(enablePullRequestAutoMerge)",
                )
            if self.head_moved:
                # Model gh's ACTUAL behaviour, so the test discriminates: the
                # refusal happens only because a pin was sent and no longer
                # matches. An unpinned merge succeeds - which is exactly the
                # bug, so deleting the pin must turn this test red.
                if "--match-head-commit" not in cmd:
                    return Result(0, "Merged pull request #42", "")
                sent = cmd[cmd.index("--match-head-commit") + 1]
                if sent != self.live_head:
                    return Result(
                        1,
                        "",
                        "Head branch was modified. Review and try the merge again.",
                    )
            return Result(0, "Merged pull request #42", "")
        if cmd[:3] == ["gh", "pr", "view"] and any("statusCheckRollup" in a for a in cmd):
            return Result(0, json.dumps(self.rollup) + "\n", "")
        return super().__call__(
            cmd, cwd=cwd, env=env, input_text=input_text, timeout=timeout
        )


def _checks_enabled(monkeypatch):
    monkeypatch.setattr(
        _merge,
        "_load_auto_merge",
        lambda _repo: AutoMergeBlock(enabled=True, require_checks_pass=True),
    )


# ── x-9d11: cleanup split from the merge; one arming path ───────────────────


def test_merge_never_passes_delete_branch_and_deletes_remote_after(
    enabled, monkeypatch, capsys, tmp_path
):
    """AC3-HP: with delete_branch_on_merge=true, the gh call carries no
    --delete-branch (no local git ops, so a worktree-held branch cannot fail
    the merge), and the REMOTE ref is deleted afterwards as cleanup."""
    (tmp_path / ".fno").mkdir()
    monkeypatch.setattr(
        _merge,
        "_load_auto_merge",
        lambda _repo: AutoMergeBlock(enabled=True, delete_branch_on_merge=True),
    )
    fake = FakeRun(gh_merge=Result(0, "Merged pull request", ""), toplevel=str(tmp_path))
    monkeypatch.setattr(_merge, "run", fake)
    assert _merge.run_merge(["42"], cwd=str(tmp_path)) == 0
    obj = _last_json(capsys)
    assert obj["outcome"] == "merged"
    assert obj.get("cleanup", "") == ""
    remote_deletes = [
        c for c in fake.calls
        if "DELETE" in c and c[-1].endswith("/git/refs/heads/feature/x")
    ]
    assert len(remote_deletes) == 1, fake.calls
    # The ref path names the PR's verified base repo (owner/repo by default).
    assert remote_deletes[0][-1] == "repos/owner/repo/git/refs/heads/feature/x"
    assert not any("baseRepository" in token for call in fake.calls for token in call)


def test_fork_pr_skips_remote_delete(
    enabled, monkeypatch, capsys, tmp_path
):
    """A fork PR's head branch lives on the fork, not this repo's origin: the
    delete must never fire (it would destroy an unrelated same-named branch on
    the base repo)."""
    (tmp_path / ".fno").mkdir()
    monkeypatch.setattr(
        _merge,
        "_load_auto_merge",
        lambda _repo: AutoMergeBlock(enabled=True, delete_branch_on_merge=True),
    )
    fake = FakeRun(
        gh_merge=Result(0, "Merged pull request", ""),
        head_repo="contributor/fork",
        base_repo="owner/repo",
        toplevel=str(tmp_path),
    )
    monkeypatch.setattr(_merge, "run", fake)
    assert _merge.run_merge(["42"], cwd=str(tmp_path)) == 0
    obj = _last_json(capsys)
    assert obj["outcome"] == "merged"
    assert obj.get("cleanup", "") == ""
    assert not [c for c in fake.calls if c[:2] == ["gh", "api"] and "DELETE" in c]


def test_unreadable_base_repo_reports_partial(
    enabled, monkeypatch, capsys, tmp_path
):
    """Missing base identity prevents a verified delete and is not success."""
    (tmp_path / ".fno").mkdir()
    monkeypatch.setattr(
        _merge,
        "_load_auto_merge",
        lambda _repo: AutoMergeBlock(enabled=True, delete_branch_on_merge=True),
    )
    fake = FakeRun(
        gh_merge=Result(0, "Merged pull request", ""),
        head_repo="owner/repo",
        base_repo="",
        toplevel=str(tmp_path),
    )
    monkeypatch.setattr(_merge, "run", fake)

    assert _merge.run_merge(["42"], cwd=str(tmp_path)) == 3
    obj = _last_json(capsys)
    assert obj["outcome"] == "partial"
    assert "feature/x" in obj["cleanup"]
    assert "base repository identity unreadable" in obj["cleanup"]
    assert not [c for c in fake.calls if c[:2] == ["gh", "api"] and "DELETE" in c]


def test_a_red_refusal_marks_the_node_failed_not_still_queued(
    enabled, monkeypatch, capsys, tmp_path
):
    """A node left `queued` by an earlier attempt must not read queued after a
    red refusal - the scoreboard consumes that field."""
    (tmp_path / ".fno").mkdir()
    _checks_enabled(monkeypatch)
    seen: list[str] = []
    monkeypatch.setattr(
        _merge, "_sync_graph_merge_status", lambda status, pr, cwd="": seen.append(status)
    )
    fake = _AutoMergeRejectingRun(rollup=_rollup("FAILURE"), toplevel=str(tmp_path))
    monkeypatch.setattr(_merge, "run", fake)
    _owner_answers(
        monkeypatch,
        "failed",
        "checks are red; require_checks_pass forbids merging without green",
    )

    assert _merge.run_merge(["42"], cwd=str(tmp_path)) == 1
    assert seen == ["failed"], seen


# ---- coverage guard (x-0eaf) ----



def test_covered_head_pins_the_merge_cmd(monkeypatch, tmp_path):
    """x-0eaf: the covered head pins the merge so a racing push cannot land an
    unreviewed head (x-9d11: there is no --auto queue to pin anymore; the
    covered head outranks the checks-verdict head on the one merge call)."""
    (tmp_path / ".fno").mkdir()
    _checks_enabled(monkeypatch)
    real_which = _merge.shutil.which
    monkeypatch.setattr(_merge.shutil, "which", lambda tool: "/usr/bin/gh" if tool == "gh" else real_which(tool))
    # Same hermeticity as the `enabled` fixture: the closure fetch a populated
    # per-worker sandbox graph triggers answers non-JSON and holds the merge
    # before any merge command is recorded.
    monkeypatch.setattr("fno.paths.graph_json", lambda: tmp_path / "graph.json")
    monkeypatch.setenv("FNO_CLAIMS_ROOT", str(tmp_path))
    # The native gate answers covered, pinned to the covered head, so the pin
    # rides down to the merge ask.
    monkeypatch.setattr(
        "fno.rust_binary.verb_call",
        _door_stub(None, _gate_verdict("coveredSHA")),
    )
    fake = _AutoMergeRejectingRun(
        rollup=_rollup("SUCCESS", head="coveredSHA"), toplevel=str(tmp_path)
    )
    monkeypatch.setattr(_merge, "run", fake)
    seen = _capture_posture(monkeypatch)
    _merge.run_merge(["42"], cwd=str(tmp_path))
    # The gate's answer and the pin must describe one commit, so the covered
    # head rides down rather than being re-derived on the other side.
    assert seen["covered_head"] == "coveredSHA"



# ---- x-f43c: coverage attested in a worktree, merge run from canonical ----



# -- x-a089: a review that is RUNNING must refuse the merge ------------------
# The predicate itself is `fno do pr review-hold check`, and the decision that
# consults it is the authorized-merge owner - the same one the terminal's
# auto-merge arm asks, which is what closed the hole where a queue armed at the
# terminal shipped code a review was still fixing. What is under test here is
# that this verb renders the owner's hold, and renders it before any merge.


def _held_owner(monkeypatch, reason):
    def _authorized(pr_number, repo, *, effect, approved, source, **kwargs):
        return {"outcome": "held", "detail": reason}

    monkeypatch.setattr(_merge, "_authorized_merge", _authorized)


def test_the_auto_merge_lane_refuses_identically(enabled, monkeypatch, capsys, tmp_path):
    """The lane with no human to hold on judgment gets the same refusal.

    `enabled` is auto_merge.enabled=True plus a working gh and a covered
    review - everything the lane needs to merge. It must still refuse.
    """
    _held_owner(
        monkeypatch,
        "worktree_dirty: /wt/x carries uncommitted changes to tracked files",
    )
    fake = FakeRun(gh_merge=Result(0, "Merged pull request", ""), toplevel=str(tmp_path))
    monkeypatch.setattr(_merge, "run", fake)
    assert _merge.run_merge(["42"], cwd=str(tmp_path)) == 2
    obj = _last_json(capsys)
    assert obj["outcome"] == "held"
    assert "uncommitted changes" in obj["reason"]
    # The proof it fired BEFORE the merge, not merely alongside it.
    assert not any("merge" in " ".join(c) for c in fake.calls if c[:1] == ["gh"])


def test_an_unreachable_owner_holds_rather_than_merging(monkeypatch, capsys, tmp_path):
    """A merge whose authorization could not be read has not been authorized.

    The owner lives in the fno-agents binary. An absent or broken one answers
    `unknown`, and `unknown` must never render as a clear merge.
    """
    from fno.rust_binary import VerbUnavailable

    def _boom(verb, payload, unavailable=VerbUnavailable, **_kw):
        raise unavailable("the fno-agents binary was not found")

    monkeypatch.setattr("fno.rust_binary.verb_call", _boom)
    monkeypatch.setattr(_merge, "_authorized_merge", _REAL_AUTHORIZED_MERGE)
    receipt = _merge._authorized_merge(
        42, str(tmp_path), effect="merge", approved=None, source=""
    )
    assert receipt["outcome"] == "unknown"
    assert "refusing to merge on an unread authorization" in receipt["detail"]


def test_optional_apps_resolves_the_shared_default(tmp_path):
    """One oracle, two readers: this resolver and the Rust gate's
    resolved_optional_bots both answer these rows against the SAME golden file,
    so the two sides cannot drift silently - which is what a hardcoded tuple on
    one side (the status view) and an empty-vec default on the other (the loop
    gate) was."""
    from pathlib import Path

    from fno.config import ReviewBlock, resolved_optional_apps

    golden = json.loads(
        (Path(__file__).resolve().parents[1] / "config" / "optional_apps_default.json").read_text()
    )
    assert resolved_optional_apps(ReviewBlock()) == golden["unset"]
    assert resolved_optional_apps(ReviewBlock(optional_apps=[])) == golden["explicit_empty"]
    # A partial list EXTENDS the built-ins; it never silently drops one.
    assert resolved_optional_apps(ReviewBlock(optional_apps=["my-app"])) == golden["partial_union"]


# --- review posture verdict : the gate reads the Rust verdict ---



# ---- the COVERED pin (external review round, P1) ----


def test_a_no_lane_gate_pins_to_the_prs_own_head(monkeypatch):
    """The stock-install opt-out. No review lane means the gate answers COVERED
    with no head, and the owner refuses an unpinned effect outright - so
    without this the repos that opted out of review could never merge at all.
    """
    monkeypatch.setattr(
        _merge, "_pr_head_ref_and_oid", lambda pr, repo: ("feature/x", "beef123", "OPEN")
    )
    assert _merge.covered_pin(42, "/repo", "") == "beef123"


def test_an_unreadable_pr_head_leaves_the_pin_empty(monkeypatch):
    """Fail-closed. An empty pin is what the owner reads as unknown, and a
    merge whose head cannot be read is exactly the one that must not run."""
    monkeypatch.setattr(_merge, "_pr_head_ref_and_oid", lambda pr, repo: None)
    assert _merge.covered_pin(42, "/repo", "") == ""


def test_a_merged_additional_pr_is_stamped_merged(monkeypatch, tmp_path):
    """x-2774 change 7's recorder leg: the merge-status sync stamps the
    merged PR whether it is the node's primary ref or one of its
    additional_prs, so the reaper's recorded-openness test can settle the
    do rows of a done, merged node. Unrecorded stays open everywhere -
    this is the recorder, never the assertion."""
    graph = tmp_path / "graph.json"
    seed_graph(graph, json.dumps({"entries": [{
        "id": "x-ba96",
        "status": "done",
        "merge_status": "merged",
        "pr_number": 1500,
        "additional_prs": [
            {"number": 1522, "url": "https://github.com/o/r/pull/1522"},
        ],
    }]}))
    monkeypatch.setattr("fno.paths.graph_json", lambda: graph)
    monkeypatch.setattr("fno.tracker.active_backend_name", lambda: "graph")
    monkeypatch.setattr(
        "fno.graph._reconcile.resolve_current_repo_slug", lambda cwd: "o/r"
    )

    _merge._sync_graph_merge_status("merged", 1522)

    saved = read_graph_strict(graph)[0]
    assert saved["additional_prs"][0]["merge_status"] == "merged"
    assert saved["merge_status"] == "merged", "the primary status is untouched"


def test_a_foreign_repo_same_number_primary_pr_is_not_stamped(monkeypatch, tmp_path):
    """A primary PR number collision skips the foreign node and stamps ours."""
    graph = tmp_path / "graph.json"
    seed_graph(graph, json.dumps({"entries": [
        {
            "id": "x-other",
            "pr_number": 1060,
            "pr_url": "https://github.com/other/repo/pull/1060",
        },
        {
            "id": "x-ours",
            "pr_number": 1060,
            "pr_url": "https://github.com/o/r/pull/1060",
        },
    ]}))
    monkeypatch.setattr("fno.paths.graph_json", lambda: graph)
    monkeypatch.setattr("fno.tracker.active_backend_name", lambda: "graph")
    monkeypatch.setattr(
        "fno.graph._reconcile.resolve_current_repo_slug", lambda cwd: "o/r"
    )

    _merge._sync_graph_merge_status("merged", 1060)

    saved = {e["id"]: e for e in read_graph_strict(graph)}
    assert not saved["x-other"].get("merge_status")
    assert saved["x-ours"]["merge_status"] == "merged"


# ---- x-626f: the lock releases when the merge lands; the reconcile child is
# bounded and parent-bound ----


def test_lock_released_before_post_merge_reconcile(enabled, monkeypatch, tmp_path, capsys):
    """AC2-HP: while the first merge is inside its post-merge work (the
    unbounded reconcile), a second merger can already take the lock - the
    race the lock closes ended at the merged receipt."""
    seen = {}

    def fake_on_confirmed(pr, cwd="", **kwargs):
        from fno.claims.core import acquire_claim

        # Raises CLAIM_UNAVAILABLE (and fails the merge) if the first merge
        # still held the lock here - the pre-x-626f behavior.
        acquire_claim(_lock_key(), "pr-merge:second", reason="peer waiting behind us")
        seen["second_acquired"] = True
        return []

    monkeypatch.setattr(_merge, "_on_confirmed_merge", fake_on_confirmed)
    fake = FakeRun(gh_merge=Result(0, "Merged pull request", ""), toplevel=str(tmp_path))
    monkeypatch.setattr(_merge, "run", fake)
    assert _merge.run_merge(["42"], cwd=str(tmp_path)) == 0
    assert seen.get("second_acquired"), "the lock must free before post-merge work runs"


def test_do_merge_defers_close_only_on_the_durable_grant_path(
    enabled, monkeypatch, tmp_path
):
    """x-3bee: the watcher's merge phase (authority="durable_grant") cannot
    hold the inline reconcile inside its slice, so _do_merge defers the
    node-close there and only there; a manifest merge closes inline as
    before."""
    seen = {}

    def recorder(pr, cwd="", *, defer_close=False):
        seen["defer_close"] = defer_close
        return []

    def fake_authorized(pr_number, repo, **kw):
        if kw.get("decide_only"):
            return {"outcome": "authorized"}
        return {"outcome": "merged"}

    monkeypatch.setattr(_merge, "_authorized_merge", fake_authorized)
    monkeypatch.setattr(_merge, "_on_confirmed_merge", recorder)
    monkeypatch.setattr(_merge, "_post_merge_remote_delete", lambda *a, **k: "")
    monkeypatch.setattr(_merge, "_run_post_merge_followups", lambda *a, **k: None)
    block = AutoMergeBlock(enabled=True)

    _merge._do_merge(
        42, block, str(tmp_path), covered_head="deadbeef", authority="durable_grant"
    )
    assert seen["defer_close"] is True

    _merge._do_merge(42, block, str(tmp_path), covered_head="deadbeef")
    assert seen["defer_close"] is False


def test_early_release_frees_the_lock_for_a_successor(enabled, monkeypatch, tmp_path):
    """AC2-ERR: after the early fire, a successor takes the lock, and the
    with-block's finally release (the same holder-checked call) leaves the
    successor's claim on disk untouched - the double-release is release_claim's
    own documented silent-success contract."""
    from fno.claims.core import acquire_claim, claim_status, release_claim

    with _merge._merge_lock(42) as (state, release_now, held_detail):
        assert state == "acquired" and release_now is not None
        assert held_detail is None
        release_now()
        # the freed lock is takeable right now, before the merge verb returns
        acquire_claim(_lock_key(), "pr-merge:successor", reason="next merger")
    assert claim_status(_lock_key()).get("holder") == "pr-merge:successor", (
        "the finally release must not free the successor's claim"
    )
    release_claim(_lock_key(), "pr-merge:successor")
    assert claim_status(_lock_key())["state"] == "free"


def test_reconcile_child_is_bounded_and_parent_bound(enabled, monkeypatch, tmp_path):
    """AC3: the merge's reconcile child carries the timeout and the
    FNO_DIE_WITH_PARENT binding, so a killed merge never orphans it."""
    import os

    captured = []
    inner = FakeRun(
        gh_merge=Result(0, "Merged pull request", ""),
        toplevel=str(tmp_path),
        # a resolvable github url: the repo slug from it scopes the reconcile
        view_url="https://github.com/owner/repo/pull/42",
    )
    graph = tmp_path / "graph.json"
    seed_graph(graph, json.dumps({"entries": []}))
    monkeypatch.setattr("fno.paths.graph_json", lambda: graph)
    monkeypatch.setattr("fno.tracker.active_backend_name", lambda: "graph")
    def fake(cmd, **kwargs):
        if "reconcile" in cmd:
            captured.append((list(cmd), kwargs))
            return Result(0, "{}", "")
        return inner(cmd, **kwargs)

    monkeypatch.setattr(_merge, "run", fake)
    assert _merge.run_merge(["42"], cwd=str(tmp_path)) == 0
    hits = [kw for cmd, kw in captured if "reconcile" in cmd]
    assert hits, "the merge must run its post-merge reconcile"
    kw = hits[0]
    assert kw.get("timeout") == 300.0
    assert kw.get("env", {}).get("FNO_DIE_WITH_PARENT") == str(os.getpid())


def _gate_verdict(head: str) -> dict:
    """A native gate verdict answer: covered, pinned to `head`."""
    return {"state": 0, "refusal": "", "covered_head": head, "note": "", "line": ""}


def test_populated_graph_closure_fetch_does_not_hold_the_merge(
    enabled, monkeypatch, capsys, tmp_path
):
    """A populated graph must not hold the merge. The dispatch hold is the
    owner's read now (`fno do pr hold-check`, its own suite), so run_merge
    consults no closure itself: a done entry in the graph only feeds the
    post-merge reconcile this test keeps hermetic, and the merge still lands
    pinned to the covered head the verdict door answers."""
    (tmp_path / ".fno").mkdir()
    _checks_enabled(monkeypatch)
    # The native gate answers covered, pinned to the covered head the merge
    # ask carries down.
    monkeypatch.setattr(
        "fno.rust_binary.verb_call",
        _door_stub(None, _gate_verdict("coveredSHA")),
    )
    # One cwd-less done entry: enough to make the graph non-empty for every
    # reader that looks, none of which may hold the merge on it.
    seed_graph(tmp_path / "graph.json", json.dumps({"entries": [{"id": "x-eeee", "status": "done"}]}))
    monkeypatch.setattr("fno.paths.graph_json", lambda: tmp_path / "graph.json")
    fake = _AutoMergeRejectingRun(
        rollup=_rollup("SUCCESS", head="coveredSHA"), toplevel=str(tmp_path)
    )
    monkeypatch.setattr(_merge, "run", fake)

    seen = _capture_posture(monkeypatch)
    assert _merge.run_merge(["42"], cwd=str(tmp_path)) == 0
    assert _last_json(capsys)["outcome"] == "merged"
    assert seen["covered_head"] == "coveredSHA"


def test_an_open_pr_still_holds_as_unreviewed(enabled, monkeypatch, capsys, tmp_path):
    """The exemption must not become a hole: an OPEN uncovered PR still holds
    as unreviewed - by decide's review verdict now (x-53c5), whose receipt the
    verb renders verbatim."""
    fake = FakeRun(gh_merge=Result(0, "Merged pull request", ""), toplevel=str(tmp_path))
    monkeypatch.setattr(_merge, "run", fake)
    monkeypatch.setattr(
        _merge, "_pr_head_ref_and_oid", lambda pr, repo: ("feature/x-a089", "abc123", "OPEN")
    )
    monkeypatch.setattr(
        _merge,
        "_authorized_merge",
        lambda pr_number, repo, **kw: {
            "outcome": "held",
            "detail": "unreviewed merge refused: no review row describes this head",
        },
    )
    assert _merge.run_merge(["42"], cwd=str(tmp_path)) == 2
    obj = _last_json(capsys)  # a held receipt renders on stdout
    assert obj["outcome"] == "held"
    assert "unreviewed merge refused" in obj["reason"]
