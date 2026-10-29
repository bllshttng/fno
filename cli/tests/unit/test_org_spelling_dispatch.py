"""Every front argv from the native check-in emitters resolves.

The emitters use the organization door. The Rust front owns the dispatch of
both role forms (crates/fno/src/lead_route.rs).
"""
from __future__ import annotations

import re
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[3]
EMITTERS = [
    REPO_ROOT / "crates/fno-agents/src/lead_checkin.rs",
    REPO_ROOT / "crates/fno-agents/src/loop_lead.rs",
]


def _front_argvs() -> list[list[str]]:
    argvs: list[list[str]] = []
    for path in EMITTERS:
        for m in re.finditer(r'\[\s*"agents"[^\]]*\]', path.read_text(encoding="utf-8")):
            tokens = re.findall(r'"([^"]*)"', m.group(0))
            if len(tokens) >= 2:
                argvs.append(tokens)
    assert len(argvs) >= 3, "the emitters moved; teach this test their new shape"
    return argvs


def test_every_emitted_argv_is_an_org_spelling():
    argvs = _front_argvs()
    assert not any("lead" in argv for argv in argvs), argvs
    assert ["agents", "org", "drain"] in argvs

    stale = [
        str(p)
        for base in ("src", "tests")
        for p in (REPO_ROOT / "crates/fno-agents" / base).rglob("*.rs")
        if re.search(r'"agents",\s*"lead"', p.read_text(encoding="utf-8"))
    ]
    assert not stale, f"dead lead spellings remain: {stale}"

