"""Capture driver for the tracker-seam golden corpus.

The Python seam package (``cli/src/fno/tracker/``) was the last Python leg of
the work-item tracker family: the sidecar graph projection, the sidecar
per-id file store, and the guarded default-backend graph read. This driver
freezes that leg's behavior while it lives, so the Rust door
(``fno-agents graph-get``'s stdin form, tracker payload) can be characterized
against it after the deletion.

Three modes. Default capture builds every case under a throwaway state root
(``FNO_STATE_DIR`` pins the graph anchor and the sidecar dir), runs the live
Python leg in-process, and freezes one ``<key>.{exit,out,err}`` triple per
case under ``tests/golden/tracker``. ``--case KEY`` runs one case and prints
its ``{exit, stdout, stderr}`` envelope as the last stdout line (the child
shape the merge_command driver uses). ``--build-only ROOT`` writes
``cases.json`` - the case table, no leg run - so the Rust parity test can
rebuild the identical fixtures import-free (it seeds the store through
``graph_store::seed_rows`` and writes the same per-id sidecar files).

The stdout envelope is the contract, normalized: every string leaf has the
case root (and its realpath spelling) replaced with ``<ROOT>``, and a loaded
``Sidecar`` dumps SPARSE - ``None`` fields dropped, coerced empties kept - so
the golden states which keys cross the seam with which values, not the
pydantic model's padding. The Rust door answers sparse maps natively; that
padding drop is the one deliberate envelope departure, recorded here.

Cases cover exactly the legs this port moves onto the door: the backend-name
resolution, the sidecar graph projection (null-list coercion,
invalid-projection degradation, archived-row skip), the sidecar per-id file
store, and the graph merge write (load-mutate-save roundtrip, missing-row
refusal). The already-native ops (read, list-open, close, snapshot) are
characterized by the unit tests beside the door, not re-frozen here.
``read_entries`` is NOT in the corpus on purpose: it wraps ``wire_rows``,
the graph family's read model, and relocates verbatim to
``fno/graph/api.py`` instead of porting - a door port would have to
re-derive the pydantic dump's null padding, a second read model. Its
behavior stays pinned by the graph/doctor/plan/provenance integration
tests that read it through their own callers.

A golden can only be captured from a live leg: after the Python package is
deleted, capture mode refuses and the frozen files are the contract.
"""

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

FIXTURE_DIR = Path(__file__).resolve().parent
TESTS_DIR = FIXTURE_DIR.parents[1]
GOLDEN_DIR = TESTS_DIR / "golden" / "tracker"
REPO_ROOT = TESTS_DIR.parents[2]
CLI_SRC = REPO_ROOT / "cli" / "src"
CLI_TESTS = REPO_ROOT / "cli" / "tests"

CORRUPT_BYTES = b"this fixture store is deliberately corrupt\n"

CASES = {
    "backend_default": {
        "rows": [{"id": "ab-1"}],
        "env": {},
        "script": "backend",
        "args": {},
    },
    "backend_env": {
        "rows": [{"id": "ab-1"}],
        "env": {"FNO_TRACKER_BACKEND": "github"},
        "script": "backend",
        "args": {},
    },
    "backend_explicit": {
        "rows": [{"id": "ab-1"}],
        "env": {"FNO_TRACKER_BACKEND": "github"},
        "script": "backend",
        "args": {"explicit": "linear"},
    },
    "sidecar_get_hit": {
        "rows": [
            {
                "id": "ab-1",
                "cwd": "/repo",
                "plan_path": "/p.md",
                "additional_prs": None,
                "cost_usd": 1.5,
            }
        ],
        "env": {},
        "script": "sidecar_get",
        "args": {"id": "ab-1"},
    },
    "sidecar_get_miss": {
        "rows": [{"id": "ab-1"}],
        "env": {},
        "script": "sidecar_get",
        "args": {"id": "ab-gone"},
    },
    "sidecar_get_invalid": {
        "rows": [{"id": "ab-1", "pr_number": "not-a-number", "cwd": "/repo"}],
        "env": {},
        "script": "sidecar_get",
        "args": {"id": "ab-1"},
    },
    "sidecar_all_graph": {
        "rows": [
            {"id": "ab-1", "cwd": "/repo", "sessions": None},
            {"id": "ab-2", "pr_number": "bad", "cwd": "/skipped"},
            {
                "id": "ab-3",
                "archived_at": "2026-09-01T00:00:00Z",
                "cwd": "/archived",
            },
            {"id": "ab-4", "source_inbox_msg": "m-1"},
        ],
        "env": {},
        "script": "sidecar_all",
        "args": {},
    },
    "sidecar_all_external": {
        "rows": [{"id": "ab-1", "cwd": "/graph-sentinel"}],
        "sidecar_files": {
            "EXT-1.json": {"id": "EXT-1", "cwd": "/one"},
            "EXT-2.json": {"id": "EXT-2", "pr_number": 7},
            "bad.json": "not json at all",
        },
        "env": {"FNO_TRACKER_BACKEND": "github"},
        "script": "sidecar_all",
        "args": {},
    },
    "sidecar_set_graph_merge": {
        "rows": [{"id": "ab-1", "cwd": "/old", "plan_path": "/p.md"}],
        "env": {},
        "script": "sidecar_set_merge",
        "args": {"id": "ab-1"},
    },
    "sidecar_set_graph_missing": {
        "rows": [{"id": "ab-1"}],
        "env": {},
        "script": "sidecar_set_missing",
        "args": {},
    },
    "sidecar_set_external": {
        "rows": [{"id": "EXT-1", "cwd": "/graph-sentinel"}],
        "sidecar_files": {
            "EXT-1.json": {"id": "EXT-1", "cwd": "/one", "plan_path": "/p.md"}
        },
        "env": {"FNO_TRACKER_BACKEND": "github"},
        "script": "sidecar_set_external",
        "args": {"id": "EXT-1"},
    },
}


def _seeded_rows(rows):
    """Rows the way the store expects them: the typed api drops a row the
    model cannot parse, so seeds carry the stamped fields (the same shape
    cli/tests' _write_graph helpers stamp)."""
    out = []
    for e in rows:
        row = {"type": "feature", "priority": "p2", "status": "idea", **e}
        row.setdefault("title", e.get("id", "node"))
        row.setdefault("slug", e.get("id", "node"))
        out.append(row)
    return out


def build_case_tree(root: Path, case: dict) -> Path:
    """One throwaway state root: the graph anchor plus any per-id files."""
    root = Path(root)
    graph = root / "db" / "graph.json"
    if case.get("corrupt"):
        graph.parent.mkdir(parents=True, exist_ok=True)
        graph.write_bytes(CORRUPT_BYTES)
        # The store anchor is the .db sibling; corrupting the json anchor only
        # is not a corrupt store. Write the same bytes over the db too.
        graph.with_suffix(".db").write_bytes(CORRUPT_BYTES)
    elif case["rows"]:
        sys.path.insert(0, str(CLI_TESTS))
        sys.path.insert(0, str(CLI_SRC))
        from fixtures.graph_seed import seed_graph

        seed_graph(graph, _seeded_rows(case["rows"]))
    else:
        graph.parent.mkdir(parents=True, exist_ok=True)
    for name, body in case.get("sidecar_files", {}).items():
        sidecar_dir = root / "sidecar"
        sidecar_dir.mkdir(parents=True, exist_ok=True)
        text = body if isinstance(body, str) else json.dumps(body)
        (sidecar_dir / name).write_text(text, encoding="utf-8")
    return root


def _sparse(sidecar) -> dict:
    """The envelope's sidecar shape: None fields dropped, coerced empties kept."""
    return {k: v for k, v in sidecar.model_dump().items() if v is not None}


def _normalize(value, root: Path):
    """Replace the root (and its realpath spelling) in every string leaf."""
    markers = {str(root), os.path.realpath(root)}
    if isinstance(value, str):
        for m in markers:
            value = value.replace(m, "<ROOT>")
        return value
    if isinstance(value, list):
        return [_normalize(v, root) for v in value]
    if isinstance(value, dict):
        return {k: _normalize(v, root) for k, v in value.items()}
    return value


def run_case(case_key: str, root: Path) -> dict:
    """Run one case against the live Python leg. Returns the stdout envelope.

    Every expected failure is caught here and frozen as a payload key; an
    unexpected exception propagates and the caller freezes it as a nonzero
    exit with the traceback on stderr - a corpus bug, never a silent success.
    """
    case = CASES[case_key]
    script = case["script"]
    args = case.get("args", {})

    from fno.tracker import NodeNotFound, active_backend_name
    from fno.tracker import sidecar as sidecar_store

    if script == "backend":
        return {"backend": active_backend_name(args.get("explicit"))}
    if script == "sidecar_get":
        return {"sidecar": _sparse(sidecar_store.load(args["id"]))}
    if script == "sidecar_all":
        return {
            "sidecars": {
                sid: _sparse(sc) for sid, sc in sidecar_store.load_all().items()
            }
        }
    if script == "sidecar_set_merge":
        sc = sidecar_store.load(args["id"])
        sc.cwd = "/new"
        sc.pr_number = 42
        saved = sidecar_store.save(sc)
        from fno.graph.store import read_graph_strict

        graph = Path(os.environ["FNO_STATE_DIR"]) / "db" / "graph.json"
        row = next(e for e in read_graph_strict(graph) if e.get("id") == args["id"])
        return {"saved": str(saved), "row_after": row}
    if script == "sidecar_set_missing":
        try:
            from fno.tracker.sidecar import Sidecar

            sidecar_store.save(Sidecar(id="ab-ghost", cwd="/nowhere"))
        except NodeNotFound:
            return {"not_found": "ab-ghost"}
        raise AssertionError("save on a missing row did not refuse")
    if script == "sidecar_set_external":
        sc = sidecar_store.load(args["id"])
        sc.pr_number = 5
        saved = sidecar_store.save(sc)
        return {
            "saved": str(saved),
            "file_after": json.loads(Path(saved).read_text(encoding="utf-8")),
        }
    raise AssertionError(f"unknown script {script}")


def _pin_env(root: Path, case: dict):
    """Hermetic child environment: the state root pinned, live-session vars
    stripped, the case's backend selection applied last."""
    env = dict(os.environ)
    for var in (
        "FNO_TRACKER_BACKEND",
        "FNO_CONFIG",
        "FNO_STATE_DIR",
        "CLAUDE_CODE_SESSION_ID",
        "TARGET_SESSION_ID",
        "SESSION_ID",
        "FNO_CAPTURE_GOLDEN",
    ):
        env.pop(var, None)
    env["FNO_STATE_DIR"] = str(root / "state")
    env["HOME"] = str(root / "home")
    (root / "home").mkdir(parents=True, exist_ok=True)
    env.update(case.get("env", {}))
    env["PYTHONPATH"] = str(CLI_SRC)
    return env


def _write_golden(key: str, envelope: dict):
    GOLDEN_DIR.mkdir(parents=True, exist_ok=True)
    (GOLDEN_DIR / f"{key}.exit").write_text(f"{envelope['exit']}\n")
    (GOLDEN_DIR / f"{key}.out").write_text(envelope["stdout"])
    (GOLDEN_DIR / f"{key}.err").write_text(envelope["stderr"])


def capture_all():
    if GOLDEN_DIR.exists():
        shutil.rmtree(GOLDEN_DIR)
    failures = []
    for key in CASES:
        root = Path(tempfile.mkdtemp(prefix="fno-tracker-capture-"))
        try:
            case_root = build_case_tree(root / "state", CASES[key])
            env = _pin_env(root, CASES[key])
            # The leg runs in a hermetic child (the --case mode), so one
            # case's env pins can never leak into the next.
            proc = subprocess.run(
                [
                    sys.executable,
                    __file__,
                    "--case",
                    key,
                    "--case-root",
                    str(case_root),
                ],
                capture_output=True,
                text=True,
                env=env,
                timeout=120,
            )
            if proc.returncode != 0:
                failures.append((key, f"child exit {proc.returncode}: {proc.stderr[-400:]}"))
                continue
            envelope = json.loads(proc.stdout.strip().splitlines()[-1])
            envelope["stdout"] = (
                json.dumps(_normalize(json.loads(envelope["stdout"]), case_root.parent), indent=2, sort_keys=True)
                + "\n"
            )
            envelope["stderr"] = _normalize(envelope["stderr"], case_root.parent)
            _write_golden(key, envelope)
        finally:
            shutil.rmtree(root, ignore_errors=True)
    if failures:
        for key, why in failures:
            print(f"FAIL {key}: {why}", file=sys.stderr)
        raise SystemExit(1)
    print(f"captured {len(CASES)} goldens under {GOLDEN_DIR}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--case", help="run one case, print its envelope")
    ap.add_argument("--case-root", help="the prebuilt state root for --case")
    ap.add_argument("--build-only", help="write cases.json, run no leg")
    args = ap.parse_args()

    if args.build_only:
        root = Path(args.build_only)
        root.mkdir(parents=True, exist_ok=True)
        (root / "cases.json").write_text(json.dumps(CASES, indent=2, sort_keys=True))
        print(f"wrote {root / 'cases.json'}")
        return
    if args.case:
        root = Path(args.case_root)
        env = dict(os.environ)
        # The child re-pins: HOME under the case root, the state root exactly
        # where build_case_tree put the store.
        env["HOME"] = str(root.parent / "home")
        (root.parent / "home").mkdir(parents=True, exist_ok=True)
        try:
            result = run_case(args.case, root)
            out = json.dumps({"exit": 0, "stdout": json.dumps(result), "stderr": ""})
            print(out)
        except Exception as exc:  # noqa: BLE001 - the envelope IS the contract
            out = json.dumps(
                {
                    "exit": 1,
                    "stdout": "",
                    "stderr": f"{type(exc).__name__}: {exc}",
                }
            )
            print(out)
        return
    capture_all()


if __name__ == "__main__":
    main()
