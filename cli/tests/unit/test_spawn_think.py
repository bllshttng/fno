"""BDD decision-matrix tests for born-with-why /think spawn (x-6a10).

Covers every user story's acceptance criteria for
:func:`fno.provenance.spawn_think.maybe_spawn_think` plus the load-bearing
invariants:

- US1: a node born with its resolved why (transcript pointer, not paraphrase).
- US2: an attended operator gets a one-line handoff, not an autonomous spawn.
- US3: an away operator gets a fire-and-forget bg /think + a forward stamp.
- US4: opt-in, bounded, non-fatal.

Claim isolation: every spawn test routes claims under a tmp FNO_CLAIMS_ROOT +
FNO_REPO_ROOT so the dispatch dedup token never touches the real .fno/claims.
The ``fno agents spawn`` subprocess is always patched at the
``_spawn_think_worker`` seam - no test shells out.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from fno.harness_identity import resolve_harness_identity
from fno.provenance import spawn_think as st
from fno.provenance.resolver import ResolvedTranscript


# ---------------------------------------------------------------------------
# Fixtures / helpers
# ---------------------------------------------------------------------------


@pytest.fixture
def iso(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    """Isolate claims + arm the gate + pin presence; return the events path."""
    monkeypatch.setenv("FNO_CLAIMS_ROOT", str(tmp_path))
    monkeypatch.setenv("FNO_REPO_ROOT", str(tmp_path))
    monkeypatch.setenv("FNO_THINK_SPAWN", "1")  # armed by default for tests
    return tmp_path / ".fno" / "events.jsonl"


def _events(events_path: Path) -> list[dict]:
    from tests._event_rows import event_rows

    return [e for e in event_rows(events_path) if not e["type"].startswith("claim_")]


def _node(**over) -> dict:
    """A generated organic node WITH an origin (eligible by default)."""
    base = {
        "id": "x-2222aaaa",
        "slug": "born-with-why",
        "title": "Idea nodes born with their why",
        "details": "the why lives in the transcript",
        "cwd": "/tmp/proj",
        "source_harness": "claude",
        "source_session_id": "abc12345",
        "source_cwd": "/tmp/sess",
        "source_node_id": "x-0000aaaa",
        "roadmap_id": None,
        "vision_path": None,
    }
    base.update(over)
    return base


def test_assemble_seed_chain_blueprint_appends_chain(monkeypatch):
    # x-edf7 US3: a fan-out seed must instruct the worker to continue into
    # /blueprint + link, else the flagged child stays designless/idea.
    _resolved(monkeypatch, ok=True)
    seed = st.assemble_seed(_node(), chain_blueprint=True)
    assert "/blueprint" in seed.prompt
    assert "FAILED pass" in seed.prompt
    assert "--plan-path" in seed.prompt


@pytest.fixture
def patch_spawn(monkeypatch: pytest.MonkeyPatch):
    """Patch the spawn seam + forward stamp; return (spawn_calls, stamp_calls)."""
    spawn_calls: list[tuple] = []
    stamp_calls: list[tuple] = []

    def fake_spawn(
        node_id,
        prompt,
        node_cwd,
        node_slug,
        reason="birth",
        invocation_suffix=None,
        model=None,
        provider=None,
        node=None,
    ):
        spawn_calls.append(
            (node_id, prompt, node_cwd, node_slug, reason, invocation_suffix, model, provider, node)
        )
        return "deadbeef"

    monkeypatch.setattr(st, "_spawn_think_worker", fake_spawn)
    monkeypatch.setattr(
        st,
        "_stamp_forward",
        lambda nid, sess, root, output_path=None: stamp_calls.append(
            (nid, sess, root, output_path)
        ),
    )
    return spawn_calls, stamp_calls


def _resolved(monkeypatch, *, ok: bool, path: str = "/x/t.jsonl", reason="not-found"):
    """Pin assemble_seed's transcript resolution result."""

    def fake(harness, sid, cwd, **kw):
        if ok:
            return ResolvedTranscript(harness, sid, cwd, True, transcript_path=path)
        return ResolvedTranscript(harness, sid, cwd, False, reason=reason)

    monkeypatch.setattr(st, "resolve_transcript", fake)


# ---------------------------------------------------------------------------
# US4 - opt-in & bounded
# ---------------------------------------------------------------------------


def test_gate_off_is_complete_noop(iso, monkeypatch, patch_spawn):
    """AC4-HP: gate off -> no event, no spawn, decision=noop."""
    monkeypatch.setenv("FNO_THINK_SPAWN", "0")
    spawn_calls, _ = patch_spawn
    res = st.maybe_spawn_think(
        _node(), env=dict(__import__("os").environ), events_path=iso, project_root=iso.parent.parent
    )
    assert res.decision == "noop" and res.event is None
    assert spawn_calls == []
    assert _events(iso) == []


def test_malformed_config_fails_safe_disabled(monkeypatch, tmp_path):
    """AC4-ERR: a settings read that raises degrades to disabled, never raises."""
    monkeypatch.delenv("FNO_THINK_SPAWN", raising=False)

    def boom():
        raise RuntimeError("settings exploded")

    monkeypatch.setattr("fno.config.load_settings", boom)
    assert st.think_spawn_enabled(project_root=tmp_path, env={}) is False


def test_blast_radius_cap(iso, monkeypatch, patch_spawn, capsys):
    """AC4-EDGE: a run over max_per_run skips the rest and logs the truncation.

    Uses REASON_WORK_START: an unforced birth never advances RunState.spawned
    at all (x-42c5 - it never reaches the real spawn seam), so the cap counter
    needs a reason that still spawns to exercise the cap.
    """
    monkeypatch.setenv("FNO_THINK_SPAWN_PRESENCE", "away")
    monkeypatch.setattr(st, "_max_per_run", lambda root: 1)
    _resolved(monkeypatch, ok=True)
    spawn_calls, _ = patch_spawn
    rs = st.RunState()
    env = dict(__import__("os").environ)

    r1 = st.maybe_spawn_think(
        _node(id="x-a"),
        reason=st.REASON_WORK_START,
        env=env,
        events_path=iso,
        project_root=iso.parent.parent,
        run_state=rs,
    )
    r2 = st.maybe_spawn_think(
        _node(id="x-b"),
        reason=st.REASON_WORK_START,
        env=env,
        events_path=iso,
        project_root=iso.parent.parent,
        run_state=rs,
    )

    assert r1.decision == "spawned"
    assert r2.decision == "skipped" and r2.reason == "cap-exceeded"
    assert len(spawn_calls) == 1
    assert "blast-radius cap" in capsys.readouterr().err


def test_dedup_at_most_one_spawn(iso, monkeypatch, patch_spawn):
    """AC4-FR: two evaluations of the same LIFECYCLE (non-birth) dispatch spawn
    at most once. Birth's own repeat-evaluation behavior is covered by
    test_unforced_birth_never_spawns_even_repeated below - birth never reaches
    the dedup claim at all (x-42c5: it resolves to offered before that step)."""
    monkeypatch.setenv("FNO_THINK_SPAWN_PRESENCE", "away")
    _resolved(monkeypatch, ok=True)
    spawn_calls, _ = patch_spawn
    env = dict(__import__("os").environ)
    node = _node()

    r1 = st.maybe_spawn_think(
        node, reason=st.REASON_WORK_START, env=env, events_path=iso, project_root=iso.parent.parent
    )
    # second observation: the dispatch:think:<id> TTL token is still live.
    r2 = st.maybe_spawn_think(
        node, reason=st.REASON_WORK_START, env=env, events_path=iso, project_root=iso.parent.parent
    )

    assert r1.decision == "spawned"
    assert r2.decision == "skipped" and r2.reason == "already-claimed"
    assert len(spawn_calls) == 1


def test_unforced_birth_refuses_at_the_guard(iso, monkeypatch, patch_spawn):
    """The unforced birth ladder is native (`fno-agents backlog birth-hook`);
    a direct reason=birth call without chain_blueprint can only skip - it must
    never fall through to the auto-spawn tail."""
    monkeypatch.setenv("FNO_THINK_SPAWN_PRESENCE", "away")
    _resolved(monkeypatch, ok=True)
    spawn_calls, stamp_calls = patch_spawn
    res = st.maybe_spawn_think(
        _node(), env=dict(__import__("os").environ), events_path=iso, project_root=iso.parent.parent
    )
    assert res.decision == "skipped" and res.reason == "unforced-birth-native"
    assert spawn_calls == [] and stamp_calls == []
    evs = _events(iso)
    assert len(evs) == 1 and evs[0]["type"] == "think_skipped"
    assert evs[0]["data"]["reason"] == "unforced-birth-native"


# ---------------------------------------------------------------------------
# US1 - node born with resolved why
# ---------------------------------------------------------------------------


def test_resolved_seed_carries_transcript_pointer(iso, monkeypatch, patch_spawn):
    """AC1-HP: resolved claude origin -> seed has the transcript path + node id.

    Uses REASON_WORK_START (a consented lifecycle trigger that still spawns)
    rather than the default birth reason: birth never spawns (x-42c5), so this
    away-spawn assertion needs a reason that still reaches the spawn seam.
    """
    monkeypatch.setenv("FNO_THINK_SPAWN_PRESENCE", "away")
    _resolved(monkeypatch, ok=True, path="/real/transcript.jsonl")
    spawn_calls, _ = patch_spawn
    res = st.maybe_spawn_think(
        _node(),
        reason=st.REASON_WORK_START,
        env=dict(__import__("os").environ),
        events_path=iso,
        project_root=iso.parent.parent,
    )
    assert res.decision == "spawned" and res.resolved is True
    prompt = spawn_calls[0][1]
    assert "/real/transcript.jsonl" in prompt  # the POINTER, not a paraphrase
    assert "x-2222aaaa" in prompt
    assert "origin node chain: x-0000aaaa" in prompt


def test_no_origin_skipped(iso, monkeypatch, patch_spawn):
    """AC1-ERR: all provenance pointers null -> skip{no-origin}, no spawn."""
    monkeypatch.setenv("FNO_THINK_SPAWN_PRESENCE", "away")
    spawn_calls, _ = patch_spawn
    node = _node(source_session_id=None, source_harness=None, source_cwd=None, source_node_id=None)
    res = st.maybe_spawn_think(
        node,
        reason=st.REASON_WORK_START,
        env=dict(__import__("os").environ),
        events_path=iso,
        project_root=iso.parent.parent,
    )
    assert res.decision == "skipped" and res.reason == "no-origin"
    assert spawn_calls == []
    evs = _events(iso)
    assert len(evs) == 1 and evs[0]["data"]["reason"] == "no-origin"


def test_bug_node_skipped_not_design_warranting(iso, monkeypatch, patch_spawn):
    # A `bug` node is a reproduce-and-fix, not a design fan-out: it must skip an
    # automatic /think even when otherwise eligible (the x-0e29 mechanism fix).
    monkeypatch.setenv("FNO_THINK_SPAWN_PRESENCE", "away")
    spawn_calls, _ = patch_spawn
    res = st.maybe_spawn_think(
        _node(type="bug"),
        reason=st.REASON_WORK_START,
        env=dict(__import__("os").environ),
        events_path=iso,
        project_root=iso.parent.parent,
    )
    assert res.decision == "skipped" and res.reason == "not-design-warranting"
    assert spawn_calls == []


def test_forced_decompose_child_bypasses_design_warranting_gate(iso, monkeypatch, patch_spawn):
    # A `needs_think` decompose child rides chain_blueprint=True as operator
    # consent (decompose is the opt-in). Even a bug/size-S child must get its
    # forced design pass, not be left an unlinked idea by the size gate (Codex P1).
    monkeypatch.setenv("FNO_THINK_SPAWN_PRESENCE", "away")
    monkeypatch.setenv("FNO_THINK_SPAWN_ATTENDED", "spawn")
    spawn_calls, _ = patch_spawn
    res = st.maybe_spawn_think(
        _node(type="bug", size="S"),
        chain_blueprint=True,
        env=dict(__import__("os").environ),
        events_path=iso,
        project_root=iso.parent.parent,
    )
    assert res.decision == "spawned"
    assert len(spawn_calls) == 1


# ---------------------------------------------------------------------------
# US2 - operator present
# ---------------------------------------------------------------------------


def test_attended_offers_line_not_spawn(iso, monkeypatch, patch_spawn, capsys):
    """AC2-HP/UI: attended -> single-line /think offer, no autonomous spawn."""
    monkeypatch.setenv("FNO_THINK_SPAWN_PRESENCE", "attended")
    _resolved(monkeypatch, ok=True, path="/t.jsonl")
    spawn_calls, _ = patch_spawn
    res = st.maybe_spawn_think(
        _node(),
        reason=st.REASON_CONVERSATIONAL,
        env=dict(__import__("os").environ),
        events_path=iso,
        project_root=iso.parent.parent,
    )
    assert res.decision == "offered" and spawn_calls == []
    assert res.offer_line.startswith("/think x-2222aaaa")
    assert "\n" not in res.offer_line  # single copy-pasteable line (AC2-UI)
    # x-af8d AC1-HP: the offer stderr line is imperative ("nothing spawned"),
    # not the old `handoff ->` status log that the agent misread as a spawn.
    err = capsys.readouterr().err
    assert "OFFER PENDING (nothing spawned)" in err
    assert "/think x-2222aaaa" in err
    assert "handoff ->" not in err
    evs = _events(iso)
    assert len(evs) == 1 and evs[0]["type"] == "think_offered"


def test_attended_spawn_optin_dispatches(iso, monkeypatch, patch_spawn):
    """AC4-HP (B, x-5d51): attended + config.think_spawn.attended=spawn -> real bg
    /think dispatch instead of the stderr offer line - on a consented (non-birth)
    trigger. Unlike the pre-x-42c5 behavior, birth itself is exempt from this
    opt-in (see test_unforced_birth_ignores_attended_spawn_optin below): the
    operator's ruling is that a birth trigger never auto-spawns, full stop, even
    when this config is armed."""
    monkeypatch.setenv("FNO_THINK_SPAWN_PRESENCE", "attended")
    monkeypatch.setenv("FNO_THINK_SPAWN_ATTENDED", "spawn")
    _resolved(monkeypatch, ok=True)
    spawn_calls, stamp_calls = patch_spawn
    res = st.maybe_spawn_think(
        _node(),
        reason=st.REASON_WORK_START,
        env=dict(__import__("os").environ),
        events_path=iso,
        project_root=iso.parent.parent,
    )
    assert res.decision == "spawned" and len(spawn_calls) == 1
    assert len(stamp_calls) == 1
    evs = _events(iso)
    assert len(evs) == 1 and evs[0]["type"] == "think_spawned"
    assert evs[0]["data"]["presence"] == "attended"  # honest about the opt-in source


# ---------------------------------------------------------------------------
# US3 - operator away
# ---------------------------------------------------------------------------


def test_away_work_start_still_spawns_and_stamps(iso, monkeypatch, patch_spawn):
    """AC3-HP/UI: a consented (non-birth) away trigger still fires the real bg
    /think spawn + node stamp + single think_spawned, exactly as birth used to
    before x-42c5 (birth itself now always offers - see the test above)."""
    monkeypatch.setenv("FNO_THINK_SPAWN_PRESENCE", "away")
    _resolved(monkeypatch, ok=True)
    spawn_calls, stamp_calls = patch_spawn
    res = st.maybe_spawn_think(
        _node(),
        reason=st.REASON_WORK_START,
        env=dict(__import__("os").environ),
        events_path=iso,
        project_root=iso.parent.parent,
    )
    assert res.decision == "spawned" and res.think_session == "deadbeef"
    assert len(spawn_calls) == 1
    assert len(stamp_calls) == 1
    nid, sess, root, output_path = stamp_calls[0]
    assert (nid, sess, root) == ("x-2222aaaa", "deadbeef", iso.parent.parent)
    # x-ff83 W1: doc lands in plans-dir; x-8af8: filename ends -<node-id>.md.
    assert output_path and output_path.endswith("-born-with-why-x-2222aaaa.md")
    assert "plans" in output_path
    evs = _events(iso)
    assert len(evs) == 1 and evs[0]["type"] == "think_spawned"
    assert evs[0]["data"]["think_session"] == "deadbeef"


def test_away_spawn_failure_releases_reservation(iso, monkeypatch, patch_spawn):
    """AC2-FR analogue: a failed spawn frees dispatch:think:<id> for a retry."""
    monkeypatch.setenv("FNO_THINK_SPAWN_PRESENCE", "away")
    _resolved(monkeypatch, ok=True)
    calls = {"n": 0}

    def flaky(*a, **k):
        calls["n"] += 1
        if calls["n"] == 1:
            raise st.SpawnError("transient")
        return "second-try"

    monkeypatch.setattr(st, "_spawn_think_worker", flaky)
    monkeypatch.setattr(st, "_stamp_forward", lambda *a, **k: None)
    env = dict(__import__("os").environ)
    node = _node()

    r1 = st.maybe_spawn_think(
        node, reason=st.REASON_WORK_START, env=env, events_path=iso, project_root=iso.parent.parent
    )
    r2 = st.maybe_spawn_think(
        node, reason=st.REASON_WORK_START, env=env, events_path=iso, project_root=iso.parent.parent
    )
    assert r1.reason == "spawn-failed"
    assert r2.decision == "spawned"  # reservation was released, retry succeeds


# ---------------------------------------------------------------------------
# Presence classifier (Locked Decision 3)
# ---------------------------------------------------------------------------


def test_presence_spawned_worker_is_away(tmp_path):
    """codex PR #9: a spawned bg worker exposes CLAUDE_CODE_SESSION_ID + the
    FNO_AGENT_SELF marker but NOT FNO_BG and may have no manifest yet; it must
    classify away (else US3's autonomous /think never fires)."""
    assert (
        st.classify_presence(
            env={"CLAUDE_CODE_SESSION_ID": "sid", "FNO_AGENT_SELF": "think-x-1-foo"},
            project_root=tmp_path,
        )
        == "away"
    )


def test_presence_foreign_manifest_ignored(tmp_path):
    """A manifest whose transcript-id != this session is not trusted."""
    fno = tmp_path / ".fno"
    fno.mkdir()
    (fno / "target-state.md").write_text(
        "claude_transcript_id: OTHER\nattended: false\n", encoding="utf-8"
    )
    # Falls through to the interactive-session signal -> attended.
    assert (
        st.classify_presence(env={"CLAUDE_CODE_SESSION_ID": "sid-123"}, project_root=tmp_path)
        == "attended"
    )


# ---------------------------------------------------------------------------
# _spawn_think_worker - dispatch pin passthrough (model/provider on the cmd)
# ---------------------------------------------------------------------------


# The mint is a real pre-spawn subprocess (x-84b2); fakes serve the spawn and
# route the naming verbs to the real binary.
_REAL_SUBPROCESS_RUN = st.subprocess.run

_NAMING_VERBS = {"name-mint", "name-codes", "name-parse"}


def _is_naming_verb(cmd) -> bool:
    return bool(_NAMING_VERBS & {str(part) for part in cmd})


def _capture_spawn_cmd(monkeypatch) -> list:
    """Patch subprocess.run inside _spawn_think_worker; return the captured cmd."""
    captured: dict = {}

    class _Proc:
        returncode = 0
        stdout = '{"short_id":"abc123"}'
        stderr = ""

    def fake_run(cmd, **kw):
        if _is_naming_verb(cmd):
            return _REAL_SUBPROCESS_RUN(cmd, **kw)
        captured["cmd"] = cmd
        return _Proc()

    monkeypatch.setattr(st.subprocess, "run", fake_run)
    return captured


def test_spawn_worker_tags_the_spawn_subprocess_with_its_cause(monkeypatch):
    """x-42c5: the reason this spawn happened rides FNO_SPAWN_TRIGGER in the
    `fno agents spawn` subprocess's own environment, so the registry row it
    writes can record a CAUSE, not just a parent session id. Before this,
    the only evidence for "did a birth trigger spawn this?" was a timestamp
    gap between a node's created_at and a worker's registry row."""
    captured: dict = {}

    class _Proc:
        returncode = 0
        stdout = '{"short_id":"abc123"}'
        stderr = ""

    def fake_run(cmd, **kw):
        if _is_naming_verb(cmd):
            return _REAL_SUBPROCESS_RUN(cmd, **kw)
        captured["cmd"] = cmd
        captured["env"] = kw.get("env")
        return _Proc()

    monkeypatch.setattr(st.subprocess, "run", fake_run)
    st._spawn_think_worker("x-1", "prompt", None, "slug", reason=st.REASON_WORK_START)
    assert captured["env"]["FNO_SPAWN_TRIGGER"] == "think_spawn:work-start"
    # The parent's other env vars still ride through (a fresh env would strand
    # PATH/HOME and break the subprocess).
    assert captured["env"].get("PATH")


def test_codex_ambient_pointer_keeps_default_worker_provider_claude(iso, monkeypatch):
    """Codex may own the source conversation without becoming the worker provider."""
    identity = resolve_harness_identity({"CODEX_THREAD_ID": "codex-thread-123"})
    assert identity.harness == "codex"

    seen: dict = {}

    def fake_resolve(harness, sid, cwd, **kw):
        seen["pointer"] = (harness, sid, cwd)
        return ResolvedTranscript(
            harness, sid, cwd, True, transcript_path="/live/codex-thread.jsonl"
        )

    class _Proc:
        returncode = 0
        stdout = '{"short_id":"abc123"}'
        stderr = ""

    def fake_run(cmd, **kw):
        if _is_naming_verb(cmd):
            return _REAL_SUBPROCESS_RUN(cmd, **kw)
        if {"doctor", "event"} <= {str(part) for part in cmd}:
            # Event emission rides the same seam; it is not the spawn argv.
            return _REAL_SUBPROCESS_RUN(cmd, **kw)
        seen["cmd"] = cmd
        return _Proc()

    monkeypatch.setattr(st, "resolve_transcript", fake_resolve)
    monkeypatch.setattr(st.subprocess, "run", fake_run)
    monkeypatch.setattr(st, "_stamp_forward", lambda *a, **kw: None)
    monkeypatch.setattr(st, "_daily_cap", lambda root: 0)

    res = st.dispatch_conversational(
        _node(),
        session_id=identity.session_id,
        cwd="/tmp/codex-live",
        harness=identity.harness or "claude",
        events_path=iso,
        project_root=iso.parent.parent,
    )

    assert res.decision == "spawned"
    assert seen["pointer"] == ("codex", "codex-thread-123", "/tmp/codex-live")
    cmd = seen["cmd"]
    assert cmd[cmd.index("--harness") + 1] == "claude"
    assert cmd[cmd.index("--substrate") + 1] == "thread"


def test_spawn_worker_threads_model_and_provider(monkeypatch):
    """A dispatch pin reaches the spawn cmd as exact --model / --provider tokens."""
    cap = _capture_spawn_cmd(monkeypatch)
    st._spawn_think_worker("x-1", "prompt", None, "slug", model="glm-4.7", provider="codex")
    cmd = cap["cmd"]
    assert cmd[cmd.index("--harness") + 1] == "codex"
    assert cmd[cmd.index("--model") + 1] == "glm-4.7"


# ---------------------------------------------------------------------------
# _think_output_path - plans-dir birth location (x-ff83 W1)
# ---------------------------------------------------------------------------


def test_think_output_path_honors_plansdirectory(monkeypatch, tmp_path):
    """AC-HP (x-8af8): plansDirectory wins; filename ends -<node-id>.md."""
    monkeypatch.setenv("FNO_REPO_ROOT", str(tmp_path))
    local = tmp_path / ".claude"
    local.mkdir()
    (local / "settings.local.json").write_text(json.dumps({"plansDirectory": "internal/fno/plans"}))
    import re

    out = st._think_output_path("x-2222aaaa", "my-slug")
    assert str(tmp_path / "internal" / "fno" / "plans") in out
    # node-id-suffixed YYYY-MM-DD-<slug>-<node-id>.md
    assert re.fullmatch(r"\d{4}-\d{2}-\d{2}-my-slug-x-2222aaaa\.md", Path(out).name)


def test_think_output_path_reuses_existing_node_doc_on_redispatch(monkeypatch, tmp_path):
    """AC-EDGE: a re-dispatch reuses this node's existing doc keyed on the node id,
    not a fresh date file - and is stable across a slug edit between dispatches."""
    monkeypatch.setenv("FNO_REPO_ROOT", str(tmp_path))
    plans = tmp_path / "plans"  # what the (patched) resolver answers
    monkeypatch.setattr("fno.paths.plans_content_dir", lambda project_root=None: plans)
    plans.mkdir(parents=True)
    prior = plans / "2020-01-01-old-slug-x-2222aaaa.md"
    prior.write_text("# earlier dispatch\n")
    # The slug changed to my-slug since the first dispatch; the node id is stable,
    # so the prior doc is reused rather than minting a second file (idempotent).
    out = st._think_output_path("x-2222aaaa", "my-slug")
    assert out == str(prior)


def test_think_output_path_unresolvable_falls_back_to_briefs(monkeypatch, tmp_path, capsys):
    """AC1-ERR: an unresolvable plans dir degrades to briefs/ with a visible warning."""

    def boom():
        raise RuntimeError("no repo root")

    monkeypatch.setattr(st, "_plans_output_dir", boom)
    out = st._think_output_path("x-2222aaaa", "my-slug")
    assert out.endswith("think-x-2222aaaa.md")
    assert "briefs" in out
    assert "plans dir unresolvable" in capsys.readouterr().err


# ---------------------------------------------------------------------------
# on_node_born() - the shared birth seam (v2 A1)
# ---------------------------------------------------------------------------


def test_on_node_born_execs_the_native_door(iso, tmp_path, monkeypatch):
    """The birth hook rides the `fno-agents backlog birth-hook` door; the
    graph path and the born node's id are the whole argv; the door's answer
    maps back as an informational ThinkSpawnResult."""
    monkeypatch.setenv("FNO_THINK_SPAWN", "1")
    seen = {}

    def fake_door(verb, args, **k):
        seen["argv"] = (verb, args)
        return None, {
            "kind": "offered",
            "event": st.EVENT_OFFERED,
            "node_id": "x-2222aaaa",
            "presence": "away",
            "resolved": False,
            "offer_line": "/think x-2222aaaa",
            "reason": None,
        }

    monkeypatch.setattr(st, "call_binary_json", fake_door)
    g = tmp_path / "graph.json"
    res = st.on_node_born({"id": "x-2222aaaa"}, graph_path=g)
    assert seen["argv"] == (
        "backlog",
        [
            "birth-hook",
            "--graph",
            str(g),
            "--node-id",
            "x-2222aaaa",
            "--events-path",
            str(st._events_path(None)),
        ],
    )
    assert res is not None and res.decision == "offered" and res.event == st.EVENT_OFFERED


def test_on_node_born_is_strictly_non_fatal(iso, monkeypatch):
    """A failing, missing, or malformed door answer resolves to None."""
    monkeypatch.setenv("FNO_THINK_SPAWN", "1")
    for shape in [("binary not found", None), (None, ["not-a-dict"])]:
        monkeypatch.setattr(st, "call_binary_json", lambda *a, s=shape: s)
        assert st.on_node_born(_node()) is None

    def kaboom(*a, **k):
        raise RuntimeError("kaboom")

    monkeypatch.setattr(st, "call_binary_json", kaboom)
    assert st.on_node_born(_node()) is None


# ---------------------------------------------------------------------------
# A2 lifecycle triggers (x-122a): work-start
# ---------------------------------------------------------------------------


def test_config_subflags_default_off_and_coerce():
    """A2 sub-flags + daily_cap default OFF/sane and fail-safe on garbage."""
    from fno.config import ThinkSpawnBlock

    b = ThinkSpawnBlock()
    assert b.on_work_start is False and b.daily_cap == 20
    # Affirmative strings enable; garbage fails safe to off; bad cap -> 20; 0 honored.
    on = ThinkSpawnBlock(on_work_start="yes", daily_cap="nonsense")
    assert on.on_work_start is True and on.daily_cap == 20
    assert ThinkSpawnBlock(on_work_start="maybe").on_work_start is False
    assert ThinkSpawnBlock(daily_cap=0).daily_cap == 0


def test_lifecycle_relevance_filter_skips_unresolved(iso, monkeypatch, patch_spawn):
    """A2 relevance filter: a lifecycle trigger needs a RESOLVED pointer, so a
    high-volume work-start moment never dispatches a context-free /think."""
    monkeypatch.setenv("FNO_THINK_SPAWN_PRESENCE", "away")
    _resolved(monkeypatch, ok=False, reason="harness-not-supported")
    spawn_calls, _ = patch_spawn
    env = dict(__import__("os").environ)
    # work-start (lifecycle) with an unresolved pointer -> skip, no spawn.
    r_life = st.maybe_spawn_think(
        _node(),
        reason=st.REASON_WORK_START,
        env=env,
        events_path=iso,
        project_root=iso.parent.parent,
    )
    assert r_life.decision == "skipped" and r_life.reason == "unresolved-pointer"
    assert len(spawn_calls) == 0


def test_daily_cap_skips_when_reached(iso, monkeypatch, patch_spawn):
    """A2 firehose guard: at the per-day ceiling an away spawn is skipped."""
    monkeypatch.setenv("FNO_THINK_SPAWN_PRESENCE", "away")
    _resolved(monkeypatch, ok=True)
    spawn_calls, _ = patch_spawn
    monkeypatch.setattr(st, "_daily_cap", lambda root: 3)
    monkeypatch.setattr(st, "_daily_count", lambda: 3)
    res = st.maybe_spawn_think(
        _node(),
        reason=st.REASON_WORK_START,
        env=dict(__import__("os").environ),
        events_path=iso,
        project_root=iso.parent.parent,
    )
    assert res.decision == "skipped" and res.reason == "daily-cap"
    assert len(spawn_calls) == 0


def test_on_node_work_start_subflag_gate(iso, monkeypatch, patch_spawn):
    """The work-start wrapper fires only when on_work_start is armed (even with layer on)."""
    monkeypatch.setenv("FNO_THINK_SPAWN_PRESENCE", "away")
    _resolved(monkeypatch, ok=True)
    spawn_calls, _ = patch_spawn
    # sub-flag OFF -> no dispatch even though FNO_THINK_SPAWN=1 (layer on).
    monkeypatch.setattr(st, "_subflag_on", lambda name, root: False)
    assert st.on_node_work_start(_node(), project_root=iso.parent.parent) is None
    assert len(spawn_calls) == 0
    # sub-flag ON -> dispatch.
    monkeypatch.setattr(st, "_subflag_on", lambda name, root: name == "on_work_start")
    res = st.on_node_work_start(_node(), project_root=iso.parent.parent)
    assert res is not None and res.decision == "spawned"
    assert len(spawn_calls) == 1


def test_worker_agent_name_reason_scoped():
    """codex P2: birth name is byte-for-byte; lifecycle names are distinct per reason.

    The spawned `fno agents spawn` name must be reason-scoped or the second
    lifecycle trigger for a node collides on name and is wrongly skipped.
    """
    # x-84b2: the spawn_think source stamped only by this path, the think
    # verb as a code; the reason stays the reason-scoped qualifier. x-57fe:
    # the node carries its bare hex.
    assert st._worker_agent_name("x-1", "slug") == "th-th-1-slug"  # default birth
    assert st._worker_agent_name("x-1", "slug", st.REASON_BIRTH) == "th-th-1-slug"
    assert st._worker_agent_name("x-1", "slug", st.REASON_WORK_START) == "th-th-1-work-start-slug"
    names = {
        st._worker_agent_name("x-1", "slug", r) for r in (st.REASON_BIRTH, st.REASON_WORK_START)
    }
    assert len(names) == 2  # no collision across a node's lifecycle


# ---------------------------------------------------------------------------
# US5 (C, x-0a9c) - explicit conversational dispatch verb
# ---------------------------------------------------------------------------


def _stored_origin_node(**over) -> dict:
    """A node whose STORED birth origin differs from the live session, so a test
    can prove the LIVE pointer (not the birth origin) is what gets carried."""
    return _node(
        source_harness="codex",
        source_session_id="STORED-birth-sid",
        source_cwd="/tmp/birth-origin",
        **over,
    )


def test_dispatch_conversational_carries_live_pointer(iso, monkeypatch, patch_spawn):
    """AC5-HP: the spawned think resolves the LIVE session pointer, NOT the node's
    stored birth origin; reason=conversational; one think_spawned event."""
    seen: dict = {}

    def fake_resolve(harness, sid, cwd, **kw):
        seen["triple"] = (harness, sid, cwd)
        return ResolvedTranscript(harness, sid, cwd, True, transcript_path="/live/t.jsonl")

    monkeypatch.setattr(st, "resolve_transcript", fake_resolve)
    spawn_calls, _ = patch_spawn

    res = st.dispatch_conversational(
        _stored_origin_node(),
        session_id="LIVE-sid",
        cwd="/tmp/live-sess",
        harness="claude",
        events_path=iso,
        project_root=iso.parent.parent,
    )
    assert res.decision == "spawned" and res.think_session == "deadbeef"
    # resolve_transcript saw the LIVE triple, never the stored (codex/STORED.../...).
    assert seen["triple"] == ("claude", "LIVE-sid", "/tmp/live-sess")
    # The prompt carries the live transcript POINTER (never a paraphrase).
    assert "/live/t.jsonl" in spawn_calls[0][1]
    # Spawn used the conversational reason => reason-scoped worker name + dedup token.
    assert spawn_calls[0][4] == st.REASON_CONVERSATIONAL
    evs = _events(iso)
    assert evs[-1]["type"] == st.EVENT_SPAWNED
    assert evs[-1]["data"]["trigger"] == "conversational"


def test_dispatch_forces_spawn_when_gate_off_and_attended(monkeypatch, tmp_path, patch_spawn):
    """AC5-HP: the explicit verb spawns even when the config gate is OFF and the
    session is attended - the invocation IS the opt-in, and it is a real spawn
    (not the default stderr offer line)."""
    monkeypatch.setenv("FNO_CLAIMS_ROOT", str(tmp_path))
    monkeypatch.setenv("FNO_REPO_ROOT", str(tmp_path))
    monkeypatch.delenv("FNO_THINK_SPAWN", raising=False)  # config gate OFF (ambient)
    monkeypatch.setenv("FNO_THINK_SPAWN_PRESENCE", "attended")  # operator at the keyboard
    _resolved(monkeypatch, ok=True)
    spawn_calls, _ = patch_spawn
    events_path = tmp_path / ".fno" / "events.jsonl"

    res = st.dispatch_conversational(
        _node(),
        session_id="LIVE-sid",
        cwd="/tmp/live",
        events_path=events_path,
        project_root=tmp_path,
    )
    # Not noop (gate forced on) and not offered (attended forced to spawn).
    assert res.decision == "spawned"
    assert res.presence == "attended"  # presence is recorded truthfully
    assert len(spawn_calls) == 1


# ---------------------------------------------------------------------------
# x-3218: provenance naming is now a caller of the canonical owner
# ---------------------------------------------------------------------------


def test_over_budget_provenance_identity_fails_closed_instead_of_shaving():
    """AC4 + AC5: the old backstop sliced the tail (shaving the discriminator).

    Two dispatches differing only by their session suffix would collapse onto
    one name and the second would be wrongly skipped as a duplicate. The
    canonical owner refuses instead.
    """
    from fno.agents.naming import AgentNameError

    long_id = "n-" + "z" * 50
    with pytest.raises(AgentNameError) as exc:
        st._worker_agent_name(long_id, "slug", st.REASON_WORK_START, "sessaaaa")
    assert long_id in str(exc.value)


# ---------------------------------------------------------------------------
# shared dispatch substrate + on_decompose_wave0
# ---------------------------------------------------------------------------


def _write_config(root: Path, body: str) -> None:
    (root / ".fno").mkdir(parents=True, exist_ok=True)
    (root / ".fno" / "config.toml").write_text(body)


def test_AC10_HP_configured_dispatch_substrate_reaches_every_spawn(monkeypatch, tmp_path):
    """The shared dispatch selector applies to every context-think spawn."""
    _write_config(tmp_path, '[dispatch]\nsubstrate = "headless"\n')
    cap = _capture_spawn_cmd(monkeypatch)
    st._spawn_think_worker("x-1", "prompt", str(tmp_path), "slug")
    assert cap["cmd"][cap["cmd"].index("--substrate") + 1] == "headless"


def test_wave0_needs_its_own_flag_not_just_enabled(tmp_path, monkeypatch):
    """Same shape as on_work_start: `enabled` alone never arms it.

    Read off the settings block rather than through `think_spawn_enabled`:
    conftest pins FNO_THINK_SPAWN=0 process-wide so no test can spawn a real
    worker, and that override outranks config by design.
    """
    monkeypatch.delenv("FNO_THINK_SPAWN_WAVE0", raising=False)
    _write_config(tmp_path, "[think_spawn]\nenabled = true\n")
    from fno.config import load_settings_for_repo

    block = load_settings_for_repo(tmp_path).think_spawn
    assert block.enabled is True
    assert block.on_decompose_wave0 is False
    assert st.think_spawn_on_decompose_wave0(project_root=tmp_path) is False


# non-bg spawn receipts (codex P1) --------------------------------------------


def _capture_with_stdout(monkeypatch, stdout: str) -> dict:
    """Patch subprocess.run with a scripted receipt; return the captured cmd."""
    captured: dict = {}

    class _Proc:
        returncode = 0
        stderr = ""

    def fake_run(cmd, **kw):
        if _is_naming_verb(cmd):
            return _REAL_SUBPROCESS_RUN(cmd, **kw)
        captured["cmd"] = cmd
        p = _Proc()
        p.stdout = stdout
        return p

    monkeypatch.setattr(st.subprocess, "run", fake_run)
    return captured


def test_a_headless_reply_launches_but_stamps_no_session_pointer(monkeypatch, tmp_path):
    """The headless `once` path writes the provider reply verbatim, not JSON.

    It must NOT raise (the work happened), and must NOT invent a session id: the
    one-shot has already torn down, so a handle here would be stamped into
    `think_session_id` and resolve to nothing on every successful spawn. Empty
    is the honest answer; the `think_spawned` event still carries `agent_name`.
    """
    _write_config(tmp_path, '[dispatch]\nsubstrate = "headless"\n')
    _capture_with_stdout(monkeypatch, "I have finished the design pass.\n")
    handle = st._spawn_think_worker("x-1", "prompt", str(tmp_path), "slug")
    assert handle == ""


def test_the_non_bg_handle_is_the_agent_name_not_a_fabricated_id(monkeypatch, tmp_path):
    """It must address a real worker: `fno agents logs <name>` has to work."""
    _write_config(tmp_path, '[dispatch]\nsubstrate = "pane"\n')
    cap = _capture_with_stdout(monkeypatch, '{"pane_id":"%1"}\n')
    handle = st._spawn_think_worker("x-1", "prompt", str(tmp_path), "slug", provider="codex")
    assert handle == cap["cmd"][cap["cmd"].index("--name") + 1]


def test_a_configured_bg_substrate_refuses_with_the_redirect(monkeypatch, tmp_path):
    """The retired spelling refuses at the resolver, not at launch."""
    from fno.agents.harness_map import DispatchResolveError

    _write_config(tmp_path, '[dispatch]\nsubstrate = "bg"\n')
    _capture_with_stdout(monkeypatch, "some banner with no receipt\n")
    with pytest.raises(DispatchResolveError, match="retired"):
        st._spawn_think_worker("x-1", "prompt", str(tmp_path), "slug")


def test_a_nonzero_exit_still_raises_on_every_substrate(monkeypatch, tmp_path):
    """Exit 0 is the launch signal; a real failure must still surface."""
    _write_config(tmp_path, '[dispatch]\nsubstrate = "pane"\n')

    class _Proc:
        returncode = 1
        stdout = ""
        stderr = "substrate unavailable"

    monkeypatch.setattr(
        st.subprocess,
        "run",
        lambda cmd, **kw: _REAL_SUBPROCESS_RUN(cmd, **kw) if _is_naming_verb(cmd) else _Proc(),
    )
    with pytest.raises(st.SpawnError, match="substrate unavailable.*gate detail unavailable"):
        st._spawn_think_worker("x-1", "prompt", str(tmp_path), "slug", provider="codex")
