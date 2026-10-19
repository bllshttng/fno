"""Characterization tests for the _merge.py port (ab-d4c98550, US1/AC1/AC5).

Mocks gh/git at the _proc.run seam so the guard-rejection, classification,
and worktree-recovery branches are exercised deterministically (they are hard
to reproduce against a live PR). Pins the JSON-line schema, the exit codes,
and the stdout-vs-stderr routing the bash used.
"""
from __future__ import annotations
from tests.fixtures.graph_seed import seed_graph

import json
from types import SimpleNamespace

import pytest

from fno.config import AutoMergeBlock
from fno.pr import _coverage_gate, _merge
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
    # x-0eaf: default to a covered review so existing merge-behavior tests
    # proceed past the coverage guard. No head_sha -> covered_head is empty, so
    # the --match-head-commit pin (and the head-pin tests' own verified_head)
    # are undisturbed; the pin is exercised by its own test below.
    monkeypatch.setattr(
        _merge,
        "_review_coverage_for_pr",
        lambda pr, repo, head=None: ({"coverage": "covered", "review_state": "reviewed", "reviewed_count": 1}, ""),
    )
    monkeypatch.setattr(_merge, "_review_lane_configured", lambda repo, pr_number=0: True)
    # x-129b flipped the unattributable default to fail-closed, so a hermetic
    # (markerless, manifestless) test env now floors the self-review
    # attestation. Merge-behavior tests below are not about that gate; tests
    # that exercise it override this with their own monkeypatch.
    monkeypatch.setattr(
        _merge, "_code_review_attestation_required", lambda repo, pr_number=0: False
    )
    # No 3am valve by default. The gate reads the override label on every
    # verdict, and an unstubbed read is a real `gh pr view` per merge case.
    monkeypatch.setattr(
        "fno.pr._reviews._override_label_actor", lambda pr, repo, r: (False, None)
    )
    # The merge's server receipt is best-effort; hermetic tests stub the
    # publisher so no real gh spawn rides along with every merge case.
    monkeypatch.setattr(
        "fno.pr._reviews.publish_coverage_status",
        lambda pr, head=None, cwd=None, repo=None, gate_verdict=None: (True, ""),
    )
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


def _door_stub(answer):
    """A verb_call fake answering only the status-rerun ask (None = the
    probe is unavailable, the fail-open old default); every other ask rides
    the real transport."""
    import fno.rust_binary as rust_binary

    real = rust_binary.verb_call

    def _fake(verb, payload, unavailable=None, **kw):
        if isinstance(payload, dict) and payload.get("op") == "status-rerun":
            if answer is None:
                raise (unavailable or rust_binary.VerbUnavailable)(
                    "status-rerun unavailable in tests"
                )
            return answer
        return real(verb, payload, unavailable=unavailable, **kw)

    return _fake


_real_lane_configured = _merge._review_lane_configured


@pytest.fixture(autouse=True)
def _real_review_lane(monkeypatch):
    """These tests exercise the lane predicate itself; restore it over the
    conftest hermetic default (which suites that merely traverse a merge need)."""
    monkeypatch.setattr(_merge, "_review_lane_configured", _real_lane_configured)


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


def _stub_owner_from_row(monkeypatch, tmp_path):
    """The refusal is decide's now: ask the real gate for its line against the
    seeded row and hand it back as the held receipt the verb renders. The
    conftest's own autouse stub owns the covered default, so a covered row
    patches nothing."""
    from fno.pr import _coverage_gate

    state, refusal, _covered, note = _coverage_gate.coverage_verdict(
        42, str(tmp_path), recompute=True
    )
    if state == _coverage_gate.COVERED:
        return None
    line = _coverage_gate.refusal_line(refusal, note)
    # Same sentence the Rust gate composes per exit code, so the render pin
    # is the wire shape, not a test-local dialect. The gate module answers
    # three states; its uncovered arm is the unreviewed-refusal sentence.
    if state == _coverage_gate.UNANSWERED:
        detail = f"coverage probe failed, merge refused: {line}" if line else (
            "coverage probe failed, merge refused"
        )
    else:
        detail = f"unreviewed merge refused: {line}" if line else (
            "unreviewed merge refused"
        )
    monkeypatch.setattr(
        _merge,
        "_authorized_merge",
        lambda pr, repo, **kw: {"outcome": "held", "detail": detail},
    )
    return detail


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


def _point_lane_read_at(monkeypatch, **fields):
    # _review_lane_configured imports load_settings_for_repo at call time, so
    # patching the module attribute is seen by the local import.

    import fno.config as cfg

    review_fields = dict(
        required_bots=None,
        optional_apps=None,
        reviewers=None,
        peers=[],
        peer_identity=None,
    )
    review_fields.update(fields)
    review = SimpleNamespace(**review_fields)
    monkeypatch.setattr(cfg, "load_settings_for_repo", lambda _path: SimpleNamespace(review=review))


@pytest.mark.parametrize(
    "fields,expected",
    [
        # Stock install: no lane, so the coverage guard does not apply.
        (dict(), False),
        # peers present but peer_identity set: peers post under the shared
        # login, not a distinct local lane. Rust agrees (loopcheck.rs:2492).
        (dict(peers=["codex"], peer_identity="fno-peer-bot"), False),
        # An identity-free peer (bare string) is a local-attestation lane.
        (dict(peers=["codex"]), True),
    ],
)
def test_review_lane_configured_matches_loopcheck_gate(monkeypatch, fields, expected, tmp_path):
    _point_lane_read_at(monkeypatch, **fields)
    assert _merge._review_lane_configured(str(tmp_path)) is expected


def test_review_lane_configured_resolves_toplevel_from_a_subdirectory(
    monkeypatch, tmp_path
):
    """A lane declared at the project root is still seen from a subdirectory.

    load_settings_for_repo reads ``<arg>/.fno/`` with NO upward walk, so passing
    the raw invocation directory made ``fno do pr merge`` run from ``cli/`` report
    "no lane" - which short-circuits ``covered`` to True and lets an unreviewed
    PR merge. The guard was decorative on that path.

    The parametrized test above cannot catch this: it monkeypatches
    load_settings_for_repo, so it pins the predicate while stubbing out the path
    resolution the bug lives in. This one uses the REAL loader against real
    files, and isolates the global layer via FNO_GLOBAL_SETTINGS_PATH
    (``/dev/null`` is the documented disable hook) - without that the
    developer's own ~/.fno lane leaks in and masks the failure entirely.
    """
    import subprocess

    monkeypatch.setenv("FNO_GLOBAL_SETTINGS_PATH", "/dev/null")
    repo = tmp_path / "repo"
    (repo / ".fno").mkdir(parents=True)
    (repo / "sub").mkdir()
    subprocess.run(["git", "init", "-q", str(repo)], check=True)
    # The lane exists ONLY at the project root.
    (repo / ".fno" / "config.toml").write_text(
        '[review]\ngithub_apps = ["chatgpt-codex-connector"]\n'
    )

    assert _merge._review_lane_configured(str(repo)) is True
    assert _merge._review_lane_configured(str(repo / "sub")) is True


def test_review_lane_configured_floors_a_code_payload(monkeypatch, tmp_path):
    """A code payload on a lane-less stock install requires review (the
    self-review floor), so the merge coverage guard engages for the same PR the
    stop gate holds. Mirrors floor_self_review in loopcheck.rs."""
    from fno.harness_identity import HARNESS_SESSION_MARKERS

    for marker, _harness in HARNESS_SESSION_MARKERS:
        monkeypatch.delenv(marker, raising=False)
    monkeypatch.setenv("CLAUDE_CODE_SESSION_ID", "claude-test-session")
    # Stock install: no lane. A code payload + pr_number -> floored to True.
    _point_lane_read_at(monkeypatch)
    monkeypatch.setattr(_merge, "_pr_payload_is_code", lambda repo, pr_number: True)
    assert _merge._review_lane_configured(str(tmp_path), 42) is True
    # Opt-out: self_review_required=false restores unreviewed code-PR shipping.
    _point_lane_read_at(monkeypatch, self_review_required=False)
    assert _merge._review_lane_configured(str(tmp_path), 42) is False
    # A docs payload -> no floor.
    _point_lane_read_at(monkeypatch)
    monkeypatch.setattr(_merge, "_pr_payload_is_code", lambda repo, pr_number: False)
    assert _merge._review_lane_configured(str(tmp_path), 42) is False


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


def test_review_floor_is_caller_independent_across_env_shapes(
    monkeypatch, tmp_path
):
    """x-129b: the same PR owes the same review whatever process asks.

    Measured on PR 1108: a claude session started from a codex shell resolves
    two harness families, and the pre-fix predicate read that ambiguity as
    'no floor', silently disengaging the only review a stock install demands.
    Ambiguity is not permission - every unattributable shape floors, and
    with the verbless-release shape retired no manifest/ambient shape
    releases at all."""
    from fno.harness_identity import HARNESS_SESSION_MARKERS

    for marker, _harness in HARNESS_SESSION_MARKERS:
        monkeypatch.delenv(marker, raising=False)
    monkeypatch.setattr(_merge, "_pr_payload_is_code", lambda repo, pr_number: True)

    def requires_review():
        _point_lane_read_at(monkeypatch)
        return (
            _merge._review_lane_configured(str(tmp_path), 42),
            _merge._code_review_attestation_required(str(tmp_path), 42),
        )

    # Clean env (a plain shell merging by hand): unattributable, floors.
    assert requires_review() == (True, True)
    # The defect's default state: claude session started from a codex shell.
    monkeypatch.setenv("CLAUDE_CODE_SESSION_ID", "c1")
    monkeypatch.setenv("CODEX_THREAD_ID", "t1")
    monkeypatch.setenv("CODEX_SESSION_ID", "s1")
    assert requires_review() == (True, True)
    # A multi-marker exotic shape, and the manifest-less poisoned env.
    monkeypatch.setenv("GEMINI_SESSION_ID", "g1")
    assert requires_review() == (True, True)
    monkeypatch.delenv("GEMINI_SESSION_ID")
    assert requires_review() == (True, True)

    # A verbless manifest cannot release alone: it is an init-time snapshot
    # that outlives its run, and nothing ties it to the PR being merged, so
    # under the poisoned multi-family ambient above it is an unattributable
    # shape and floors.
    state = tmp_path / ".fno"
    state.mkdir(exist_ok=True)
    (state / "target-state.md").write_text(
        "---\nharness: agy\n---\n", encoding="utf-8"
    )
    assert requires_review() == (True, True)
    (state / "target-state.md").write_text(
        "---\nharness: claude\n---\n", encoding="utf-8"
    )
    assert requires_review() == (True, True)
    # Same verdict for a verbless manifest under a poisoned claude ambient
    # (ambiguous, so no agreeing second read).
    (state / "target-state.md").write_text(
        "---\nharness: gemini\n---\n", encoding="utf-8"
    )
    assert requires_review() == (True, True)
    # No release exists any more: with the verbless-release shape retired,
    # even a clean single-family gemini ambient agreeing with a gemini
    # manifest still floors.
    monkeypatch.delenv("CODEX_THREAD_ID")
    monkeypatch.delenv("CODEX_SESSION_ID")
    monkeypatch.delenv("CLAUDE_CODE_SESSION_ID")
    monkeypatch.setenv("GEMINI_SESSION_ID", "g1")
    assert requires_review() == (True, True)
    # A verbless manifest that CONTRADICTS a clean verbful ambient read
    # floors too: the manifest is an init-time snapshot that outlives its
    # run, and a dead run's verbless stamp must not disengage the floor for
    # a verbful PR merged from that checkout.
    monkeypatch.delenv("GEMINI_SESSION_ID")
    monkeypatch.setenv("CLAUDE_CODE_SESSION_ID", "c1")
    assert requires_review() == (True, True)
    # The unknown sentinel is no attribution: ambient alone decides, and a
    # poisoned ambient therefore floors.
    (state / "target-state.md").write_text(
        "---\nharness: unknown\n---\n", encoding="utf-8"
    )
    monkeypatch.setenv("CODEX_THREAD_ID", "t1")
    monkeypatch.setenv("CODEX_SESSION_ID", "s1")
    assert requires_review() == (True, True)


def test_pr_payload_classifier_is_documentation_aware_and_fail_closed(monkeypatch):
    """The merge-side classifier matches loopcheck's notion of documentation and
    fails closed on a missing gh (a degraded probe must not bypass the guard)."""
    assert _merge._is_documentation_path("docs/a.md") is True
    assert _merge._is_documentation_path("README.md") is True
    assert _merge._is_documentation_path("src/lib.rs") is False
    assert _merge._is_documentation_path(".fno/config.toml") is False
    monkeypatch.setattr(_merge.shutil, "which", lambda _x: None)
    assert _merge._pr_payload_is_code("/nope", 42) is True


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


def test_coverage_override_is_not_vetoed_by_its_stale_failure_status(
    enabled, monkeypatch, capsys, tmp_path
):
    (tmp_path / ".fno").mkdir()
    _checks_enabled(monkeypatch)
    monkeypatch.setattr(
        "fno.pr._reviews._override_label_actor",
        lambda pr, repo, runner: (True, "maintainer"),
    )
    rollup = {
        "state": "OPEN",
        "headRefOid": "deadbeefcafe",
        "statusCheckRollup": [
            {"name": "ci", "status": "COMPLETED", "conclusion": "SUCCESS"},
            {"context": "fno/review-coverage", "state": "FAILURE"},
        ],
    }
    fake = _AutoMergeRejectingRun(rollup=rollup, toplevel=str(tmp_path))
    monkeypatch.setattr(_merge, "run", fake)

    assert _merge.run_merge(["42"], cwd=str(tmp_path)) == 0
    assert _last_json(capsys)["outcome"] == "merged"


def test_coverage_missing_refuses(enabled, monkeypatch, capsys, tmp_path):
    """No review_coverage event -> Unknown -> the sanctioned merge refuses."""
    # Pin the head the stubbed (absent) row would describe: the gate refuses a
    # head it could not fetch, and this test pins the missing-ROW refusal.
    monkeypatch.setattr(_merge, "_pr_head_oid", lambda pr, repo: "abc")
    monkeypatch.setattr(_merge, "_review_coverage_for_pr", lambda pr, repo, head=None: (None, ""))
    _stub_owner_from_row(monkeypatch, tmp_path)
    assert _merge.run_merge(["42"], cwd=str(tmp_path)) == 2
    obj = _last_json(capsys)  # a held receipt renders on stdout
    assert obj["outcome"] == "held"
    assert "unreviewed" in obj["reason"]


def test_covered_merge_publishes_coverage_status(enabled, monkeypatch, capsys, tmp_path):
    """A covered merge leaves the server-visible receipt behind.

    The commit status the repo ruleset requires is written from the same
    predicate that satisfied the gate here, so the sanctioned path can never be
    the one missing the green marker GitHub judges merges by. The receipt is
    pinned to the covered head: the sha the verdict describes, and the sha the
    merge itself is pinned to.
    """
    (tmp_path / ".fno").mkdir()
    fake = FakeRun(gh_merge=Result(0, "Merged pull request", ""), toplevel=str(tmp_path))
    monkeypatch.setattr(_merge, "run", fake)
    monkeypatch.setattr(
        _merge,
        "_review_coverage_for_pr",
        lambda pr, repo, head=None: (
            {
                "coverage": "covered",
                "review_state": "reviewed",
                "reviewed_count": 1,
                "head_sha": "abc",
            },
            "",
        ),
    )
    monkeypatch.setattr(_merge, "_pr_head_oid", lambda pr, repo: "abc")
    monkeypatch.setattr(
        _merge, "_code_review_attestation_required", lambda repo, pr_number=0: False
    )
    published = []
    monkeypatch.setattr(
        "fno.pr._reviews.publish_coverage_status",
        lambda pr, head=None, cwd=None, repo=None, gate_verdict=None: published.append(
            (pr, head, gate_verdict)
        )
        or (True, ""),
    )
    assert _merge.run_merge(["42"], cwd=str(tmp_path)) == 0
    assert _last_json(capsys)["outcome"] == "merged"
    assert published and published[0][:2] == (42, "abc"), (
        "covered merge must publish the status on the covered head"
    )
    verdict = published[0][2]
    assert verdict is not None and verdict[0] == _coverage_gate.COVERED, (
        "the receipt must carry the gate's own verdict tuple, never a fresh "
        "read that may have flipped between the gate and the stamp"
    )


def test_fresh_eval_carrying_only_a_stale_bot_verdict_refuses(
    enabled, monkeypatch, capsys, tmp_path
):
    """x-5b99: the exact hole, at the gate that was supposed to catch it.

    The staleness check below the coverage guard compares the EVAL head against
    the current PR head, and it works. It just answers a different question: a
    coverage event computed thirty seconds ago is fresh by that measure while
    the only verdict inside it came from a bot that read a commit two commits
    back. A fresh eval carrying a stale bot verdict walked straight through.

    What closes it is upstream (loop-check no longer counts a stale verdict, so
    the count arrives at zero and the word arrives as "uncovered"), which is why
    the merge logic here is UNCHANGED. This test pins that the shape now
    refuses, so a later change that restores the old serialization cannot
    quietly reopen the door.
    """
    monkeypatch.setattr(
        _merge,
        "_review_coverage_for_pr",
        lambda pr, repo, head=None: ({
            "coverage": "uncovered",
            "reviewed_count": 0,
            # Fresh eval: this IS the PR's current head, so the staleness check
            # has nothing to object to.
            "head_sha": "89bc0b91",
            "verdicts": [
                {
                    "producer": "github_app",
                    "name": "chatgpt-codex-connector",
                    "verdict": "stale",
                    "reviewed_sha": "8e557ccd",
                    "freshness": "stale",
                }
            ],
        }, ""),
    )
    monkeypatch.setattr(_merge, "_pr_head_oid", lambda pr, repo: "89bc0b91")
    _stub_owner_from_row(monkeypatch, tmp_path)
    assert _merge.run_merge(["42"], cwd=str(tmp_path)) == 2
    obj = _last_json(capsys)
    assert obj["outcome"] == "held"
    # The refusal names coverage, not staleness: the eval was not stale.
    assert "uncovered" in obj["reason"]


def test_code_review_gate_rejects_unrelated_github_app_coverage(
    enabled, monkeypatch, capsys, tmp_path
):
    """A required local code-review cannot be replaced by an unrelated App review."""
    # This test IS about the attestation gate, so restore the real predicate
    # the `enabled` fixture defaults off (hermetic-env floor release).
    monkeypatch.setattr(
        _merge,
        "_code_review_attestation_required",
        lambda repo, pr_number=0: True,
    )
    state = tmp_path / ".fno"
    state.mkdir()
    (state / "config.toml").write_text(
        '[review]\nreviewers = ["code-review"]\n', encoding="utf-8"
    )
    fake = FakeRun(gh_merge=Result(0, "Merged pull request", ""), toplevel=str(tmp_path))
    monkeypatch.setattr(_merge, "run", fake)
    monkeypatch.setattr(
        _merge,
        "_review_coverage_for_pr",
        lambda pr, repo, head=None: ({
            "coverage": "covered",
            "review_state": "reviewed",
            "reviewed_count": 1,
            "head_sha": "abc",
            "verdicts": [
                {
                    "name": "chatgpt-codex-connector",
                    "producer": "github_app",
                    "verdict": "reviewed",
                }
            ],
        }, ""),
    )

    _stub_owner_from_row(monkeypatch, tmp_path)
    assert _merge.run_merge(["42"], cwd=str(tmp_path)) == 2
    obj = _last_json(capsys)
    assert obj["outcome"] == "held"
    assert "code-review" in obj["reason"]
    assert not any(c[:3] == ["gh", "pr", "merge"] for c in fake.calls)


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
    monkeypatch.setattr(
        _merge,
        "_review_coverage_for_pr",
        lambda pr, repo, head=None: ({"coverage": "covered", "review_state": "reviewed", "reviewed_count": 1, "head_sha": "coveredSHA"}, ""),
    )
    monkeypatch.setattr(_merge, "_review_lane_configured", lambda repo, pr_number=0: True)
    # Fresh coverage: the live PR head IS the covered head, so the gate passes
    # and the covered head reaches the merge command.
    monkeypatch.setattr(_merge, "_pr_head_oid", lambda pr, repo: "coveredSHA")
    # The gate's chain read scopes by the PR's refs; an unstubbed probe here
    # answered UNANSWERED off the real transport and wiped the pin.
    monkeypatch.setattr(
        _merge, "_pr_base_head_refs", lambda pr, repo: ("main", "feature/x")
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


def test_genuine_zero_names_an_absent_reviewer_and_its_consequence():
    """Parity with the Rust receipt, and this is the twin that matters more: the
    merge gate here reads coverage only, so a worker who follows a bare "run the
    review verb" while a configured reviewer is still out self-attests and merges
    ahead of a bot that never spoke. Name who is outstanding, and say what
    covering locally would do - do not steer to either."""
    cov = {
        "coverage": "covered",
        "reviewed_count": 0,
        "head_sha": "abc",
        "verdicts": [
            {"producer": "github_app", "name": "gemini-code-assist", "verdict": "absent"},
            {"producer": "github_app", "name": "chatgpt-codex-connector", "verdict": "refused"},
        ],
    }
    reason = _merge._coverage_refused_reason(cov)
    assert "waiting on gemini-code-assist" in reason
    # Never prescribes a local re-attest while someone is outstanding: this gate
    # reads coverage alone, so a worker that self-attests past a REQUIRED bot
    # lands the PR before its blocking finding posts, and this line cannot tell
    # required from optional.
    assert "review verb" not in reason
    # But it is not a bare wait either - an absent App that is never installed
    # would strand the PR - so a safe move is always named.
    assert "check config.review" in reason
    # The refused reviewer is not misreported as something we are waiting for.
    assert "chatgpt-codex-connector" not in reason


def test_coverage_refusal_appends_the_sized_invocation_hint():
    """The refusal hands the worker the exact invocation (rendered by the
    Python single source), verbatim and backticked, after the instruction.
    Without a hint the line reads exactly as before - a broken render must
    never read as a different verdict."""
    cov = {"coverage": "covered", "reviewed_count": 0, "head_sha": "abc", "verdicts": []}
    hint = "/verb-from-the-builder --flags"
    reason = _merge._coverage_refused_reason(cov, self_review_hint=hint)
    assert "run the review verb at HEAD" in reason
    assert f"- `{hint}`" in reason
    bare = _merge._coverage_refused_reason(cov)
    assert "verb-from-the-builder" not in bare
    # The ordering and the producer ride on both arms, hint or no hint: the
    # stop gate says the same thing about the same uncovered head.
    for line in (reason, bare):
        assert "close every finding, commit and push first" in line
        assert "skills/review/scripts/emit-attestation.sh" in line
    # The emitter refuses a call with no reviewer name, so the rendered command
    # carries the argument. A bare path here would exit 1 for whoever copies it.
    assert "`bash skills/review/scripts/emit-attestation.sh <reviewer>`" in bare
    assert bare.endswith("<reviewer>`)")

    # The stale-head arm carries the hint the same way.
    stale = {"coverage": "covered", "reviewed_count": 0, "head_sha": "zzz", "verdicts": []}
    moved = _merge._coverage_refused_reason(stale, head="abc", self_review_hint=hint)
    assert "re-run the review verb at HEAD" in moved
    assert f"- `{hint}`" in moved


def test_stale_head_blocked_receipt_carries_the_cause(enabled, monkeypatch, capsys, tmp_path):
    """End to end through run_merge: the emitted blocked receipt - the line a
    worker actually reads - names the moved head, not a bare count."""
    monkeypatch.setattr(
        _merge,
        "_review_coverage_for_pr",
        lambda pr, repo, head=None: ({"coverage": "covered", "review_state": "reviewed", "reviewed_count": 2, "head_sha": "oldhead0"}, ""),
    )
    monkeypatch.setattr(_merge, "_pr_head_oid", lambda pr, repo: "newhead0")
    _stub_owner_from_row(monkeypatch, tmp_path)
    assert _merge.run_merge(["42"], cwd=str(tmp_path)) == 2
    reason = _last_json(capsys)["reason"]
    assert "0 reviewed" not in reason
    assert "oldhead0" in reason and "newhead0" in reason


# ---- x-f43c: coverage attested in a worktree, merge run from canonical ----


def _git_repo(root, remote):
    """A checkout with a remote, so the coverage reader can resolve its slug."""
    import subprocess

    root.mkdir(parents=True, exist_ok=True)
    subprocess.run(["git", "init", "-q"], cwd=str(root), check=True)
    subprocess.run(["git", "remote", "add", "origin", remote], cwd=str(root), check=True)
    (root / ".fno").mkdir(exist_ok=True)
    return root


def _global_events_at(monkeypatch, root):
    """Point the global events journal at a tmp dir and return it.

    Patches the resolver the code actually calls rather than an env var:
    an env pin moves only the readers that honor it (``FNO_STATE_DIR`` moves
    ``state_dir``/``locks_dir``, not every journal path), so patching the
    resolver is the strict way to keep a negative assertion honest.
    """
    from fno import paths

    root.mkdir(parents=True, exist_ok=True)
    monkeypatch.setattr(paths, "global_events_json", lambda: root / "events.jsonl")
    return root


def _write_coverage(events_path, pr, *, repo=None, ts="2026-08-08T02:35:30Z", count=1):
    data = {
        "pr": pr,
        "coverage": "covered",
        "review_state": "reviewed",
        "reviewed_count": count,
        "head_sha": "a3f4b413b",
        "verdicts": [{
            "name": "code-review",
            "producer": "local_attestation",
            "verdict": "reviewed",
            "reviewed_sha": "a3f4b413b",
            "freshness": "fresh",
        }],
    }
    if repo is not None:
        data["repo"] = repo
    events_path.parent.mkdir(parents=True, exist_ok=True)
    with open(events_path, "a", encoding="utf-8") as fh:
        fh.write(
            json.dumps({"ts": ts, "type": "review_coverage", "source": "hook", "data": data})
            + "\n"
        )


def test_worktree_attestation_is_visible_from_canonical(monkeypatch, tmp_path):
    """THE REPRO (PR #781). A worker attests coverage inside a worktree, then
    runs the merge gate from canonical. That refused, because the stop hook
    writes the events file of the directory it runs in while the gate reads the
    events file of the directory the merge runs in - so a satisfied gate read as
    an unsatisfiable one, and the worker escalated a green PR.
    """
    remote = "git@github.com:bllshttng/fno.git"
    canonical = _git_repo(tmp_path / "canonical", remote)
    worktree = _git_repo(tmp_path / "wt-x-4c98", remote)
    global_dir = _global_events_at(monkeypatch, tmp_path / "home" / ".fno")

    # The review ran in the worktree; emit_to_both writes its project log and
    # the one global log.
    _write_coverage(worktree / ".fno" / "events.jsonl", 781, repo="github.com/bllshttng/fno")
    _write_coverage(global_dir / "events.jsonl", 781, repo="github.com/bllshttng/fno")

    # Canonical's own log never saw it. That absence is the whole specimen.
    assert not (canonical / ".fno" / "events.jsonl").exists()

    cov, _note = _merge._review_coverage_for_pr(781, str(canonical))
    assert cov is not None, "coverage attested in a worktree must be visible from canonical"
    assert cov["coverage"] == "covered"
    assert cov["reviewed_count"] == 1


def test_same_named_repos_under_different_owners_do_not_share_coverage(monkeypatch, tmp_path):
    """codex P1: the global log key must be the FULL repo identity.

    `org-a/widget` and `org-b/widget` both reduce to `widget` under a last-path-
    segment slug. With a shared PR number that let one repo's coverage satisfy
    the other's auto-merge guard, and a fork shares head SHAs so the staleness
    check would not catch it either.
    """
    canonical = _git_repo(tmp_path / "canonical", "git@github.com:org-a/widget.git")
    global_dir = _global_events_at(monkeypatch, tmp_path / "home" / ".fno")

    _write_coverage(global_dir / "events.jsonl", 781, repo="github.com/org-b/widget")
    assert _merge._review_coverage_for_pr(781, str(canonical))[0] is None

    # The same repo's own identity still resolves.
    _write_coverage(global_dir / "events.jsonl", 781, repo="github.com/org-a/widget")
    assert _merge._review_coverage_for_pr(781, str(canonical))[0] is not None


def test_equal_timestamps_take_the_safer_verdict(monkeypatch, tmp_path):
    """codex P2: timestamps are second-precision and emit_to_both writes both
    logs in the same instant, so a strict `>` made "project log wins" a silent
    tiebreak - letting a stale covered event outrank a same-second unknown and
    clear a guard whose whole job is to fail closed."""
    canonical = _git_repo(tmp_path / "canonical", "git@github.com:org-a/widget.git")
    global_dir = _global_events_at(monkeypatch, tmp_path / "home" / ".fno")

    ts = "2026-08-08T02:35:30Z"
    _write_coverage(canonical / ".fno" / "events.jsonl", 781, ts=ts, count=1)
    events = global_dir / "events.jsonl"
    events.write_text(
        json.dumps({
            "ts": ts,
            "type": "review_coverage",
            "data": {
                "pr": 781, "coverage": "unknown", "head_sha": "a3f4b413b",
                "repo": "github.com/org-a/widget",
            },
        }) + "\n",
        encoding="utf-8",
    )

    cov, _note = _merge._review_coverage_for_pr(781, str(canonical))
    assert cov is not None and cov["coverage"] == "unknown"


def test_corrupt_line_does_not_wedge_the_gate(monkeypatch, tmp_path):
    """A malformed line in a months-old append-only log must not hide a real
    attestation: read_events raised on the first bad line, and every caller
    turns an exception into a refusal, so one corrupt byte would have wedged
    merges permanently and without explanation."""
    canonical = _git_repo(tmp_path / "canonical", "git@github.com:bllshttng/fno.git")
    _global_events_at(monkeypatch, tmp_path / "home" / ".fno")

    # The project log the gate reads is the space journal (x-d2e9).
    from fno.paths import project_log

    events = project_log("events.jsonl", project_root=canonical)
    events.parent.mkdir(parents=True, exist_ok=True)
    events.write_text('{"type": "review_coverage", TRUNCATED\n', encoding="utf-8")
    _write_coverage(events, 781)

    cov, _note = _merge._review_coverage_for_pr(781, str(canonical))
    assert cov is not None and cov["coverage"] == "covered"


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


def _covered_row(posture):
    row = {
        "coverage": "covered",
        "reviewed_count": 1,
        "head_sha": "a" * 40,
        "verdicts": [
            {
                "producer": "local_attestation",
                "name": "code-review",
                "verdict": "reviewed",
                "reviewed_sha": "a" * 40,
                "freshness": "fresh",
            }
        ],
    }
    if posture is not None:
        row["review_posture"] = posture
    return row


def test_posture_refusal_refuses_a_fresh_row_that_names_no_rung():
    """A fresh recompute with no posture proves the producer binary predates
    the field; the refusal names the upgrade."""
    from fno.pr._coverage_gate import posture_refusal

    refusal = posture_refusal(_covered_row(None), recomputed=True)
    assert refusal is not None
    assert "unknown" in refusal
    assert "fno doctor update" in refusal


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
