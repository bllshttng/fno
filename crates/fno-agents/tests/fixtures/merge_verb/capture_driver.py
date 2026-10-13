"""Capture driver for the merge-verb golden corpus (cases.json).

Three modes. Default capture builds every case under a throwaway root, runs the live Python leg in a hermetic child, and freezes exit, streams, and a folded call journal as goldens under tests/golden/merge_verb. `--case DIR` is the child mode: it re-applies the environment pins, runs one entry, and prints one `{"exit", "stdout", "stderr"}` envelope as the last stdout line. `--build-only ROOT` builds the fixture tree and the stub binaries without running any leg and prints a JSON map the Rust parity test reuses.

The four stubs under `stub-bin/` (fno-agents, fno-agents-worker, gh, fno-front) answer each call from an `answers/*.json` file and journal every call to `journals/*.jsonl`. The folded, masked journals become the `<key>.calls` golden, so the future Rust leg must issue the same calls in the same order. Consecutive duplicate calls collapse to `<op>*` in that journal: the merge-lock wait polls on a wall-clock budget, so an exact poll count would freeze local machine speed into the contract. A stub with no answer for a call fails loud (stderr note, exit 3) instead of improvising: a missing answer is a corpus bug, never a silent success. Stubs locate the case tree through `FNO_MERGE_CASE_DIR`, so cwd does not matter.

The goldens freeze the Python legs while they live. After the legs retire the frozen files are the contract, and a rewritten oracle cannot re-derive them: a golden can only be captured from the leg it characterizes.
"""

import argparse
import json
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

FIXTURE_DIR = Path(__file__).resolve().parent
TESTS_DIR = FIXTURE_DIR.parents[1]
GOLDEN_DIR = TESTS_DIR / "golden" / "merge_verb"
REPO_ROOT = TESTS_DIR.parents[2]
CORRUPT_BYTES = b"this fixture graph is deliberately corrupt\n"

# Live-session identity and routing variables that must never leak into a
# hermetic child. The child re-pins the FNO_* ones right after stripping.
STRIP_VARS = [
    "CLAUDE_CODE_SESSION_ID",
    "TARGET_SESSION_ID",
    "SESSION_ID",
    "FNO_HARNESS_NAME",
    "FNO_HARNESS_SESSION_ID",
    "CLAUDE_PLUGIN_ROOT",
    "TARGET_MISSION_ID",
    "FNO_AGENTS_BIN",
    "FNO_AGENTS_FRONT",
    "FNO_AGENTS_WORKER",
    "FNO_REPO_ROOT",
    "FNO_CAPTURE_GOLDEN",
    "FNO_TRACKER_BACKEND",
    "FNO_EVENTS_PATH",
    "FNO_STATE_DIR",
    "FNO_CLAIMS_ROOT",
    "FNO_CONFIG",
    "FNO_AGENTS_HOME",
    "FNO_MERGE_CASE_DIR",
    "GIT_DIR",
    "GIT_WORK_TREE",
]

# Answers every merge/hold case can rely on. pr-worktree refusal 3 with "no
# local worktree" is the transport's own not-found dialect. There is no
# default gh api-jq answer: the remote branch delete early-outs unless
# delete_branch_on_merge is set, and only that case carries one.
DEFAULT_ANSWERS = {
    "pr-worktree": {"exit": 3, "stderr": "no local worktree for pr"},
    "claim-acquire": {
        "json": {
            "claim": {
                "key": "merge-lock:stub",
                "holder": "stub",
                "acquired_at": 1760000000000,
                "expires_at": 1760000120000,
                "pid": 123,
                "host": "stub-host",
                "pid_provenance": "ambient",
            }
        }
    },
    "claim-release": {"json": {"released": True}},
    "claim-status": {"json": {"state": "free"}},
}

DEFAULT_FRONT_ANSWERS = {
    "backlog-reconcile": {
        "json": {"closed": [], "closure_bound": [], "closure_claims": []}
    },
    "inbox-law": {"exit": 1, "stderr": "no law matched"},
}

STUB_AGENTS = r"""#!/usr/bin/python3
# Stand-in for the fno-agents binary. Answers every call from an
# answers/op-<op>[-<n>].json file and journals every call, so the capture
# can prove which ops the leg issued and in what order. A call with no
# answer file fails loud: a missing answer is a corpus bug.
import json
import os
import sys


def root():
    return os.environ.get("FNO_MERGE_CASE_DIR") or os.getcwd()


def resolve_op(argv):
    # claim <sub> <key> --json is argv-shaped, not payload-shaped.
    if argv and argv[0] == "claim":
        sub = argv[1] if len(argv) > 1 else ""
        key = argv[2] if (sub == "status" and len(argv) > 2) else None
        return "claim-" + sub, key
    payload = None
    for arg in argv:
        if arg.lstrip()[:1] != "{":
            continue
        try:
            data = json.loads(arg)
        except ValueError:
            continue
        if isinstance(data, dict):
            payload = data
            break
    if payload is None:
        try:
            data = json.loads(sys.stdin.read())
        except Exception:
            data = None
        if isinstance(data, dict):
            payload = data
    verb = argv[0] if argv else ""
    if payload is None:
        return verb, None
    return payload.get("op") or verb, None


def main():
    argv = sys.argv[1:]
    op, key = resolve_op(argv)
    journals = os.path.join(root(), "journals")
    os.makedirs(journals, exist_ok=True)
    counter = os.path.join(journals, "counters.json")
    counts = {}
    if os.path.exists(counter):
        with open(counter) as fh:
            try:
                counts = json.load(fh)
            except ValueError:
                counts = {}
    n = int(counts.get(op, 0)) + 1
    counts[op] = n
    with open(counter, "w") as fh:
        fh.write(json.dumps(counts, sort_keys=True))
    row = {"op": op, "key": key, "n": n}
    with open(os.path.join(journals, "fno-agents.jsonl"), "a") as fh:
        fh.write(json.dumps(row, sort_keys=True) + "\n")
    answers = os.path.join(root(), "answers")
    answer = None
    for name in ("op-%s-%d.json" % (op, n), "op-%s.json" % op):
        path = os.path.join(answers, name)
        if os.path.exists(path):
            with open(path) as fh:
                answer = json.load(fh)
            break
    if answer is None:
        sys.stderr.write("stub fno-agents: no answer for op %s (call %d)\n" % (op, n))
        sys.exit(3)
    stdout = answer.get("stdout") or ""
    if stdout:
        sys.stdout.write(stdout)
    if "json" in answer:
        sys.stdout.write(json.dumps(answer["json"], sort_keys=True) + "\n")
    sys.stdout.flush()
    stderr = answer.get("stderr") or ""
    if stderr:
        sys.stderr.write(stderr)
    sys.stderr.flush()
    sys.exit(int(answer.get("exit") or 0))


main()
"""

STUB_WORKER = r"""#!/usr/bin/python3
# Stand-in for the fno-agents-worker graph-read binary. Serves the rows the
# fixture planted in worker-rows.json; a graph file that exists but does not
# parse answers the unreadable envelope instead.
import json
import os
import sys


def root():
    return os.environ.get("FNO_MERGE_CASE_DIR") or os.getcwd()


def main():
    argv = sys.argv[1:]
    graph = None
    if "--graph" in argv:
        i = argv.index("--graph")
        if i + 1 < len(argv):
            graph = argv[i + 1]
    try:
        sys.stdin.read()
    except Exception:
        pass
    journals = os.path.join(root(), "journals")
    os.makedirs(journals, exist_ok=True)
    with open(os.path.join(journals, "fno-agents-worker.jsonl"), "a") as fh:
        fh.write(json.dumps({"op": "read"}, sort_keys=True) + "\n")
    rows = []
    unreadable = False
    if graph and os.path.exists(graph):
        try:
            with open(graph) as fh:
                json.load(fh)
        except Exception:
            unreadable = True
    if not unreadable:
        rows_path = os.path.join(root(), "worker-rows.json")
        if os.path.exists(rows_path):
            with open(rows_path) as fh:
                rows = json.load(fh)
    if unreadable:
        out = {"ok": False, "error": {"kind": "unreadable", "message": "cannot parse graph file"}}
    else:
        out = {"ok": True, "result": {"rows": rows}}
    sys.stdout.write(json.dumps(out, sort_keys=True) + "\n")
    sys.stdout.flush()


main()
"""

STUB_GH = r"""#!/usr/bin/python3
# Stand-in for the gh CLI. Maps each argv shape to a named shape, journals
# it, then answers from answers/gh-<shape>.json. Unmapped argv fails loud.
# pr merge --disable-auto without an answer is a silent success: the caller
# uses it as a best-effort disarm.
import json
import os
import sys


def root():
    return os.environ.get("FNO_MERGE_CASE_DIR") or os.getcwd()


def shape_of(argv):
    if len(argv) >= 2 and argv[0] == "pr" and argv[1] == "view":
        return "view"
    if len(argv) >= 2 and argv[0] == "pr" and argv[1] == "merge":
        if "--disable-auto" in argv:
            return "merge-disable-auto"
        return "merge"
    if argv and argv[0] == "api":
        if "--jq" in argv:
            return "api-jq"
        return "api"
    return None


def emit(answer):
    stdout = answer.get("stdout") or ""
    if stdout:
        sys.stdout.write(stdout)
    if "json" in answer:
        sys.stdout.write(json.dumps(answer["json"], sort_keys=True) + "\n")
    sys.stdout.flush()
    stderr = answer.get("stderr") or ""
    if stderr:
        sys.stderr.write(stderr)
    sys.stderr.flush()
    sys.exit(int(answer.get("exit") or 0))


def main():
    argv = sys.argv[1:]
    shape = shape_of(argv)
    journals = os.path.join(root(), "journals")
    os.makedirs(journals, exist_ok=True)
    with open(os.path.join(journals, "gh.jsonl"), "a") as fh:
        fh.write(json.dumps({"op": shape}, sort_keys=True) + "\n")
    if shape is None:
        sys.stderr.write("stub gh: unhandled argv: %s\n" % " ".join(argv))
        sys.exit(3)
    path = os.path.join(root(), "answers", "gh-%s.json" % shape)
    if not os.path.exists(path):
        if shape == "merge-disable-auto":
            sys.exit(0)
        sys.stderr.write("stub gh: no answer for shape %s\n" % shape)
        sys.exit(3)
    with open(path) as fh:
        emit(json.load(fh))


main()
"""

STUB_FRONT = r"""#!/usr/bin/python3
# Stand-in for the fno front binary. The payload rides stdin under a fixed
# two-word argv; the op is those words joined. Answers from
# answers/front-<op>.json; a miss fails loud.
import json
import os
import sys


def root():
    return os.environ.get("FNO_MERGE_CASE_DIR") or os.getcwd()


def emit(answer):
    stdout = answer.get("stdout") or ""
    if stdout:
        sys.stdout.write(stdout)
    if "json" in answer:
        sys.stdout.write(json.dumps(answer["json"], sort_keys=True) + "\n")
    sys.stdout.flush()
    stderr = answer.get("stderr") or ""
    if stderr:
        sys.stderr.write(stderr)
    sys.stderr.flush()
    sys.exit(int(answer.get("exit") or 0))


def main():
    argv = sys.argv[1:]
    op = "-".join(argv[:2])
    try:
        sys.stdin.read()
    except Exception:
        pass
    journals = os.path.join(root(), "journals")
    os.makedirs(journals, exist_ok=True)
    with open(os.path.join(journals, "front.jsonl"), "a") as fh:
        fh.write(json.dumps({"op": op}, sort_keys=True) + "\n")
    path = os.path.join(root(), "answers", "front-%s.json" % op)
    if not os.path.exists(path):
        sys.stderr.write("stub fno-front: no answer for op %s\n" % op)
        sys.exit(3)
    with open(path) as fh:
        emit(json.load(fh))


main()
"""

STUBS = {
    "fno-agents": STUB_AGENTS,
    "fno-agents-worker": STUB_WORKER,
    "gh": STUB_GH,
    "fno-front": STUB_FRONT,
}


def slug(label):
    """Mirror of common::slug in tests/common/mod.rs."""
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


def load_cases():
    return json.loads((FIXTURE_DIR / "cases.json").read_text())


def write_stubs(stubbin):
    stubbin.mkdir(parents=True, exist_ok=True)
    for name, body in STUBS.items():
        path = stubbin / name
        path.write_text(body)
        os.chmod(str(path), 0o755)
    return stubbin


def git_env():
    env = dict(os.environ)
    env.update(
        {
            "GIT_CONFIG_GLOBAL": "/dev/null",
            "GIT_CONFIG_SYSTEM": "/dev/null",
            "GIT_AUTHOR_DATE": "2026-10-13T00:00:00Z",
            "GIT_COMMITTER_DATE": "2026-10-13T00:00:00Z",
            "GIT_TERMINAL_PROMPT": "0",
        }
    )
    return env


def git_run(argv, cwd):
    proc = subprocess.run(
        ["git", *argv],
        cwd=str(cwd),
        env=git_env(),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    if proc.returncode != 0:
        raise SystemExit("git %s failed: %s" % (" ".join(argv), proc.stderr.strip()))
    return proc.stdout.strip()


def build_git(case_dir):
    git_run(["init", "-q", "-b", "main"], case_dir)
    git_run(["config", "user.email", "fixture@example.com"], case_dir)
    git_run(["config", "user.name", "Fixture"], case_dir)
    git_run(["commit", "--allow-empty", "-q", "-m", "seed"], case_dir)
    return git_run(["rev-parse", "HEAD"], case_dir)


def build_origin(case_dir):
    bare = case_dir / "origin.git"
    git_run(["clone", "--bare", "-q", str(case_dir), str(bare)], case_dir)
    git_run(
        ["remote", "add", "origin", "https://github.com/acme/widget.git"], case_dir
    )
    # The remote name stays the public URL; the insteadOf rewrite sends any
    # real network access to the throwaway bare clone at git run time.
    git_run(
        [
            "config",
            "url.%s.insteadOf" % bare,
            "https://github.com/acme/widget.git",
        ],
        case_dir,
    )


def substitute_placeholders(case_dir, head_sha):
    for dirpath, dirnames, filenames in os.walk(str(case_dir)):
        dirnames[:] = [d for d in dirnames if d not in (".git", "origin.git")]
        for name in filenames:
            path = Path(dirpath) / name
            data = path.read_bytes()
            if b"__CASE_DIR__" not in data and b"__HEAD_SHA__" not in data:
                continue
            try:
                text = data.decode("utf-8")
            except UnicodeDecodeError:
                continue
            text = text.replace("__CASE_DIR__", str(case_dir))
            if head_sha:
                text = text.replace("__HEAD_SHA__", head_sha)
            path.write_text(text)


def build_case(root_path, case, index):
    case_dir = root_path / ("case-%02d" % index)
    case_dir.mkdir(parents=True)
    # state_dir is a TOP-LEVEL key: under a [table] header the config loader
    # reads it as a table field and warns it is unmodeled.
    (case_dir / "config.toml").write_text('state_dir = "%s"\n' % case_dir)
    fno_dir = case_dir / ".fno"
    fno_dir.mkdir()
    config_lines = ["[auto_merge]"]
    for key in sorted(case.get("config") or {}):
        config_lines.append("%s = %s" % (key, json.dumps(case["config"][key])))
    (fno_dir / "config.toml").write_text("\n".join(config_lines) + "\n")
    state = case.get("state") or {}
    if state:
        (fno_dir / "target-state.md").write_text(
            "".join("%s: %s\n" % (key, state[key]) for key in state)
        )
    # The corpus carries the gh answer map at top level or nested inside
    # answers; normalize both spellings here.
    raw_answers = dict(case.get("answers") or {})
    gh_map = dict(case.get("gh") or {})
    gh_map.update(raw_answers.pop("gh", {}) or {})
    answers = dict(DEFAULT_ANSWERS)
    answers.update(raw_answers)
    answers_dir = case_dir / "answers"
    answers_dir.mkdir()
    for key, answer in answers.items():
        (answers_dir / ("op-%s.json" % key)).write_text(
            json.dumps(answer, sort_keys=True) + "\n"
        )
    for shape, answer in gh_map.items():
        (answers_dir / ("gh-%s.json" % shape)).write_text(
            json.dumps(answer, sort_keys=True) + "\n"
        )
    for op, answer in DEFAULT_FRONT_ANSWERS.items():
        (answers_dir / ("front-%s.json" % op)).write_text(
            json.dumps(answer, sort_keys=True) + "\n"
        )
    rows = case.get("rows") or []
    (case_dir / "worker-rows.json").write_text(json.dumps(rows, sort_keys=True) + "\n")
    (case_dir / "journals").mkdir()
    (case_dir / "home" / "agents-home").mkdir(parents=True)
    (case_dir / "claims").mkdir()
    if case.get("corrupt_graph"):
        (case_dir / "graph.json").write_bytes(CORRUPT_BYTES)
        (case_dir / "graph.db").write_bytes(CORRUPT_BYTES)
    elif rows:
        # The worker stub serves rows from worker-rows.json; the graph file
        # only needs to exist and parse for the readable path.
        (case_dir / "graph.json").write_text('{"rows": []}\n')
    head_sha = None
    if case.get("git"):
        head_sha = build_git(case_dir)
    if case.get("origin"):
        build_origin(case_dir)
    (case_dir / "case.json").write_text(
        json.dumps(case, sort_keys=True, indent=2) + "\n"
    )
    substitute_placeholders(case_dir, head_sha)
    return case_dir


def run_hold_door(pr_number):
    # Mirrors the hold door in cli/src/fno/pr/cli.py: resolve the worktree,
    # read the hold, echo + exit 3 on a hold, or print the clear line.
    import typer

    from fno.pr import _hold, _review_hold

    repo = os.getcwd()
    try:
        worktree = _review_hold.resolve_pr_worktree(pr_number, repo)
        reason = _hold.merge_hold_reason(pr_number, worktree or repo)
        if reason:
            typer.echo(reason, err=True)
            raise typer.Exit(code=3)
        typer.echo("PR %d: no plan dispatch hold" % pr_number)
        return 0
    except typer.Exit as exc:
        return int(getattr(exc, "exit_code", getattr(exc, "code", 1)) or 0)


def run_fence():
    from fno.claims.incarnation import (
        incarnation_fence_blocks,
        resolve_fence_session_uuid,
    )

    uuid = resolve_fence_session_uuid(os.getcwd())
    blocked, reason = incarnation_fence_blocks(uuid)
    sys.stdout.write("blocked=%s\nreason=%s\n" % (blocked, reason))
    return 0


def run_entry(case):
    entry = case.get("entry")
    if entry == "merge":
        from fno.pr import _merge

        argv = ([case["pr"]] if case.get("pr") else []) + list(case.get("argv") or [])
        return _merge.run_merge(
            argv, cwd=os.getcwd(), authority=case.get("authority") or "manifest"
        )
    if entry == "hold":
        return run_hold_door(int(case["pr"]))
    if entry == "result":
        from fno.pr import _merge_result

        return _merge_result.run_merge_result_check(int(case["pr"]), cwd=os.getcwd())
    if entry == "fence":
        return run_fence()
    raise SystemExit("unknown entry: %r" % entry)


def cmd_case(case_dir):
    case_dir = Path(case_dir).resolve()
    case = json.loads((case_dir / "case.json").read_text())
    stubbin = case_dir.parent / "stub-bin"
    env = dict(os.environ)
    for var in STRIP_VARS:
        env.pop(var, None)
    env["FNO_CONFIG"] = str(case_dir / "config.toml")
    env["FNO_STATE_DIR"] = str(case_dir)
    env["FNO_AGENTS_HOME"] = str(case_dir / "home" / "agents-home")
    env["HOME"] = str(case_dir / "home")
    env["FNO_EVENTS_PATH"] = str(case_dir / "journals" / "events.jsonl")
    env["FNO_CLAIMS_ROOT"] = str(case_dir / "claims")
    env["FNO_MERGE_CASE_DIR"] = str(case_dir)
    env["FNO_AGENTS_BIN"] = str(stubbin / "fno-agents")
    env["FNO_AGENTS_WORKER"] = str(stubbin / "fno-agents-worker")
    env["FNO_AGENTS_FRONT"] = str(stubbin / "fno-front")
    env["GIT_TERMINAL_PROMPT"] = "0"
    env["GIT_CONFIG_GLOBAL"] = "/dev/null"
    env["GIT_CONFIG_SYSTEM"] = "/dev/null"
    if case.get("path") == "empty":
        env["PATH"] = ""
    else:
        env["PATH"] = "%s:/usr/bin:/bin" % stubbin
    env.update(case.get("env") or {})
    os.environ.clear()
    os.environ.update(env)
    os.chdir(str(case_dir))
    saved_out, saved_err = os.dup(1), os.dup(2)
    out_fd = os.open(
        str(case_dir / "leg-out.txt"), os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600
    )
    err_fd = os.open(
        str(case_dir / "leg-err.txt"), os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600
    )
    sys.stdout.flush()
    sys.stderr.flush()
    os.dup2(out_fd, 1)
    os.dup2(err_fd, 2)
    os.close(out_fd)
    os.close(err_fd)
    try:
        rc = run_entry(case)
    except SystemExit as exc:
        # A leg verdict may arrive as SystemExit; the envelope must still
        # print or the parent reads a missing envelope as the failure.
        rc = 0 if exc.code is None else exc.code
        if not isinstance(rc, int):
            rc = 1
    sys.stdout.flush()
    sys.stderr.flush()
    os.dup2(saved_out, 1)
    os.dup2(saved_err, 2)
    os.close(saved_out)
    os.close(saved_err)
    envelope = {
        "exit": rc,
        "stdout": (case_dir / "leg-out.txt").read_text(),
        "stderr": (case_dir / "leg-err.txt").read_text(),
    }
    sys.stdout.write(json.dumps(envelope) + "\n")
    sys.stdout.flush()


def collapse(rows):
    """First-appearance order; a consecutive run longer than one becomes
    `<op>*` so a wall-clock poll loop freezes no local iteration count."""
    out = []
    for row in rows:
        if out and out[-1][0] == row:
            out[-1][1] += 1
        else:
            out.append([row, 1])
    return [op if n == 1 else "%s*" % op for op, n in out]


def fold_calls(case_dir):
    def read_rows(name):
        path = case_dir / "journals" / name
        if not path.exists():
            return []
        rows = []
        for line in path.read_text().splitlines():
            line = line.strip()
            if not line:
                continue
            try:
                rows.append(json.loads(line))
            except ValueError:
                rows.append({"op": "<unparsed>"})
        return rows

    agents = read_rows("fno-agents.jsonl")
    return {
        "agents": collapse([row.get("op") for row in agents]),
        "worker": collapse([row.get("op") for row in read_rows("fno-agents-worker.jsonl")]),
        "gh": collapse([row.get("op") for row in read_rows("gh.jsonl")]),
        "front": collapse([row.get("op") for row in read_rows("front.jsonl")]),
        "asked_keys": collapse(
            [row.get("key") for row in agents if row.get("op") == "claim-status"]
        ),
        "events": collapse([row.get("type") for row in read_rows("events.jsonl")]),
    }


def mask_text(text, case_dir):
    for spelling in (str(case_dir), os.path.realpath(str(case_dir))):
        text = text.replace(spelling, "<CASE>")
    text = re.sub(r"\(pid=[^)]*\)", "(pid=<ERR>)", text)
    text = re.sub(r"graph is unreadable: [^;]*", "graph is unreadable: <ERR>", text)
    text = re.sub(r"held \d+s", "held <T>s", text)
    return text


def run_child(case_dir):
    proc = subprocess.run(
        [sys.executable, str(Path(__file__).resolve()), "--case", str(case_dir)],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        stdin=subprocess.DEVNULL,
        env={**os.environ, "PYTHONPATH": str(REPO_ROOT / "cli" / "src")},
        timeout=420,
    )
    if proc.returncode != 0:
        leg_err = case_dir / "leg-err.txt"
        raise SystemExit(
            "child failed for %s (rc %s)\nchild stderr:\n%s\nleg-err:\n%s"
            % (
                case_dir.name,
                proc.returncode,
                proc.stderr.decode(errors="replace"),
                leg_err.read_text() if leg_err.exists() else "<no leg-err.txt>",
            )
        )
    out_lines = [
        line for line in proc.stdout.decode().splitlines() if line.strip()
    ]
    if not out_lines:
        raise SystemExit("child printed no envelope for %s" % case_dir.name)
    return json.loads(out_lines[-1])


def capture(only):
    root_path = Path(tempfile.mkdtemp(prefix="merge-verb-"))
    print("fixture root: %s" % root_path, flush=True)
    write_stubs(root_path / "stub-bin")
    cases = load_cases()
    frozen = 0
    for index, case in enumerate(cases):
        key = slug(case["label"])
        if only and key != only:
            continue
        case_dir = build_case(root_path, case, index)
        for stale in (case_dir / "journals").glob("*.jsonl"):
            stale.unlink()
        envelope = run_child(case_dir)
        calls = mask_text(json.dumps(fold_calls(case_dir), sort_keys=True), case_dir)
        out = mask_text(envelope.get("stdout", ""), case_dir)
        err = mask_text(envelope.get("stderr", ""), case_dir)
        GOLDEN_DIR.mkdir(parents=True, exist_ok=True)
        (GOLDEN_DIR / (key + ".exit")).write_text("%d\n" % envelope["exit"])
        (GOLDEN_DIR / (key + ".out")).write_text(out)
        if err:
            (GOLDEN_DIR / (key + ".err")).write_text(err)
        (GOLDEN_DIR / (key + ".calls")).write_text(calls + "\n")
        frozen += 1
        print(
            "froze %s -> %s (exit %s)" % (case["label"], key, envelope["exit"]),
            flush=True,
        )
    if only and frozen == 0:
        raise SystemExit("--only %s matched no case" % only)


def build_only(root_arg):
    root_path = Path(root_arg).resolve()
    root_path.mkdir(parents=True, exist_ok=True)
    stubbin = write_stubs(root_path / "stub-bin")
    listing = []
    for index, case in enumerate(load_cases()):
        case_dir = build_case(root_path, case, index)
        listing.append(
            {"label": case["label"], "slug": slug(case["label"]), "dir": str(case_dir)}
        )
    print(
        json.dumps(
            {"root": str(root_path), "stubbin": str(stubbin), "cases": listing},
            indent=2,
        )
    )


def main():
    parser = argparse.ArgumentParser(
        description="capture the merge-verb golden corpus from the live Python legs"
    )
    parser.add_argument("--case", help="child mode: run one built case dir")
    parser.add_argument(
        "--build-only",
        metavar="ROOT",
        help="build the fixture tree under ROOT and print a JSON map",
    )
    parser.add_argument("--only", metavar="SLUG", help="capture one case by golden key")
    args = parser.parse_args()
    if args.case:
        cmd_case(args.case)
    elif args.build_only:
        build_only(args.build_only)
    else:
        capture(args.only)


if __name__ == "__main__":
    main()
