"""Tests for fno.agents.self_stamp — a2a envelope auto-stamp (x-605c, US4).

`from` defaults to the invoking session's canonical handle (floor "fno" with no
ambient identity); `model` resolves from the invoking harness's own transcript
store (floor "unknown"). `--from-name` overrides `from` verbatim.
"""
from __future__ import annotations

import json

from fno.agents import discover, self_stamp


_IDENTITY_MARKERS = (
    "CODEX_THREAD_ID",
    "CLAUDE_CODE_SESSION_ID",
    "CODEX_SESSION_ID",
    "GEMINI_SESSION_ID",
)


_HARNESS_BY_MARKER = {
    "CODEX_THREAD_ID": "codex",
    "CLAUDE_CODE_SESSION_ID": "claude",
    "CODEX_SESSION_ID": "codex",
    "GEMINI_SESSION_ID": "gemini",
    "OPENCODE_SESSION_ID": "opencode",
}


def _set_identity(monkeypatch, marker, session_id):
    for name in _IDENTITY_MARKERS:
        monkeypatch.delenv(name, raising=False)
    monkeypatch.setenv(marker, session_id)
    # The stamper proves ownership via the process tree; pin it to the marker's
    # harness so these tests are independent of which harness runs pytest (a
    # codex runner would otherwise contradict a CLAUDE marker and floor it).
    monkeypatch.setattr(
        "fno.claims.session_pid.resolve_session_harness",
        lambda from_pid=None: _HARNESS_BY_MARKER.get(marker),
    )


def _write_claude_transcript(projects_dir, *, session_id, model):
    enc = "-Users-x-proj"
    d = projects_dir / enc
    d.mkdir(parents=True, exist_ok=True)
    line = json.dumps({"type": "assistant", "message": {"model": model}})
    (d / f"{session_id}.jsonl").write_text(line + "\n", encoding="utf-8")


def _write_codex_rollout(sessions_dir, *, session_id, records):
    path = sessions_dir / "2026" / "07" / "12" / f"rollout-{session_id}.jsonl"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("".join(json.dumps(record) + "\n" for record in records), encoding="utf-8")
    return path


def test_resolve_self_identity_prefers_proven_harness(monkeypatch):
    env = {
        "CODEX_THREAD_ID": "01a02125-aaaa-bbbb-cccc-dddddddddddd",
        "CLAUDE_CODE_SESSION_ID": "2782a6e1-aaaa-bbbb-cccc-dddddddddddd",
    }
    monkeypatch.setattr(
        "fno.claims.session_pid.resolve_session_harness",
        lambda from_pid=None: "claude",
    )
    identity = self_stamp.resolve_self_identity(env)
    assert identity.disposition == "proven"
    assert identity.harness == "claude"
    assert identity.session_id == env["CLAUDE_CODE_SESSION_ID"]


def test_resolve_self_identity_rejects_canonical_collision(monkeypatch):
    """The stamped pair rides the collide callback into the registry's
    agreement check (AC10-HP, post-x-0992): this layer computes nothing about
    self on its own, and a row the registry reads as the caller's OWN is never
    contention. A stamped id a live stranger owns is refused, named."""
    captured: dict = {}

    def fake_detector(session_id, *, self_binding, walk_harness=None):
        captured["self_binding"] = self_binding
        captured["walk_harness"] = walk_harness
        return "victim-lead" if session_id == "victim-session" else None

    env = {
        "FNO_HARNESS_NAME": "claude",
        "FNO_HARNESS_SESSION_ID": "victim-session",
    }
    monkeypatch.setattr(
        "fno.claims.session_pid.resolve_session_harness",
        lambda from_pid=None: "claude",
    )
    monkeypatch.setattr(
        "fno.agents.registry.row_owning_session_id", fake_detector
    )

    identity = self_stamp.resolve_self_identity(env)

    assert captured["self_binding"] == ("claude", "victim-session")
    assert captured["walk_harness"] == "claude"
    assert identity.disposition == "ambiguous"
    assert identity.session_id is None
    assert identity.harness is None
    assert identity.rejected[0]["owner"] == "victim-lead"


def test_self_stamp_variant_f_resolves_to_the_thread_id(monkeypatch):
    """AC3 through the runtime lane (x-a409): a name_only codex stamp over a
    root+thread id pair. The rollout witness sees both rollout ids; codex sets
    CODEX_SESSION_ID to the root and CODEX_THREAD_ID per thread, so the thread
    id is the process's own, and stamp_from resolves the handle, not "fno"."""
    root = "019cc081-de0d-7283-97cc-751c46742a07"
    thread = "019cc082-1111-7283-97cc-751c46742a08"
    monkeypatch.setattr(
        "fno.claims.session_pid.resolve_session_harness",
        lambda from_pid=None: "codex",
    )
    monkeypatch.setattr(
        "fno.claims.self_identity.resolve_attester_identity",
        lambda env=None: ("", "env_only"),
    )
    monkeypatch.setattr(
        "fno.agents.codex_rollout.codex_rollout_witness",
        lambda harness, env=None: frozenset({root, thread}),
    )
    monkeypatch.setenv("FNO_HARNESS_NAME", "codex")
    monkeypatch.setenv("CODEX_SESSION_ID", root)
    monkeypatch.setenv("CODEX_THREAD_ID", thread)

    identity = self_stamp.resolve_self_identity()

    assert identity.disposition == "single"
    assert identity.session_id == thread
    assert identity.harness == "codex"
    assert identity.rejected == ()
    assert self_stamp.stamp_from(None) == thread[:8]


def test_restarted_session_resolves_through_its_own_row(monkeypatch):
    """After a restart the same session id meets its own re-registered
    live row (same name, refreshed in place). A name_only stamp with a stale
    FNO_AGENT_SELF and an env_only attester used to refuse ("owned by live row
    <name>"), mailing as "fno" and answering holder_unattributable; the walk's
    family ground resolves the row as self and the mail handle follows."""
    sid = "9a063cd3-aaaa-bbbb-cccc-dddddddddddd"

    class _Row:
        name = "kestrel"
        harness = "claude"
        status = "live"
        harness_session_id = sid
        aliases = ()
        # The restart signature: the spawn-time pid is dead after the resume.
        pid = 99999999

    monkeypatch.setattr(
        "fno.claims.session_pid.resolve_session_harness",
        lambda from_pid=None: "claude",
    )
    monkeypatch.setattr(
        "fno.claims.self_identity.resolve_attester_identity",
        lambda _env: ("", "env_only"),
    )
    monkeypatch.setattr(
        "fno.agents.registry.load_registry", lambda registry_path=None: [_Row()]
    )

    env = {
        "FNO_HARNESS_NAME": "claude",
        "FNO_AGENT_SELF": "stale-worker-name",
        "CLAUDE_CODE_SESSION_ID": sid,
    }
    identity = self_stamp.resolve_self_identity(env)
    assert identity.disposition == "single"
    assert identity.session_id == sid
    assert identity.harness == "claude"
    assert identity.rejected == ()
    # The mail leg: the same env in os.environ resolves the handle, not "fno".
    monkeypatch.setenv("FNO_HARNESS_NAME", "claude")
    monkeypatch.setenv("FNO_AGENT_SELF", "stale-worker-name")
    monkeypatch.setenv("CLAUDE_CODE_SESSION_ID", sid)
    assert self_stamp.stamp_from(None) == sid[:8]


def test_ac1_hp_floors_unknown_when_markers_unset_despite_reachable_transcript(
    tmp_path, monkeypatch
):
    """The markers gate identity: a reachable transcript must NOT resurrect a
    model when no ambient marker is set. This is the hermetic-preflight posture -
    a fresh CI checkout has no marker and floors to "unknown" even though a
    transcript store happens to be reachable (x-bbe7 US3a)."""
    sid = "9a063cd3-aaaa-bbbb-cccc-dddddddddddd"
    projects = tmp_path / "projects"
    _write_claude_transcript(projects, session_id=sid, model="claude-opus-4-8")
    monkeypatch.setenv(discover.PROJECTS_DIR_ENV, str(projects))
    for name in _IDENTITY_MARKERS:
        monkeypatch.delenv(name, raising=False)
    assert self_stamp.resolve_self_model() == "unknown"
    assert self_stamp.stamp_from(None) == "fno"


def test_claude_model_ignores_later_sidechain_assistant(tmp_path, monkeypatch):
    sid = "11111111-aaaa-bbbb-cccc-dddddddddddd"
    projects = tmp_path / "projects"
    transcript = projects / "-Users-x-proj" / f"{sid}.jsonl"
    transcript.parent.mkdir(parents=True)
    records = [
        {"type": "assistant", "message": {"model": "claude-opus-4-8"}},
        {
            "type": "assistant",
            "isSidechain": True,
            "message": {"model": "claude-haiku-4-5"},
        },
        {"type": "user", "message": {"model": "claude-fake"}},
    ]
    transcript.write_text(
        "".join(json.dumps(record) + "\n" for record in records), encoding="utf-8"
    )
    monkeypatch.setenv(discover.PROJECTS_DIR_ENV, str(projects))
    _set_identity(monkeypatch, "CLAUDE_CODE_SESSION_ID", sid)

    assert self_stamp.resolve_self_model() == "claude-opus-4-8"


def test_codex_model_escalates_past_bounded_windows(tmp_path, monkeypatch):
    sid = "22222222-aaaa-bbbb-cccc-dddddddddddd"
    sessions = tmp_path / "sessions"
    padding = {"type": "event_msg", "payload": {"text": "x" * 4096}}
    records = [
        {"type": "turn_context", "payload": {"model": "gpt-a"}},
        {"type": "turn_context", "payload": {"model": "gpt-b"}},
        *([padding] * 520),
    ]
    _write_codex_rollout(sessions, session_id=sid, records=records)
    monkeypatch.setenv(discover.CODEX_SESSIONS_DIR_ENV, str(sessions))
    _set_identity(monkeypatch, "CODEX_THREAD_ID", sid)

    assert self_stamp.resolve_self_model() == "gpt-b"


