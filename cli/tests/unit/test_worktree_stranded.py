"""The three-way stranded-worktree classifier.

`classify()` is pure, so the fixture table drives it directly with no git or
subprocess involved. The one thing that must NOT be pure-fixture-tested is
the unsound-probe regression: a prior sweep proved that a branch existing on
origin at an OLDER sha reads as healthy under a name-existence check, so
that one drives a real temp git repo the same way the gate corpus does,
exercising the actual `wt_unpushed_count` shell function classify()'s
git input comes from.
"""
import subprocess
from pathlib import Path

import pytest

from fno.rust_binary import VerbUnavailable
from fno.worktree_stranded import (
    ABANDONED,
    CLEAN,
    LIVE,
    PR_OPEN,
    SHIPPED,
    STRANDED,
    UNKNOWN,
    Row,
    _unpushed_batch,
    apply_sweep,
    classify,
)


@pytest.fixture(autouse=True)
def _reset_unpushed_cache(monkeypatch):
    """The unpushed-probe cache is module-global: a failed fetch in one test
    (a probe on a non-repo, a timed-out fetch) memoizes stale into every
    later probe in the same worker, so which test poisons which is decided
    by the xdist partition, not the file order. Pin both flags per test;
    teardown restores whatever a production call set mid-test."""
    import fno.worktree_stranded as _ws

    monkeypatch.setattr(_ws, "_remote_refs_fresh", False)
    monkeypatch.setattr(_ws, "_remote_refs_stale", False)


def _base_kwargs(**overrides) -> dict:
    kwargs = dict(
        path="/wt/x-abcd",
        branch="feature/x-abcd",
        unpushed=3,
        unpushed_ok=True,
        node="x-abcd",
        node_entry={"status": "ready"},
        graph_ok=True,
        registry_status=None,
        registry_ok=True,
    )
    kwargs.update(overrides)
    return kwargs


# --- the seven classes, fixture table ---------------------------------


@pytest.mark.parametrize(
    "overrides,expected",
    [
        pytest.param({"unpushed": 0}, CLEAN, id="clean-zero-unpushed"),
        pytest.param({"node": None, "node_entry": None}, UNKNOWN, id="unknown-node-unresolved"),
        pytest.param(
            {"node_entry": {"status": "done"}}, SHIPPED, id="shipped-done-status"
        ),
        pytest.param(
            {"node_entry": {"status": "superseded"}}, ABANDONED, id="abandoned-superseded"
        ),
        pytest.param({"registry_status": "busy"}, LIVE, id="live-busy"),
        pytest.param(
            {"node_entry": {"status": "ready", "pr_number": 42}}, PR_OPEN, id="pr-open"
        ),
        pytest.param({}, STRANDED, id="stranded-otherwise"),
    ],
)
def test_classify_seven_classes(overrides, expected):
    row = classify(**_base_kwargs(**overrides))
    assert row.klass == expected


def test_live_outranks_terminal_node_status():
    """A code-review finding: a node auto-transitioning to a terminal status
    WHILE a worker is still mid-commit in that worktree must still read
    LIVE, not SHIPPED/ABANDONED - the same LIVE-outranks-everything-below-it
    principle that also holds for PR_OPEN. Reading
    this as ABANDONED would have the SessionStart hook suggest `worktree
    cleanup --merged` on a genuinely live session."""
    row = classify(**_base_kwargs(registry_status="busy", node_entry={"status": "done"}))
    assert row.klass == LIVE


# --- both fail-open paths -----------------------------------------------


@pytest.mark.parametrize(
    "overrides",
    [
        pytest.param({"unpushed_ok": False}, id="git-read-failed"),
        pytest.param({"graph_ok": False}, id="graph-read-failed"),
        pytest.param({"registry_ok": False}, id="fleet-read-failed"),
    ],
)
def test_any_failed_input_is_unknown(overrides):
    row = classify(**_base_kwargs(**overrides))
    assert row.klass == UNKNOWN
    assert "read failed" in row.facts["reason"]


# --- UNKNOWN never pushes -------------------------------------------------


def test_apply_sweep_only_acts_on_stranded_and_unknown(monkeypatch):
    calls = []

    def fake_run(args, **kwargs):
        calls.append(args)
        return subprocess.CompletedProcess(args, 0, stdout="{}", stderr="")

    monkeypatch.setattr("fno.worktree_stranded.subprocess.run", fake_run)

    rows = [
        Row(CLEAN, "x-1", 0, "now", {"path": "/a", "branch": "b1"}),
        Row(SHIPPED, "x-2", 5, "now", {"path": "/b", "branch": "b2"}),
        Row(LIVE, "x-3", 5, "now", {"path": "/c", "branch": "b3"}),
        Row(UNKNOWN, "x-4", 5, "now", {"path": "/d", "branch": "b4", "reason": "r"}),
    ]
    outcomes = apply_sweep(rows)

    assert len(outcomes) == 1
    assert outcomes[0]["node"] == "x-4"
    assert not any("push" in args for args in calls)


def test_apply_sweep_wake_false_still_records_unknown_but_skips_stranded(monkeypatch):
    """A code-review finding: config.recovery.watchdog="report" must still
    classify (so counts stay honest) but never push or file - the same
    wake vs report split the fleet watchdog leg draws at apply_verdict.
    Recording UNKNOWN is reporting, not acting, so it stays unconditional."""
    calls = []

    def fake_run(args, **kwargs):
        calls.append(args)
        return subprocess.CompletedProcess(args, 0, stdout="{}", stderr="")

    monkeypatch.setattr("fno.worktree_stranded.subprocess.run", fake_run)

    rows = [
        Row(STRANDED, "x-5", 5, "now", {"path": "/e", "branch": "b5"}),
        Row(UNKNOWN, "x-4", 5, "now", {"path": "/d", "branch": "b4", "reason": "r"}),
    ]
    outcomes = apply_sweep(rows, wake=False)

    assert len(outcomes) == 1
    assert outcomes[0]["node"] == "x-4"
    assert not any("push" in args for args in calls)


# --- regression: the unsound name-existence probe -----------------------


def _git(cwd: Path, *args: str) -> str:
    r = subprocess.run(["git", *args], cwd=cwd, capture_output=True, text=True)
    assert r.returncode == 0, f"git {' '.join(args)} failed: {r.stderr}"
    return r.stdout


def test_remote_branch_at_older_sha_still_reads_unpushed(tmp_path):
    """A branch existing on origin is not proof the local HEAD's commits
    are on it. A branch-name-exists probe reads this worktree as healthy;
    the sound probe (rev-list --not --remotes, what classify()'s unpushed
    input actually comes from) must not."""
    remote = tmp_path / "remote.git"
    subprocess.run(["git", "init", "--bare", "-q", str(remote)], check=True)

    work = tmp_path / "work"
    work.mkdir()
    _git(work, "init", "-q", "-b", "feature/x-ab12")
    _git(work, "config", "user.email", "t@t.co")
    _git(work, "config", "user.name", "t")
    _git(work, "remote", "add", "origin", str(remote))
    (work / "f.txt").write_text("one\n")
    _git(work, "add", "f.txt")
    _git(work, "commit", "-q", "-m", "first")
    _git(work, "push", "-q", "origin", "HEAD:refs/heads/feature/x-ab12")

    # Local moves ahead; origin/feature/x-ab12 stays at the older sha.
    (work / "f.txt").write_text("two\n")
    _git(work, "add", "f.txt")
    _git(work, "commit", "-q", "-m", "second, never pushed")

    counts = _unpushed_batch([("feature/x-ab12", str(work))])
    count, ok, age, has_remote = counts[str(work)]
    assert ok is True
    assert count > 0
    assert age != ""
    assert has_remote is True


def test_unpushed_batch_missing_path_reads_unverifiable(tmp_path):
    """A code-review finding: a path that vanished mid-sweep (removed by
    something else while the batch was running - a real race, not a
    hypothetical) used to fail toward the safe COUNT (1, unpushed) but
    silently keep ok=True, since `wt_unpushed_count` printed no "not
    verifiable" text for that case. Constraint 4 needs the read itself
    marked unreliable, not just the count nudged safe - a genuinely
    positive read and a coerced fallback must not look the same to
    classify()'s fail-open gate."""
    ghost = tmp_path / "does-not-exist"

    counts = _unpushed_batch([(None, str(ghost))])
    count, ok, _age, _has_remote = counts[str(ghost)]
    assert ok is False
    assert count == 1


# ── the stranded push's leak veto ───────────────────────────────────────────

def test_act_on_stranded_skips_push_when_scan_reports_a_leak(monkeypatch):
    """A worktree whose outbound commits carry a Claude session URL is never
    published: the push act records the refusal and the sweep stops."""
    calls = []

    def fake_verb_call(verb, payload, **kwargs):
        calls.append(verb)
        assert verb[:2] == ["pr-push", "--scan-only"]
        return {
            "leaks": [
                {
                    "sha": "0c6804bf17aa",
                    "lines": ["Claude-Session: claude.ai/code/session_01Ab"],
                }
            ]
        }

    monkeypatch.setattr("fno.worktree_stranded.verb_call", fake_verb_call)

    row = Row(STRANDED, "x-abcd", 3, "1 hour ago", {"path": "/wt/x-abcd", "branch": "feature/x-abcd"})
    outcome = act_on_stranded(row)

    assert outcome["stopped_at"] == "push"
    act = outcome["acts"][0]
    assert act["act"] == "push" and act["ok"] is False
    assert "session URL" in act["detail"]
    assert "0c6804bf17aa"[:12] in act["detail"]


def test_act_on_stranded_fails_closed_when_scanner_unavailable(monkeypatch):
    """No scanner means no push: recovery never publishes unscanned work."""

    def fake_verb_call(verb, payload, **kwargs):
        raise VerbUnavailable("the fno-agents binary was not found")

    monkeypatch.setattr("fno.worktree_stranded.verb_call", fake_verb_call)

    row = Row(STRANDED, "x-abcd", 3, "1 hour ago", {"path": "/wt/x-abcd", "branch": "feature/x-abcd"})
    outcome = act_on_stranded(row)

    assert outcome["stopped_at"] == "push"
    assert outcome["acts"][0]["ok"] is False
    assert "outbound scan unavailable" in outcome["acts"][0]["detail"]
