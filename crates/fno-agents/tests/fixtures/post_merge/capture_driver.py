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
    raise SystemExit("unknown subject: %r" % subject)


def build_mint_case(root_path, case, index):
    case_dir = root_path / ("case-%02d" % index)
    (case_dir / "agents-home").mkdir(parents=True)
    (case_dir / "payload.json").write_text(
        json.dumps(case["payload"], sort_keys=True) + "\n"
    )
    (case_dir / "case.json").write_text(json.dumps(case, sort_keys=True, indent=2) + "\n")
    return case_dir


def mask(text, case_dir):
    for spelling in (str(case_dir), os.path.realpath(str(case_dir))):
        text = text.replace(spelling, "<CASE>")
    text = re.sub(r"ledger unreadable: [^\"]*", "ledger unreadable: <ERR>", text)
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
        raise SystemExit(
            "child failed for %s (rc %s)\nchild stderr:\n%s"
            % (case_dir.name, proc.returncode, proc.stderr.decode(errors="replace"))
        )
    out_lines = [line for line in proc.stdout.decode().splitlines() if line.strip()]
    if not out_lines:
        raise SystemExit("child printed no envelope for %s" % case_dir.name)
    return json.loads(out_lines[-1])


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
            {"label": case["label"], "slug": slug(case["label"]), "dir": str(case_dir)}
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
        sys.stdout.write(json.dumps(envelope) + "\n")
        return
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
        sys.stdout.write(json.dumps(envelope) + "\n")
        return
    raise SystemExit("unknown subject: %r" % subject)


def cmd_case(case_dir):
    case_dir = Path(case_dir).resolve()
    case = json.loads((case_dir / "case.json").read_text())
    os.chdir(str(case_dir))
    sys.path.insert(0, str(REPO_ROOT / "cli" / "src"))
    run_entry(case)


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
