#!/usr/bin/env python3
"""Python oracle for the pr-closure produce port (branch-ids, ensure,
trailer, bind).

Two modes:

* Freeze: ``--freeze --cases <cases.json> --golden-dir <dir>`` builds every
  fixture under a fresh temp root, runs the real Python leg in-process,
  masks the moving parts, and writes the goldens in the tests/common/mod.rs
  format (``<key>.exit``, ``<key>.out``, and ``<key>.err`` only when stderr
  is non-empty). Run it with ``uv run python`` under
  ``PYTHONPATH=<worktree>/cli/src`` and ``FNO_AGENTS_BIN=<built fno-agents>``,
  never through the test runner.

* Envelope: ``--case <dir>`` reads the ``case.json`` the parity test wrote
  into the fixture, runs one leg, and prints one
  ``{"exit","stdout","stderr"}`` JSON object with RAW streams. The caller
  owns the env pins and the masking.

The legs re-implement only the cli.py glue semantics and the command
contracts; every scanner, trailer, and bind decision comes from the real
``fno`` library functions, so the frozen goldens pin the library's
behavior rather than a re-derivation of it. The bind stamp loop
(``_stamp_ship_on_pr_link``) is skipped: the stamp is best-effort, never
prints, and would only pollute the fixture's graph.
"""

import argparse
import contextlib
import json
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

from fno.graph._constants import is_wellformed_node_id
from fno.graph.api import wire_rows
from fno.graph.store import commit_rows_via_store
from fno.paths import graph_json
from fno.pr.closure import (
    BranchResolutionError,
    bind_created_pr_from_branch,
    branch_node_ids,
    ensure_closure_trailer,
    known_node_ids,
    render_pr_closure_trailer,
    resolve_branch_node_id,
)
from fno.graph.api import active_backend_name

CORRUPT_BYTES = b"this fixture graph is deliberately corrupt\n"


def assert_source_import():
    """Guard against the installed wheel: fno must resolve from a cli/src tree."""
    import fno

    marker = os.sep + "cli" + os.sep + "src" + os.sep
    if marker not in os.path.abspath(fno.__file__):
        raise SystemExit(
            f"oracle: fno resolved to {fno.__file__}; PYTHONPATH must pin a cli/src tree"
        )


def slug(label):
    """The tests/common/mod.rs slug: lowercase alnum, other runs collapse to _."""
    out = []
    prev_us = False
    for ch in label:
        if ch.isascii() and ch.isalnum():
            out.append(ch.lower())
            prev_us = False
        elif not prev_us:
            out.append("_")
            prev_us = True
    return "".join(out).strip("_")


def mask(text, case_dir):
    """Normalize the moving parts the Rust side masks identically.

    The case dir in both spellings (macOS /var is a symlink to
    /private/var), then the two graph-read failure shapes. Exception text
    differs between the legs; the masked shape is the contract.
    """
    for spelling in dict.fromkeys([str(case_dir), os.path.realpath(str(case_dir))]):
        text = text.replace(spelling, "<CASE>")
    text = re.sub(r"the graph read failed \([^)]*\)", "the graph read failed (<ERR>)", text)
    text = re.sub(r'graph read failed: [^"]*', "graph read failed: <ERR>", text)
    return text


@contextlib.contextmanager
def pushd(path):
    old = os.getcwd()
    os.chdir(path)
    try:
        yield
    finally:
        os.chdir(old)


# -------------------------------------------------------------------------
# Fixture build (freeze mode only; the parity test mirrors this in Rust)
# -------------------------------------------------------------------------

def build_git(case_dir, spec):
    def git(*argv):
        env = dict(os.environ)
        env["GIT_CONFIG_GLOBAL"] = "/dev/null"
        env["GIT_CONFIG_SYSTEM"] = "/dev/null"
        proc = subprocess.run(
            ["git", *argv], cwd=case_dir, capture_output=True, text=True, env=env
        )
        if proc.returncode != 0:
            raise RuntimeError(f"git {' '.join(argv)} failed: {proc.stderr.strip()}")

    git("init", "-q", "-b", "main")
    git("config", "user.email", "fixture@example.com")
    git("config", "user.name", "Fixture")
    git("commit", "--allow-empty", "-q", "-m", "seed")
    if spec == "detached":
        git("checkout", "--detach", "-q")
    else:
        git("checkout", "-q", "-b", spec["branch"])


def build_fixture(root, case, index):
    case_dir = os.path.join(root, f"case-{index:02d}")
    os.makedirs(case_dir)
    graph = Path(case_dir) / "graph.json"

    # state_dir is a TOP-LEVEL key: under a [table] header the config loader
    # reads it as a table field and warns it is unmodeled.
    config = f'state_dir = "{case_dir}"\n'
    (Path(case_dir) / "config.toml").write_text(config)

    rows = case.get("rows") or []
    if rows:
        commit_rows_via_store(graph, lambda entries: [dict(r) for r in rows])

    if case.get("corrupt"):
        graph.write_bytes(CORRUPT_BYTES)
        graph.with_suffix(".db").write_bytes(CORRUPT_BYTES)

    spec = case.get("git")
    if spec:
        build_git(case_dir, spec)

    (Path(case_dir) / "case.json").write_text(json.dumps(case, indent=2))
    return case_dir


# -------------------------------------------------------------------------
# Legs. Each returns (exit, stdout, stderr) with the trailing newlines the
# real command would have printed.
# -------------------------------------------------------------------------

def leg_branch_ids(case):
    head_ref = case.get("head_ref")
    if not isinstance(head_ref, str):
        head_ref = ""
    ids = branch_node_ids(head_ref)
    out = {"ids": ids}
    if case.get("verify"):
        try:
            known = known_node_ids()
        except Exception as exc:
            return 4, "", f"pr-closure-branch-ids: the graph read failed ({exc})\n"
        out["real"] = [nid for nid in ids if nid in known]
    return 0, json.dumps(out, separators=(",", ":")) + "\n", ""


def leg_ensure(case):
    body = case.get("body")
    if not isinstance(body, str):
        body = ""
    head_ref = case.get("head_ref")
    if not isinstance(head_ref, str):
        head_ref = ""
    extra_ids = case.get("extra_ids") or []
    try:
        new_body = ensure_closure_trailer(body, head_ref, extra_ids=list(extra_ids))
    except Exception as exc:
        return 4, "", f"pr-closure-ensure: the graph read failed ({exc})\n"
    return 0, json.dumps({"body": new_body}, separators=(",", ":")) + "\n", ""


def leg_trailer(case):
    out = []
    err = []

    def echo(msg, to_err=False):
        (err if to_err else out).append(msg + "\n")

    node = case.get("node")
    extra = list(case.get("extra") or [])
    external = active_backend_name() != "graph"
    if node is None and external:
        echo(
            "closure-trailer: branch resolution needs the graph backend;"
            " pass the node explicitly instead",
            to_err=True,
        )
        return 1, "".join(out), "".join(err)
    if node is not None and external:
        return 0, "", ""
    try:
        entries = wire_rows(path=graph_json())
    except Exception as exc:
        echo(
            f"closure-trailer: the graph read failed ({exc});"
            " exit 4 names a dead reader, not an unresolvable node",
            to_err=True,
        )
        return 4, "".join(out), "".join(err)
    if node is None:
        known_ids = frozenset(
            str(entry["id"])
            for entry in entries
            if isinstance(entry, dict) and isinstance(entry.get("id"), str)
        )
        try:
            node_id = resolve_branch_node_id(known_ids, cwd=os.getcwd())
        except BranchResolutionError as exc:
            echo(f"closure-trailer: {exc}", to_err=True)
            return 1, "".join(out), "".join(err)
    else:
        node_id = str(node)
    dropped = [e for e in extra if not is_wellformed_node_id(e)]
    if dropped:
        echo(
            "warning: dropping malformed --extra id(s) from the trailer:"
            f" {', '.join(dropped)} (need the full <prefix>-<hex> form)",
            to_err=True,
        )
    line = render_pr_closure_trailer(entries, node_id, extra_ids=list(extra))
    if line:
        echo(line)
    return 0, "".join(out), "".join(err)


def leg_bind(case_dir, case):
    url = case.get("url") or ""
    node = case.get("node")
    result = bind_created_pr_from_branch(url, cwd=case_dir, node_id=node)
    envelope = json.dumps(
        {
            "outcome": result.outcome,
            "claimed_ids": list(result.claimed_ids),
            "refusal": result.refusal,
        },
        separators=(",", ":"),
    )
    return (0 if result.outcome == "bound" else 1), envelope + "\n", ""


def run_leg(case_dir, case):
    command = case.get("verb", "trailer")
    if command == "branch-ids":
        return leg_branch_ids(case)
    if command == "ensure":
        return leg_ensure(case)
    if command == "trailer":
        return leg_trailer(case)
    if command == "bind":
        return leg_bind(case_dir, case)
    raise SystemExit(f"oracle: unknown command {command!r}")


def run_case(case_dir, case, *, own_env):
    if own_env:
        os.environ["FNO_CONFIG"] = str(Path(case_dir) / "config.toml")
        os.environ["FNO_STATE_DIR"] = str(case_dir)
        backend = case.get("backend")
        if backend:
            os.environ["FNO_TRACKER_BACKEND"] = backend
        else:
            os.environ.pop("FNO_TRACKER_BACKEND", None)
    with pushd(case_dir):
        return run_leg(case_dir, case)


def freeze(cases_path, golden_dir):
    if not os.environ.get("FNO_AGENTS_BIN"):
        raise SystemExit(
            "oracle: freeze needs FNO_AGENTS_BIN (a built fno-agents binary)"
            " for the parse/render forwarders"
        )
    cases = json.loads(Path(cases_path).read_text())
    golden_dir = Path(golden_dir)
    golden_dir.mkdir(parents=True, exist_ok=True)
    root = tempfile.mkdtemp(prefix="pr-closure-goldens-")
    for index, case in enumerate(cases):
        case_dir = build_fixture(root, case, index)
        code, stdout, stderr = run_case(case_dir, case, own_env=True)
        stdout = mask(stdout, case_dir)
        stderr = mask(stderr, case_dir)
        key = slug(case["label"])
        (golden_dir / f"{key}.exit").write_text(f"{code}\n")
        streams = [stdout] + ([stderr] if stderr else [])
        for suffix, text in zip(["out", "err"], streams):
            (golden_dir / f"{key}.{suffix}").write_text(text)
    print(f"oracle: froze {len(cases)} goldens into {golden_dir}")


def main():
    assert_source_import()
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--freeze", action="store_true")
    parser.add_argument("--cases")
    parser.add_argument("--golden-dir")
    parser.add_argument("--case")
    args = parser.parse_args()

    if args.freeze:
        if not (args.cases and args.golden_dir):
            raise SystemExit("oracle: --freeze needs --cases and --golden-dir")
        freeze(args.cases, args.golden_dir)
        return

    if not args.case:
        raise SystemExit("oracle: pass --case <fixture dir>, or --freeze")
    case_dir = os.path.abspath(args.case)
    case = json.loads((Path(case_dir) / "case.json").read_text())
    code, stdout, stderr = run_case(case_dir, case, own_env=False)
    print(json.dumps({"exit": code, "stdout": stdout, "stderr": stderr},
                     separators=(",", ":")))


if __name__ == "__main__":
    main()
