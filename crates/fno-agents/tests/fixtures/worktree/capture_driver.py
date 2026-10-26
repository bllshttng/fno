"""Capture driver for the worktree-family golden corpus.

The Python legs (cli/src/fno/worktree_paths.py, worktree_status.py,
worktree_stranded.py, worktree_cli/overlaps.py, worktree_cli/cli.py's ensure,
runtime/registry.py's register_worker, runtime/worktree.py's list_worktrees)
were the last Python leg of the worktree family. This driver freezes their
behavior while they live, so the Rust ops (crates/fno-agents/src/worktree/)
can be characterized against them after the deletion.

Run from the repo root:  python3 crates/fno-agents/tests/fixtures/worktree/capture_driver.py
It needs only stdlib plus the in-repo cli/src package (sys.path-pinned below);
it runs no test suite and builds nothing.

Two outputs. Default capture builds every case under a throwaway fixture root,
runs the live Python leg in-process, and freezes one ``<key>.{exit,out,err}``
triple per case under ``tests/golden/worktree``. ``--case KEY`` runs one case
and prints its ``{exit, stdout, stderr}`` envelope as the last stdout line.
``--build-only ROOT`` writes ``cases.json`` - the case table, no leg run - so
the Rust parity test rebuilds the identical fixtures import-free.

Normalization is the contract's own: the fixture repo root folds to ``<ROOT>``,
the worktrees base to ``<BASE>``, the fixture home to ``<HOME>``, ISO-8601
timestamps to ``<TS>``, git's relative commit ages to ``<AGE>``, and the
conductor relocation target to ``<CONDUCTOR>``. Both sides apply the same
folds, so the golden states behavior, not machine paths.

A golden can only be captured from a live leg: after the Python family is
deleted, capture mode refuses and the frozen files are the contract.
"""

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from datetime import datetime, timezone
from pathlib import Path

FIXTURE_DIR = Path(__file__).resolve().parent
TESTS_DIR = FIXTURE_DIR.parents[1]
REPO_ROOT = TESTS_DIR.parents[2]
CLI_SRC = REPO_ROOT / "cli" / "src"
GOLDEN_DIR = TESTS_DIR / "golden" / "worktree"
CONDUCTOR = Path("~/conductor/workspaces").expanduser()

sys.path.insert(0, str(CLI_SRC))

ISO_TS = re.compile(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:\+00:00|Z)")
GIT_AGE = re.compile(r"\b\d+ (?:second|minute|hour|day|week|month|year)s? ago\b")


def normalize(text, root, home):
    """Fold machine paths and times to stable tokens, longest first."""
    base = root / "wtbase"
    folds = [
        (str(base.resolve()), "<BASE>"),
        (str(base), "<BASE>"),
        (str(root.resolve()), "<ROOT>"),
        (str(root), "<ROOT>"),
        (str(home.resolve()), "<HOME>"),
        (str(home), "<HOME>"),
    ]
    for raw, tok in folds:
        text = text.replace(raw, tok)
    text = ISO_TS.sub("<TS>", text)
    text = GIT_AGE.sub("<AGE>", text)
    return text


def canon_exit(code):
    return f"{code}\n"


def envelope(case, code, out, err, root, home):
    return (
        canon_exit(code),
        normalize(out, root, home),
        normalize(err, root, home),
    )


# --- fixture builders ------------------------------------------------------


def run_git(cwd, *args):
    # Pinned dates keep commit shas stable across capture runs, so goldens
    # that carry a sha (the apply case's pushed branch tip) stay valid.
    env = {
        **os.environ,
        "GIT_AUTHOR_DATE": "2026-01-01T00:00:00+00:00",
        "GIT_COMMITTER_DATE": "2026-01-01T00:00:00+00:00",
    }
    proc = subprocess.run(
        ["git", "-C", str(cwd), *args], capture_output=True, text=True, env=env
    )
    if proc.returncode != 0:
        raise RuntimeError(f"git -C {cwd} {args}: {proc.stderr.strip()}")
    return proc.stdout


def init_repo(path, origin=None, branch="main"):
    path.mkdir(parents=True, exist_ok=True)
    run_git(path, "init", "-q", "-b", branch)
    run_git(path, "config", "user.email", "fixture@example.com")
    run_git(path, "config", "user.name", "fixture")
    (path / "file.txt").write_text("one\n")
    run_git(path, "add", "-A")
    run_git(path, "commit", "-q", "-m", "base")
    if origin is not None:
        run_git(path, "remote", "add", "origin", str(origin))
        run_git(path, "push", "-q", "-u", "origin", branch)


def bare_with_main(path):
    """A bare remote holding one main commit; returns the seed repo too."""
    path.mkdir(parents=True, exist_ok=True)
    origin = path / "origin.git"
    run_git(path, "init", "-q", "--bare", "-b", "main", str(origin))
    seed = path / "seed"
    init_repo(seed, origin=origin)
    return origin, seed


def write_config(repo, text, name="config.toml"):
    fno_dir = repo / ".fno"
    fno_dir.mkdir(parents=True, exist_ok=True)
    (fno_dir / name).write_text(text)


def base_env(root, home):
    env = {
        "HOME": str(home),
        "FNO_STATE_DIR": str(root / "state"),
        "FNO_GLOBAL_SETTINGS_PATH": str(home / "absent-global-settings.yaml"),
        "FNO_AGENTS_RUNTIME": "python",
    }
    env["FNO_GLOBAL_SETTINGS_PATH"] = str(home / "absent-global-settings.yaml")
    (root / "state").mkdir(parents=True, exist_ok=True)
    return env


def registry_fixture(path, rows):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps(
            {
                "schema_version": 26,
                "min_writer_version": 26,
                "agents": rows,
            }
        )
        + "\n"
    )


LIVE_STATUS_ROW = {
    "name": "wt-live-glm",
    "short_id": "wt-live-glm-id",
    "harness": "claude",
    "status": "busy",
    "cwd": "__WT1__",
    "created_at": "2026-10-25T10:00:00+00:00",
    "last_reconciled_at": "2026-10-25T11:00:00+00:00",
}
EXITED_STATUS_ROW = {
    "name": "wt-exited-glm",
    "short_id": "wt-exited-glm-id",
    "harness": "claude",
    "status": "stopped",
    "cwd": "__WT2__",
    "created_at": "2026-10-25T09:00:00+00:00",
    "exited_at": "2026-10-25T09:30:00+00:00",
}


# --- case runners (the Python legs, in process) ----------------------------


def run_policy(case, root, home, env):
    from fno.worktree_paths import resolve_worktree_policy

    repo = root / case["repo"]
    os.environ.update(env)
    os.environ.pop("FNO_WORKTREE_POLICY", None)
    if case.get("env_policy"):
        os.environ["FNO_WORKTREE_POLICY"] = case["env_policy"]
    try:
        pol = resolve_worktree_policy(repo, case.get("harness"))
    except Exception as exc:  # noqa: BLE001 - the refusal text is the contract
        return 1, "", f"{type(exc).__name__}: {exc}\n"
    payload = {
        "policy": pol.policy,
        "base": str(pol.base),
        "project": pol.project,
        "source": pol.source,
        "requested_policy": pol.requested_policy,
        "degraded": pol.degraded,
        "note": pol.note,
    }
    return 0, json.dumps(payload, separators=(",", ":"), sort_keys=True) + "\n", ""


def run_ensure(case, root, home, env):
    from fno.worktree_cli.cli import _worktree_ensure

    os.environ.update(env)
    os.environ.pop("FNO_WORKTREE_POLICY", None)
    if case.get("env_policy"):
        os.environ["FNO_WORKTREE_POLICY"] = case["env_policy"]
    repo = str(root / case["repo"])
    if case.get("linked"):
        # The tree the fresh-cut case created, passed as --repo: a linked
        # worktree asking to nest is the refusal shape.
        repo = str(root / case["repo"] / ".claude" / "worktrees" / "zz-fixture-ensure-fresh")
    import contextlib
    import io as _io

    out_buf, err_buf = _io.StringIO(), _io.StringIO()
    code = 0
    with contextlib.redirect_stdout(out_buf), contextlib.redirect_stderr(err_buf):
        try:
            code = _worktree_ensure(
                repo, case["name"], case.get("branch"), case.get("harness")
            )
        except SystemExit as exc:  # typer.Exit paths surface as code only
            code = int(exc.code or 0)
    return code, out_buf.getvalue(), err_buf.getvalue()


def run_status(case, root, home, env):
    from fno import worktree_status

    os.environ.update(env)
    repo = root / case["repo"]
    argv = []
    if case.get("json"):
        argv.append("--json")
    argv += ["--repo", str(repo)]
    import contextlib
    import io as _io

    out_buf, err_buf = _io.StringIO(), _io.StringIO()
    code = 0
    with contextlib.redirect_stdout(out_buf), contextlib.redirect_stderr(err_buf):
        code = worktree_status.main(argv)
    return code, out_buf.getvalue(), err_buf.getvalue()


def run_list(case, root, home, env):
    from fno.runtime.worktree import list_worktrees

    os.environ.update(env)
    repo = root / case["repo"]
    rows = list_worktrees(repo_root=repo)
    return 0, json.dumps(rows, separators=(",", ":"), sort_keys=True) + "\n", ""


def run_stranded_classify(case, root, home, env):
    from fno.worktree_stranded import classify

    os.environ.update(env)
    node_entry = case.get("node_entry")
    row = classify(
        path=case["path"],
        branch=case.get("branch"),
        unpushed=case.get("unpushed", 1),
        unpushed_ok=case.get("unpushed_ok", True),
        node=case.get("node"),
        node_entry=node_entry,
        graph_ok=case.get("graph_ok", True),
        registry_status=case.get("registry_status"),
        registry_ok=case.get("registry_ok", True),
        age=case.get("age", "unknown"),
        has_remote=case.get("has_remote"),
    )
    payload = {
        "class": row.klass,
        "node": row.node,
        "unpushed": row.unpushed,
        "age": row.age,
        "facts": row.facts,
    }
    return 0, json.dumps(payload, separators=(",", ":"), sort_keys=True) + "\n", ""


def run_resolve_node(case, root, home, env):
    from fno.worktree_stranded import resolve_node_id

    os.environ.update(env)
    entries_by_id = {e["id"]: e for e in case["entries"]}
    wt = root / case["wt"]
    if case.get("manifest_line"):
        d = wt / ".fno"
        d.mkdir(parents=True, exist_ok=True)
        (d / "target-state.md").write_text(
            f"---\nsession: x\n---\ngraph_node_id: {case['manifest_line']}\n"
        )
    node, entry = resolve_node_id(str(wt), case.get("branch"), entries_by_id)
    payload = {"node": node, "entry_id": (entry or {}).get("id")}
    return 0, json.dumps(payload, separators=(",", ":"), sort_keys=True) + "\n", ""


def _pin_origin(repo, origin):
    """The clone already carries origin; keep it pointing at the fixture."""
    run_git(repo, "remote", "set-url", "origin", str(origin))
    run_git(repo, "fetch", "-q", "origin")
    run_git(repo, "reset", "-q", "--hard", "origin/main")
    run_git(repo, "branch", "-q", "--set-upstream-to=origin/main")


def run_sweep(case, root, home, env):
    """The sweep driver over a fixture repo with a local bare origin."""
    import fno.worktree_stranded as ws

    os.environ.update(env)
    repo = root / case["repo"]
    origin = root / (case.get("origin") or "")
    if origin.exists():
        _pin_origin(repo, origin)
    # A second worktree ahead of origin: the stranded shape.
    wt = (root / "sweep-wt").resolve()
    run_git(repo, "worktree", "add", "-q", "-b", "feature/sweep-node-xb00", str(wt))
    (wt / "file.txt").write_text("two\n")
    run_git(wt, "add", "-A")
    run_git(wt, "commit", "-q", "-m", "ahead")
    ws._remote_refs_fresh = False
    ws._remote_refs_stale = False
    entries = case.get("graph_entries") or []
    ws.read_graph_strict = lambda: entries
    rows = ws.sweep(repo)
    payload = [
        {
            "class": r.klass,
            "node": r.node,
            "unpushed": r.unpushed,
            "age": r.age,
            "facts": {k: v for k, v in r.facts.items() if k != "path"},
            "path_token": (
                "wt"
                if str(Path(r.facts.get("path", "")).resolve()).startswith(str(wt))
                else "main"
            ),
        }
        for r in rows
    ]
    return 0, json.dumps(payload, separators=(",", ":"), sort_keys=True) + "\n", ""


def run_apply(case, root, home, env):
    """act_on_stranded against a local bare origin, event journal pinned."""
    import fno.worktree_stranded as ws

    os.environ.update(env)
    repo = root / case["repo"]
    origin = root / (case.get("origin") or "")
    _pin_origin(repo, origin)
    wt = root / "apply-wt"
    run_git(repo, "worktree", "add", "-q", "-b", "feature/apply-node-xc00", str(wt))
    (wt / "file.txt").write_text("two\n")
    run_git(wt, "add", "-A")
    run_git(wt, "commit", "-q", "-m", "ahead")
    row = ws.classify(
        path=str(wt),
        branch="feature/apply-node-xc00",
        unpushed=1,
        unpushed_ok=True,
        node="x-c000",
        node_entry={"id": "x-c000", "status": "in_progress"},
        graph_ok=True,
        registry_status=None,
        registry_ok=True,
    )
    outcome = ws.act_on_stranded(row)
    pushed = run_git(origin, "rev-parse", "--verify", "-q", "feature/apply-node-xc00")
    payload = {"outcome": outcome, "pushed_sha": pushed.strip()[:7]}
    return 0, json.dumps(payload, separators=(",", ":"), sort_keys=True) + "\n", ""


def _pin_raw_overlap_contract(ov):
    """Pin the overlap legs to the raw-journal contract.

    The Rust ops (crates/fno-agents/src/worktree/overlaps.rs) read and append
    the raw journal only. The live Python leg reads back through the native
    event store and commits through it, so an unpinned capture characterizes
    whatever store state earlier cases leaked into the shared fixture root
    (the first freeze caught exactly that: four report goldens read the same
    leftover rows). Pin both edges: the store read answers unavailable (the
    leg's documented raw fallback branch) and the append writes the envelope
    line straight into the journal file.
    """
    from fno.events import store_client as _store

    _store.native_rows = lambda *a, **k: None
    # The recorded envelope's ts is the real clock; against the pinned fold
    # now (2026-10-26T00:00:00+00:00) an afternoon capture lands outside the
    # window and the deduped observation drops, so the fold flips with the
    # wall clock. Pin the stamp like the Rust side pins FNO_TEST_NOW.
    import fno.events as _events

    _events._ts_now = lambda: "2026-10-26T00:00:00.000000Z"

    def _raw_append(event, journal, lock_timeout_seconds=10.0):
        journal = Path(journal)
        journal.parent.mkdir(parents=True, exist_ok=True)
        with journal.open("a", encoding="utf-8") as fh:
            fh.write(json.dumps(event, separators=(",", ":"), sort_keys=True) + "\n")
        return None

    ov.append_event = _raw_append


def _isolated_journal(root, case):
    journal = root / "events" / "global-events.jsonl"
    if journal.parent.exists():
        shutil.rmtree(journal.parent)
    journal.parent.mkdir(parents=True, exist_ok=True)
    if case.get("journal_lines"):
        journal.write_text("".join(case["journal_lines"]))
    return journal


def run_overlap_record(case, root, home, env):
    from fno.worktree_cli import overlaps as ov

    os.environ.update(env)
    _pin_raw_overlap_contract(ov)
    journal = _isolated_journal(root, case)
    result, code = ov.overlap_record(
        since_days=case.get("since", 28),
        journal=journal,
        stdin=case["stdin"],
        now=datetime(2026, 10, 26, tzinfo=timezone.utc),
    )
    return (
        code,
        json.dumps(result, separators=(",", ":"), sort_keys=True, default=str) + "\n",
        "",
    )


def run_overlap_report(case, root, home, env):
    from fno.worktree_cli import overlaps as ov

    os.environ.update(env)
    _pin_raw_overlap_contract(ov)
    journal = _isolated_journal(root, case)
    report, code = ov.overlaps_report(
        since_days=case.get("since", 28),
        json_out=case.get("json", True),
        journal=journal,
        now=datetime(2026, 10, 26, tzinfo=timezone.utc),
    )
    text = ""
    if case.get("render"):
        text = ov.render_overlaps_text(report) + "\n"
        return code, text, ""
    return (
        code,
        json.dumps(report, separators=(",", ":"), sort_keys=True, default=str) + "\n",
        "",
    )


def run_fold(case, root, home, env):
    from fno.worktree_cli.overlaps import fold_overlap_events

    now = datetime(2026, 10, 26, tzinfo=timezone.utc)
    folded = fold_overlap_events(
        case["events"], since_days=case.get("since", 28), now=now
    )
    return (
        0,
        json.dumps(folded, separators=(",", ":"), sort_keys=True, default=str) + "\n",
        "",
    )


def run_dispatch_pin(case, root, home, env):
    from fno.worktree_paths import UNDECLARED_REPO_RECEIPT, undeclared_dispatch_pin

    os.environ.update(env)
    os.environ.pop("FNO_WORKTREE_POLICY", None)
    target = root / case["target"]
    caller = root / case["caller"] if case.get("caller") else root
    pin = undeclared_dispatch_pin(target, caller, case.get("harness"))
    payload = {"pin": pin, "receipt": UNDECLARED_REPO_RECEIPT if pin else None}
    return 0, json.dumps(payload, separators=(",", ":"), sort_keys=True) + "\n", ""


def run_register_worker(case, root, home, env):
    from fno.runtime.registry import register_worker

    os.environ.update(env)
    workers_file = root / "state" / "workers.jsonl"
    entry = register_worker(
        worker_id=case["worker_id"],
        task=case.get("task", ""),
        campaign=case.get("campaign", ""),
        workers_file=workers_file,
    )
    return 0, json.dumps(entry, separators=(",", ":"), sort_keys=True) + "\n", ""


RUNNERS = {
    "policy": run_policy,
    "ensure": run_ensure,
    "status": run_status,
    "list": run_list,
    "stranded_classify": run_stranded_classify,
    "resolve_node": run_resolve_node,
    "sweep": run_sweep,
    "apply": run_apply,
    "overlap_record": run_overlap_record,
    "overlap_report": run_overlap_report,
    "fold": run_fold,
    "dispatch_pin": run_dispatch_pin,
    "register_worker": run_register_worker,
}


# --- the case table ---------------------------------------------------------

OVERLAP_LINE = {
    "type": "worktree_overlap_observed",
    "ts": "2026-10-20T12:00:00+00:00",
    "source": "daemon",
    "data": {
        "observer_session_id": "sess-a",
        "peer_session_ids": ["sess-b"],
        "repository_key": "/fix/repo",
        "worktree_key": "/fix/repo/wt",
        "observation_id": "a" * 64,
        "live_window_seconds": 120,
    },
}


def overlap_line(**over):
    import copy

    line = copy.deepcopy(OVERLAP_LINE)
    line["data"].update(over)
    return json.dumps(line, separators=(",", ":"), sort_keys=True) + "\n"


def _true_observation_id(data):
    """The digest the validator recomputes; "a"*64 never passes it."""
    blob = json.dumps(
        [
            data["repository_key"],
            data["worktree_key"],
            data["observer_session_id"],
            sorted(set(data["peer_session_ids"])),
        ],
        separators=(",", ":"),
        ensure_ascii=False,
    )
    return hashlib.sha256(blob.encode("utf-8")).hexdigest()


def valid_overlap_line(**over):
    """overlap_line the schema actually admits: source hook and the
    observation_id the validator recomputes, so a pre-seeded row counts as an
    observation instead of a malformed line."""
    import copy

    line = copy.deepcopy(OVERLAP_LINE)
    line["source"] = "hook"
    line["data"].update(over)
    line["data"]["observation_id"] = _true_observation_id(line["data"])
    return json.dumps(line, separators=(",", ":"), sort_keys=True) + "\n"


def cases():
    return [
        # --- policy --------------------------------------------------------
        {"subject": "policy", "label": "default answers harness-native for claude",
         "repo": "r1", "harness": "claude"},
        {"subject": "policy", "label": "default degrades to external for codex",
         "repo": "r1", "harness": "codex"},
        {"subject": "policy", "label": "global never wins over the default",
         "repo": "r1", "harness": "claude",
         "global_config": "[worktree]\npolicy = \"never\"\n"},
        {"subject": "policy", "label": "per-project path match answers external",
         "repo": "r1", "harness": "claude",
         "repo_config": "[work.workspaces.w1.projects]\n",
         "repo_config_full": '[[work.workspaces.w1.projects]]\npath = "__ROOT__/r1"\nworktree = "external"\n'},
        {"subject": "policy", "label": "per-project name match answers never",
         "repo": "r2", "harness": "claude", "origin_url": "git@example.com:org/r2repo.git",
         "repo_config_full": '[[work.workspaces.w1.projects]]\nname = "r2repo"\nworktree = "never"\n'},
        {"subject": "policy", "label": "invalid global policy refuses",
         "repo": "r1", "harness": "claude",
         "global_config": "[worktree]\npolicy = \"bogus\"\n"},
        {"subject": "policy", "label": "unparseable repo config refuses",
         "repo": "r1", "harness": "claude",
         "repo_config": "work = [broken\n"},
        {"subject": "policy", "label": "work wrong type refuses",
         "repo": "r1", "harness": "claude",
         "repo_config": 'work = "flat"\n'},
        {"subject": "policy", "label": "env override wins and is validated",
         "repo": "r1", "harness": "claude", "env_policy": "external"},
        {"subject": "policy", "label": "invalid env override refuses",
         "repo": "r1", "harness": "claude", "env_policy": "wat"},
        {"subject": "policy", "label": "explicit worktrees base degrades harness-native",
         "repo": "r1", "harness": "claude",
         "global_config": "[paths]\nworktrees_base = \"__ROOT__/wtbase\"\n"},
        {"subject": "policy", "label": "conductor canonical degrades with a note",
         "repo": "r1", "harness": "claude",
         "global_config": "[worktree]\nuse_conductor_canonical = true\n"},
        {"subject": "policy", "label": "bare string project id resolves",
         "repo": "r2", "harness": "claude",
         "repo_config": 'project = "myproject"\n'},
        {"subject": "policy", "label": "project table id resolves",
         "repo": "r2", "harness": "claude",
         "repo_config": "[project]\nid = \"tableid\"\n"},
        {"subject": "policy", "label": "state dir fallback anchors the base",
         "repo": "r1", "harness": "codex"},
        # --- ensure ----------------------------------------------------------
        {"subject": "ensure", "label": "fresh cut from origin main",
         "repo": "e1", "name": "zz-fixture-ensure-fresh", "harness": "claude",
         "origin": True},
        {"subject": "ensure", "label": "reuse of an existing tree",
         "repo": "e1", "name": "zz-fixture-ensure-fresh", "harness": "claude",
         "origin": True, "pre": ["ensure:fresh_cut_from_origin_main"]},
        {"subject": "ensure", "label": "branch switch on a reused tree",
         "repo": "e1", "name": "zz-fixture-ensure-fresh", "harness": "claude",
         "branch": "feature/switched", "origin": True,
         "pre": ["ensure:fresh_cut_from_origin_main"]},
        {"subject": "ensure", "label": "never policy launches in place",
         "repo": "e1", "name": "zz-fixture-ensure-never", "harness": "claude",
         "origin": True, "env_policy": "never"},
        {"subject": "ensure", "label": "never policy conflicts with an explicit branch",
         "repo": "e1", "name": "zz-fixture-ensure-never2", "harness": "claude",
         "branch": "feature/blocked", "origin": True, "env_policy": "never"},
        {"subject": "ensure", "label": "invalid policy refuses with empty stdout",
         "repo": "e1", "name": "zz-fixture-ensure-bad", "harness": "claude",
         "origin": True, "env_policy": "wat"},
        {"subject": "ensure", "label": "linked worktree refuses to nest",
         "repo": "e1", "name": "zz-fixture-ensure-nested", "harness": "claude",
         "origin": True, "linked": True},
        {"subject": "ensure", "label": "non-git repo refuses",
         "repo": "plain", "name": "zz-fixture-ensure-plain", "harness": "claude"},
        # --- status ----------------------------------------------------------
        {"subject": "status", "label": "text form joins the registry",
         "repo": "s1", "registry": True},
        {"subject": "status", "label": "json form carries the summary",
         "repo": "s1", "json": True, "registry": True},
        {"subject": "status", "label": "missing registry degrades with a warning",
         "repo": "s1"},
        # --- list ------------------------------------------------------------
        {"subject": "list", "label": "fno base and legacy base rows only",
         "repo": "s1", "build_wts": ["s1", "legacy"]},
        # --- stranded classify ------------------------------------------------
        {"subject": "stranded_classify", "label": "zero unpushed reads clean",
         "path": "/r/wt", "branch": "feature/x-a", "unpushed": 0, "node": "x-a000",
         "node_entry": {"id": "x-a000", "status": "in_progress"}},
        {"subject": "stranded_classify", "label": "a failed read answers unknown first",
         "path": "/r/wt", "branch": "feature/x-a", "unpushed": 1, "unpushed_ok": False,
         "node": "x-a000", "node_entry": {"id": "x-a000", "status": "in_progress"}},
        {"subject": "stranded_classify", "label": "unresolved node answers unknown",
         "path": "/r/wt", "branch": "feature/x-a", "unpushed": 1,
         "node": None, "node_entry": None},
        {"subject": "stranded_classify", "label": "a live worker outranks node status",
         "path": "/r/wt", "branch": "feature/x-a", "unpushed": 1, "node": "x-a000",
         "node_entry": {"id": "x-a000", "status": "done"},
         "registry_status": "busy"},
        {"subject": "stranded_classify", "label": "done node reads shipped",
         "path": "/r/wt", "branch": "feature/x-a", "unpushed": 1, "node": "x-a000",
         "node_entry": {"id": "x-a000", "status": "done"}},
        {"subject": "stranded_classify", "label": "quiet terminal reads abandoned",
         "path": "/r/wt", "branch": "feature/x-a", "unpushed": 1, "node": "x-a000",
         "node_entry": {"id": "x-a000", "status": "deferred"}},
        {"subject": "stranded_classify", "label": "open pr keeps work visible",
         "path": "/r/wt", "branch": "feature/x-a", "unpushed": 1, "node": "x-a000",
         "node_entry": {"id": "x-a000", "status": "in_review", "pr_number": 12}},
        {"subject": "stranded_classify", "label": "in-progress with no pr reads stranded",
         "path": "/r/wt", "branch": "feature/x-a", "unpushed": 1, "node": "x-a000",
         "node_entry": {"id": "x-a000", "status": "in_progress"}},
        {"subject": "stranded_classify", "label": "facts carry pr number and live flag",
         "path": "/r/wt", "branch": "feature/x-a", "unpushed": 2, "node": "x-a000",
         "node_entry": {"id": "x-a000", "status": "in_review", "pr_number": 12},
         "registry_status": "idle", "has_remote": True},
        # --- resolve node ------------------------------------------------------
        {"subject": "resolve_node", "label": "exact id resolves from the basename",
         "wt": "x-d000", "entries": [{"id": "x-d000", "title": "t", "slug": "d-slug"}]},
        {"subject": "resolve_node", "label": "slug resolves case-insensitively",
         "wt": "plain-dir", "branch": "feature/d-slug",
         "entries": [{"id": "x-d001", "title": "t", "slug": "d-slug"}]},
        {"subject": "resolve_node", "label": "bare hex re-prefixes",
         "wt": "plain-dir", "branch": "feature/d002",
         "entries": [{"id": "x-d002", "title": "t", "slug": "d-two"}]},
        {"subject": "resolve_node", "label": "manifest line wins over nothing",
         "wt": "plain-dir2", "manifest_line": "x-d003",
         "entries": [{"id": "x-d003", "title": "t", "slug": "d-three"}]},
        {"subject": "resolve_node", "label": "no hit answers none",
         "wt": "plain-dir3", "branch": "feature/nothing",
         "entries": [{"id": "x-d004", "title": "t", "slug": "d-four"}]},
        # --- sweep + apply -------------------------------------------------------
        {"subject": "sweep", "label": "sweep classifies ahead and merged rows",
         "repo": "sw1", "origin": "sw-origin.git", "seed": "sw-seed",
         "graph_entries": [{"id": "x-b00", "title": "sweep node", "slug": "sweep-node-xb00",
                            "status": "in_progress"}]},
        {"subject": "apply", "label": "stranded act pushes and stops clean",
         "repo": "ap1", "origin": "ap-origin.git", "seed": "ap-seed"},
        # --- overlaps ------------------------------------------------------------
        {"subject": "fold", "label": "dedup folds one observation to one row",
         "since": 28, "events": [OVERLAP_LINE, OVERLAP_LINE]},
        {"subject": "fold", "label": "out-of-window observations drop",
         "since": 1, "events": [OVERLAP_LINE]},
        {"subject": "overlap_record", "label": "valid payload records and folds",
         "stdin": json.dumps({"observer_session_id": "sess-a",
                              "peer_session_ids": ["sess-b"],
                              "repository_common_dir": "/fix/repo",
                              "worktree_git_dir": "/fix/repo/wt"}),
         "journal_lines": [valid_overlap_line()]},
        {"subject": "overlap_record", "label": "invalid json answers invalid-input",
         "stdin": "not json"},
        {"subject": "overlap_record", "label": "empty peers answer invalid-input",
         "stdin": json.dumps({"observer_session_id": "sess-a",
                              "peer_session_ids": []})},
        {"subject": "overlap_record", "label": "empty observer answers schema-rejected",
         "stdin": json.dumps({"observer_session_id": "",
                              "peer_session_ids": ["sess-b"],
                              "repository_common_dir": "/fix/repo",
                              "worktree_git_dir": "/fix/repo/wt"})},
        {"subject": "overlap_report", "label": "missing journal is a valid empty source",
         "json": True},
        {"subject": "overlap_report", "label": "complete journal exits zero",
         "json": True, "journal_lines": [valid_overlap_line()]},
        {"subject": "overlap_report", "label": "malformed line makes partial exit one",
         "json": True, "journal_lines": [valid_overlap_line(), "{broken\n"]},
        {"subject": "overlap_report", "label": "text render names the counts",
         "render": True, "journal_lines": [valid_overlap_line()]},
        # --- dispatch pin ----------------------------------------------------------
        {"subject": "dispatch_pin", "label": "same repo identity pins nothing",
         "target": "p1", "caller": "p1"},
        {"subject": "dispatch_pin", "label": "foreign undeclared repo pins never",
         "target": "p2", "caller": "p1", "harness": "claude"},
        {"subject": "dispatch_pin", "label": "foreign declared repo pins nothing",
         "target": "p3", "caller": "p1", "harness": "claude",
         "repo_config_full": '[[work.workspaces.w1.projects]]\nname = "p3"\nworktree = "external"\n'},
        # --- register worker --------------------------------------------------------
        {"subject": "register_worker", "label": "entry lands in the jsonl registry",
         "worker_id": "t-fixture-worker", "task": "fixture task", "campaign": "camp-x"},
    ]


# --- fixture tree -----------------------------------------------------------


def build_fixtures(root, home, case_list):
    """Build the shared fixture tree once; per-case states layer on top."""
    root.mkdir(parents=True, exist_ok=True)
    home.mkdir(parents=True, exist_ok=True)
    (root / "plain").mkdir(exist_ok=True)

    # Simple repos: r1 (default), r2 (own origin), s1 (status rows), p1..p3.
    init_repo(root / "r1")
    init_repo(root / "r2")
    run_git(root / "r2", "remote", "add", "origin", "git@example.com:org/r2repo.git")
    init_repo(root / "s1")
    init_repo(root / "p1")
    init_repo(root / "p2")
    init_repo(root / "p3")

    # Ensure fixtures: e1 cloned from its bare origin (shared history).
    origin, seed = bare_with_main(root / "e1-fixture")
    os.rename(origin, root / "e1-origin.git")
    run_git(root, "clone", "-q", str(root / "e1-origin.git"), str(root / "e1"))
    run_git(root / "e1", "config", "user.email", "fixture@example.com")
    run_git(root / "e1", "config", "user.name", "fixture")

    # Sweep/apply fixtures: the working repo clones its bare remote, so the
    # histories share shas and the sweep's fetch/rev-list legs run offline.
    for name in ("sw", "ap"):
        o, s = bare_with_main(root / f"{name}-fixture")
        os.rename(o, root / f"{name}-origin.git")
        run_git(root, "clone", "-q", str(root / f"{name}-origin.git"), str(root / f"{name}1"))
        run_git(root / f"{name}1", "config", "user.email", "fixture@example.com")
        run_git(root / f"{name}1", "config", "user.name", "fixture")

    # Shared status fixture: one live row on wt1, one exited row on wt2.
    # The registry cwd spells the path the way git's porcelain does (the
    # resolved macOS spelling), or the join reads none.
    wt1 = (root / "s1-wts" / "wt1").resolve()
    wt2 = (root / "s1-wts" / "wt2").resolve()
    run_git(root / "s1", "worktree", "add", "-q", "-b", "feature/live-one", str(wt1))
    run_git(root / "s1", "worktree", "add", "-q", "-b", "feature/exited-two", str(wt2))
    registry_fixture(
        root / "registry.json",
        [
            {**LIVE_STATUS_ROW, "cwd": str(wt1)},
            {**EXITED_STATUS_ROW, "cwd": str(wt2)},
        ],
    )
    # Legacy-base tree for the list case: under <repo>/.claude/worktrees.
    legacy = (root / "s1" / ".claude" / "worktrees" / "legacy-one").resolve()
    run_git(root / "s1", "worktree", "add", "-q", "-b", "feature/legacy-one", str(legacy))
    # A worktree under the FNO_STATE_DIR base too.
    state_wt = (root / "state" / "worktrees" / "state-one").resolve()
    run_git(root / "s1", "worktree", "add", "-q", "-b", "feature/state-one", str(state_wt))

    # Overlap journal dir.
    (root / "events").mkdir(exist_ok=True)


def apply_case_overrides(root, home, case):
    """Layer one case's config shapes before its leg runs."""
    env = base_env(root, home)
    if case.get("global_config"):
        cfg = case["global_config"].replace("__ROOT__", str(root))
        gp = home / "global-config.toml"
        gp.write_text(cfg)
        env["FNO_GLOBAL_SETTINGS_PATH"] = str(gp)
    if case.get("repo_config"):
        write_config(root / case["repo"], case["repo_config"].replace("__ROOT__", str(root)))
    if case.get("repo_config_full"):
        target = root / case.get("repo") if case.get("repo") else root / case["target"]
        write_config(target, case["repo_config_full"].replace("__ROOT__", str(root)))
    if case.get("registry"):
        env["WORKTREE_STATUS_REGISTRY"] = str(root / "registry.json")
    return env


def case_env_key(case):
    return case["subject"] + ":" + case["label"]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--case", help="run one case, print its envelope")
    parser.add_argument("--build-only", type=Path, help="write cases.json, no legs")
    args = parser.parse_args()

    case_list = cases()
    if args.build_only:
        args.build_only.write_text(
            json.dumps(case_list, separators=(",", ":"), sort_keys=True) + "\n"
        )
        return 0

    tmp = Path(tempfile.mkdtemp(prefix="wt-golden-"))
    root, home = tmp / "root", tmp / "home"
    build_fixtures(root, home, case_list)

    if args.case:
        key = args.case
        matches = [c for c in case_list if case_env_key(c) == key]
        if not matches:
            print(f"no case keyed {key}", file=sys.stderr)
            return 2
        case = matches[0]
        env = apply_case_overrides(root, home, case)
        code, out, err = run_case(case, root, home, env)
        print(
            json.dumps(
                {
                    "exit": code,
                    "stdout": normalize(out, root, home),
                    "stderr": normalize(err, root, home),
                },
                indent=1,
            )
        )
        return 0

    # Default capture: freeze every golden, then the case table.
    GOLDEN_DIR.mkdir(parents=True, exist_ok=True)
    captured = 0
    for case in case_list:
        env = apply_case_overrides(root, home, case)
        code, out, err = run_case(case, root, home, env)
        key = slug(case_env_key(case))
        code_text, out_text, err_text = envelope(case, code, out, err, root, home)
        (GOLDEN_DIR / f"{key}.exit").write_text(code_text)
        (GOLDEN_DIR / f"{key}.out").write_text(out_text)
        if err_text:
            (GOLDEN_DIR / f"{key}.err").write_text(err_text)
        captured += 1
        print(f"froze {key} exit={code}")
    (FIXTURE_DIR / "cases.json").write_text(
        json.dumps(case_list, separators=(",", ":"), sort_keys=True) + "\n"
    )
    print(f"captured {captured} goldens into {GOLDEN_DIR}")
    return 0


_ENV_KEYS = (
    "HOME",
    "FNO_STATE_DIR",
    "FNO_GLOBAL_SETTINGS_PATH",
    "FNO_WORKTREE_POLICY",
    "WORKTREE_STATUS_REGISTRY",
    "FNO_AGENTS_RUNTIME",
)


def run_case(case, root, home, env):
    """Run one case's leg under its env pins, restoring the ambient env."""
    saved = {k: os.environ.get(k) for k in _ENV_KEYS}
    os.environ.update(env)
    try:
        clear_settings_cache()
        return RUNNERS[case["subject"]](case, root, home, env)
    finally:
        for k, v in saved.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v


def clear_settings_cache():
    """Drop fno.paths' cached settings so one case's pins never leak."""
    import fno.paths as _p

    fn = getattr(_p, "_settings", None)
    cache = getattr(fn, "cache_clear", None)
    if cache is not None:
        cache()


def slug(key):
    """Filename-safe golden key: lowercase, non-alnum runs collapse to _."""
    out = []
    prev_us = False
    for ch in key:
        if ch.isascii() and ch.isalnum():
            out.append(ch.lower())
            prev_us = False
        elif not prev_us:
            out.append("_")
            prev_us = True
    return "".join(out).strip("_")


if __name__ == "__main__":
    raise SystemExit(main())
