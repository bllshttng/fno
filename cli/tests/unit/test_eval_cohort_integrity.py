"""Cohort integrity: declared splits validated natively before any worker
call; tuning exports are train-only; held-out reporting is aggregate-only.
The load/validate/fold logic is native (fno-agents evals-attempt); these
tests pin the Python transport shapes around that door."""
from __future__ import annotations

import shlex
import subprocess
from pathlib import Path
from typing import Optional

from typer.testing import CliRunner

import fno.evals.bank as bank_mod
from fno.evals.bank import COHORTS_FILENAME, _cohort_door
from fno.evals.cli import evals_app

runner = CliRunner()


def _task_yaml(tid: str, tier: str = "regression") -> str:
    return f"id: {tid}\ntier: {tier}\ngrade:\n  - {{kind: exit, command: 'true'}}\n"


def _repo_with_bank(tmp_path: Path, tasks: list) -> tuple[Path, Path, str]:
    """A real git repo with evals/bank/<id>.yaml committed.

    *tasks* holds ids (regression tier) or (id, tier) pairs.
    Returns (repo_root, bank_dir, bank_current_rev)."""
    root = tmp_path / "repo"
    root.mkdir()
    def g(*args: str) -> None:
        subprocess.run(["git", *args], cwd=str(root), check=True, capture_output=True)
    g("init", "-q")
    g("config", "user.email", "t@t.t")
    g("config", "user.name", "t")
    bank = root / "evals" / "bank"
    bank.mkdir(parents=True)
    for entry in tasks:
        tid, tier = entry if isinstance(entry, tuple) else (entry, "regression")
        (bank / f"{tid}.yaml").write_text(_task_yaml(tid, tier), encoding="utf-8")
    g("add", "-A")
    g("commit", "-qm", "bank seed")
    rev = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=str(root), capture_output=True, text=True, check=True
    ).stdout.strip()
    return root, bank, rev


def _write_decl(bank: Path, *, train: list, validation: list, qualification: list,
                bank_rev: str = "HEAD") -> Path:
    path = bank / COHORTS_FILENAME
    lines = [f"bank_rev: {bank_rev}"]
    for role, ids in (("train", train), ("validation", validation),
                      ("qualification", qualification)):
        lines.append(f"{role}: [{', '.join(ids)}]")
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")
    return path


def _patch_repo_root(monkeypatch, root: Path) -> None:
    """Point resolve_canonical_repo_root at the tmp repo (the commands import
    it from fno.paths at call time)."""
    import fno.paths

    monkeypatch.setattr(fno.paths, "resolve_canonical_repo_root", lambda: root)


def _stub_gate(monkeypatch, door: Optional[tuple]) -> None:
    """Canned `_cohort_door` answer: (rc, payload); None = no declared split."""
    if door is not None:
        monkeypatch.setattr(bank_mod, "_cohort_door", lambda tail: door)


def _stub_run_pipeline(monkeypatch, calls: dict) -> None:
    """Stub the spawn-side of `evals run`: sweep_orphans + run_task."""
    import fno.evals.runner as runner_mod

    def fake_run_task(task, *, repeat, repo_root, **kw):
        calls["run"] = calls.get("run", 0) + 1
        from fno.evals.runner import RunResult

        return [RunResult(task.id, task.tier, True, "", 0.0, 0)]

    monkeypatch.setattr(runner_mod, "run_task", fake_run_task)
    monkeypatch.setattr(runner_mod, "sweep_orphans", lambda root: 0)


# --------------------------------------------------------------------------- #
# the gate fails closed without the binary
# --------------------------------------------------------------------------- #


def test_gate_fail_closed_when_binary_absent(tmp_path, monkeypatch) -> None:
    monkeypatch.setattr("fno.rust_binary.find_dev_binary", lambda: None)
    monkeypatch.setattr("fno.rust_binary.resolve_binary", lambda: None)
    root, bank, rev = _repo_with_bank(tmp_path, ["t1"])
    _write_decl(bank, train=["t1"], validation=[], qualification=[], bank_rev=rev)
    rc, payload = _cohort_door(["--cohorts-yaml", str(bank / COHORTS_FILENAME)])
    assert rc == 2 and "not found" in payload["error"]


# --------------------------------------------------------------------------- #
# run gating: validation fires BEFORE any worker call
# --------------------------------------------------------------------------- #

def test_run_refuses_door_refusal_before_any_spawn(tmp_path, monkeypatch) -> None:
    calls: dict = {}
    _stub_run_pipeline(monkeypatch, calls)
    root, bank, rev = _repo_with_bank(tmp_path, ["t1", "t2"])
    _write_decl(bank, train=["t1"], validation=["t1"], qualification=[])
    _patch_repo_root(monkeypatch, root)
    _stub_gate(monkeypatch, (0, {"ok": False,
                             "errors": ["task id 't1' is in both 'train' and 'validation'"]}))
    res = runner.invoke(evals_app, ["run", "--bank", str(bank), "--yes"])
    assert res.exit_code == 2
    assert "cohort split refused" in res.stdout + (res.stderr or "")
    assert calls.get("run", 0) == 0  # no worker call


def test_run_gate_reads_the_whole_bank_not_the_selection(tmp_path, monkeypatch) -> None:
    """A --tier run validates the declared split against every bank id.

    The split covers the bank, so a selection that omits a declared id must
    not starve the gate's membership check (a --tier regression run with a
    capability-bearing declaration used to exit 2 on every invocation)."""
    import json as _json

    calls: dict = {}
    door = (0, {"ok": True, "bank_unchanged": True})
    _stub_run_pipeline(monkeypatch, calls)

    def capture(tail):
        calls["tail"] = tail
        return door

    monkeypatch.setattr(bank_mod, "_cohort_door", capture)
    root, bank, rev = _repo_with_bank(tmp_path, [("c1", "capability"), "r1"])
    _write_decl(bank, train=["c1"], validation=[], qualification=[], bank_rev=rev)
    _patch_repo_root(monkeypatch, root)
    res = runner.invoke(evals_app, ["run", "--bank", str(bank), "--tier", "regression", "--yes"])
    assert res.exit_code == 0, res.stdout + (res.stderr or "")
    assert calls.get("run", 0) == 1  # the selected task ran
    known = _json.loads(calls["tail"][calls["tail"].index("--known-ids") + 1])
    assert sorted(known) == ["c1", "r1"]  # the whole bank, not the selection


# --------------------------------------------------------------------------- #
# tuning export: train-only, refused without a current declared split
# --------------------------------------------------------------------------- #

def _fake_binary(tmp_path: Path, stdout: str, *, exit_code: int = 0) -> str:
    """A stub fno-agents binary that prints canned output and exits."""
    script = tmp_path / "fake-agents.sh"
    script.write_text(
        "#!/bin/sh\nprintf '%s' " + shlex.quote(stdout) + f"\nexit {exit_code}\n",
        encoding="utf-8",
    )
    script.chmod(0o755)
    return str(script)


def test_export_requires_a_declared_split(tmp_path, monkeypatch) -> None:
    root, bank, _rev = _repo_with_bank(tmp_path, ["t1"])
    _patch_repo_root(monkeypatch, root)
    out = tmp_path / "tuning.jsonl"
    res = runner.invoke(evals_app, ["export", "--bank", str(bank),
                                    "--out", str(out)])
    assert res.exit_code == 1
    assert "no declared cohort split" in res.stdout + (res.stderr or "")
    assert not out.exists()


# --------------------------------------------------------------------------- #
# qualify: the allowed aggregate projection, folded natively
# --------------------------------------------------------------------------- #


def test_qualify_maps_door_failure_to_exit_2(tmp_path, monkeypatch) -> None:
    root, bank, rev = _repo_with_bank(tmp_path, ["q1"])
    _write_decl(bank, train=[], validation=[], qualification=["q1"], bank_rev=rev)
    _patch_repo_root(monkeypatch, root)
    fake = _fake_binary(tmp_path, "{}", exit_code=1)
    monkeypatch.setattr("fno.rust_binary.find_dev_binary", lambda: fake)
    monkeypatch.setattr("fno.rust_binary.resolve_binary", lambda: fake)
    res = runner.invoke(evals_app, ["qualify", "--bank", str(bank)])
    assert res.exit_code == 2
