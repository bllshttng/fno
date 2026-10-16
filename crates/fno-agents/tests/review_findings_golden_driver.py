#!/usr/bin/env python3
"""Capture driver for the `review_findings` goldens.

The Python leg (`fno do review classify --emit-record` and the
`fno do review post-dispositions` leaf) was the parity oracle until wave 2
of the port deleted it. This driver ran that leg through the same argv the
Rust `review-level findings` actions accept, under env and fixture pins both
legs reproduce, and froze one golden triple per case (`<key>.exit` /
`<key>.out` / `<key>.err` when the case pins stderr, plus `<key>.log` /
`<key>.body` for the fake gh's argv log and POST body) under
`tests/golden/review_findings/`. It also generated the committed per-case
fixture files under `tests/golden/review_findings/cases/<label>/`, the
`gh_stub.sh`, and `cases.json`, so the parity test runs the built binary
against byte-identical fixtures.

`review_findings_parity.rs` is the lockstep consumer of the same files. It
normalizes the argv log the same way this driver froze it (every
` --paginate` token dropped: the native comments read walks every page, the
Python leg read one). Add a case to BOTH or neither. After the deletion the
driver is kept as provenance: re-running a capture needs the deleted Python
leg, and a bare run says so.

The `post page two marker` case is rust-only: the Python leg read one
comment page, so no Python oracle exists for it; its golden is hand-written
against the native page walk.

The `classify *` table and the `classify * retired` rows are the 3b
classify port's contribution. The classify cases were captured from the
live Python classify leaf right before this PR deleted it (one final
`--only classify` capture; the other tables' goldens were left untouched so
a newer deployed fno-agents binary could not churn them). The `retired`
rows are rust-only and hand-written like the page-two case: the Python
--attest body they parameterize is deleted by the same change that refuses
the flags, so no Python oracle ever existed for the refusal.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import stat
import subprocess
import sys
import tempfile
from pathlib import Path

GOLDEN_DIR = Path(__file__).resolve().parent / "golden" / "review_findings"

# Every prefix that can leak machine identity into the config candidate walk
# or the review round. The parity test scrubs the same set before pinning
# the child's env.
SCRUB_PREFIXES = (
    "FNO_",
    "TARGET_",
    "CLAUDE_",
    "CODEX_",
    "GEMINI_",
    "OPENCODE_",
    "ANTHROPIC_",
    "OPENAI_",
    "ZAI_",
    "GLM_",
)

REVIEWER = "code-review"
HEAD = "1111111111111111111111111111111111111111"
HEAD9 = HEAD[:9]
OTHER_HEAD = "2222222222222222222222222222222222222222"
COMMENT_MARKER = f"<!-- fno-review-round head={HEAD} round=1 reviewer={REVIEWER} -->"


def slug(label: str) -> str:
    """Mirror `common::slug`: lowercase, non-alphanumeric runs to one `_`."""
    out: list[str] = []
    prev_us = False
    for ch in label:
        if ch.isascii() and ch.isalnum():
            out.append(ch.lower())
            prev_us = False
        elif not prev_us:
            out.append("_")
            prev_us = True
    return "".join(out).strip("_")


def finding(n: int = 1, **overrides) -> dict:
    base = {
        "category": "correctness",
        "file": f"f{n}.py",
        "line": n,
        "summary": f"summary {n}",
        "failure_scenario": f"scenario {n}",
    }
    base.update(overrides)
    return base


def disp(key: str, disposition: str, reason: str) -> dict:
    return {"finding_key": key, "disposition": disposition, "reason": reason}


def payload_with_dispositions(**kw) -> dict:
    body = {
        "findings": [finding(1), finding(2, category="nit")],
        "dispositions": [
            disp("f1.py:1:correctness", "fixed", 'absent: "x" does not hold'),
            disp("f2.py:2:nit", "nonblocking", "cosmetic"),
        ],
    }
    body.update(kw)
    return body


def comment_row(body: str) -> dict:
    return {"body": body}


PR_INFO = {"head": {"sha": HEAD, "ref": "feature/x"}, "base": {"ref": "main"}}
PULLS_ROW = {"number": 7, "state": "open"}

# --- CASE TABLE ----------------------------------------------------------
# Lockstep twin of the CASES read by review_findings_parity.rs (from
# cases.json, which this driver writes). err modes:
#   None            stderr is not pinned (uninteresting or leg-specific)
#   {"strip": p}    freeze stderr with the leading Python prefix p removed
#                   (the classify leaf's `classify: <file>: ` wrapper)
#   {"exact": True} freeze stderr byte-for-byte (both legs print the same)
#   {"prefix": p}   freeze the fixed prefix p; parity asserts starts_with

RECORD_CASES = [
    # Shapes.
    {"label": "record bare array", "payload": [finding(1), finding(2, category="typo")]},
    {"label": "record object findings key", "payload": {"findings": [finding(1)]}},
    {"label": "record empty array", "payload": []},
    {
        "label": "record no findings key",
        "payload": {"oops": True},
        "err": {"strip": "classify: findings.json: "},
    },
    {
        "label": "record findings not array",
        "payload": {"findings": 7},
        "err": {"strip": "classify: findings.json: "},
    },
    {
        "label": "record top level string",
        "payload": "hello",
        "err": {"strip": "classify: findings.json: "},
    },
    # The rule, in evaluation order.
    {"label": "record harmless no verdict", "payload": [finding(1, category="typo", verdict=None)]},
    {"label": "record harmless confirmed", "payload": [finding(1, category="typo", verdict="CONFIRMED")]},
    {"label": "record harmless padded confirmed", "payload": [finding(1, category="typo", verdict=" confirmed ")]},
    {"label": "record plausible harmless", "payload": [finding(1, category="typo", verdict="PLAUSIBLE")]},
    {"label": "record unknown category", "payload": [finding(1, category="vibes")]},
    {"label": "record absent category", "payload": [{"file": "a.py", "line": 1, "summary": "s", "failure_scenario": "f"}]},
    {"label": "record empty category", "payload": [finding(1, category="")]},
    {"label": "record whitespace category", "payload": [finding(1, category="   ")]},
    {"label": "record missing file", "payload": [{"category": "typo", "line": 1, "summary": "s", "failure_scenario": "f"}]},
    {"label": "record missing summary", "payload": [{"category": "typo", "file": "a.py", "line": 1, "failure_scenario": "f"}]},
    {"label": "record missing failure scenario", "payload": [{"category": "typo", "file": "a.py", "line": 1, "summary": "s"}]},
    {"label": "record non object element", "payload": [7]},
    {"label": "record unmappable object", "payload": [{"autofix_class": "manual", "owner": "x"}]},
    # Casing and trim.
    {"label": "record category case typo", "payload": [finding(1, category="Typo")]},
    {"label": "record category nbsp padded", "payload": [finding(1, category=" typo ")]},
    {"label": "record category unit separator padded", "payload": [finding(1, category="\x1ftypo\x1f")]},
    # The `line` field through Python str().
    {"label": "record line int", "payload": [finding(1, line=4, category="typo")]},
    {"label": "record line string", "payload": [finding(1, line="7", category="typo")]},
    {"label": "record line float whole", "payload": [finding(1, line=4.0, category="perf")]},
    {"label": "record line float frac", "payload": [finding(1, line=4.5, category="perf")]},
    {"label": "record line true", "payload": [finding(1, line=True, category="perf")]},
    {"label": "record line null", "payload": [finding(1, line=None, category="perf")]},
    {"label": "record line zero", "payload": [finding(1, line=0, category="perf")]},
    {"label": "record line array", "payload": [finding(1, line=[1, 2], category="perf")]},
    {"label": "record line object", "payload": [finding(1, line={"a": 1}, category="perf")]},
    # autofix_class and owner.
    {"label": "record autofix known words", "payload": [
        finding(1, autofix_class="gated_auto"), finding(2, autofix_class="manual"), finding(3, autofix_class="advisory"),
    ]},
    {"label": "record autofix upper case", "payload": [finding(1, autofix_class="GATED_AUTO")]},
    {"label": "record autofix unknown", "payload": [finding(1, autofix_class="bogus")]},
    {"label": "record owner string and number", "payload": [finding(1, owner="team-x"), finding(2, owner=7)]},
    # Summary cap.
    {"label": "record summary at cap", "payload": [finding(1, summary="s" * 240)]},
    {"label": "record summary over cap", "payload": [finding(1, summary="s" * 241)]},
    {"label": "record summary two byte over cap", "payload": [finding(1, summary="é" * 241)]},
    # Config.
    {"label": "record config unset", "payload": [finding(1, category="perf")], "config": ""},
    {"label": "record config perf", "payload": [finding(1, category="perf")], "config": '[review]\nnonblocking_categories = ["perf"]\n'},
    {"label": "record config empty list", "payload": [finding(1, category="perf")], "config": '[review]\nnonblocking_categories = []\n'},
    {"label": "record config padded entry", "payload": [finding(1, category="perf")], "config": '[review]\nnonblocking_categories = [" Perf "]\n'},
    {
        "label": "record config string malformed",
        "payload": [finding(1, category="perf")],
        "config": '[review]\nnonblocking_categories = "perf"\n',
        "err": {"exact": True},
    },
    {"label": "record config unloadable", "payload": [finding(1, category="perf")], "config": "[review\nbroken"},
    # Histogram order, first seen, missing category as "".
    {
        "label": "record histogram first seen order",
        "payload": [finding(1, category="Typo"), finding(2, category="style"), finding(3, category="perf"), finding(4, category=None)],
    },
    # Dispositions.
    {
        "label": "record dispositions valid",
        "payload": payload_with_dispositions(),
    },
    {
        "label": "record dispositions not array",
        "payload": {"findings": [], "dispositions": "nope"},
        "err": {"strip": "classify: findings.json: "},
    },
    {
        "label": "record disposition entry not object",
        "payload": {"findings": [], "dispositions": ["x"]},
        "err": {"strip": "classify: findings.json: "},
    },
    {
        "label": "record disposition missing key",
        "payload": {"findings": [], "dispositions": [{"disposition": "fixed", "reason": "r"}]},
        "err": {"strip": "classify: findings.json: "},
    },
    {
        "label": "record disposition whitespace key",
        "payload": {"findings": [], "dispositions": [{"finding_key": "   ", "disposition": "fixed", "reason": "r"}]},
        "err": {"strip": "classify: findings.json: "},
    },
    {
        "label": "record disposition unknown word",
        "payload": {"findings": [], "dispositions": [{"finding_key": "x", "disposition": "vibes", "reason": "r"}]},
        "err": {"strip": "classify: findings.json: "},
    },
    {
        "label": "record disposition empty reason",
        "payload": {"findings": [], "dispositions": [{"finding_key": "x", "disposition": "fixed", "reason": "  "}]},
        "err": {"strip": "classify: findings.json: "},
    },
    {
        "label": "record declined without quote",
        "payload": {"findings": [], "dispositions": [{"finding_key": "x", "disposition": "declined", "reason": "no quote here"}]},
        "err": {"strip": "classify: findings.json: "},
    },
    {
        "label": "record declined with quote",
        "payload": {"findings": [], "dispositions": [{"finding_key": "x", "disposition": "declined", "reason": 'absent: "return None"'}]},
    },
    {
        "label": "record disposition extra keys dropped",
        "payload": {"findings": [], "dispositions": [{"finding_key": "x", "disposition": "fixed", "reason": "r", "extra": 1}]},
    },
    {"label": "record dispositions empty array", "payload": {"findings": [finding(1)], "dispositions": []}},
    # review_round pass-through.
    {"label": "record round two", "payload": {"findings": [finding(1)], "review_round": 2}},
    {"label": "record round zero kept", "payload": {"findings": [finding(1)], "review_round": 0}},
    {
        "label": "record round negative refused",
        "payload": {"findings": [finding(1)], "review_round": -1},
        "err": {"strip": "classify: findings.json: "},
    },
    {
        "label": "record round bool refused",
        "payload": {"findings": [finding(1)], "review_round": True},
        "err": {"strip": "classify: findings.json: "},
    },
    {
        "label": "record round float refused",
        "payload": {"findings": [finding(1)], "review_round": 1.0},
        "err": {"strip": "classify: findings.json: "},
    },
    {
        "label": "record round string refused",
        "payload": {"findings": [finding(1)], "review_round": "2"},
        "err": {"strip": "classify: findings.json: "},
    },
    # Bounds (fixtures from test_review_cli's truncation tests).
    {"label": "record bounds 201 findings", "payload": [finding(n) for n in range(201)]},
    {"label": "record bounds 200 max summaries", "payload": [finding(n, summary="s" * 240) for n in range(200)]},
    {"label": "record bounds key blowup drops tail", "payload": [finding(n, file="x" * 1200) for n in range(60)]},
]

POST_CASES = [
    # No dispositions: nothing posted, empty gh log. The happy case below is
    # the positive control that the fake gh is on the path (its log is frozen
    # non-empty).
    {
        "label": "post no dispositions",
        "payload": [finding(1)],
        "args": ["--findings-file", "findings.json", "--head", HEAD, "--pr", "7"],
        "fixtures": {"comments_p1.json": []},
    },
    {
        "label": "post happy",
        "payload": payload_with_dispositions(),
        "args": ["--findings-file", "findings.json", "--head", HEAD, "--pr", "7"],
        "fixtures": {"comments_p1.json": [], "pr_info.json": PR_INFO},
    },
    {
        "label": "post pr-number flag",
        "payload": payload_with_dispositions(),
        "args": ["--findings-file", "findings.json", "--head", HEAD, "--pr-number", "7"],
        "fixtures": {"comments_p1.json": [], "pr_info.json": PR_INFO},
    },
    {
        "label": "post resolves pr from branch",
        "payload": payload_with_dispositions(),
        "args": ["--findings-file", "findings.json", "--head", HEAD],
        "fixtures": {
            "comments_p1.json": [],
            "pr_info.json": PR_INFO,
            "pulls_list.json": [PULLS_ROW],
        },
    },
    {
        "label": "post no pr for branch",
        "payload": payload_with_dispositions(),
        "args": ["--findings-file", "findings.json", "--head", HEAD],
        "fixtures": {"comments_p1.json": [], "pulls_list.json": []},
        "err": {"exact": True},
    },
    {
        "label": "post detached head no pr",
        "payload": payload_with_dispositions(),
        "args": ["--findings-file", "findings.json", "--head", HEAD],
        "detached": True,
        "fixtures": {"comments_p1.json": []},
        "err": {"exact": True},
    },
    {
        "label": "post already posted",
        "payload": payload_with_dispositions(),
        "args": ["--findings-file", "findings.json", "--head", HEAD, "--pr", "7"],
        "fixtures": {
            "comments_p1.json": [comment_row(f"unrelated\n{COMMENT_MARKER}\ntail")],
            "pr_info.json": PR_INFO,
        },
    },
    {
        "label": "post marker other head",
        "payload": payload_with_dispositions(),
        "args": ["--findings-file", "findings.json", "--head", HEAD, "--pr", "7"],
        "fixtures": {
            "comments_p1.json": [
                comment_row(f"<!-- fno-review-round head={OTHER_HEAD} round=1 reviewer={REVIEWER} -->"),
            ],
            "pr_info.json": PR_INFO,
        },
    },
    {
        "label": "post head mismatch refuses",
        "payload": payload_with_dispositions(),
        "args": ["--findings-file", "findings.json", "--head", OTHER_HEAD, "--pr", "7"],
        "fixtures": {"comments_p1.json": [], "pr_info.json": PR_INFO},
        "err": {"exact": True},
    },
    {
        "label": "post abbreviated head refuses",
        "payload": payload_with_dispositions(),
        "args": ["--findings-file", "findings.json", "--head", HEAD9, "--pr", "7"],
        "fixtures": {"comments_p1.json": [], "pr_info.json": PR_INFO},
        "err": {"exact": True},
    },
    {
        "label": "post pr read fails",
        "payload": payload_with_dispositions(),
        "args": ["--findings-file", "findings.json", "--head", HEAD, "--pr", "7"],
        "fixtures": {"comments_p1.json": [], "pr_info_fail": ""},
        "err": {"prefix": "post-dispositions: PR read failed: "},
    },
    {
        "label": "post comments read fails",
        "payload": payload_with_dispositions(),
        "args": ["--findings-file", "findings.json", "--head", HEAD, "--pr", "7"],
        "fixtures": {"comments_fail": "", "pr_info.json": PR_INFO},
        "err": {"exact": True},
    },
    {
        "label": "post comment post fails",
        "payload": payload_with_dispositions(),
        "args": ["--findings-file", "findings.json", "--head", HEAD, "--pr", "7"],
        "fixtures": {"post_fail": "", "comments_p1.json": [], "pr_info.json": PR_INFO},
        "err": {"exact": True},
    },
    {
        "label": "post no origin remote",
        "payload": payload_with_dispositions(),
        "args": ["--findings-file", "findings.json", "--head", HEAD, "--pr", "7"],
        "origin": "none",
        "fixtures": {"comments_p1.json": []},
        "err": {"exact": True},
    },
    {
        "label": "post non github origin",
        "payload": payload_with_dispositions(),
        "args": ["--findings-file", "findings.json", "--head", HEAD, "--pr", "7"],
        "origin": "gitlab",
        "fixtures": {"comments_p1.json": []},
        "err": {"prefix": "post-dispositions: could not resolve owner/repo: "},
    },
    # Round resolution: flag > non-empty env > payload round > 1.
    {
        "label": "post round flag",
        "payload": payload_with_dispositions(),
        "args": ["--findings-file", "findings.json", "--head", HEAD, "--pr", "7", "--review-round", "2"],
        "fixtures": {"comments_p1.json": [], "pr_info.json": PR_INFO},
    },
    {
        "label": "post round env",
        "payload": payload_with_dispositions(),
        "args": ["--findings-file", "findings.json", "--head", HEAD, "--pr", "7"],
        "env": {"FNO_REVIEW_ROUND": "3"},
        "fixtures": {"comments_p1.json": [], "pr_info.json": PR_INFO},
    },
    {
        "label": "post round flag beats env",
        "payload": payload_with_dispositions(),
        "args": ["--findings-file", "findings.json", "--head", HEAD, "--pr", "7", "--review-round", "2"],
        "env": {"FNO_REVIEW_ROUND": "3"},
        "fixtures": {"comments_p1.json": [], "pr_info.json": PR_INFO},
    },
    {
        "label": "post round env zero is round zero",
        "payload": payload_with_dispositions(),
        "args": ["--findings-file", "findings.json", "--head", HEAD, "--pr", "7"],
        "env": {"FNO_REVIEW_ROUND": "0"},
        "fixtures": {"comments_p1.json": [], "pr_info.json": PR_INFO},
    },
    {
        "label": "post round payload",
        "payload": payload_with_dispositions(review_round=4),
        "args": ["--findings-file", "findings.json", "--head", HEAD, "--pr", "7"],
        "fixtures": {"comments_p1.json": [], "pr_info.json": PR_INFO},
    },
    {
        "label": "post round payload zero is round one",
        "payload": payload_with_dispositions(review_round=0),
        "args": ["--findings-file", "findings.json", "--head", HEAD, "--pr", "7"],
        "fixtures": {"comments_p1.json": [], "pr_info.json": PR_INFO},
    },
    {
        "label": "post round default one",
        "payload": payload_with_dispositions(),
        "args": ["--findings-file", "findings.json", "--head", HEAD, "--pr", "7"],
        "fixtures": {"comments_p1.json": [], "pr_info.json": PR_INFO},
    },
    {
        "label": "post round env junk",
        "payload": payload_with_dispositions(),
        "args": ["--findings-file", "findings.json", "--head", HEAD, "--pr", "7"],
        "env": {"FNO_REVIEW_ROUND": "x"},
        "fixtures": {"comments_p1.json": [], "pr_info.json": PR_INFO},
    },
    {
        "label": "post reviewer peer",
        "payload": payload_with_dispositions(),
        "args": ["--findings-file", "findings.json", "--head", HEAD, "--pr", "7", "--reviewer", "peer"],
        "fixtures": {"comments_p1.json": [], "pr_info.json": PR_INFO},
    },
    {
        "label": "post unkeyed element",
        "payload": {
            "findings": [7, finding(1)],
            "dispositions": [disp("f1.py:1:correctness", "fixed", 'absent: "x" does not hold')],
        },
        "args": ["--findings-file", "findings.json", "--head", HEAD, "--pr", "7"],
        "fixtures": {"comments_p1.json": [], "pr_info.json": PR_INFO},
    },
    # Usage errors: exit 2, the typer usage text is leg-specific, so stderr
    # is unpinned and stdout is frozen empty.
    {
        "label": "post usage missing findings file",
        "payload": {"findings": [finding(1)], "dispositions": [disp("k", "fixed", "r")]},
        "args": ["--head", HEAD],
        "fixtures": {},
    },
    {
        "label": "post usage missing head",
        "payload": payload_with_dispositions(),
        "args": ["--findings-file", "findings.json"],
        "fixtures": {},
    },
    {
        "label": "post usage unknown flag",
        "payload": payload_with_dispositions(),
        "args": ["--findings-file", "findings.json", "--head", HEAD, "--bogus"],
        "fixtures": {},
    },
]

RUST_ONLY_CASE = {
    "label": "post page two marker",
    "action": "post",
    "args": ["--findings-file", "findings.json", "--head", HEAD, "--pr", "7"],
    "env": {},
    "origin": "github",
    "detached": False,
}

# The classify leaf's own surface, captured from the Python leg before this
# PR deleted it. Refusal order is Python's: file read, JSON parse, record
# build, then the round check. The two Python-text refusals (errno text,
# json error text) freeze a declared PREFIX, not the machine-written tail;
# the record-build refusal strips the wrapper because the tail is the door's
# own Rust reason on both legs.
CLASSIFY_CASES = [
    # Happy bare: the summary line producers parse.
    {
        "label": "classify bare",
        "payload": [finding(1), finding(2, category="typo")],
        "args": ["--findings-file", "findings.json"],
    },
    # Happy record: the bounded record the emitter projects.
    {
        "label": "classify emit record",
        "payload": [finding(1), finding(2, category="typo")],
        "args": ["--findings-file", "findings.json", "--emit-record"],
    },
    # The flag stamps the round last in the record.
    {
        "label": "classify emit record round flag",
        "payload": {"findings": [finding(1)]},
        "args": ["--findings-file", "findings.json", "--emit-record", "--review-round", "2"],
    },
    # The flag overrides a round the payload carried.
    {
        "label": "classify round flag overrides payload",
        "payload": {"findings": [finding(1)], "review_round": 4},
        "args": ["--findings-file", "findings.json", "--emit-record", "--review-round", "2"],
    },
    # Zero is a real round: Python overrides on `is not None`, not truthiness.
    {
        "label": "classify round zero flag",
        "payload": {"findings": [finding(1)]},
        "args": ["--findings-file", "findings.json", "--emit-record", "--review-round", "0"],
    },
    {
        "label": "classify missing file refused",
        "payload": {"findings": [finding(1)]},
        "args": ["--findings-file", "absent.json"],
        "err": {"prefix": "classify: cannot read absent.json: "},
    },
    {
        "label": "classify invalid json refused",
        "raw": '{"findings": [}',
        "args": ["--findings-file", "findings.json"],
        "err": {"prefix": "classify: findings.json is not valid JSON: "},
    },
    {
        "label": "classify build refusal",
        "payload": {"oops": True},
        "args": ["--findings-file", "findings.json"],
        # Prefix, not strip: the record cases strip because the record
        # action prints the bare reason, but the classify action prints its
        # own `classify: <F>: ` wrapper, and parity compares classify
        # stderr whole.
        "err": {"prefix": "classify: findings.json: "},
    },
    {
        "label": "classify round negative refused",
        "payload": {"findings": [finding(1)]},
        "args": ["--findings-file", "findings.json", "--review-round", "-1"],
        "err": {"exact": True},
    },
    # The typer usage text is leg-specific: stderr unpinned, stdout frozen
    # empty, same posture as the post usage cases.
    {
        "label": "classify round not an integer",
        "payload": {"findings": [finding(1)]},
        "args": ["--findings-file", "findings.json", "--review-round", "x"],
    },
]

ALL_CASES = RECORD_CASES + POST_CASES + CLASSIFY_CASES

# Rust-only, hand-written: the retired attest flags. Their whole job was
# parameterizing the Python --attest body this PR deletes, so the refusal is
# all that remains and no Python oracle exists. The fixture file is present
# only because the parity harness copies it unconditionally; the refusal
# fires at argv parse, before any read.
RETIRED_CASES = [
    {
        "label": "classify attest retired",
        "args": ["--findings-file", "findings.json", "--attest", "code-review"],
    },
    {
        "label": "classify attest equals retired",
        "args": ["--findings-file=findings.json", "--attest=code-review"],
    },
    {
        "label": "classify reviewer context retired",
        "args": ["--findings-file", "findings.json", "--reviewer-context", "fork"],
    },
    {
        "label": "classify execution context retired",
        "args": ["--findings-file", "findings.json", "--execution-context", "inline"],
    },
    {
        "label": "classify output contract retired",
        "args": ["--findings-file", "findings.json", "--output-contract", "json_block"],
    },
]

RETIRED_FIXTURE_PAYLOAD = {"findings": [finding(1)]}


def retired_message(flag: str) -> str:
    """The one-line refusal the native classify prints for a retired flag.

    The hand-written `.err` goldens and the Rust refusal must agree byte for
    byte; both spell this through this helper's shape.
    """
    return (
        f"classify: {flag} is retired; write the attestation with "
        "skills/review/scripts/emit-attestation.sh <reviewer> [verdict] "
        "[reviewer_context] [execution_context] [output_contract] --findings-file F\n"
    )


def cases_json() -> list[dict]:
    """The lockstep table the parity test reads: run spec without payloads."""
    rows = []
    for spec in ALL_CASES:
        if spec in RECORD_CASES:
            action = "record"
        elif spec in POST_CASES:
            action = "post"
        else:
            action = "classify"
        row = {
            "label": spec["label"],
            "action": action,
            "args": spec.get("args", []),
            "env": spec.get("env", {}),
            "origin": spec.get("origin", "github"),
            "detached": bool(spec.get("detached")),
        }
        err = spec.get("err")
        if err and "prefix" in err:
            row["err_prefix"] = err["prefix"]
        rows.append(row)
    rows.append(RUST_ONLY_CASE)
    for spec in RETIRED_CASES:
        rows.append(
            {
                "label": spec["label"],
                "action": "classify",
                "args": spec["args"],
                "env": {},
                "origin": "github",
                "detached": False,
            }
        )
    return rows


def scrubbed_environ() -> dict[str, str]:
    return {
        k: v
        for k, v in os.environ.items()
        if not k.startswith(SCRUB_PREFIXES)
    }


GH_STUB = """\
#!/bin/sh
# Fake gh for the review_findings goldens (frozen by
# review_findings_golden_driver.py, read by review_findings_parity.rs).
# Every call logs one space-joined argv line to $FAKE_GH_LOG, with the -f
# body argument's value written to $FAKE_GH_BODY instead (newlines must
# survive). Answers come from $FAKE_GH_CASE fixture files.
log_line=""
prev=""
for a in "$@"; do
  if [ "$prev" = "-f" ]; then
    printf '%s' "${a#body=}" > "${FAKE_GH_BODY:?FAKE_GH_BODY must be set}"
    prev="$a"
    continue
  fi
  if [ -z "$log_line" ]; then log_line="$a"; else log_line="$log_line $a"; fi
  prev="$a"
done
printf '%s\\n' "$log_line" >> "${FAKE_GH_LOG:?FAKE_GH_LOG must be set}"
case "$log_line" in
  *rate_limit*)
    printf '{"resources":{"core":{"remaining":4990,"reset":1}}}\\n'
    exit 0
    ;;
esac
if [ "$1" != "api" ]; then
  echo "fake-gh: unexpected: $log_line" >&2
  exit 1
fi
post=0
for a in "$@"; do
  if [ "$a" = "POST" ]; then post=1; fi
done
if [ "$post" = 1 ]; then
  if [ -f "$FAKE_GH_CASE/post_fail" ]; then echo boom >&2; exit 1; fi
  printf '{}\\n'
  exit 0
fi
path=""
for a in "$@"; do
  case "$a" in
    *repos/*) path="$a" ;;
  esac
done
if [ -z "$path" ]; then
  echo "fake-gh: no repos path: $log_line" >&2
  exit 1
fi
case "$path" in
  *"pulls?state=all"*)
    if [ -f "$FAKE_GH_CASE/pulls_fail" ]; then echo boom >&2; exit 1; fi
    if [ -f "$FAKE_GH_CASE/pulls_list.json" ]; then
      cat "$FAKE_GH_CASE/pulls_list.json"
    else
      printf '[]\\n'
    fi
    exit 0
    ;;
  *"comments"*)
    if [ -f "$FAKE_GH_CASE/comments_fail" ]; then echo boom >&2; exit 1; fi
    for a in "$@"; do
      if [ "$a" = "--paginate" ] && [ -f "$FAKE_GH_CASE/comments_both.json" ]; then
        cat "$FAKE_GH_CASE/comments_both.json"
        exit 0
      fi
    done
    cat "$FAKE_GH_CASE/comments_p1.json"
    exit 0
    ;;
  *"pulls/"*)
    if [ -f "$FAKE_GH_CASE/pr_info_fail" ]; then echo boom >&2; exit 1; fi
    cat "$FAKE_GH_CASE/pr_info.json"
    exit 0
    ;;
esac
echo "fake-gh: unmatched path: $path" >&2
exit 1
"""


def git(repo: Path, *args: str, env: dict[str, str]) -> None:
    subprocess.run(
        ["git", *args],
        cwd=repo,
        env=env,
        check=True,
        capture_output=True,
    )


def payload_text(spec: dict) -> str:
    """The findings.json bytes: verbatim when the case pins raw text (the
    invalid-JSON refusal), else the payload serialized."""
    if "raw" in spec:
        return spec["raw"]
    return json.dumps(spec["payload"], ensure_ascii=False)


def write_fixtures(case_dir: Path, spec: dict) -> None:
    """The committed per-case fixture files, generated once at capture."""
    case_dir.mkdir(parents=True, exist_ok=True)
    (case_dir / "findings.json").write_text(payload_text(spec), encoding="utf-8")
    config = spec.get("config")
    if config is not None:
        (case_dir / "config.toml").write_text(config, encoding="utf-8")
    elif (case_dir / "config.toml").exists():
        (case_dir / "config.toml").unlink()
    fixtures = spec.get("fixtures") or {}
    for name in ("pulls_fail", "pr_info_fail", "comments_fail", "post_fail"):
        marker = case_dir / name
        if name in fixtures:
            marker.write_text("", encoding="utf-8")
        elif marker.exists():
            marker.unlink()
    for name in ("pulls_list.json", "pr_info.json", "comments_p1.json"):
        if name in fixtures:
            (case_dir / name).write_text(json.dumps(fixtures[name]), encoding="utf-8")


def build_runtime(spec: dict, case_dir: Path, root: Path, label_slug: str) -> Path:
    """One runtime dir: the repo the leg runs in, the fake gh, fresh HOME."""
    runtime = root / label_slug
    if runtime.exists():
        shutil.rmtree(runtime)
    runtime.mkdir(parents=True)
    # A fresh HOME makes the Python loader run the one-time path migration,
    # which prints an absolute settings path to stderr. Seed the home so the
    # migration is already done and stderr stays quiet; the Rust leg has no
    # migration and must not pay for one.
    fno_home = runtime / "home" / ".fno"
    fno_home.mkdir(parents=True)
    (fno_home / "config.toml").write_text("", encoding="utf-8")
    (fno_home / ".path-migration-done").write_text("", encoding="utf-8")
    repo = runtime / "repo"
    repo.mkdir()
    env = scrubbed_environ()
    env["GIT_CONFIG_GLOBAL"] = "/dev/null"
    env["GIT_CONFIG_SYSTEM"] = "/dev/null"
    git(repo, "init", "-q", "-b", "main", env=env)
    origin = spec.get("origin", "github")
    if origin == "github":
        git(repo, "remote", "add", "origin", "https://github.com/o/r.git", env=env)
    elif origin == "gitlab":
        git(repo, "remote", "add", "origin", "https://gitlab.com/o/r.git", env=env)
    if spec.get("detached"):
        git(
            repo,
            "-c", "user.email=t@example.test",
            "-c", "user.name=t",
            "commit", "--allow-empty", "-q", "-m", "base",
            env=env,
        )
        git(repo, "checkout", "-q", "--detach", env=env)
    bindir = runtime / "bin"
    bindir.mkdir()
    stub = bindir / "gh"
    stub.write_text(GH_STUB, encoding="utf-8")
    stub.chmod(stub.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
    return runtime


def child_environ(spec: dict, case_dir: Path, runtime: Path) -> dict[str, str]:
    env = scrubbed_environ()
    env["HOME"] = str(runtime / "home")
    env["GIT_CONFIG_GLOBAL"] = "/dev/null"
    env["GIT_CONFIG_SYSTEM"] = "/dev/null"
    config = case_dir / "config.toml"
    if config.exists():
        env["FNO_CONFIG"] = str(config)
    env.update(spec.get("env") or {})
    env["PATH"] = str(runtime / "bin") + os.pathsep + env.get("PATH", "")
    return env


def run_leg(spec: dict, case_dir: Path, runtime: Path) -> tuple[str, str, int]:
    (runtime / "repo" / "findings.json").write_text(payload_text(spec), encoding="utf-8")
    log = runtime / "gh.log"
    body = runtime / "gh-body.txt"
    for stale in (log, body):
        if stale.exists():
            stale.unlink()
    env = child_environ(spec, case_dir, runtime)
    env["FAKE_GH_LOG"] = str(log)
    env["FAKE_GH_BODY"] = str(body)
    env["FAKE_GH_CASE"] = str(case_dir)
    repo_root = Path(__file__).resolve().parents[3]
    env["PYTHONPATH"] = str(repo_root / "cli" / "src")
    if spec in RECORD_CASES:
        argv = [
            sys.executable, "-m", "fno.cli", "do", "review", "classify",
            "--findings-file", "findings.json", "--emit-record",
        ]
    elif spec in POST_CASES:
        argv = [
            sys.executable, "-m", "fno.cli", "do", "review", "post-dispositions",
            *spec["args"],
        ]
    else:
        argv = [
            sys.executable, "-m", "fno.cli", "do", "review", "classify",
            *spec["args"],
        ]
    proc = subprocess.run(
        argv,
        cwd=runtime / "repo",
        env=env,
        capture_output=True,
        text=True,
    )
    return proc.stdout, proc.stderr, proc.returncode


def strip_wrapper_noise(text: str) -> str:
    """Cut `gh proxy: <sentence>` segments, newline included, wherever the
    Python leg's gh wrapper fused them into a message.

    review_findings_parity.rs applies the same cut to its observed streams:
    the wrapper is machine dressing, and the Python leg's own classifier
    (`_rest._WRAPPER_NOISE`) already treats it as noise everywhere else.
    """
    return re.sub(r"gh proxy: [^\n]*\n", "", text)


def freeze(runtime: Path, spec: dict, out: str, err: str, code: int) -> None:
    key = slug(spec["label"])
    out = strip_wrapper_noise(out)
    err = strip_wrapper_noise(err)
    (GOLDEN_DIR / f"{key}.exit").write_text(f"{code}\n", encoding="utf-8")
    (GOLDEN_DIR / f"{key}.out").write_text(out, encoding="utf-8")
    err_mode = spec.get("err")
    if err_mode and "strip" in err_mode:
        if err.startswith(err_mode["strip"]):
            err = err[len(err_mode["strip"]):]
        else:
            raise SystemExit(f"{key}: stderr lacks the strip prefix: {err[:120]}")
        (GOLDEN_DIR / f"{key}.err").write_text(err, encoding="utf-8")
    elif err_mode and err_mode.get("exact"):
        (GOLDEN_DIR / f"{key}.err").write_text(err, encoding="utf-8")
    elif err_mode and "prefix" in err_mode:
        (GOLDEN_DIR / f"{key}.err").write_text(err_mode["prefix"], encoding="utf-8")
    log = runtime / "gh.log"
    if log.exists():
        # The native comments read walks every page (--paginate); the Python
        # leg read one. review_findings_parity.rs drops the same token before
        # comparing, so the log parity holds on the one call that differs.
        text = log.read_text(encoding="utf-8").replace(" --paginate", "")
        (GOLDEN_DIR / f"{key}.log").write_text(text, encoding="utf-8")
    else:
        (GOLDEN_DIR / f"{key}.log").unlink(missing_ok=True)
    body = runtime / "gh-body.txt"
    if body.exists():
        (GOLDEN_DIR / f"{key}.body").write_text(body.read_text(encoding="utf-8"), encoding="utf-8")
    else:
        (GOLDEN_DIR / f"{key}.body").unlink(missing_ok=True)


def write_rust_only_fixtures() -> None:
    """The page-two case: a full first page, the marker on page two."""
    key = slug(RUST_ONLY_CASE["label"])
    case_dir = GOLDEN_DIR / "cases" / key
    case_dir.mkdir(parents=True, exist_ok=True)
    page_one = [comment_row(f"filler {n}") for n in range(100)]
    page_two = [
        comment_row(f"unrelated\n{COMMENT_MARKER}\ntail"),
        comment_row("also unrelated"),
    ]
    (case_dir / "findings.json").write_text(
        json.dumps(payload_with_dispositions()), encoding="utf-8"
    )
    (case_dir / "comments_p1.json").write_text(json.dumps(page_one), encoding="utf-8")
    (case_dir / "comments_p2.json").write_text(json.dumps(page_two), encoding="utf-8")
    (case_dir / "comments_both.json").write_text(
        json.dumps(page_one + page_two), encoding="utf-8"
    )
    (case_dir / "pr_info.json").write_text(json.dumps(PR_INFO), encoding="utf-8")
    for stale in (GOLDEN_DIR / f"{key}.log", GOLDEN_DIR / f"{key}.body"):
        stale.unlink(missing_ok=True)
    # Hand-written golden: the marker is found on page two, so nothing is
    # posted; the argv log carries the paginated comments read with the
    # --paginate token already normalized out.
    (GOLDEN_DIR / f"{key}.exit").write_text("0\n", encoding="utf-8")
    (GOLDEN_DIR / f"{key}.out").write_text(
        f"post-dispositions: round comment for {HEAD9} already posted\n",
        encoding="utf-8",
    )
    (GOLDEN_DIR / f"{key}.log").write_text(
        f"api repos/o/r/pulls/7\napi repos/o/r/issues/7/comments?per_page=100\n",
        encoding="utf-8",
    )


def write_retired_flag_goldens() -> None:
    """Hand-written rust-only goldens for the retired attest flags.

    The refusal fires at argv parse, so every case reads the same shape:
    exit 2, empty stdout, the one-line refusal on stderr, and no gh call
    (no log, no body). The fixture exists only for the parity harness's
    unconditional copy.
    """
    for spec in RETIRED_CASES:
        key = slug(spec["label"])
        case_dir = GOLDEN_DIR / "cases" / key
        case_dir.mkdir(parents=True, exist_ok=True)
        (case_dir / "findings.json").write_text(
            json.dumps(RETIRED_FIXTURE_PAYLOAD), encoding="utf-8"
        )
        flag = next(
            a
            for a in spec["args"]
            if a.startswith("--") and not a.startswith("--findings-file")
        )
        flag = flag.split("=", 1)[0]
        for stale in (GOLDEN_DIR / f"{key}.log", GOLDEN_DIR / f"{key}.body"):
            stale.unlink(missing_ok=True)
        (GOLDEN_DIR / f"{key}.exit").write_text("2\n", encoding="utf-8")
        (GOLDEN_DIR / f"{key}.out").write_text("", encoding="utf-8")
        (GOLDEN_DIR / f"{key}.err").write_text(retired_message(flag), encoding="utf-8")


def write_stub_and_cases() -> None:
    stub = GOLDEN_DIR / "gh_stub.sh"
    stub.write_text(GH_STUB, encoding="utf-8")
    stub.chmod(stub.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
    (GOLDEN_DIR / "cases.json").write_text(
        json.dumps(cases_json(), indent=2) + "\n", encoding="utf-8"
    )


def capture_specs(specs: list[dict], root: Path) -> list[str]:
    """Run each spec against the live Python leg and freeze its golden."""
    failures: list[str] = []
    for spec in specs:
        key = slug(spec["label"])
        case_dir = GOLDEN_DIR / "cases" / key
        write_fixtures(case_dir, spec)
        runtime = build_runtime(spec, case_dir, root, key)
        out, err, code = run_leg(spec, case_dir, runtime)
        if not out and code == 0:
            failures.append(key)
        freeze(runtime, spec, out, err, code)
        print(f"{key} exit={code} out={len(out)} err={len(err)}", flush=True)
    return failures


def write_goldens() -> int:
    if GOLDEN_DIR.exists():
        shutil.rmtree(GOLDEN_DIR)
    GOLDEN_DIR.mkdir(parents=True)
    write_stub_and_cases()
    root = Path(tempfile.mkdtemp(prefix="review-findings-capture-"))
    try:
        failures = capture_specs(ALL_CASES, root)
        write_rust_only_fixtures()
        write_retired_flag_goldens()
    finally:
        shutil.rmtree(root, ignore_errors=True)
    if failures:
        print(f"EMPTY STDOUT (exit 0) for: {', '.join(failures)}", file=sys.stderr)
        return 1
    return 0


def capture_classify_only() -> int:
    """Capture ONLY the classify table into the existing golden dir.

    The full run deletes the dir and replays every case against whatever
    fno-agents binary is deployed, which can churn goldens this capture is
    not about. The classify cases are additive: the dir stays, the derived
    files (gh_stub.sh, cases.json) rewrite whole, and only classify_* files
    are written.
    """
    if not GOLDEN_DIR.is_dir():
        print(f"{GOLDEN_DIR} does not exist; run --write-goldens first", file=sys.stderr)
        return 2
    write_stub_and_cases()
    root = Path(tempfile.mkdtemp(prefix="review-findings-capture-"))
    try:
        failures = capture_specs(CLASSIFY_CASES, root)
        write_retired_flag_goldens()
    finally:
        shutil.rmtree(root, ignore_errors=True)
    if failures:
        print(f"EMPTY STDOUT (exit 0) for: {', '.join(failures)}", file=sys.stderr)
        return 1
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--write-goldens", action="store_true")
    parser.add_argument(
        "--only",
        choices=["classify"],
        help="capture one table into the existing golden dir without replaying the rest",
    )
    args = parser.parse_args()
    if args.only == "classify":
        return capture_classify_only()
    if args.write_goldens:
        return write_goldens()
    parser.print_usage(sys.stderr)
    print(
        "the Python leg this driver captured is deleted; the goldens are"
        " frozen and review_findings_parity.rs is their consumer",
        file=sys.stderr,
    )
    return 2


if __name__ == "__main__":
    sys.exit(main())
