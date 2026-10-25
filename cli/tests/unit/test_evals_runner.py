"""Runner + grading + history (US2): AC1-HP, AC3-ERR, AC5-FR.

Every test injects a fake spawn - no real model, no money. A real git repo is
built in tmp so the disposable-worktree lifecycle is exercised for real.
"""
from __future__ import annotations

import subprocess
from pathlib import Path

import pytest

from fno.evals import history as _history
from fno.evals.bank import GradeCheck, TaskSpec
from fno.evals.grading import grade
import fno.evals.runner as _runner
from fno.evals.runner import SpawnResult, run_task


def _rows(path):
    """Read back written history rows (the fold lives native; tests read JSONL)."""
    import json as _json
    if not path.exists():
        return []
    return [_json.loads(line) for line in path.read_text().splitlines() if line.strip()]


# --------------------------------------------------------------------------- #
# helpers
# --------------------------------------------------------------------------- #

@pytest.fixture(autouse=True)
def _isolate_worktree_base(tmp_path, monkeypatch):
    """Keep disposable eval worktrees inside tmp, never the real worktree base."""
    base = tmp_path / "wtbase"
    base.mkdir()
    monkeypatch.setattr("fno.worktree_paths.worktree_base", lambda: base)


def _git_repo(tmp_path: Path) -> Path:
    root = tmp_path / "repo"
    root.mkdir()
    def g(*args: str) -> None:
        subprocess.run(["git", *args], cwd=str(root), check=True, capture_output=True)
    g("init", "-q")
    g("config", "user.email", "t@t.t")
    g("config", "user.name", "t")
    (root / "seed.txt").write_text("hello world\n", encoding="utf-8")
    g("add", "-A")
    g("commit", "-qm", "seed")
    return root


def _task(**kw) -> TaskSpec:
    base = dict(id="t", tier="regression", grade=[GradeCheck("exit", command="true")])
    base.update(kw)
    return TaskSpec(**base)  # type: ignore[arg-type]


def _row(**kw):
    """A lane row in the shape bank.resolve_lane answers: the inventory
    field vocabulary as a SimpleNamespace (the fold lives native)."""
    from types import SimpleNamespace
    base = dict(name="", harness="", model="", route="", account="", effort="")
    base.update(kw)
    return SimpleNamespace(**base)


# --------------------------------------------------------------------------- #
# grading (pure, no worktree)
# --------------------------------------------------------------------------- #


def test_grade_fail_reports_first_reason(tmp_path: Path) -> None:
    task = _task(grade=[
        GradeCheck("exit", command="true"),
        GradeCheck("file-exists", path="missing.txt"),
    ])
    out = grade(task, tmp_path)
    assert not out.passed
    assert "missing.txt" in out.reason


# --------------------------------------------------------------------------- #
# runner - AC1-HP
# --------------------------------------------------------------------------- #

def test_run_grade_only_task_appends_history_and_removes_worktree(tmp_path: Path) -> None:
    root = _git_repo(tmp_path)
    hp = tmp_path / "hist.jsonl"
    task = _task(grade=[GradeCheck("file-exists", path="seed.txt")])  # seed exists in fixture

    before = _worktree_count(root)
    results = run_task(task, repeat=1, repo_root=root, history_path=hp,
                       spawn=_never_called_spawn)
    assert results[0].passed
    rows = _rows(hp)
    assert len(rows) == 1
    assert rows[0]["pass"] is True and rows[0]["tier"] == "regression"
    assert rows[0]["variant"] == "baseline"  # default run records the round
    assert rows[0]["bank_rev"]  # HEAD sha recorded
    assert _worktree_count(root) == before  # worktree removed after grading


def test_sweep_spares_a_tree_whose_run_is_alive(tmp_path: Path) -> None:
    # Two concurrent `evals run` streams: each run's sweep used to delete the
    # other's live tree, and its worker died on a missing cwd.
    import os
    from fno.worktree_paths import worktree_base

    root = _git_repo(tmp_path)
    finished = subprocess.Popen(["true"])
    finished.wait()
    live = worktree_base() / "evals" / f"t-{os.getpid()}-1"
    dead = worktree_base() / "evals" / f"t-{finished.pid}-2"
    for tree in (live, dead):
        subprocess.run(["git", "worktree", "add", "--detach", str(tree), "HEAD"],
                       cwd=str(root), check=True, capture_output=True)

    assert _runner.sweep_orphans(root) == 1
    assert live.exists() and not dead.exists()


def test_config_disabled_skips_the_default_spawn(tmp_path: Path, monkeypatch) -> None:
    """x-aaaf wave 2 (AC4-HP): config.evals.enabled=false stops the REAL
    default spawn (no `spawn=` injected) - it must never reach _default_spawn."""
    root = _git_repo(tmp_path)
    hp = tmp_path / "hist.jsonl"
    monkeypatch.setattr(_runner, "evals_enabled", lambda: False)

    def _boom(*a, **kw):
        raise AssertionError("_default_spawn must not run when the gate is off")

    monkeypatch.setattr(_runner, "_default_spawn", _boom)

    task = _task(prompt="x", grade=[GradeCheck("file-exists", path="made.txt")])
    results = run_task(task, repeat=1, repo_root=root, history_path=hp)
    assert not results[0].passed
    assert "config.evals.enabled is false" in results[0].reason


def test_default_spawn_trial_leaves_no_claude_project_dir(tmp_path: Path, monkeypatch) -> None:
    """A real trial's claude projects dir goes after grading; others stay."""
    root = _git_repo(tmp_path)
    projects = tmp_path / "cfg" / "projects"
    keep = projects / "-Users-me-repo"
    keep.mkdir(parents=True)
    monkeypatch.setenv("CLAUDE_CONFIG_DIR", str(tmp_path / "cfg"))
    monkeypatch.setattr(_runner, "evals_enabled", lambda: True)
    seen: list[Path] = []

    def _fake(prompt, workdir, timeout_s, **kw):
        trial = projects / str(workdir).replace("/", "-").replace(".", "-")
        trial.mkdir()
        (trial / "s.jsonl").write_text("{}\n")
        seen.append(trial)
        return SpawnResult(True, worker_name="ev-th-evals-1-2")

    monkeypatch.setattr(_runner, "_default_spawn", _fake)
    run_task(_task(prompt="x"), repeat=1, repo_root=root, history_path=tmp_path / "h.jsonl")
    assert seen and not seen[0].exists()
    assert keep.exists()
    # An account lane's trial lands under that account's config dir.
    acct = tmp_path / "acct" / "projects" / "-w-t"
    acct.mkdir(parents=True)
    overlay = _runner._account_env.AccountOverlay("a1", {"CLAUDE_CONFIG_DIR": str(tmp_path / "acct")}, "own-dir")
    monkeypatch.setattr(_runner._account_env, "resolve_account_overlay", lambda account: overlay)
    _runner._forget_trial_transcripts(Path("/w/t"), type("Lane", (), {"account": "a1"})())
    assert not acct.exists() and keep.exists()


# AC3-ERR: spawn failure is a graded fail, remaining repeats still run.
def test_spawn_failure_is_graded_fail_not_crash(tmp_path: Path) -> None:
    root = _git_repo(tmp_path)
    hp = tmp_path / "hist.jsonl"

    def spawn(prompt: str, workdir: Path, timeout_s: int) -> SpawnResult:
        return SpawnResult(False, "spawn exit 1: provider down")

    task = _task(prompt="x", grade=[GradeCheck("file-exists", path="made.txt")])
    results = run_task(task, repeat=3, repo_root=root, history_path=hp, spawn=spawn)
    assert len(results) == 3
    assert all(not r.passed for r in results)
    assert all("provider down" in r.reason for r in results)
    assert len(_rows(hp)) == 3  # every run recorded


# --------------------------------------------------------------------------- #
# variant axis: a scored round names baseline or v<N> and checks out its ref
# --------------------------------------------------------------------------- #

def _branch(root: Path, name: str, filename: str) -> None:
    def g(*args: str) -> None:
        subprocess.run(["git", *args], cwd=str(root), check=True, capture_output=True)
    g("checkout", "-b", name)
    (root / filename).write_text("x\n", encoding="utf-8")
    g("add", "-A")
    g("commit", "-qm", filename)
    g("checkout", "-")


def _git_sha(root: Path, ref: str) -> str:
    proc = subprocess.run(["git", "rev-parse", ref], cwd=str(root),
                          capture_output=True, text=True, check=True)
    return proc.stdout.strip()


def test_variant_run_checks_out_variant_ref_and_records_row(tmp_path: Path) -> None:
    root = _git_repo(tmp_path)
    _branch(root, "v1-work", "extra.txt")
    hp = tmp_path / "hist.jsonl"
    task = _task(grade=[GradeCheck("file-exists", path="extra.txt")])
    before = _worktree_count(root)

    results = run_task(task, repeat=1, repo_root=root, history_path=hp,
                       spawn=_never_called_spawn, variant="v1", variant_ref="v1-work")
    assert results[0].passed
    assert results[0].variant == "v1"
    rows = _rows(hp)
    assert rows[0]["variant"] == "v1"
    assert rows[0]["bank_rev"] == _git_sha(root, "v1-work")
    assert _worktree_count(root) == before  # worktree removed after grading


def test_variant_bad_name_refuses_before_any_worktree(tmp_path: Path) -> None:
    root = _git_repo(tmp_path)
    hp = tmp_path / "hist.jsonl"
    before = _worktree_count(root)

    with pytest.raises(ValueError, match=r"baseline\|v<N>"):
        run_task(_task(), repeat=1, repo_root=root, history_path=hp,
                 spawn=_never_called_spawn, variant="round2")
    with pytest.raises(ValueError, match="baseline"):
        run_task(_task(), repeat=1, repo_root=root, history_path=hp,
                 spawn=_never_called_spawn, variant="baseline", variant_ref="x")
    assert len(_rows(hp)) == 0  # nothing recorded
    assert _worktree_count(root) == before          # no worktree made


# --------------------------------------------------------------------------- #
# attempt identity + native verdict (x-ecda AC1-HP, AC1-EDGE)
# --------------------------------------------------------------------------- #

def test_attempt_row_carries_identity_obs_and_native_verdict(tmp_path, monkeypatch) -> None:
    """Every attempt persists with a unique identity and structured
    observations; the native verdict labels the row (here canned)."""
    root = _git_repo(tmp_path)
    hp = tmp_path / "hist.jsonl"
    monkeypatch.setattr(_runner, "_native_verdict",
                        lambda row: {"status": "graded", "retryable": False, "graded": True})
    task = _task(grade=[GradeCheck("file-exists", path="seed.txt")])
    results = run_task(task, repeat=2, repo_root=root, history_path=hp,
                       spawn=_never_called_spawn)
    assert [r.status for r in results] == ["graded", "graded"]
    rows = _rows(hp)
    assert len(rows) == 2
    assert len({r["attempt_id"] for r in rows}) == 2  # unique per attempt
    assert rows[0]["run_id"] == rows[1]["run_id"]  # same sweep
    assert all(r["attempt_index"] == 0 for r in rows)
    assert rows[0]["obs"] == {
        "fixture_prepared": True, "worker_required": False,
        "grader_ran": True, "grader_passed": True,
    }
    assert rows[0]["status"] == "graded" and rows[0]["retryable"] is False


def test_infrastructure_failure_retries_and_persists_both_attempts(tmp_path, monkeypatch) -> None:
    """AC1-HP: a fixture failure is retryable infrastructure evidence - the
    retry runs, both attempts stay attributable, neither consumes the slot."""
    root = _git_repo(tmp_path)
    hp = tmp_path / "hist.jsonl"
    monkeypatch.setattr(_runner, "_native_verdict",
                        lambda row: {"status": "infrastructure", "retryable": True})
    results = run_task(_task(), repeat=1, repo_root=root, history_path=hp,
                       spawn=_never_called_spawn, variant="v1", variant_ref="no-such-ref",
                       max_retries=1)
    assert [r.attempt_index for r in results] == [0, 1]
    rows = _rows(hp)
    assert len(rows) == 2
    assert all(r["obs"]["fixture_prepared"] is False for r in rows)
    assert all(r["status"] == "infrastructure" for r in rows)
    assert {r["attempt_id"] for r in rows} == {r.attempt_id for r in results}


def test_graded_failure_never_retries(tmp_path, monkeypatch) -> None:
    """A valid task grade (pass or fail) is final: max_retries never repeats it."""
    root = _git_repo(tmp_path)
    hp = tmp_path / "hist.jsonl"
    monkeypatch.setattr(_runner, "_native_verdict",
                        lambda row: {"status": "graded", "retryable": False, "graded": False})
    task = _task(grade=[GradeCheck("file-exists", path="missing.txt")])
    results = run_task(task, repeat=1, repo_root=root, history_path=hp,
                       spawn=_never_called_spawn, max_retries=3)
    assert len(results) == 1
    assert results[0].status == "graded" and not results[0].passed
    assert len(_rows(hp)) == 1


def test_history_append_attempt_refuses_unattributable_rows(tmp_path) -> None:
    hp = tmp_path / "h.jsonl"
    with pytest.raises(ValueError, match="attempt row"):
        _history.append_attempt(hp, {"task_id": "a", "pass": True})
    row = {"attempt_id": "x", "run_id": "r", "obs": {"fixture_prepared": True}}
    _history.append_attempt(hp, row)  # valid identity + evidence: persists
    assert len(_rows(hp)) == 1


# --------------------------------------------------------------------------- #
# small helpers
# --------------------------------------------------------------------------- #

def _never_called_spawn(prompt: str, workdir: Path, timeout_s: int) -> SpawnResult:
    raise AssertionError("grade-only task must not spawn a worker")


# --------------------------------------------------------------------------- #
# lane requested/observed evidence - AC1-HP, AC1-EDGE, AC1-ERR
# --------------------------------------------------------------------------- #

_LANE = _row(name="astra-high", harness="codex", model="gpt-6-astra", effort="high")


def test_lane_hp_records_requested_and_observed_configuration(tmp_path: Path) -> None:
    root = _git_repo(tmp_path)
    hp = tmp_path / "hist.jsonl"

    def spawn(prompt: str, workdir: Path, timeout_s: int) -> SpawnResult:
        (workdir / "made.txt").write_text("ok\n", encoding="utf-8")
        return SpawnResult(True, worker_name="eval-worker-1")

    def observe(lane, workdir, started_wall, *, attempted, spawned, worker_name):
        assert worker_name == "eval-worker-1"
        assert attempted and spawned
        return {"requested_lane": "astra-high", "requested_harness": "codex",
                "requested_model": "gpt-6-astra", "requested_effort": "high",
                "observed_harness": "codex", "observed_model": "gpt-6-astra",
                "observed_model_basis": "verified", "observed_effort": "high",
                "observed_session_id": "sess-1",
                "substituted": False, "lane_status": "ok",
                "usage": None, "usage_source": None}

    task = _task(prompt="do the thing", grade=[GradeCheck("file-exists", path="made.txt")])
    run_task(task, repeat=1, repo_root=root, history_path=hp, spawn=spawn,
             lane=_LANE, experiment_id="cohort-a", observe=observe)
    row = _rows(hp)[0]
    assert row["requested_lane"] == "astra-high"
    assert row["requested_harness"] == "codex"
    assert row["requested_model"] == "gpt-6-astra"
    assert row["observed_harness"] == "codex"
    assert row["observed_model"] == "gpt-6-astra"
    assert row["observed_session_id"] == "sess-1"
    assert row["lane_status"] == "ok"
    assert row["substituted"] is False
    assert row["experiment_id"] == "cohort-a"


def test_lane_err_unavailable_never_grades_a_substitute_as_requested(tmp_path: Path) -> None:
    """AC1-ERR: a spawn refusal records unavailable with no observed model -
    the run is graded a fail, never a pass under the requested lane."""
    root = _git_repo(tmp_path)
    hp = tmp_path / "hist.jsonl"

    def spawn(prompt: str, workdir: Path, timeout_s: int) -> SpawnResult:
        return SpawnResult(False, "spawn exit 2: account lacks model")

    seen: dict = {}

    def observe(lane, workdir, started_wall, *, attempted, spawned, worker_name):
        seen.update(attempted=attempted, spawned=spawned)
        return {"requested_lane": "astra-high", "requested_harness": "codex",
                "requested_model": "gpt-6-astra", "requested_effort": "high",
                "observed_harness": None, "observed_model": None,
                "substituted": False, "lane_status": "unavailable",
                "usage": None, "usage_source": None}

    task = _task(prompt="do the thing", grade=[GradeCheck("file-exists", path="made.txt")])
    results = run_task(task, repeat=1, repo_root=root, history_path=hp, spawn=spawn,
                       lane=_LANE, observe=observe)
    assert not results[0].passed
    assert seen == {"attempted": True, "spawned": False}, "the door still learns the spawn refused"
    row = _rows(hp)[0]
    assert row["lane_status"] == "unavailable"
    assert row["observed_model"] is None
    assert row["requested_model"] == "gpt-6-astra"


def test_lane_successful_headless_spawn_with_no_observable_identity_is_unverified(tmp_path, monkeypatch) -> None:
    """A spawned attempt whose store holds no transcript for the attempt
    workdir reads unverified, never a false unavailable that reads like the
    lane refused capacity. An unreachable door reads the same way, never a
    crash and never a guessed status (AC10-EDGE)."""
    root = _git_repo(tmp_path)
    hp = tmp_path / "hist.jsonl"

    def spawn(prompt: str, workdir: Path, timeout_s: int) -> SpawnResult:
        (workdir / "made.txt").write_text("ok\n", encoding="utf-8")
        return SpawnResult(True, worker_name="eval-worker-3")

    def observe(lane, workdir, started_wall, *, attempted, spawned, worker_name):
        # The door's own unverified answer: a spawned attempt whose store holds
        # no transcript for the attempt workdir.
        return {"requested_lane": "astra-high", "requested_harness": "codex",
                "requested_model": "gpt-6-astra", "requested_effort": "high",
                "observed_model": None, "observed_harness": None,
                "lane_status": "unverified",
                "lane_reason": "no claude transcript for this workdir",
                "usage": None}

    task = _task(prompt="do the thing", grade=[GradeCheck("file-exists", path="made.txt")])
    results = run_task(task, repeat=1, repo_root=root, history_path=hp, spawn=spawn,
                       lane=_LANE, observe=observe)
    assert results[0].passed
    row = _rows(hp)[0]
    assert row["lane_status"] == "unverified"
    assert row["requested_lane"] == "astra-high"
    assert row["observed_model"] is None

    # Second phase: the door itself unreachable degrades the row the same way.
    def fake_command_call(command, payload, **kwargs):
        from fno.rust_binary import CommandUnavailable

        raise CommandUnavailable("binary missing")

    monkeypatch.setattr("fno.rust_binary.command_call", fake_command_call)
    hp2 = tmp_path / "hist2.jsonl"
    results = run_task(task, repeat=1, repo_root=root, history_path=hp2, spawn=spawn, lane=_LANE)
    assert results[0].passed
    row = _rows(hp2)[0]
    assert row["lane_status"] == "unverified"
    assert row["lane_reason"] == "observe door unavailable"
    assert row["usage"] is None


def _worktree_count(root: Path) -> int:
    proc = subprocess.run(["git", "worktree", "list", "--porcelain"],
                          cwd=str(root), capture_output=True, text=True)
    return proc.stdout.count("worktree ")


def _patch_account_env(monkeypatch, registered_ids=()) -> None:
    """Stub the providers registry: `registered_ids` are known to by_id, and
    resolve only those; a known-but-unresolvable id and a crashing registry
    are reachable through the ids below."""
    import fno.agents.account_env as account_env

    class _Cfg:
        by_id = {i: object() for i in registered_ids}

    monkeypatch.setattr(account_env, "load_providers", lambda **_: _Cfg())

    def _resolve(account_id, **_):
        if account_id == "acct-broken":
            raise account_env.AccountResolutionError(
                "account 'acct-broken' config_dir does not exist; register a live login there first"
            )
        if account_id not in _Cfg.by_id:
            raise account_env.AccountResolutionError(f"account {account_id!r} is not registered")
        return object()

    monkeypatch.setattr(account_env, "resolve_account_overlay", _resolve)


def test_lane_account_rides_only_for_a_registered_account(monkeypatch) -> None:
    """A registered, resolvable lane account rides --account; an empty one
    rides nothing; both return no refusal and no drop note."""
    _patch_account_env(monkeypatch, registered_ids=["acct-ok"])
    registered = _row(name="a", harness="claude", model="m", route="v/m", account="acct-ok")
    empty = _row(name="c", harness="claude", model="m", route="v/m", account="")

    assert _runner._lane_account(registered) == ("acct-ok", "", "")
    assert _runner._lane_account(empty) == (None, "", "")


def test_lane_account_drops_only_an_unregistered_id_on_a_routed_lane(monkeypatch) -> None:
    """The zai case: an alias the registry does not hold drops --account on a
    routed lane (the seat form) with a drop note for the row. The same alias
    on a route-less lane refuses the spawn instead."""
    _patch_account_env(monkeypatch, registered_ids=[])
    routed = _row(name="z", harness="claude", model="m", route="zai/glm", account="zai")
    routeless = _row(name="n", harness="claude", model="m", route="", account="zai")

    flag, refusal, drop_note = _runner._lane_account(routed)
    assert flag is None and refusal == "" and "zai" in drop_note

    flag, refusal, drop_note = _runner._lane_account(routeless)
    assert flag is None and "not registered" in refusal and "fail closed" in refusal


def test_lane_account_fails_closed_on_every_other_refusal(monkeypatch) -> None:
    """A registered account whose login/config_dir is gone, and a crashing
    registry, refuse the spawn; neither may degrade to the seat form."""
    _patch_account_env(monkeypatch, registered_ids=["acct-broken"])
    broken = _row(name="b", harness="claude", model="m", route="v/m", account="acct-broken")
    flag, refusal, _ = _runner._lane_account(broken)
    assert flag is None and "acct-broken" in refusal

    import fno.agents.account_env as account_env

    def _boom(**_):
        raise ValueError("providers.toml is not valid TOML")

    monkeypatch.setattr(account_env, "load_providers", _boom)
    flag, refusal, _ = _runner._lane_account(broken)
    assert flag is None and "refused" in refusal


def test_default_spawn_refuses_closed_before_any_worker(monkeypatch, tmp_path) -> None:
    """The fail-closed refusal is a graded spawn failure, decided before the
    worker name is minted and before any subprocess runs."""
    _patch_account_env(monkeypatch, registered_ids=[])
    row = _row(name="n", harness="claude", model="m", route="", account="zai")
    got = _runner._default_spawn("p", tmp_path, 60, lane=row)
    assert not got.ok
    assert "not registered" in got.reason


def test_lane_row_shows_a_dropped_account(tmp_path) -> None:
    """A dropped account cannot read as a clean ok row: the row carries
    requested_account, account_dropped, the drop reason, and a lane_status
    that is not ok."""
    root = _git_repo(tmp_path)
    hp = tmp_path / "hist.jsonl"

    def spawn(prompt: str, workdir: Path, timeout_s: int) -> SpawnResult:
        (workdir / "made.txt").write_text("ok\n", encoding="utf-8")
        return SpawnResult(True, worker_name="w1",
                           lane_note="lane account 'zai' dropped; routed lane rides its route credential")

    def observe(lane, workdir, started_wall, *, attempted, spawned, worker_name):
        return {"requested_lane": "zai-flash", "requested_harness": "claude",
                "requested_model": "glm-5.3-flash[1m]", "observed_harness": "claude",
                "observed_model": "glm-5.3-flash", "substituted": False,
                "lane_status": "ok", "usage": None, "usage_source": None}

    lane = _row(name="zai-flash", harness="claude", model="glm-5.3-flash[1m]",
                        route="zai/glm-5.3-flash[1m]", account="zai")
    task = _task(prompt="do the thing", grade=[GradeCheck("file-exists", path="made.txt")])
    run_task(task, repeat=1, repo_root=root, history_path=hp, spawn=spawn,
             lane=lane, observe=observe)
    row = _rows(hp)[0]
    assert row["requested_account"] == "zai"
    assert row["account_dropped"] is True
    assert "dropped" in row["lane_reason"]
    assert row["lane_status"] == "account-dropped"


def test_lane_row_keeps_a_registered_account_clean(tmp_path) -> None:
    """A lane whose account rides shows no drop evidence at all."""
    root = _git_repo(tmp_path)
    hp = tmp_path / "hist.jsonl"

    def spawn(prompt: str, workdir: Path, timeout_s: int) -> SpawnResult:
        (workdir / "made.txt").write_text("ok\n", encoding="utf-8")
        return SpawnResult(True, worker_name="w2")

    def observe(lane, workdir, started_wall, *, attempted, spawned, worker_name):
        return {"requested_lane": "pinned", "observed_model": "m", "substituted": False,
                "lane_status": "ok", "usage": None, "usage_source": None}

    lane = _row(name="pinned", harness="claude", model="m", route="v/m", account="acct-ok")
    task = _task(prompt="do the thing", grade=[GradeCheck("file-exists", path="made.txt")])
    run_task(task, repeat=1, repo_root=root, history_path=hp, spawn=spawn,
             lane=lane, observe=observe)
    row = _rows(hp)[0]
    assert row["requested_account"] == "acct-ok"
    assert "account_dropped" not in row
    assert row["lane_status"] == "ok"
