"""CLI-wiring coverage for `fno doctor observer` (x-57a5, Task 7).

Sweep and replay are exercised with injected seams (fake corpus, fake gh, fake
spawn, no real worktree) so no test touches the network, real ~/.fno do state, or
spawns a session. Asserts the anti-silent contract (a state word + the right
events), the insufficient guard (no run_complete), replay's A2 rejection, the
replay tool-fault path, and the one hard failure: an isolation violation.
"""

from __future__ import annotations

import json
import subprocess

import pytest
import typer
from typer.testing import CliRunner

from fno.observer import cli

runner = CliRunner()


@pytest.fixture(autouse=True)
def _unpaused_loop_gate(monkeypatch):
    from fno import loops

    monkeypatch.setattr(loops, "loops_paused", lambda: False)


def _item(session_id, node_id, plan_path):
    return {
        "session_id": session_id,
        "graph_node_id": node_id,
        "plan_path": str(plan_path) if plan_path else None,
        "skill_id": "fno:blueprint",
        "skill_version": "abc1234",
        "method": "phase-proxy",
        "shipped": True,
        "termination_reason": "DonePRGreen",
        "judgeable": False,
        "outcome": None,
        "attribution_class": None,
    }


def _wire(monkeypatch, tmp_path, items):
    """Redirect events + digest into tmp; inject the corpus."""
    events_path = tmp_path / "events.jsonl"
    monkeypatch.setattr(cli, "_events_paths", lambda: [events_path])
    monkeypatch.setattr(cli, "_load_corpus", lambda skill, since: ({"items": items, "attributed": len(items)}, {}))
    import fno.paths as paths
    monkeypatch.setattr(paths, "observer_reports_dir", lambda *a, **k: tmp_path / "reports")
    return events_path


def _events(path):
    from tests._event_rows import event_rows

    return event_rows(path)


def _good_plan(tmp_path, name):
    p = tmp_path / name
    p.write_text(
        "---\ntitle: t\n---\n\n# Plan\n\n## Failure Modes\n\nboundaries\n\n"
        "## Execution Strategy\n\n```yaml\ntasks:\n- id: '1'\n  surface: ['a.py']\n```\n"
    )
    return p


# --------------------------------------------------------------------------- #
# help + A2 rejection
# --------------------------------------------------------------------------- #

def test_observer_dir_rejects_traversal_project_id(monkeypatch, tmp_path):
    """A caller-supplied project_id carrying path separators / traversal must
    not escape internal/<project>/ - it falls back to the configured id."""
    import fno.paths as paths

    class _S:
        class paths:
            observer_reports_dir = None

        class project:
            id = "safeproj"

        class obsidian:
            enabled = False
            vault = None

    monkeypatch.setattr(paths, "_settings", lambda: _S())
    monkeypatch.setattr(paths, "state_dir", lambda: tmp_path)
    d = paths.observer_reports_dir(project_id="../../etc")
    assert ".." not in str(d)
    assert d == tmp_path / "observer-reports" / "safeproj"


def test_replay_review_rejected_nonzero():
    r = runner.invoke(cli.observer_app, ["replay", "--skill", "review", "--corpus-item", "s1"])
    assert r.exit_code != 0
    assert "A2" in r.output


# --------------------------------------------------------------------------- #
# sweep
# --------------------------------------------------------------------------- #

def test_sweep_insufficient_emits_no_run_complete(monkeypatch, tmp_path):
    items = [_item(f"s{i}", f"x-{i}", _good_plan(tmp_path, f"p{i}.md")) for i in range(5)]
    events_path = _wire(monkeypatch, tmp_path, items)
    r = runner.invoke(cli.observer_app, ["sweep", "--skill", "blueprint"])
    assert r.exit_code == 0
    assert "insufficient" in r.output and "5" in r.output
    # AC1-UI: nothing to complete -> no terminal event
    assert not any(e["type"] == "skill_eval_run_complete" for e in _events(events_path))


def test_sweep_ok_emits_findings_run_complete_and_digest(monkeypatch, tmp_path):
    items = [_item(f"s{i}", f"x-{i}", _good_plan(tmp_path, f"p{i}.md")) for i in range(10)]
    events_path = _wire(monkeypatch, tmp_path, items)
    r = runner.invoke(cli.observer_app, ["sweep", "--skill", "blueprint"])
    assert r.exit_code == 0, r.output
    assert r.output.startswith("ok:")
    evs = _events(events_path)
    findings = [e for e in evs if e["type"] == "skill_eval_finding"]
    completes = [e for e in evs if e["type"] == "skill_eval_run_complete"]
    assert len(completes) == 1
    assert completes[0]["data"]["coverage_pct"] == 100
    assert findings and all(f["data"]["verdict"] in ("pass", "degraded", "fail") for f in findings)
    # digest written
    assert list((tmp_path / "reports").glob("blueprint-*.md"))


# --------------------------------------------------------------------------- #
# review sweep reads the attestation evidence
# --------------------------------------------------------------------------- #

def _wire_review(monkeypatch, tmp_path, items, by_id, stdout, returncode=0):
    """Redirect events + digest into tmp; inject corpus + stub the binary round-trip."""
    import fno.paths as paths
    events_path = tmp_path / "events.jsonl"
    monkeypatch.setattr(cli, "_events_paths", lambda: [events_path])
    monkeypatch.setattr(cli, "_load_corpus", lambda skill, since: ({"items": items, "attributed": len(items)}, by_id))
    monkeypatch.setattr(paths, "observer_reports_dir", lambda *a, **k: tmp_path / "reports")
    monkeypatch.setattr(cli, "find_dev_binary", lambda: "/fake/fno-agents")
    captured = {}
    real_run = subprocess.run

    def fake_run(cmd, *a, **k):
        if "review-summary" not in cmd:
            return real_run(cmd, *a, **k)  # event-store commits keep their real path
        captured["payload"] = json.loads(k.get("input") or "{}")
        return subprocess.CompletedProcess(cmd, returncode, stdout=stdout, stderr="")

    monkeypatch.setattr(cli.subprocess, "run", fake_run)
    return captured, events_path


def test_sweep_review_reads_evidence_and_digest_names_it(monkeypatch, tmp_path):
    """One binary round-trip classifies every item; the digest carries the
    evidence line verbatim after the verdicts line (AC2-HP)."""
    items = [_item(f"s{i}", f"x-{i}", None) for i in range(10)]
    by_id = {f"x-{i}": {"id": f"x-{i}", "pr_number": 100 + i} for i in range(10)}
    states = ["scored", "clean", "no_pr"] + ["no_attestation"] * 7
    ev_items = [
        {"node": f"x-{i}", "pr_number": 100 + i, "state": s, "finding_precision": "pass" if s == "scored" else None}
        for i, s in enumerate(states)
    ]
    evidence_line = "evidence: scored=1 clean=1 missing=8 (no_node=0 no_pr=1 no_attestation=7)"
    captured, events_path = _wire_review(
        monkeypatch, tmp_path, items, by_id,
        json.dumps({"items": ev_items, "evidence_line": evidence_line, "counts": {}}),
    )
    r = runner.invoke(cli.observer_app, ["sweep", "--skill", "review"])
    assert r.exit_code == 0, r.output
    payload = captured["payload"]
    assert payload == [{"node": f"x-{i}", "pr_number": 100 + i} for i in range(10)]
    findings = [e for e in _events(events_path) if e["type"] == "skill_eval_finding"]
    assert len(findings) == 1
    assert findings[0]["data"]["dimension"] == "finding_precision"
    assert findings[0]["data"]["verdict"] == "pass"
    digest = list((tmp_path / "reports").glob("review-*.md"))[0].read_text()
    assert evidence_line in digest
    assert digest.index("verdicts:") < digest.index(evidence_line)
    assert evidence_line in r.output


def test_sweep_review_unread_evidence_names_the_fault(monkeypatch, tmp_path):
    """A failed binary round-trip leaves every item a gap, names the fault in
    the digest, and fabricates no verdict (AC2-ERR)."""
    items = [_item(f"s{i}", f"x-{i}", None) for i in range(10)]
    by_id = {f"x-{i}": {"id": f"x-{i}", "pr_number": 100 + i} for i in range(10)}
    captured, events_path = _wire_review(monkeypatch, tmp_path, items, by_id, "{}", returncode=1)
    r = runner.invoke(cli.observer_app, ["sweep", "--skill", "review"])
    assert r.exit_code == 0, r.output
    assert r.output.startswith("partial:")
    digest = list((tmp_path / "reports").glob("review-*.md"))[0].read_text()
    assert "evidence: unread (fno-agents review-summary --evidence failed)" in digest
    assert not [e for e in _events(events_path) if e["type"] == "skill_eval_finding"]


# --------------------------------------------------------------------------- #
# gh fan-out cap (x-dbdf US4 / AC2-EDGE)
# --------------------------------------------------------------------------- #

def test_sweep_wires_the_gh_cap(monkeypatch, tmp_path):
    """End-to-end: 15 nodes with no on-disk plan (all fall to gh) under a cap of
    3 -> the real gh runner is invoked at most 3 times for the whole run."""
    monkeypatch.setenv("FNO_OBSERVER_GH_CAP", "3")
    import importlib
    importlib.reload(cli)

    calls = {"n": 0}

    def counting_gh(args):
        calls["n"] += 1
        return 0, "# Plan\n\n## Failure Modes\n\nx\n\n## Execution Strategy\n\n```yaml\ntasks: []\n```\n", ""

    monkeypatch.setattr(cli, "_default_gh", counting_gh)
    items = [_item(f"s{i}", f"x-{i}", None) for i in range(15)]
    by_id = {f"x-{i}": {"pr_number": 100 + i, "pr_url": f"https://github.com/o/r/pull/{100 + i}"} for i in range(15)}
    events_path = tmp_path / "events.jsonl"
    monkeypatch.setattr(cli, "_events_paths", lambda: [events_path])
    monkeypatch.setattr(cli, "_load_corpus", lambda skill, since: ({"items": items, "attributed": len(items)}, by_id))
    import fno.paths as paths
    monkeypatch.setattr(paths, "observer_reports_dir", lambda *a, **k: tmp_path / "reports")

    r = runner.invoke(cli.observer_app, ["sweep", "--skill", "blueprint"])
    assert r.exit_code == 0, r.output
    assert calls["n"] == 3  # cap held across the whole sweep

    monkeypatch.delenv("FNO_OBSERVER_GH_CAP", raising=False)
    importlib.reload(cli)


# --------------------------------------------------------------------------- #
# target sweep (x-6ff0)
# --------------------------------------------------------------------------- #

def _wire_target(monkeypatch, tmp_path, corpus, meta=None):
    """Inject the PR-anchored corpus + redirect events/digest into tmp."""
    events_path = tmp_path / "events.jsonl"
    monkeypatch.setattr(cli, "_events_paths", lambda: [events_path])
    monkeypatch.setattr(cli, "_load_target_corpus", lambda since, repo, gh: (corpus, meta or {"repos_scoped": 1, "dropped_repos": 0, "truncated_repos": []}))
    import fno.paths as paths
    monkeypatch.setattr(paths, "observer_reports_dir", lambda *a, **k: tmp_path / "reports")
    return events_path


def _target_item(pr_number, node_id, *, outcome="merged_clean", merged=True, ci_reds=0, loop_fires=1, terminal="DonePRGreen"):
    return {
        "pr_number": pr_number, "pr_url": f"https://github.com/o/r/pull/{pr_number}", "repo": "o/r",
        "merged": merged, "graph_node_id": node_id, "session_ids": [f"s{pr_number}"],
        "signals": {"promises": 1, "loop_fires": loop_fires, "ci_reds": ci_reds,
                    "terminal_reasons": [terminal] if terminal else [], "tokens_total": None,
                    "duration_minutes": None, "turns": None, "toolcalls": None},
        "terminal_reasons": [terminal] if terminal else [],
        "judgeable": merged and outcome is not None, "outcome": outcome, "attribution_class": None,
    }


def _target_corpus(items, *, unattributed=0, no_pr=0, stop_cause=None, prs_total=None):
    n = prs_total if prs_total is not None else len(items) + unattributed
    return {
        "items": items,
        "unattributed": [{"pr_number": 900 + i, "pr_url": None, "repo": None, "merged": True, "graph_node_id": None} for i in range(unattributed)],
        "no_pr": [{"session_ids": [f"np{i}"], "termination_reason": "NoProgress", "graph_node_id": None} for i in range(no_pr)],
        "coverage": {"prs_total": n, "attributed": len(items), "unattributed_pr": unattributed,
                     "no_pr_attempts": no_pr, "no_pr_stop_cause": stop_cause or ({"NoProgress": no_pr} if no_pr else {})},
    }


def test_target_all_repo_gh_outage_is_not_no_data(monkeypatch, tmp_path):
    """An outage where every scoped repo's gh call failed must NOT report no_data
    (exit 0) - the denominator was never observed; exit non-zero so automation
    sees the coverage failure."""
    corpus = _target_corpus([], prs_total=0)
    _wire_target(monkeypatch, tmp_path, corpus, meta={"repos_scoped": 2, "dropped_repos": 2, "truncated_repos": []})
    r = runner.invoke(cli.observer_app, ["sweep", "--skill", "target"])
    assert r.exit_code == 1
    assert "gh unavailable" in r.output and "no_data" not in r.output


def test_target_ok_emits_findings_run_complete_digest(monkeypatch, tmp_path):
    items = [_target_item(i, f"x-{i}") for i in range(10)]
    events_path = _wire_target(monkeypatch, tmp_path, _target_corpus(items, no_pr=3))
    r = runner.invoke(cli.observer_app, ["sweep", "--skill", "target"])
    assert r.exit_code == 0, r.output
    assert r.output.startswith("ok:")
    evs = _events(events_path)
    assert len([e for e in evs if e["type"] == "skill_eval_run_complete"]) == 1
    findings = [e for e in evs if e["type"] == "skill_eval_finding"]
    assert findings and all(f["data"]["skill_id"] == "fno:target" for f in findings)
    assert all(f["data"]["corpus_item_id"].startswith("pr-") for f in findings)
    digest = next((tmp_path / "reports").glob("target-*.md")).read_text()
    # AC3-HP: unconditional no_pr line + attempt->PR formula
    assert "no_pr_attempts: 3" in digest
    assert "attempt -> PR:" in digest
    # AC2-FR: the cross-tab renders in the summary block (above the failure ranking)
    assert digest.index("Falsifier cross-tab") < digest.index("Failure ranking")


# --------------------------------------------------------------------------- #
# replay
# --------------------------------------------------------------------------- #

def _wire_replay(monkeypatch, tmp_path, item):
    events_path = _wire(monkeypatch, tmp_path, [item])
    monkeypatch.setattr(cli, "_read_plan_text", lambda *a, **k: "# Doc\n\nbuild a widget\n")
    import fno.backlog.single_flight as sf
    monkeypatch.setattr(
        sf, "acquire_flight",
        lambda key, **k: sf.Flight(key=key, holder="replay:fake", held=False),
    )
    return events_path


def test_replay_happy_path_emits_tagged_finding(monkeypatch, tmp_path):
    item = _item("s-rep", "x-rep", None)
    events_path = _wire_replay(monkeypatch, tmp_path, item)
    fresh = "---\ntitle: t\n---\n\n## Failure Modes\nx\n\n## Execution Strategy\n```yaml\ntasks:\n- id: '1'\n  surface: ['a.py']\n```\n"
    cli._replay(
        skill="blueprint", corpus_item="s-rep", skill_ref="cand-branch",
        run_id="obs-fno:blueprint-test", since=90,
        spawn=lambda name, prompt, *, cwd, timeout: (0, fresh, ""),
        run_worktree=False,
    )
    evs = _events(events_path)
    findings = [e for e in evs if e["type"] == "skill_eval_finding"]
    assert findings and all(f["data"].get("skill_ref") == "cand-branch" for f in findings)
    # A1: replay never emits shipped_outcome
    assert all(f["data"]["dimension"] != "shipped_outcome" for f in findings)
    completes = [e for e in evs if e["type"] == "skill_eval_run_complete"]
    assert completes and completes[0]["data"].get("skill_ref") == "cand-branch"
    # truthful single-item batch numbers (not a padded corpus_size=10)
    assert completes[0]["data"]["corpus_size"] == 1
    assert completes[0]["data"]["coverage_pct"] == 100


def test_replay_concurrent_holder_refuses_without_stomping(monkeypatch, tmp_path):
    item = _item("s-rep", "x-rep", None)
    _wire_replay(monkeypatch, tmp_path, item)
    import fno.backlog.single_flight as sf
    def _held(key, **k):
        return sf.Flight(key=key, holder="peer:999", held=True)
    monkeypatch.setattr(sf, "acquire_flight", _held)
    with pytest.raises(typer.Exit) as exc:
        cli._replay(
            skill="blueprint", corpus_item="s-rep", skill_ref=None,
            run_id="obs-fno:blueprint-conc", since=90,
            spawn=lambda *a, **k: (0, "x", ""), run_worktree=False,
        )
    assert exc.value.exit_code == 4  # refused cleanly, no worktree touched


def test_sweep_fatal_when_canonical_event_unwritable(monkeypatch, tmp_path):
    # codex P2: the CLI must not report ok/write a digest when the canonical
    # skill_eval_run_complete could not be recorded.
    items = [_item(f"s{i}", f"x-{i}", _good_plan(tmp_path, f"p{i}.md")) for i in range(10)]
    _wire(monkeypatch, tmp_path, items)
    monkeypatch.setattr(cli, "_emit_run_complete", lambda summary, paths: False)
    r = runner.invoke(cli.observer_app, ["sweep", "--skill", "blueprint"])
    assert r.exit_code == 5
    assert "not recorded" in r.output
    assert not list((tmp_path / "reports").glob("*.md"))  # no digest on failure


def test_replay_isolation_violation_hard_fails(monkeypatch, tmp_path):
    item = _item("s-rep", "x-rep", None)
    events_path = _wire_replay(monkeypatch, tmp_path, item)
    monkeypatch.setattr(cli, "_write_workdir_settings", lambda wd: None)
    from fno import loops
    monkeypatch.setattr(loops, "loops_paused", lambda: False)

    class _P:
        returncode, stdout, stderr = 0, "", ""

    real_run = cli.subprocess.run

    def run(argv, *args, **kwargs):
        return _P() if argv[0] == "git" else real_run(argv, *args, **kwargs)

    monkeypatch.setattr(cli.subprocess, "run", run)
    from fno.observer import isolation
    monkeypatch.setattr(isolation, "collect_eval_session_ids", lambda wd, **k: ({"leak"}, None))
    monkeypatch.setattr(
        isolation, "check_isolation",
        lambda ids, paths: isolation.IsolationResult(
            verdict="violated",
            violations=[isolation.Violation(path=tmp_path / "ledger.json", session_id="leak", line_number=1, detail="x")],
        ),
    )
    fresh = "---\ntitle: t\n---\n\n## Failure Modes\nx\n\n## Execution Strategy\n```yaml\ntasks:\n- id: '1'\n  surface: ['a.py']\n```\n"
    with pytest.raises(typer.Exit) as exc:
        cli._replay(
            skill="blueprint", corpus_item="s-rep", skill_ref=None,
            run_id="obs-fno:blueprint-iso", since=90,
            spawn=lambda name, prompt, *, cwd, timeout: (0, fresh, ""),
            run_worktree=True,
        )
    assert exc.value.exit_code == 3  # the ONE non-advisory failure
    # codex P1: the scan runs BEFORE any emit, so a violated run voids all output
    assert not any(e["type"] == "skill_eval_run_complete" for e in _events(events_path))
    assert not any(e["type"] == "skill_eval_finding" for e in _events(events_path))
