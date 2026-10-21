"""Capture driver for the post-merge golden corpus (cases.json).

Modes. Default capture builds every case under a throwaway root, runs the
live Python leg in a hermetic child, and freezes the arm-shaped envelope as
goldens under tests/golden/post_merge. `--case DIR` is the child mode: it
re-applies the environment pins, runs one entry, and prints one
`{"exit", "stdout"}` envelope as the last stdout line. `--build-only ROOT`
builds the fixture tree without running any leg and prints a JSON map the
Rust parity test reuses.

Each case names a `subject`; the subject selects the Python leg. Goldens
freeze the pr-watch arm's output SHAPE (compact, sort_keys), so the Rust arm
must produce byte-identical bytes. Paths that vary per run mask to <CASE>.
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
GOLDEN_DIR = TESTS_DIR / "golden" / "post_merge"
REPO_ROOT = TESTS_DIR.parents[2]
CORRUPT_BYTES = b"this fixture ledger is deliberately corrupt\n"

# Live-session identity and routing vars that must never reach a hermetic
# child (the merge-command driver's list, trimmed to what this corpus needs).
STRIP_VARS = [
    "CLAUDE_CODE_SESSION_ID",
    "TARGET_SESSION_ID",
    "SESSION_ID",
    "FNO_HARNESS_NAME",
    "FNO_HARNESS_SESSION_ID",
    "CLAUDE_PLUGIN_ROOT",
    "FNO_AGENTS_BIN",
    "FNO_BIN",
    "FNO_AGENTS_HOME",
    "FNO_CLAIMS_ROOT",
    "FNO_STATE_DIR",
    "FNO_EVENTS_PATH",
    "FNO_TRACKER_BACKEND",
    "FNO_POST_MERGE_CASE",
    "GIT_DIR",
    "GIT_WORK_TREE",
]


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
    cases = json.loads((FIXTURE_DIR / "cases.json").read_text())
    # The ritual cases are Python literals (they outgrew the JSON file).
    import importlib.util

    spec = importlib.util.spec_from_file_location(
        "cases_ritual", FIXTURE_DIR / "cases_ritual.py"
    )
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return cases + list(mod.CASES)


def build_pr_sessions_case(root_path, case, index):
    case_dir = root_path / ("case-%02d" % index)
    case_dir.mkdir(parents=True)
    kind = case.get("ledger_kind") or "write"
    if kind == "write":
        (case_dir / "ledger.json").write_text(
            json.dumps(case["ledger"], sort_keys=True) + "\n"
        )
    elif kind == "corrupt":
        (case_dir / "ledger.json").write_bytes(CORRUPT_BYTES)
    elif kind == "dict-no-entries":
        (case_dir / "ledger.json").write_text("{}\n")
    # "missing": no ledger file is written.
    (case_dir / "payload.json").write_text(
        json.dumps(
            {
                "ledger": str(case_dir / "ledger.json"),
                "pr": case["pr"],
                "repo_slug": case["slug"],
            }
        )
        + "\n"
    )
    (case_dir / "case.json").write_text(json.dumps(case, sort_keys=True, indent=2) + "\n")
    return case_dir


def build_case(root_path, case, index):
    subject = case.get("subject") or "pr-sessions"
    if subject == "pr-sessions":
        return build_pr_sessions_case(root_path, case, index)
    if subject == "mint":
        return build_mint_case(root_path, case, index)
    if subject == "ritual":
        return build_ritual_case(root_path, case, index)
    raise SystemExit("unknown subject: %r" % subject)


def build_mint_case(root_path, case, index):
    case_dir = root_path / ("case-%02d" % index)
    (case_dir / "agents-home").mkdir(parents=True)
    (case_dir / "payload.json").write_text(
        json.dumps(case["payload"], sort_keys=True) + "\n"
    )
    (case_dir / "case.json").write_text(json.dumps(case, sort_keys=True, indent=2) + "\n")
    return case_dir


# -- the ritual subject ------------------------------------------------------

STUB_FNO = r"""#!/usr/bin/python3
# Shared stub for fno / fno-py: answers each call from
# answers/op-<op>.json and journals it. Unknown argv fails loud (exit 3).
import json, os, sys

root = os.environ.get("FNO_POST_MERGE_CASE") or os.getcwd()
argv = sys.argv[1:]
op = None
if len(argv) >= 2 and argv[0] == "backlog" and argv[1] == "reconcile":
    op = "backlog-reconcile"
elif len(argv) >= 2 and argv[0] == "backlog" and argv[1] == "advance":
    op = "backlog-advance"
elif len(argv) >= 2 and argv[0] == "backlog" and argv[1] == "find":
    op = "backlog-find"
elif len(argv) >= 2 and argv[0] == "retro" and argv[1] == "run":
    op = "retro-run"
elif len(argv) >= 2 and argv[0] == "do" and argv[1] == "plan":
    op = "do-plan"
elif len(argv) >= 2 and argv[0] == "doctor" and argv[1] == "skill-diff":
    op = "doctor-skill-diff"
elif len(argv) >= 3 and argv[0] == "do" and argv[1] == "pr" and argv[2] == "sync-canonical":
    op = "do-pr-sync"
elif len(argv) >= 3 and argv[0] == "agents" and argv[1] == "claim" and argv[2] == "acquire":
    op = "claim-acquire"
elif len(argv) >= 3 and argv[0] == "agents" and argv[1] == "claim" and argv[2] == "release":
    op = "claim-release"
elif len(argv) >= 2 and argv[0] == "agents" and argv[1] == "list":
    op = "agents-list"
elif len(argv) >= 2 and argv[0] == "agents" and argv[1] == "spawn":
    op = "agents-spawn"
if op is None:
    sys.stderr.write("stub fno: unhandled argv: %s\n" % " ".join(argv[:8]))
    sys.exit(3)
journals = os.path.join(root, "journals")
os.makedirs(journals, exist_ok=True)
with open(os.path.join(journals, "stub.jsonl"), "a") as fh:
    fh.write(json.dumps({"op": op, "argv": argv[:10]}, sort_keys=True) + "\n")
path = os.path.join(root, "answers", "op-%s.json" % op)
if not os.path.exists(path):
    sys.stderr.write("stub fno: no answer for op %s\n" % op)
    sys.exit(3)
with open(path) as fh:
    answer = json.load(fh)
out = answer.get("stdout") or ""
if out:
    sys.stdout.write(out)
if "json" in answer:
    sys.stdout.write(json.dumps(answer["json"], sort_keys=True) + "\n")
sys.stdout.flush()
err = answer.get("stderr") or ""
if err:
    sys.stderr.write(err)
sys.exit(int(answer.get("exit") or 0))
"""

STUB_GH = r"""#!/usr/bin/python3
# Stub gh: shapes pr-view-state, pr-view-diff, pr-list, repo-view.
import json, os, sys

root = os.environ.get("FNO_POST_MERGE_CASE") or os.getcwd()
argv = sys.argv[1:]
shape = None
if len(argv) >= 2 and argv[0] == "pr" and argv[1] == "view":
    joined = " ".join(argv)
    if "additions,deletions,changedFiles" in joined:
        shape = "pr-view-diff"
    else:
        shape = "pr-view-state"
elif len(argv) >= 2 and argv[0] == "pr" and argv[1] == "list":
    shape = "pr-list"
elif len(argv) >= 2 and argv[0] == "repo" and argv[1] == "view":
    shape = "repo-view"
if shape is None:
    sys.stderr.write("stub gh: unhandled argv: %s\n" % " ".join(argv[:8]))
    sys.exit(3)
journals = os.path.join(root, "journals")
os.makedirs(journals, exist_ok=True)
with open(os.path.join(journals, "gh.jsonl"), "a") as fh:
    fh.write(json.dumps({"op": shape}, sort_keys=True) + "\n")
path = os.path.join(root, "answers", "gh-%s.json" % shape)
if not os.path.exists(path):
    sys.stderr.write("stub gh: no answer for shape %s\n" % shape)
    sys.exit(3)
with open(path) as fh:
    answer = json.load(fh)
if "json" in answer:
    sys.stdout.write(json.dumps(answer["json"], sort_keys=True) + "\n")
out = answer.get("stdout") or ""
if out:
    sys.stdout.write(out)
sys.exit(int(answer.get("exit") or 0))
"""

STUB_AGENTS = r"""#!/usr/bin/python3
# Stub fno-agents: answers name-mint, name-codes and merge-cleanup-mint.
import hashlib, json, os, sys

root = os.environ.get("FNO_POST_MERGE_CASE") or os.getcwd()
argv = sys.argv[1:]
journals = os.path.join(root, "journals")
os.makedirs(journals, exist_ok=True)
with open(os.path.join(journals, "agents.jsonl"), "a") as fh:
    fh.write(json.dumps({"argv": argv[:10]}, sort_keys=True) + "\n")
if "name-codes" in argv:
    codes = os.path.join(root, "answers", "name-codes.json")
    with open(codes) as fh:
        sys.stdout.write(json.dumps(json.load(fh), sort_keys=True) + "\n")
    sys.exit(0)
if "name-mint" in argv:
    name = os.path.join(root, "answers", "name-mint.txt")
    with open(name) as fh:
        sys.stdout.write(fh.read())
    sys.exit(0)
if "merge-cleanup-mint" in argv:
    payload = json.loads(sys.stdin.read())
    identity = "|".join(
        [str(payload.get("project")), str(payload.get("pr")), payload.get("branch") or ""]
    )
    rid = "merge-cleanup-" + hashlib.sha256(identity.encode()).hexdigest()[:20]
    sys.stdout.write(json.dumps({"request_id": rid}, sort_keys=True) + "\n")
    sys.exit(0)
sys.stderr.write("stub fno-agents: unhandled argv: %s\n" % " ".join(argv[:8]))
sys.exit(3)
"""


def git_run(argv, cwd):
    import subprocess as sp

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
    proc = sp.run(
        ["git", *argv], cwd=str(cwd), env=env,
        stdout=sp.PIPE, stderr=sp.PIPE, text=True,
    )
    if proc.returncode != 0:
        raise SystemExit("git %s failed: %s" % (" ".join(argv), proc.stderr.strip()))
    return proc.stdout.strip()


def build_ritual_case(root_path, case, index):
    case_dir = root_path / ("case-%02d" % index)
    for d in ("home", "agents-home", "claims", "journals", "answers", "stub-bin"):
        (case_dir / d).mkdir(parents=True)
    for name, body in (
        ("fno", STUB_FNO), ("fno-py", STUB_FNO), ("gh", STUB_GH), ("fno-agents", STUB_AGENTS),
    ):
        p = case_dir / "stub-bin" / name
        p.write_text(body)
        os.chmod(str(p), 0o755)
    cfg = case.get("config") or {}
    lines = []
    lines.append("[post_merge]")
    for key in sorted(cfg.get("post_merge") or {}):
        lines.append("%s = %s" % (key, json.dumps(cfg["post_merge"][key])))
    lines.append("")
    lines.append("[autonomy]")
    for key in sorted(cfg.get("autonomy") or {}):
        lines.append("%s = %s" % (key, json.dumps(cfg["autonomy"][key])))
    config_text = "\n".join(lines) + "\n"
    (case_dir / "config.toml").write_text(config_text)
    (case_dir / ".fno").mkdir(exist_ok=True)
    (case_dir / ".fno" / "config.toml").write_text(config_text)
    for op, answer in (case.get("op_answers") or {}).items():
        (case_dir / "answers" / ("op-%s.json" % op)).write_text(
            json.dumps(answer, sort_keys=True) + "\n"
        )
    for shape, answer in (case.get("gh") or {}).items():
        (case_dir / "answers" / ("gh-%s.json" % shape)).write_text(
            json.dumps(answer, sort_keys=True) + "\n"
        )
    (case_dir / "answers" / "name-codes.json").write_text(json.dumps({
        "sources": ["pm"], "verbs": ["r"], "word_codes": {}, "provenance": [],
    }, sort_keys=True) + "\n")
    (case_dir / "answers" / "name-mint.txt").write_text("pm-r-x-1-pr-42\n")
    git_run(["init", "-q", "-b", "main"], case_dir)
    git_run(["config", "user.email", "fixture@example.com"], case_dir)
    git_run(["config", "user.name", "Fixture"], case_dir)
    git_run(["commit", "--allow-empty", "-q", "-m", "seed"], case_dir)
    git_run(["remote", "add", "origin", "https://github.com/o/r.git"], case_dir)
    # The hermetic env the child and the Rust parity test both apply.
    case["env"] = {
        "FNO_POST_MERGE_CASE": str(case_dir),
        "PATH": "%s:/usr/bin:/bin" % (case_dir / "stub-bin"),
        "FNO_BIN": str(case_dir / "stub-bin" / "fno"),
        "FNO_AGENTS_BIN": str(case_dir / "stub-bin" / "fno-agents"),
        "HOME": str(case_dir / "home"),
        "FNO_AGENTS_HOME": str(case_dir / "agents-home"),
        "FNO_CLAIMS_ROOT": str(case_dir / "claims"),
        "GIT_CONFIG_GLOBAL": "/dev/null",
        "GIT_CONFIG_SYSTEM": "/dev/null",
        "GIT_TERMINAL_PROMPT": "0",
    }
    (case_dir / "case.json").write_text(json.dumps(case, sort_keys=True, indent=2) + "\n")
    return case_dir


def mask(text, case_dir):
    for spelling in (str(case_dir), os.path.realpath(str(case_dir))):
        text = text.replace(spelling, "<CASE>")
    text = re.sub(r"ledger unreadable: [^\"]*", "ledger unreadable: <ERR>", text)
    # The Python daemon-envelope emit hopped through `fno doctor event
    # emit-envelope`; the stub binary can't answer it, so the capture saw a
    # warning that production never prints (the real binary handles the
    # argv, and the native port writes the envelope in-process). Mask the
    # artifact: the contract is a silent stderr.
    text = re.sub(
        r"fno agents: warning: daemon envelope [a-z_]+:"
        r" stub fno: unhandled argv: doctor event emit-envelope[^\n]*\n?",
        "",
        text,
    )
    return text


def run_child(case_dir, case=None):
    env = {**os.environ, "PYTHONPATH": str(REPO_ROOT / "cli" / "src")}
    case = case or json.loads((case_dir / "case.json").read_text())
    for var in STRIP_VARS:
        env.pop(var, None)
    env.update(case.get("env") or {})
    proc = subprocess.run(
        [sys.executable, str(Path(__file__).resolve()), "--case", str(case_dir)],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        stdin=subprocess.DEVNULL,
        env=env,
        timeout=420,
    )
    if proc.returncode != 0:
        raise SystemExit(
            "child failed for %s (rc %s)\nchild stderr:\n%s"
            % (case_dir.name, proc.returncode, proc.stderr.decode(errors="replace"))
        )
    out_lines = [line for line in proc.stdout.decode().splitlines() if line.strip()]
    if not out_lines:
        raise SystemExit("child printed no envelope for %s" % case_dir.name)
    return json.loads(out_lines[-1])


def pin_module(ref, path, name):
    """The capture pin: the last commit that still carried the Python leg.
    A golden can only be re-frozen by running THIS driver at that ref."""
    import subprocess as sp
    import types

    raw = sp.run(
        ["git", "show", "%s:%s" % (ref, path)],
        capture_output=True, text=True, check=True, cwd=str(REPO_ROOT),
    ).stdout
    mod = types.ModuleType(name)
    # Register BEFORE exec: module-level @dataclass resolves the module
    # through sys.modules while the body is still executing.
    sys.modules[name] = mod
    if "." in name:
        parent = name.rsplit(".", 1)[0]
        setattr(sys.modules.get(parent) or __import__(parent), name.rsplit(".", 1)[1], mod)
    exec(compile(raw, path, "exec"), mod.__dict__)
    return mod


def freeze(key, envelope, case_dir):
    GOLDEN_DIR.mkdir(parents=True, exist_ok=True)
    (GOLDEN_DIR / (key + ".exit")).write_text("%d\n" % envelope["exit"])
    out = mask(envelope.get("stdout", ""), case_dir)
    if out:
        (GOLDEN_DIR / (key + ".out")).write_text(out)
    err = envelope.get("stderr") or ""
    if err:
        (GOLDEN_DIR / (key + ".err")).write_text(mask(err, case_dir))


def capture(only):
    root_path = Path(tempfile.mkdtemp(prefix="post-merge-"))
    print("fixture root: %s" % root_path, flush=True)
    frozen = 0
    for index, case in enumerate(load_cases()):
        key = slug(case["label"])
        if only and key != only:
            continue
        case_dir = build_case(root_path, case, index)
        try:
            envelope = run_child(case_dir)
        except SystemExit as exc:
            # A dead oracle (its module deleted by the port) only bars a
            # RE-freeze of its cases; the frozen goldens on disk stand.
            print("skip %s (oracle gone): %s" % (case["label"], exc), flush=True)
            continue
        freeze(key, envelope, case_dir)
        frozen += 1
        print("froze %s -> %s (exit %s)" % (case["label"], key, envelope["exit"]), flush=True)
    if only and frozen == 0:
        raise SystemExit("--only %s matched no case" % only)


def build_only(root_arg):
    root_path = Path(root_arg).resolve()
    root_path.mkdir(parents=True, exist_ok=True)
    listing = []
    for index, case in enumerate(load_cases()):
        case_dir = build_case(root_path, case, index)
        listing.append(
            {
                "label": case["label"],
                "slug": slug(case["label"]),
                "dir": str(case_dir),
                "subject": case.get("subject") or "pr-sessions",
            }
        )
    print(
        json.dumps({"root": str(root_path), "cases": listing}, indent=2),
        flush=True,
    )


def run_entry(case):
    subject = case.get("subject") or "pr-sessions"
    if subject == "pr-sessions":
        try:
            from fno.ledger_join import reason_is_infra_failure, resolve_pr_sessions
        except ImportError as exc:
            raise SystemExit(
                "the pr-sessions oracle is gone; its goldens are frozen on disk"
                " and can never be re-frozen (%s)" % exc
            )

        payload = json.loads((Path.cwd() / "payload.json").read_text())
        sessions, reason = resolve_pr_sessions(
            Path(payload["ledger"]), payload["pr"], payload["repo_slug"]
        )
        envelope = {
            "exit": 0,
            "stdout": json.dumps(
                {
                    "infra": reason_is_infra_failure(reason),
                    "reason": reason,
                    "sessions": sessions,
                },
                sort_keys=True,
                separators=(",", ":"),
            )
            + "\n",
            "stderr": "",
        }
        return envelope
    if subject == "mint":
        import subprocess as sp
        import types

        # The capture pin: the last commit that still carried the Python mint
        # (this PR's task 2.1 deleted it from events.py). A golden can only be
        # re-frozen by running THIS driver at that ref.
        pin = "6ef818c913"
        raw = sp.run(
            ["git", "show", pin + ":cli/src/fno/agents/events.py"],
            capture_output=True, text=True, check=True, cwd=str(REPO_ROOT),
        ).stdout
        import fno.agents.events as live_events

        old = types.ModuleType("fno.agents.events")
        exec(compile(raw, "events_capture_pin.py", "exec"), old.__dict__)
        kwargs = json.loads((Path.cwd() / "payload.json").read_text())

        journal = Path.cwd() / "capture-journal.jsonl"

        def write_row(kind, data, source="python"):
            with journal.open("a") as fh:
                fh.write(
                    json.dumps(
                        {"ts": "<TS>", "type": kind, "source": source, "data": data},
                        sort_keys=True, separators=(",", ":"),
                    )
                    + "\n"
                )

        old._emit_daemon_envelope = write_row
        kwargs.setdefault(
            "candidate_row_names",
            old.rows_for_cleanup(kwargs.get("worktree"), kwargs.get("node_ids") or []),
        )
        old.emit_merge_cleanup_requested(**kwargs)
        rows = [json.loads(ln) for ln in journal.read_text().splitlines() if ln.strip()]
        requested = [r for r in rows if r["type"] == "merge_cleanup_requested"]
        data = dict(requested[-1]["data"])
        if kwargs.get("merged_at") is None:
            data["merged_at"] = "<STAMP>"
        envelope = {
            "exit": 0,
            "stdout": json.dumps(data, sort_keys=True, separators=(",", ":")) + "\n",
            "stderr": "",
        }
        return envelope
    if subject == "ritual":
        # The oracle went native in this PR; the pin restores BOTH the old
        # ritual and the old events helpers it imports. A golden can only be
        # re-frozen by running THIS driver at that ref.
        try:
            from fno.pr import _ritual as live
        except ImportError:
            live = None
        if live is None:
            pin_module("6ef818c913", "cli/src/fno/agents/events.py", "fno.agents.events")
            live = pin_module("6ef818c913", "cli/src/fno/pr/_ritual.py", "fno.pr._ritual")
        rc = live.run_ritual(case["pr"], bool(case.get("autonomous")), cwd=Path.cwd())
        envelope = {
            "exit": rc,
            "stdout": "",
            "stderr": "",
        }
        # Receipts print through typer to the child's stdout; the parent
        # redirects the child's streams to these files, so read them back.
        # Flush first: the receipts sit in this process's stdout buffer.
        sys.stdout.flush()
        sys.stderr.flush()
        envelope["stdout"] = (Path.cwd() / "leg-out.txt").read_text()
        envelope["stderr"] = (Path.cwd() / "leg-err.txt").read_text()
        return envelope
    raise SystemExit("unknown subject: %r" % subject)


def cmd_case(case_dir):
    case_dir = Path(case_dir).resolve()
    case = json.loads((case_dir / "case.json").read_text())
    os.chdir(str(case_dir))
    sys.path.insert(0, str(REPO_ROOT / "cli" / "src"))
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
        envelope = run_entry(case)
    finally:
        sys.stdout.flush()
        sys.stderr.flush()
        os.dup2(saved_out, 1)
        os.dup2(saved_err, 2)
        os.close(saved_out)
        os.close(saved_err)
    # The envelope travels on the REAL stdout, after the leg streams are
    # restored; run_child reads it as the last stdout line.
    sys.stdout.write(json.dumps(envelope) + "\n")


def main():
    parser = argparse.ArgumentParser(
        description="capture the post-merge golden corpus from the live Python legs"
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
