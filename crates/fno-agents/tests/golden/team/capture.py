#!/usr/bin/env python3
"""Capture the team render goldens from the Python leg (run once, before it
was deleted):

    uv run --project cli python crates/fno-agents/tests/golden/team/capture.py

The registry, graph, fold and gate reads are live state, so each case freezes
them: `gather_team` answers the case's team, the fold answers the case's
fold, the gate answers the case's verdict. goldens.json records those inputs
with the bytes `render_team` printed for the table, `--json` and `--nodes`.
The Rust test feeds the same inputs to the native render.
"""

from __future__ import annotations

import copy
import json
import sys
from pathlib import Path

from fno.agents import team as t

OUT = Path(__file__).resolve().parent


def role(holder, level, scope, *, agree=True, reason=None, status="idle", source="row",
         grantor="human"):
    return {
        "holder": holder, "level": level, "scope": scope, "grantor": grantor,
        "status": status, "agree": agree, "reason": reason, "manifest_path": None,
        "manifest_session": None, "role_source": source,
    }


def team(roles, conflicts=(), *, sweep_ran=True, manifest_only=0):
    return {
        "roles": roles,
        "conflicts": list(conflicts),
        "registry_readable": True,
        "graph_readable": True,
        "summary": {
            "total": len(roles) - manifest_only,
            "manifest_only": manifest_only,
            "sweep_ran": sweep_ran,
            "disagreements": sum(1 for r in roles if r["agree"] is False),
            "unknowns": sum(1 for r in roles if r["agree"] is None),
            "splits": sum(1 for r in roles if r["role_source"] == "split"),
        },
    }


STUCK = {"unclaimed": ["x-aaaa"], "blocked": [], "unproven_claim": [], "in_review": [],
         "blind": [], "threshold_minutes": 30}
NODES = {"status": "resolved", "counts": {"ready": 1}, "nodes": [
    {"id": "x-aaaa", "status": "ready", "sessions": ["sid-live", "sid-gone", "sid-unknown"]}]}
FOLD = {"scope_nodes": {"fno": NODES, "x-epic1": NODES}, "stuck": STUCK,
        "stuck_line": "1 unclaimed (x-aaaa)"}
GATE_OK = {"verdict": "admit", "max_live": 8, "live_workers": 2}
SESSIONS = {"sid-live": "idle", "sid-gone": "exited"}

ROLES = [
    role("lead-fno", 1, "fno"),
    role("lead-epic", 2, "x-epic1", agree=False, reason="'x-epic1' is a feature, not an epic",
         source="split"),
    role("half", None, "x-epic2", agree=False,
         reason="half a role: scope is set but level is missing"),
    role("x-gone", 2, "x-gone", agree=None, status="manifest-only", source="manifest",
         reason="role lives on the manifest; no live registry row holds it"),
]
PAIR = {"scope": "x-epic1", "holders": ["lead-epic", "half"], "members": ["x-epic1"]}

CASES = [
    ("full", team(ROLES, [PAIR], manifest_only=1), FOLD, GATE_OK),
    ("gate-unknown", team(ROLES[:1]), FOLD, {"verdict": "unknown", "reason": "gate_unavailable"}),
    ("gate-refused", team(ROLES[:1]), FOLD, {"verdict": "refused", "reason": "max_live"}),
    ("fold-did-not-run", team(ROLES[:1]), {}, GATE_OK),
    ("fold-predates", team(ROLES[:1]), {"scope_nodes": {"fno": NODES}}, GATE_OK),
    ("sweep-did-not-run", team(ROLES[:1], sweep_ran=False), FOLD, GATE_OK),
    ("no-roles", team([]), FOLD, GATE_OK),
    ("unreadable", {
        "roles": None, "conflicts": None, "registry_readable": False, "graph_readable": None,
        "summary": {"total": None, "disagreements": None, "unknowns": None, "splits": None,
                    "reason": "registry unreadable: boom"},
    }, FOLD, GATE_OK),
]


def fold_like_python(roles, folded):
    """The fold's assignment step, as fold_scope_nodes runs it after a read."""
    if not roles:
        return {}
    if not folded:
        for r in roles:
            r.setdefault("scope_nodes", {"status": "unresolved",
                                         "reason": "the fold could not run: frozen"})
        return {}
    for r in roles:
        r["scope_nodes"] = copy.deepcopy(folded["scope_nodes"].get(
            r.get("scope"),
            {"status": "unresolved", "reason": "the fold answered no row for this scope"}))
    return folded


def annotate_like_python(roles):
    for r in roles:
        fold = r.get("scope_nodes")
        if not isinstance(fold, dict):
            continue
        for node in fold.get("nodes") or []:
            node["sessions"] = [
                {"id": s, "live": None if s not in SESSIONS
                 else SESSIONS[s] not in {"exited", "orphaned", "failed", "permanent_dead"},
                 "status": SESSIONS.get(s)}
                for s in node.get("sessions") or [] if isinstance(s, str)
            ]
    return True


def main() -> int:
    out = {}
    for case, frozen_team, folded, gate in CASES:
        for mode, as_json, nodes in (("table", False, False), ("json", True, False),
                                     ("nodes", False, True)):
            t.gather_team = lambda: copy.deepcopy(frozen_team)
            t.fold_scope_nodes = lambda roles: fold_like_python(roles, copy.deepcopy(folded))
            t._annotate_sessions = annotate_like_python
            t._gate_read = lambda: copy.deepcopy(gate)
            out[f"{case}|{mode}"] = t.render_team(as_json, nodes=nodes)
        out[f"{case}|inputs"] = {"team": frozen_team, "fold": folded, "gate": gate}
    out["sessions"] = SESSIONS
    json.dump(out, sys.stdout, indent=1, sort_keys=True)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
