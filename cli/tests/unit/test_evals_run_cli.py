"""`fno doctor evals run` CLI exit-code contract (no spawn / no worktree needed)."""
from __future__ import annotations

from pathlib import Path

from typer.testing import CliRunner

from fno.evals.cli import evals_app

runner = CliRunner()


def _bank(tmp_path: Path, name: str, body: str) -> Path:
    d = tmp_path / "bank"
    d.mkdir(exist_ok=True)
    (d / name).write_text(body, encoding="utf-8")
    return d


def test_invalid_bank_task_exits_2(tmp_path: Path) -> None:
    d = _bank(tmp_path, "bad.yaml", "id: b\ntier: regression\ngrade: []\n")
    res = runner.invoke(evals_app, ["run", "--bank", str(d)])
    assert res.exit_code == 2


def test_prompt_task_defaults_provider(tmp_path: Path, monkeypatch) -> None:
    """codex P2: a prompt-bearing task with no --provider defaults to claude so
    the headless spawn is never provider-less. Assert run_task gets a provider."""
    import fno.evals.runner as runner_mod

    seen: dict = {}

    def fake_run_task(task, *, repeat, repo_root, worker_provider=None, **kw):
        seen["provider"] = worker_provider
        return [runner_mod.RunResult(task.id, task.tier, True, "", 0.0, 0)]

    # run_command does `from fno.evals.runner import run_task, sweep_orphans` at
    # call time, so patching the source module is what the import resolves.
    monkeypatch.setattr(runner_mod, "run_task", fake_run_task)
    monkeypatch.setattr(runner_mod, "sweep_orphans", lambda root: 0)

    d = _bank(tmp_path, "cap.yaml",
              "id: cap\ntier: capability\nprompt: do it\ngrade:\n  - {kind: exit, command: 'true'}\n")
    res = runner.invoke(evals_app, ["run", "--bank", str(d)])
    assert res.exit_code == 0
    assert seen.get("provider") == "claude"
    assert "defaulting" in res.stdout


# --- variant axis ---

def test_bad_variant_name_exits_1_before_bank_load(tmp_path: Path) -> None:
    # The bank dir does not exist: reaching "no bank" would mean validation
    # never ran; the variant refusal must come first.
    res = runner.invoke(evals_app, ["run", "--bank", str(tmp_path / "nope"),
                                    "--variant", "round2"])
    assert res.exit_code == 1
    out = res.stdout + (res.stderr or "")
    assert "must be 'baseline' or 'v<N>', got 'round2'" in out


def test_variant_flags_pass_through_to_run_task(tmp_path: Path, monkeypatch) -> None:
    import fno.evals.runner as runner_mod

    seen: dict = {}

    def fake_run_task(task, *, repeat, repo_root, worker_provider=None,
                      variant="baseline", variant_ref=None, **kw):
        seen["variant"] = variant
        seen["variant_ref"] = variant_ref
        return [runner_mod.RunResult(task.id, task.tier, True, "", 0.0, 0, variant)]

    monkeypatch.setattr(runner_mod, "run_task", fake_run_task)
    monkeypatch.setattr(runner_mod, "sweep_orphans", lambda root: 0)

    d = _bank(tmp_path, "a.yaml",
              "id: a\ntier: regression\ngrade:\n  - {kind: exit, command: 'true'}\n")
    res = runner.invoke(evals_app, ["run", "--bank", str(d),
                                    "--variant", "v2", "--ref", "HEAD"])
    assert res.exit_code == 0
    assert seen == {"variant": "v2", "variant_ref": "HEAD"}
    assert "a [v2]" in res.stdout  # the summary line names the round


# --- lane / cohort flags (x-fd52) ---


def test_lane_and_cohort_flags_pass_through_to_run_task(tmp_path: Path, monkeypatch) -> None:
    import fno.evals.bank as bank_mod
    import fno.evals.runner as runner_mod
    from types import SimpleNamespace

    seen: dict = {}
    monkeypatch.setattr(
        bank_mod, "resolve_lane",
        lambda name, **kw: SimpleNamespace(
            name=name, harness="codex", model="gpt-6-astra", route="", account="", effort="high",
        ),
    )

    def fake_run_task(task, *, repeat, repo_root, worker_provider=None, **kw):
        seen["lane"] = kw.get("lane")
        seen["experiment_id"] = kw.get("experiment_id")
        seen["provider"] = worker_provider
        return [runner_mod.RunResult(task.id, task.tier, True, "", 0.0, 0)]

    monkeypatch.setattr(runner_mod, "run_task", fake_run_task)
    monkeypatch.setattr(runner_mod, "sweep_orphans", lambda root: 0)

    d = _bank(tmp_path, "cap.yaml",
              "id: cap\ntier: capability\nprompt: do it\ngrade:\n  - {kind: exit, command: 'true'}\n")
    res = runner.invoke(evals_app, ["run", "--bank", str(d), "--lane", "astra-high",
                                    "--cohort", "cohort-a"])
    assert res.exit_code == 0
    assert seen["lane"].name == "astra-high"
    assert seen["experiment_id"] == "cohort-a"
    # A lane is a complete coordinate; the provider default must not override it.
    assert seen["provider"] is None
