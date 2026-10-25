"""Capture driver for the relay-transcript golden corpus.

Default capture builds every case's fixture store under a throwaway root,
runs the live Python leg (``fno.relay.registry.transcript_path_for``) in
process, and freezes door-shaped goldens under tests/golden/relay_transcript.
``--case DIR`` prints one ``{"exit", "stdout", "stderr"}`` envelope for a
single built case, the shape the merge_command driver established.

The goldens freeze the Python leg while it lives. After the leg retires the
frozen files are the contract, and a rewritten oracle cannot re-derive them:
a golden can only be captured from the leg it characterizes.
"""

import argparse
import json
import os
import sys
import tempfile
from pathlib import Path

FIXTURE_DIR = Path(__file__).resolve().parent
TESTS_DIR = FIXTURE_DIR.parents[1]
GOLDEN_DIR = TESTS_DIR / "golden" / "relay_transcript"

# The planted store shapes. The uuid and dir names are constants shared with
# the Rust parity test, which rebuilds the same tree under its own root.
CASE_UUID = "d8996f9b-8854-4f22-8c28-c7819c6d0316"
OTHER_UUID = "11111111-2222-3333-4444-555555555555"
PROJ_DIR = "-Users-bb16--claude-jobs-x"

CASES = [
    # The counted shape: the exact transcript file in one project dir.
    {"label": "hit one project dir", "files": [(PROJ_DIR, CASE_UUID + ".jsonl")]},
    # No store content: the miss both legs must answer None on.
    {"label": "miss empty store", "files": []},
    # A dotted-stem sibling artifact is never a transcript (the Rust store
    # listing skips stems containing a dot; the Python exact glob misses it).
    {
        "label": "miss dotted sibling only",
        "files": [(PROJ_DIR, CASE_UUID + ".orphaned-x.jsonl")],
    },
    # Another session's transcript answers nothing for this id.
    {
        "label": "miss other session",
        "files": [(PROJ_DIR, OTHER_UUID + ".jsonl")],
    },
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


def build_case(root_path, case, index):
    case_dir = root_path / ("case-%02d" % index)
    case_dir.mkdir(parents=True)
    projects = case_dir / "projects"
    for proj_dir, name in case["files"]:
        target = projects / proj_dir
        target.mkdir(parents=True, exist_ok=True)
        (target / name).write_text("{}")
    return case_dir


def run_leg(projects_dir, session_id):
    from fno.relay.registry import transcript_path_for

    return transcript_path_for(session_id, projects_dir=projects_dir)


def door_envelope(answer):
    """The capture protocol's envelope, in the transcript-path door's shape.

    The door answers one JSON line, ``{"path": <str or null>}``; the frozen
    stdout is that line rendered from the PYTHON answer, so the parity test
    can compare the Rust door's bytes against the old leg's verdict.
    """
    line = json.dumps({"path": answer}) + "\n"
    return {"exit": 0, "stdout": line, "stderr": ""}


def cmd_case(case_dir):
    case_dir = Path(case_dir).resolve()
    case = json.loads((case_dir / "case.json").read_text())
    answer = run_leg(case_dir / "projects", CASE_UUID)
    sys.stdout.write(json.dumps(door_envelope(answer)) + "\n")


def capture(only):
    root_path = Path(tempfile.mkdtemp(prefix="relay-transcript-"))
    print("fixture root: %s" % root_path, flush=True)
    frozen = 0
    for index, case in enumerate(CASES):
        key = slug(case["label"])
        if only and key != only:
            continue
        case_dir = build_case(root_path, case, index)
        (case_dir / "case.json").write_text(
            json.dumps(case, sort_keys=True, indent=2) + "\n"
        )
        answer = run_leg(case_dir / "projects", CASE_UUID)
        envelope = door_envelope(answer)
        # Mask the throwaway root so the golden carries the shape, not the
        # local temp path; the parity test substitutes its own root.
        masked = envelope["stdout"].replace(str(case_dir), "<CASE>")
        GOLDEN_DIR.mkdir(parents=True, exist_ok=True)
        (GOLDEN_DIR / (key + ".exit")).write_text("%d\n" % envelope["exit"])
        (GOLDEN_DIR / (key + ".out")).write_text(masked)
        frozen += 1
        print("froze %s -> %s (answer %r)" % (case["label"], key, answer), flush=True)
    if only and frozen == 0:
        raise SystemExit("--only %s matched no case" % only)


def main():
    parser = argparse.ArgumentParser(
        description="capture the relay-transcript golden corpus from the live Python leg"
    )
    parser.add_argument("--case", help="child mode: run one built case dir")
    parser.add_argument("--only", metavar="SLUG", help="capture one case by golden key")
    args = parser.parse_args()
    if args.case:
        cmd_case(args.case)
    else:
        capture(args.only)


if __name__ == "__main__":
    main()
