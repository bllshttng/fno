"""Capture driver for the think-dispatch golden corpus (pure.json).

Freezes the deterministic parts of the Python `fno do think dispatch` leg
(`fno.provenance.cli.dispatch` over `fno.provenance.spawn_think`) into
`tests/golden/think_dispatch/pure.json`, one `{"fn", "input", "output"}` case
per line. The Rust parity test (`tests/think_dispatch_parity.rs`) replays every
case against `fno_agents::think_dispatch`.

Families:

- `assemble_seed`: the spawn prompt and offer line for a fixed node, a fixed
  transcript resolution, and a fixed output path.
- `output_path`: `_find_node_doc` and `_think_output_path` over a throwaway
  plans dir built from the case's file list. Paths come back relative to the
  plans dir (`<pdir>/...`) and the mint date as `{date}`.
- `worker_agent_name`: the conversational worker name, or an error marker.
- `parse_short_id`: the spawn receipt parse over sample stdout.
- `dedup_key`: the reason-scoped dedup token, read from a matrix run.
- `tail`: the conversational decision matrix with every side effect patched
  at its seam: the result, the decision events, and the seam calls.
- `cli`: the command's flag, provenance, and node-resolution glue with the
  dispatch core patched out: exit code, stdout, stderr, and what reached the
  core.

Run from the repo root with the worktree source on the path:

    PYTHONPATH=cli/src python3 crates/fno-agents/tests/fixtures/think_dispatch/capture_driver.py

The goldens freeze the Python leg while it lives. After it retires the frozen
file is the contract: a golden can only be captured from the leg it
characterizes.
"""

import contextlib
import io
import json
import os
import tempfile
from datetime import datetime, timezone
from pathlib import Path

FIXTURE_DIR = Path(__file__).resolve().parent
TESTS_DIR = FIXTURE_DIR.parents[1]
GOLDEN = TESTS_DIR / "golden" / "think_dispatch" / "pure.json"

STRIP_VARS = [
    "CODEX_THREAD_ID",
    "CLAUDE_CODE_SESSION_ID",
    "CODEX_SESSION_ID",
    "GEMINI_SESSION_ID",
    "OPENCODE_SESSION_ID",
    "CLAUDE_SESSION_ID",
    "FNO_THINK_SPAWN",
    "FNO_THINK_SPAWN_ATTENDED",
    "FNO_THINK_SPAWN_PRESENCE",
    "FNO_TRACKER_BACKEND",
]

BASE_NODE = {
    "id": "x-2222aaaa",
    "slug": "born-with-why",
    "title": "Idea nodes born with their why",
    "details": "the why lives in the transcript",
    "cwd": "/tmp/proj",
    "source_node_id": "x-0000aaaa",
}


def node(**over):
    out = dict(BASE_NODE)
    for key, value in over.items():
        if value is None:
            out.pop(key, None)
        else:
            out[key] = value
    return out


def transcript(path=None, resolved=None, reason=None, ambiguous=False):
    return {
        "path": path,
        "resolved": bool(path) if resolved is None else resolved,
        "reason": reason,
        "ambiguous": ambiguous,
    }


def fake_resolver(spec):
    from fno.provenance.resolver import ResolvedTranscript

    def resolve(harness, sid, cwd, **_kw):
        return ResolvedTranscript(
            harness,
            sid,
            cwd,
            spec["resolved"],
            transcript_path=spec["path"],
            reason=spec["reason"],
            ambiguous=spec["ambiguous"],
        )

    return resolve


def seed_cases(st):
    specs = [
        (node(source_harness="claude", source_session_id="abc12345", source_cwd="/tmp/sess"),
         transcript("/x/t.jsonl"), "/plans/2026-01-01-born-with-why-x-2222aaaa.md"),
        (node(source_harness="claude", source_session_id="abc12345", source_cwd="/tmp/sess"),
         transcript("/x/t.jsonl", ambiguous=True), ""),
        (node(source_harness="codex", source_session_id="s-1", source_cwd="/tmp/live"),
         transcript(reason="not-found"), "/plans/out.md"),
        (node(source_harness="gemini", source_session_id="g-1", source_cwd="/c"),
         transcript(reason=None), "/plans/out.md"),
        (node(slug=None, details=None, source_node_id=None, source_session_id="abc"),
         transcript(reason="missing-input"), ""),
        (node(details="   \n  padded details  \n", slug="", title=""),
         transcript("/y/t.jsonl"), "/plans/a.md"),
        (node(id=None, title=None, source_node_id="", source_harness="", source_cwd=""),
         transcript(reason="harness-not-supported"), ""),
        (node(source_harness="claude", source_session_id="abc12345", source_cwd="/tmp/sess"),
         transcript(None, resolved=True), "/plans/b.md"),
        (node(details="multi\nline\ndetails", title="Unicode tïtle"),
         transcript("/z/é.jsonl"), "/plans/c.md"),
    ]
    cases = []
    for n, t, out in specs:
        st.resolve_transcript = fake_resolver(t)
        st._think_output_path = lambda nid, slug, root=None, _o=out: _o
        seed = st.assemble_seed(dict(n))
        cases.append({
            "fn": "assemble_seed",
            "input": {"node": n, "transcript": t, "output_path": out},
            "output": {
                "prompt": seed.prompt,
                "offer_line": seed.offer_line,
                "resolved": seed.resolved,
                "output_path": seed.output_path,
            },
        })
    return cases


OUTPUT_SPECS = [
    # A frontmatter claim wins over a name match, in sorted order.
    {"node_id": "x-2222aaaa", "slug": "my-slug", "files": [
        {"name": "2020-01-01-a-x-2222aaaa.md", "text": "# by name\n"},
        {"name": "2020-02-02-stub.md", "text": "---\nclaims: x-2222aaaa\n---\nbody\n"},
    ]},
    {"node_id": "x-2222aaaa", "slug": "my-slug", "files": [
        {"name": "2020-03-03-stub.md", "text": "---\ntitle: t\ngraph_node_id: \"x-2222aaaa\"  \n---\n"},
    ]},
    {"node_id": "x-2222aaaa", "slug": "my-slug", "files": [
        {"name": "2020-03-03-stub.md", "text": "---\nclaims: 'x-2222aaaa'\n"},
    ]},
    # A claim in the body only is not a claim; the name match wins.
    {"node_id": "x-2222aaaa", "slug": "my-slug", "files": [
        {"name": "2020-01-01-body.md", "text": "---\ntitle: t\n---\nclaims: x-2222aaaa\n"},
        {"name": "2020-04-04-old-slug-x-2222aaaa.md", "text": "# earlier\n"},
    ]},
    # No frontmatter fence at all.
    {"node_id": "x-2222aaaa", "slug": "my-slug", "files": [
        {"name": "2020-01-01-nofm.md", "text": "claims: x-2222aaaa\n"},
    ]},
    # A legacy exact-slug doc is reused; `awesome-<slug>` never over-matches.
    {"node_id": "x-2222aaaa", "slug": "my-slug", "files": [
        {"name": "2020-01-01-awesome-my-slug.md", "text": "# other\n"},
        {"name": "2020-05-05-my-slug.md", "text": "# legacy\n"},
    ]},
    {"node_id": "x-2222aaaa", "slug": "-my-slug-", "files": [
        {"name": "2020-05-05-my-slug.md", "text": "# legacy\n"},
    ]},
    # Mint: slug, empty slug, dash-only slug, non-matching neighbours.
    {"node_id": "x-2222aaaa", "slug": "my-slug", "files": [
        {"name": "2020-01-01-other-x-3333bbbb.md", "text": "# other node\n"},
        {"name": "notes.md", "text": "claims: x-2222aaaa\n"},
        {"name": "2020-01-01-x-2222aaaa.txt", "text": ""},
    ]},
    {"node_id": "x-2222aaaa", "slug": "", "files": []},
    {"node_id": "x-2222aaaa", "slug": "---", "files": []},
    # A hidden dated file is invisible to the glob.
    {"node_id": "x-2222aaaa", "slug": "s", "files": [
        {"name": ".2020-01-01-x-2222aaaa.md", "text": ""},
    ]},
]


def rel(path, pdir):
    if path is None:
        return None
    text = str(path)
    prefix = str(pdir) + os.sep
    return "<pdir>/" + text[len(prefix):] if text.startswith(prefix) else text


def output_cases(st, originals):
    for name, value in originals.items():
        setattr(st, name, value)
    cases = []
    today = datetime.now(timezone.utc).strftime("%Y-%m-%d")
    for spec in OUTPUT_SPECS:
        with tempfile.TemporaryDirectory() as tmp:
            pdir = Path(tmp).resolve() / "plans"
            pdir.mkdir()
            for f in spec["files"]:
                (pdir / f["name"]).write_text(f["text"], encoding="utf-8")
            found = st._find_node_doc(pdir, spec["node_id"])
            st._plans_output_dir = lambda root=None, _p=pdir: _p
            out = st._think_output_path(spec["node_id"], spec["slug"])
            out = rel(out, pdir).replace(today, "{date}")
            cases.append({
                "fn": "output_path",
                "input": spec,
                "output": {"find_node_doc": rel(found, pdir), "think_output_path": out},
            })
    return cases


def name_cases(st):
    specs = [
        ("x-1", "slug", "abc12345", None),
        ("x-2222aaaa", "born-with-why", "LIVE-sid", None),
        ("x-2222aaaa", "Idea nodes born with their why", "abc12345", None),
        ("x-2222aaaa", "born-with-why", "abc12345", "glm-4.7"),
        ("x-2222aaaa", None, "abc12345", None),
        ("x-2222aaaa", "born-with-why", None, None),
        ("x-2222aaaa", "", "", ""),
        ("n-" + "z" * 50, "slug", "sessaaaa", None),
    ]
    cases = []
    for node_id, slug, suffix, model in specs:
        try:
            out = st._worker_agent_name(node_id, slug, "conversational", suffix, model=model)
        except Exception:  # noqa: BLE001 - the error marker is the golden
            out = {"error": True}
        cases.append({
            "fn": "worker_agent_name",
            "input": {"node_id": node_id, "slug": slug, "suffix": suffix, "model": model},
            "output": out,
        })
    return cases


def short_id_cases(st):
    samples = [
        '{"short_id":"abc123"}',
        '{"short_id": "abc123"}\n',
        '{\n  "name": "th-th-1",\n  "short_id": "pretty1"\n}\n',
        'banner line\n{"short_id":"real123","name":"x"}\ntrailer\n',
        'log: wrote "short_id" somewhere\n{"short_id":"after"}\n',
        '{"pane_id":"%1"}\n',
        'I have finished the design pass.\n',
        '',
        '{"short_id":""}\n{"short_id":"second"}\n',
        '{"short_id": 42}',
        '{"name": "a"}\n{"short_id": "line2"}',
    ]
    return [
        {"fn": "parse_short_id", "input": {"stdout": s}, "output": st._parse_short_id(s)}
        for s in samples
    ]


def tail_specs():
    ok = {"spawn": "ok:deadbeef"}
    live = {"session_id": "LIVE-sid-123456789", "cwd": "/tmp/live", "harness": "claude"}
    return [
        dict(label="spawned-resolved", node=node(), transcript=transcript("/live/t.jsonl"), **live, **ok),
        dict(label="spawned-unresolved", node=node(), transcript=transcript(reason="not-found"),
             session_id="s1", cwd="/c", harness="codex", **ok),
        dict(label="autonomy-off", node=node(), autonomy=False, **live, **ok),
        dict(label="no-node-id", node=node(id=None), **live, **ok),
        dict(label="empty-node-id", node=node(id=""), **live, **ok),
        dict(label="bulk-roadmap", node=node(roadmap_id="r-1"), **live, **ok),
        dict(label="bulk-vision", node=node(vision_path="/v.md"), **live, **ok),
        dict(label="null-bulk-keys", node=node(roadmap_id=None, vision_path=None), **live, **ok),
        dict(label="no-origin", node=node(), session_id="   ", cwd="/c", harness="claude", **ok),
        dict(label="bug-still-spawns", node=node(type="bug", size="S"), **live, **ok),
        dict(label="cap-zero", node=node(), cap=0, **live, **ok),
        dict(label="daily-cap", node=node(), day_cap=3, day_count=3, **live, **ok),
        dict(label="daily-cap-off", node=node(), day_cap=0, day_count=99, **live, **ok),
        dict(label="claim-live", node=node(), claim_live=True, **live, **ok),
        dict(label="claim-held", node=node(), acquire="held", **live, **ok),
        dict(label="claim-error", node=node(), acquire="error:disk full", **live, **ok),
        dict(label="already-running", node=node(), spawn="exists", **live),
        dict(label="spawn-failed", node=node(), spawn="error:fno agents spawn exited 1: boom", **live),
        dict(label="attended-spawns", node=node(), presence="attended", **live, **ok),
        dict(label="headless-empty-session", node=node(), spawn="ok:", **live),
        dict(label="no-output-path", node=node(), output_path="", **live, **ok),
        dict(label="resolved-cwd-and-pins", node=node(_resolved_cwd="/root/proj", model="glm-4.7",
                                                      provider="claude", slug=""),
             **live, **ok),
        dict(label="title-as-slug-no-cwd", node=node(slug=None, cwd=None), session_id="abc", cwd="/c",
             harness="claude", **ok),
        dict(label="stored-origin-overlaid", node=node(source_harness="codex", source_session_id="STORED",
                                                       source_cwd="/birth"),
             **live, **ok),
    ]


def tail_case(st, spec):
    import fno.claims.core as core
    import fno.config as config

    t = spec.get("transcript", transcript("/x/t.jsonl"))
    calls = {"probed": [], "acquired": [], "released": [], "spawn": None, "bumps": 0,
             "stamps": [], "transcript": None, "output_path": None}
    events = []

    config.autonomy_master_enabled = lambda root=None: spec.get("autonomy", True)
    st._max_per_run = lambda root: spec.get("cap", 5)
    st.classify_presence = lambda **kw: spec.get("presence", "away")

    def resolve(harness, sid, cwd, **kw):
        calls["transcript"] = [harness, sid, cwd]
        return fake_resolver(t)(harness, sid, cwd)

    st.resolve_transcript = resolve
    out_path = spec.get("output_path", "/plans/out.md")

    def think_output_path(nid, slug, root=None):
        calls["output_path"] = [nid, slug]
        return out_path

    st._think_output_path = think_output_path
    st._daily_cap = lambda root: spec.get("day_cap", 20)
    st._daily_count = lambda: spec.get("day_count", 0)

    def claim_live(key):
        calls["probed"].append(key)
        return spec.get("claim_live", False)

    st._claim_is_live = claim_live
    acquire = spec.get("acquire", "ok")

    def acquire_claim(key, holder, **kw):
        calls["acquired"].append(key)
        if acquire == "held":
            raise core.ClaimContended("busy")
        if acquire.startswith("error:"):
            raise RuntimeError(acquire[len("error:"):])
        return None

    core.acquire_claim = acquire_claim
    st._safe_release = lambda key, holder: calls["released"].append(key)
    spawn = spec["spawn"]

    def spawn_worker(node_id, prompt, node_cwd, node_slug, reason="birth",
                     invocation_suffix=None, model=None, provider=None, node=None):
        calls["spawn"] = {"node_id": node_id, "prompt": prompt, "node_cwd": node_cwd,
                          "node_slug": node_slug, "reason": reason,
                          "suffix": invocation_suffix, "model": model, "provider": provider}
        if spawn == "exists":
            raise st.SpawnAlreadyRunning("agent exists")
        if spawn.startswith("error:"):
            raise st.SpawnError(spawn[len("error:"):])
        return spawn[len("ok:"):]

    st._spawn_think_worker = spawn_worker

    def bump():
        calls["bumps"] += 1

    st._bump_daily_count = bump
    st._stamp_forward = lambda nid, sess, root, output_path=None: calls["stamps"].append(
        [nid, sess, output_path]
    )
    st._emit = lambda kind, data, path: events.append([kind, dict(data)])

    err = io.StringIO()
    with contextlib.redirect_stderr(err):
        res = st.dispatch_conversational(
            dict(spec["node"]),
            session_id=spec["session_id"],
            cwd=spec["cwd"],
            harness=spec["harness"],
            events_path=Path(os.devnull),
            env={},
        )
    result = {
        "decision": res.decision,
        "event": res.event,
        "reason": res.reason,
        "node_id": res.node_id,
        "presence": res.presence,
        "resolved": res.resolved,
        "think_session": res.think_session,
        "detail": res.detail,
    }
    inputs = {k: v for k, v in spec.items() if k != "transcript"}
    inputs["transcript"] = t
    return {
        "fn": "tail",
        "input": inputs,
        "output": {
            "result": result,
            "events": events,
            "calls": calls,
            "notes": [line for line in err.getvalue().splitlines() if line],
        },
    }


def tail_cases(st):
    return [tail_case(st, spec) for spec in tail_specs()]


def dedup_cases(tail):
    cases = []
    seen = set()
    for case in tail:
        acquired = case["output"]["calls"]["acquired"]
        if not acquired:
            continue
        key = (case["input"]["node"]["id"], case["input"]["session_id"])
        if key in seen:
            continue
        seen.add(key)
        cases.append({
            "fn": "dedup_key",
            "input": {"node_id": key[0], "session_id": key[1]},
            "output": acquired[0],
        })
    return cases


CLI_ENTRIES = [
    {"id": "x-0a9c", "slug": "conv-think", "title": "conversational command",
     "source_session_id": "stored", "source_cwd": "/birth"},
    {"id": "x-0b0b", "slug": "proj-node", "title": "projected", "project": "nowhere"},
]

SPAWNED = {"decision": "spawned", "event": "think_spawned", "node_id": "x-0a9c",
           "presence": "attended", "resolved": True, "think_session": "abc123"}
SKIPPED = {"decision": "skipped", "event": "think_skipped", "reason": "daily-cap",
           "node_id": "x-0a9c", "presence": "away", "detail": "daily_cap=20"}
NOOP = {"decision": "noop", "event": None, "reason": "disabled", "node_id": "x-0a9c"}
CLAUDE = {"source_session_id": "live-sid", "source_harness": "claude", "source_cwd": "/live/sess"}
CODEX = {"source_session_id": "codex-thread", "source_harness": "codex", "source_cwd": "/live/codex"}
NONE = {"source_session_id": None, "source_harness": None, "source_cwd": None}


def cli_specs():
    return [
        dict(argv=["x-0a9c"], prov=CLAUDE, result=SPAWNED),
        dict(argv=["x-0a9c", "--json"], prov=CLAUDE, result=SPAWNED),
        dict(argv=["x-0a9c", "-J"], prov=CLAUDE, result=SKIPPED),
        dict(argv=["x-0a9c"], prov=CLAUDE, result=SKIPPED),
        dict(argv=["x-0a9c", "--json"], prov=CLAUDE, result=NOOP),
        dict(argv=["x-0a9c"], prov=CLAUDE, result=NOOP),
        dict(argv=["CONV-THINK", "--cwd", "/live/here/../x"], prov=CLAUDE, result=SPAWNED),
        dict(argv=["x-0a9c", "--cwd", "rel/dir"], prov=CLAUDE, result=SPAWNED),
        dict(argv=["x-0a9c", "--cwd", "   "], prov=CLAUDE, result=SPAWNED),
        dict(argv=["x-0a9c", "--session-id", "explicit", "--harness", "codex"], prov=NONE,
             result=SPAWNED),
        dict(argv=["x-0a9c", "--session-id", ""], prov=CLAUDE, result=SPAWNED),
        dict(argv=["x-0a9c", "--session-id", "  "], prov=CLAUDE, result=SPAWNED),
        dict(argv=["x-0a9c"], prov=NONE, result=SPAWNED),
        dict(argv=["x-0a9c", "--session-id", "sid-only"], prov=NONE, result=SPAWNED),
        dict(argv=["x-0a9c", "--json"], prov=CODEX, result=SPAWNED),
        dict(argv=["x-0a9c", "--provider", "codex", "--json"], prov=CODEX, result=SPAWNED),
        dict(argv=["x-0a9c", "-p", "codex"], prov=CLAUDE, result=SPAWNED),
        dict(argv=["x-0a9c", "--provider", " claude ", "-m", "glm-4.7"], prov=CLAUDE,
             result=SPAWNED),
        dict(argv=["x-0a9c", "--provider", "  "], prov=CLAUDE, result=SPAWNED),
        dict(argv=["x-0a9c", "--model", "   "], prov=CLAUDE, result=SPAWNED),
        dict(argv=["x-0a9c", "--model", "", "--provider", ""], prov=NONE, result=SPAWNED),
        dict(argv=["no-such-node-xyz"], prov=CLAUDE, result=SPAWNED),
        dict(argv=["it's-missing"], prov=CLAUDE, result=SPAWNED),
        dict(argv=["proj-node", "--model=m1"], prov=CLAUDE, result=SPAWNED),
    ]


def cli_case(spec):
    from typer.testing import CliRunner

    import fno.graph._intake as intake
    import fno.graph.cli as graph_cli
    import fno.provenance.spawn_think as st
    import fno.tracker.metadata as metadata
    from fno.provenance import cli as think_cli
    from fno.provenance.spawn_think import ThinkSpawnResult

    graph_cli._session_provenance = lambda *a, **k: dict(spec["prov"])
    metadata.read_entries = lambda reader, **k: [dict(e) for e in CLI_ENTRIES]
    intake.project_root_from_settings = lambda project: None
    reached = {}

    def fake_dispatch(target, *, session_id, cwd, harness="claude", **kw):
        reached.update(
            node_id=target.get("id"),
            model=target.get("model"),
            provider=target.get("provider"),
            session_id=session_id,
            cwd=cwd,
            harness=harness,
        )
        return ThinkSpawnResult(**spec["result"])

    st.dispatch_conversational = fake_dispatch
    os.chdir("/")
    r = CliRunner().invoke(think_cli.think_app, ["dispatch", *spec["argv"]])
    return {
        "fn": "cli",
        "input": {"argv": spec["argv"], "provenance": spec["prov"], "result": spec["result"],
                  "entries": CLI_ENTRIES, "process_cwd": "/"},
        "output": {"exit": r.exit_code, "stdout": r.stdout, "stderr": r.stderr,
                   "reached": reached or None},
    }


def main():
    for var in STRIP_VARS:
        os.environ.pop(var, None)
    from fno.provenance import spawn_think as st

    originals = {
        name: getattr(st, name)
        for name in ("resolve_transcript", "_think_output_path", "_plans_output_dir")
    }
    cases = []
    cases += seed_cases(st)
    cases += output_cases(st, originals)
    cases += name_cases(st)
    cases += short_id_cases(st)
    tail = tail_cases(st)
    cases += dedup_cases(tail)
    cases += tail
    cases += [cli_case(spec) for spec in cli_specs()]
    lines = ",\n".join(json.dumps(c, ensure_ascii=False) for c in cases)
    GOLDEN.parent.mkdir(parents=True, exist_ok=True)
    GOLDEN.write_text(
        '{"oracle": "fno.provenance.spawn_think + fno.provenance.cli (Python leg)", "cases": [\n'
        + lines
        + "\n]}\n",
        encoding="utf-8",
    )
    print(f"wrote {len(cases)} cases to {GOLDEN}")


if __name__ == "__main__":
    main()
