"""Capture driver for the skill-diff golden corpus.

Runs the pure decision surface of the Python skill-diff proposer (engine,
guards, prompt build, proposal parser) over fixed inputs and freezes one
``{"input", "expected"}`` JSON golden per case under tests/golden/skill_diff.
The Rust port replays every case through its own functions and asserts
equality with the frozen expected values, so the corpus pins the contract the
Python leg held at capture time.

Run: PYTHONPATH=<repo>/cli/src python3 capture_driver.py

No store, no network, no subprocess: every case is a pure function of its
input. The oracle leg (cli/src/fno/skill_diff/) is deleted in the port that
lands this corpus, so the goldens cannot be re-derived afterward.
"""

import json
import sys
from pathlib import Path

FIXTURE_DIR = Path(__file__).resolve().parent
GOLDEN_DIR = FIXTURE_DIR.parents[1] / "golden" / "skill_diff"

sys.path.insert(0, str(FIXTURE_DIR.parents[4] / "cli" / "src"))

from fno.skill_diff import engine, guards, synthesize  # noqa: E402


def _rc(run_id, skill_id="fno:blueprint", top="structural_validity", fails=2):
    return {
        "type": "skill_eval_run_complete",
        "data": {"run_id": run_id, "skill_id": skill_id,
                 "failure_ranking": [{"dimension": top, "fail_count": fails}]},
    }


def _finding(run_id, verdict="fail", dim="structural_validity", tool_fault=False,
             skill_id="fno:blueprint", corpus_item_id=None):
    d = {"run_id": run_id, "skill_id": skill_id, "dimension": dim, "verdict": verdict}
    if tool_fault:
        d["tool_fault"] = True
    if corpus_item_id is not None:
        d["corpus_item_id"] = corpus_item_id
    return {"type": "skill_eval_finding", "data": d}


def _proposed(run_id, skill_id="fno:blueprint", added=1, removed=0):
    return {"type": "skill_diff_proposed",
            "data": {"run_id": run_id, "skill_id": skill_id, "pr_number": 1,
                     "branch": f"skill-diff-{run_id}", "cited_finding_ids": [],
                     "added_lines": added, "removed_lines": removed}}


def hunk(file="skills/blueprint/SKILL.md", old="", new="x", cites=("s1",), rationale=""):
    return {"file": file, "old_text": old, "new_text": new,
            "cited_finding_ids": list(cites), "rationale": rationale}


def case_unprocessed_basic():
    evs = [_rc("r1"), _rc("r2")]
    return evs, engine.unprocessed_runs(evs, "fno:blueprint")


def case_unprocessed_idempotent():
    evs = [_rc("r1"), _rc("r2"),
           {"type": "skill_diff_proposed", "data": {"run_id": "r1", "skill_id": "fno:blueprint"}},
           {"type": "skill_diff_noop", "data": {"run_id": "r2", "skill_id": "fno:blueprint"}}]
    return evs, engine.unprocessed_runs(evs, "fno:blueprint")


def case_unprocessed_skill_ref():
    evs = [_rc("sweep1"),
           {"type": "skill_eval_run_complete",
            "data": {"run_id": "after1", "skill_id": "fno:blueprint", "skill_ref": "deadbeef"}},
           {"type": "skill_eval_run_complete",
            "data": {"run_id": "after2", "skill_id": "fno:blueprint", "skill_ref": None}}]
    return evs, engine.unprocessed_runs(evs, "fno:blueprint")


def case_unprocessed_pair_key():
    evs = [_rc("shared", skill_id="fno:blueprint"), _rc("shared", skill_id="fno:review"),
           {"type": "skill_diff_proposed", "data": {"run_id": "shared", "skill_id": "fno:blueprint"}}]
    return evs, {"blueprint": engine.unprocessed_runs(evs, "fno:blueprint"),
                 "review": engine.unprocessed_runs(evs, "fno:review")}


def case_findings_tool_fault():
    evs = [_finding("r1"), _finding("r1", tool_fault=True), _finding("r1", verdict="pass")]
    return evs, engine.findings_for_run(evs, "r1")


def case_ranking_from_run_complete():
    evs = [_rc("r1", top="collision_free", fails=3)]
    return evs, engine.failure_ranking(evs, "r1", "fno:blueprint")


def case_ranking_fallback_sorted():
    evs = [_finding("r1", dim="zeta"), _finding("r1", dim="alpha"),
           _finding("r1", dim="zeta", verdict="pass"), _finding("r1", dim="alpha")]
    return evs, engine.failure_ranking(evs, "r1", "fno:blueprint")


def case_has_actionable():
    evs = [_finding("r1", verdict="pass"), _finding("r1", dim="other", verdict="degraded")]
    return evs, {"actionable": engine.has_actionable_findings(evs, "r1", "fno:blueprint")}


def case_top_dimension():
    evs = [_rc("r1", top="collision_free", fails=1)]
    return evs, engine.top_dimension(evs, "r1", "fno:blueprint")


def case_local_maxima_trips():
    evs = [_rc("r1", top="collision_free"), _rc("r2", top="collision_free"),
           _rc("r3", top="collision_free"), _finding("r3", dim="collision_free"),
           _proposed("r1"), _proposed("r2")]
    return evs, engine.local_maxima_tripped(evs, "fno:blueprint", "r3")


def case_local_maxima_no_proposal():
    evs = [_rc("r1", top="collision_free"), _rc("r2", top="collision_free"),
           _rc("r3", top="collision_free")]
    return evs, engine.local_maxima_tripped(evs, "fno:blueprint", "r3")


def case_local_maxima_short_window():
    evs = [_rc("r1", top="collision_free"), _rc("r2", top="collision_free"), _proposed("r1")]
    return evs, engine.local_maxima_tripped(evs, "fno:blueprint", "r2")


def case_local_maxima_different_top():
    evs = [_rc("r1", top="collision_free"), _rc("r2", top="collision_free"),
           _rc("r3", top="naming"), _finding("r3", dim="naming"), _proposed("r1")]
    return evs, engine.local_maxima_tripped(evs, "fno:blueprint", "r3")


def case_build_pr_body_drifted():
    body = engine.build_pr_body(
        run_id="r1", skill_id="fno:blueprint",
        hunks=[{"file": "skills/blueprint/SKILL.md", "cited_finding_ids": ["s1"], "rationale": "why"}],
        justification=None, bloat=None,
        version_observed="aaa", version_against="bbb", is_review_skill=False)
    return {"run_id": "r1", "skill_id": "fno:blueprint",
            "hunks": [{"file": "skills/blueprint/SKILL.md", "cited_finding_ids": ["s1"], "rationale": "why"}],
            "version_observed": "aaa", "version_against": "bbb"}, body


def case_build_pr_body_full():
    bloat = {"net_growth": 190, "window": 1, "threshold": 120}
    hunks = [
        {"file": "skills/blueprint/SKILL.md", "old_text": "a", "new_text": "b",
         "cited_finding_ids": ["s1", "s2"], "rationale": "first"},
        {"file": "skills/blueprint/references/x.md", "old_text": "",
         "new_text": "added", "cited_finding_ids": ["s3"], "rationale": ""},
    ]
    body = engine.build_pr_body(
        run_id="r9", skill_id="fno:review", hunks=hunks,
        justification="the checklists were stale", bloat=bloat,
        version_observed="same", version_against="same", is_review_skill=True)
    return {"run_id": "r9", "skill_id": "fno:review", "hunks": hunks,
            "justification": "the checklists were stale", "bloat": bloat,
            "version_observed": "same", "version_against": "same"}, body


def case_filter_cited_hunks():
    hunks = [hunk(new="keep"), hunk(cites=[]), hunk(cites=[""]), hunk(new="drop-missing", cites=[])]
    kept, dropped = guards.filter_cited_hunks(hunks)
    return {"hunks": hunks}, {"kept": kept, "dropped_new": [d["new_text"] for d in dropped]}


def case_additive_threshold():
    over = [hunk(new="x\n" * (guards.ADDITIVE_LINE_THRESHOLD + 1))]
    under = [hunk(old="y", new="x\n" * (guards.ADDITIVE_LINE_THRESHOLD + 1))]
    return {"threshold": guards.ADDITIVE_LINE_THRESHOLD}, {
        "over": guards.additive_only_needs_justification(over),
        "under": guards.additive_only_needs_justification(under)}


def case_bloat_flag():
    over = [{"added_lines": 200, "removed_lines": 10}]
    under = [{"added_lines": 10, "removed_lines": 5}]
    return {"over": over, "under": under}, {
        "over": guards.bloat_flag(over), "under": guards.bloat_flag(under)}


def case_redaction_internal():
    text = "cites internal/fno/plan.md and ~/.fno/graph.json and /.fno/ state"
    return {"text": text, "project_names": []}, guards.redaction_violations(text, [])


def case_redaction_projects():
    return {"text": "touches acme and fno and acme-corp", "project_names": ["acme", "fno"]}, \
        guards.redaction_violations("touches acme and fno and acme-corp", ["acme", "fno"])


def case_parse_nested():
    text = ('{"verdict":"propose_pr","hunks":['
            '{"file":"a","new_text":"x","cited_finding_ids":["s1"]},'
            '{"file":"b","new_text":"y","cited_finding_ids":["s2"]}],"justification":"j"}')
    p = synthesize.parse_proposal(text)
    return {"text": text}, {"verdict": p.verdict,
                            "files": [h["file"] for h in p.hunks],
                            "justification": p.justification}


def case_parse_fenced():
    text = '```json\n{"verdict":"no_diff_helps","hunks":[{"file":"a","cited_finding_ids":["s1"]}],"no_diff_reason":"arch"}\n```'
    p = synthesize.parse_proposal(text)
    return {"text": text}, {"verdict": p.verdict, "hunks": len(p.hunks),
                            "no_diff_reason": p.no_diff_reason}


def case_parse_unknown_verdict():
    text = '{"verdict":"ship_it"}'
    try:
        synthesize.parse_proposal(text)
        err = None
    except synthesize.ProposalParseError as exc:
        err = str(exc)
    return {"text": text}, {"error": err}


def case_parse_string_cites():
    text = '{"verdict":"propose_pr","hunks":[{"file":"a","cited_finding_ids":"s1"}]}'
    try:
        synthesize.parse_proposal(text)
        err = None
    except synthesize.ProposalParseError as exc:
        err = str(exc)
    return {"text": text}, {"error": err}


def case_parse_no_json():
    text = "the model refused"
    try:
        synthesize.parse_proposal(text)
        err = None
    except synthesize.ProposalParseError as exc:
        err = str(exc)
    return {"text": text}, {"error": err}


def case_build_prompt_driver():
    findings = [{"dimension": "collision_free", "verdict": "fail", "corpus_item_id": "c1",
                 "evidence": "ev" * 300}]
    ranking = [{"dimension": "collision_free", "fail_count": 2}]
    history = [{"run_id": "r0", "added_lines": 4, "removed_lines": 1,
                "cited_finding_ids": ["s0"]}]
    files = {"skills/blueprint/SKILL.md": "# Blueprint\nbody"}
    prompt = synthesize.build_prompt(
        skill_id="fno:blueprint", skill_files=files, findings=findings,
        ranking=ranking, history=history,
        additive_threshold=guards.ADDITIVE_LINE_THRESHOLD)
    return {"skill_id": "fno:blueprint", "skill_files": files, "findings": findings,
            "ranking": ranking, "history": history,
            "additive_threshold": guards.ADDITIVE_LINE_THRESHOLD}, prompt


def case_build_prompt_plain():
    files = {"skills/custom/SKILL.md": "# Custom"}
    prompt = synthesize.build_prompt(
        skill_id="fno:custom", skill_files=files, findings=[], ranking=[],
        history=[], additive_threshold=guards.ADDITIVE_LINE_THRESHOLD)
    return {"skill_id": "fno:custom", "skill_files": files, "findings": [],
            "ranking": [], "history": [],
            "additive_threshold": guards.ADDITIVE_LINE_THRESHOLD}, prompt


CASES = [
    ("unprocessed_basic", case_unprocessed_basic),
    ("unprocessed_idempotent", case_unprocessed_idempotent),
    ("unprocessed_skill_ref", case_unprocessed_skill_ref),
    ("unprocessed_pair_key", case_unprocessed_pair_key),
    ("findings_tool_fault", case_findings_tool_fault),
    ("ranking_from_run_complete", case_ranking_from_run_complete),
    ("ranking_fallback_sorted", case_ranking_fallback_sorted),
    ("has_actionable", case_has_actionable),
    ("top_dimension", case_top_dimension),
    ("local_maxima_trips", case_local_maxima_trips),
    ("local_maxima_no_proposal", case_local_maxima_no_proposal),
    ("local_maxima_short_window", case_local_maxima_short_window),
    ("local_maxima_different_top", case_local_maxima_different_top),
    ("build_pr_body_drifted", case_build_pr_body_drifted),
    ("build_pr_body_full", case_build_pr_body_full),
    ("filter_cited_hunks", case_filter_cited_hunks),
    ("additive_threshold", case_additive_threshold),
    ("bloat_flag", case_bloat_flag),
    ("redaction_internal", case_redaction_internal),
    ("redaction_projects", case_redaction_projects),
    ("parse_nested", case_parse_nested),
    ("parse_fenced", case_parse_fenced),
    ("parse_unknown_verdict", case_parse_unknown_verdict),
    ("parse_string_cites", case_parse_string_cites),
    ("parse_no_json", case_parse_no_json),
    ("build_prompt_driver", case_build_prompt_driver),
    ("build_prompt_plain", case_build_prompt_plain),
]


def main() -> int:
    GOLDEN_DIR.mkdir(parents=True, exist_ok=True)
    for slug, fn in CASES:
        data_input, expected = fn()
        golden = {"input": data_input, "expected": expected}
        path = GOLDEN_DIR / f"{slug}.json"
        path.write_text(json.dumps(golden, indent=1, sort_keys=True) + "\n", encoding="utf-8")
        print(f"froze {path}")
    print(f"{len(CASES)} goldens under {GOLDEN_DIR}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
