"""Task 2.2: spawn-time parent edge — spawned_by_session/harness/cwd.

Also covers x-42c5's spawn_trigger field: the CAUSE of a spawn (distinct from
the parent-edge WHO above), ambient-captured from FNO_SPAWN_TRIGGER the same
way spawned_by_* is captured from the session env vars.

Acceptance criteria (operator-locked):

  AC-HP: spawn a claude worker when CLAUDE_CODE_SESSION_ID is set ->
         registry row AND agent_spawned event carry spawned_by_session,
         spawned_by_harness="claude", spawned_by_cwd=parent PWD.
  AC-EDGE-codex: when CODEX_SESSION_ID is set (and no claude var) ->
         harness="codex", session=codex id, cwd captured.
  AC-EDGE-gemini: when GEMINI_SESSION_ID is set (and no claude/codex) ->
         harness="gemini", session=gemini id.
  AC-EDGE-none: when NO session env vars are set -> all three fields are
         None, spawn still succeeds (no raise).
  AC-EDGE-multi: when CODEX_THREAD_ID is set with legacy vars -> thread wins.
"""
from __future__ import annotations


import pytest

from fno.paths_testing import use_tmpdir
from fno.agents import events as events_mod


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


@pytest.fixture(autouse=True)
def _clear_parent_markers(monkeypatch):
    for marker in (
        "CODEX_THREAD_ID",
        "CLAUDE_CODE_SESSION_ID",
        "CODEX_SESSION_ID",
        "GEMINI_SESSION_ID",
    ):
        monkeypatch.delenv(marker, raising=False)


@pytest.fixture
def workdir_claude(tmp_path, monkeypatch):
    """Isolated fno home with a fake claude binary."""
    from tests.agents._fake_claude import install_fake_claude
    use_tmpdir(monkeypatch, tmp_path)
    bin_dir = tmp_path / "bin"
    install_fake_claude(bin_dir)
    monkeypatch.setenv("PATH", str(bin_dir))
    return tmp_path


@pytest.fixture
def captured_emits(monkeypatch):
    """Capture spawn births as (kind, data) tuples.

    Births write the daemon envelope via emit_spawned (x-8cd5 Wave 6), not the
    flat events.emit, so this intercepts the spawn seam the ACs assert on.
    """
    calls: list[tuple[str, dict]] = []

    def _capture(**data):
        calls.append(("agent_spawned", data))

    monkeypatch.setattr(events_mod, "emit_spawned", _capture)
    return calls


# ---------------------------------------------------------------------------
# AC-HP: claude spawn with CLAUDE_CODE_SESSION_ID set
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# AC-EDGE: codex session env (no claude var set)
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# AC-EDGE: gemini session env
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# AC-EDGE: no session env vars -> all three fields None, spawn succeeds
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# AC-EDGE: CODEX_THREAD_ID wins over legacy markers
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# AC-HP: exactly one agent_spawned emitted (not duplicated)
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# x-42c5: spawn_trigger — the CAUSE of a spawn, distinct from the parent edge
# ---------------------------------------------------------------------------


# --- x-132c: the gaps the fleet measurement found (0 of 30 rows stamped) ------

_PARENT_MARKERS = (
    "CODEX_THREAD_ID",
    "CLAUDE_CODE_SESSION_ID",
    "CODEX_SESSION_ID",
    "GEMINI_SESSION_ID",
    "OPENCODE_SESSION_ID",
)


def _only_marker(monkeypatch, var: str, value: str):
    """Clear every harness marker, then set exactly one, so the ambient
    resolver sees one unambiguous family regardless of the host session."""
    for m in _PARENT_MARKERS:
        monkeypatch.delenv(m, raising=False)
    monkeypatch.setenv(var, value)


def test_register_adopted_row_stamps_the_adopting_session(tmp_path, monkeypatch):
    """An adopted row records the session that vouched for it on
    ``adopted_by_session`` (x-5283 LD3) and keeps ``spawned_by_*`` empty:
    vouching is not spawning, and the share is charged on the spawn edge."""
    from fno.agents.registry import register_existing_session

    _only_marker(monkeypatch, "CLAUDE_CODE_SESSION_ID", "adopter-session-1")
    entry = register_existing_session(
        provider="claude",
        session_id="found-orphan-id",
        cwd=str(tmp_path),
        origin="adopted",
        registry_path=tmp_path / "registry.json",
    )
    assert entry.spawned_by_session is None
    assert entry.spawned_by_harness is None
    assert entry.adopted_by_session == "adopter-session-1"


def test_register_self_registration_never_stamps_its_own_id(tmp_path, monkeypatch):
    """A caller that registers the session it runs in (a mail hold, any
    self-join) passes no operator origin; the identity guard must refuse the
    stamp anyway - a row naming itself as parent is a cycle the renderer has
    to defend against, and the registry can simply never write it."""
    from fno.agents.registry import register_existing_session

    _only_marker(monkeypatch, "CLAUDE_CODE_SESSION_ID", "own-session-id")
    for origin in (None, "adopted"):
        entry = register_existing_session(
            provider="claude",
            session_id="own-session-id",
            cwd=str(tmp_path),
            origin=origin,
            registry_path=tmp_path / f"reg-{origin}.json",
        )
        assert entry.spawned_by_session is None, f"origin={origin}"
        assert entry.spawned_by_cwd is None, f"origin={origin}"


# ---------------------------------------------------------------------------
# AC4-HP (x-5283): a birth with no recordable requester names WHY in the event
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# x-5c25: a null parent must say which of its three meanings it carries
# ---------------------------------------------------------------------------


def test_ac5_hp_marker_less_spawn_still_names_the_harness(monkeypatch):
    """AC5-HP: no marker in the env, but the process walk names claude.

    Measured on 2026-09-08: 24 registry rows carried spawned_by_cwd and no
    session, and every one of their agent_spawned events said
    "identity disposition=empty, markers=no markers". The row could not be told
    apart from a human-run spawn. The walk answers the family question when the
    id cannot be proved, so the harness half is recorded and the id stays null.
    """
    import fno.claims.self_identity as self_identity
    import fno.claims.session_pid as session_pid
    from fno.agents.spawn_lineage import _capture_parent_edge

    class _Empty:
        harness = None
        session_id = None
        disposition = "empty"
        markers_present = ()

    monkeypatch.setattr(self_identity, "resolve_self_identity", lambda: _Empty())
    monkeypatch.setattr(session_pid, "resolve_session_harness", lambda: "claude")
    monkeypatch.setenv("PWD", "/parent/working/dir")

    assert _capture_parent_edge() == (None, "claude", "/parent/working/dir")


def test_a_present_marker_that_resolves_to_nothing_stays_nothing(monkeypatch):
    """A contradiction across TWO families attributes nothing, and the walk's
    verdict is not overturned. A marker that IS present and resolved to
    nothing is the x-b57a / x-0992 rule for mixed families; the one-family
    fallback resolves only single-family refusals (x-f8c3), never this one."""
    import fno.claims.self_identity as self_identity
    import fno.claims.session_pid as session_pid
    from fno.agents.spawn_lineage import _capture_parent_edge

    class _Contradicted:
        harness = None
        session_id = None
        disposition = "contradiction"
        markers_present = (
            ("CODEX_THREAD_ID", "codex", "t-1"),
            ("CLAUDE_CODE_SESSION_ID", "claude", "cl-1"),
        )

    monkeypatch.setattr(self_identity, "resolve_self_identity", lambda: _Contradicted())
    monkeypatch.setattr(session_pid, "resolve_session_harness", lambda: "claude")
    monkeypatch.setenv("PWD", "/parent/working/dir")
    monkeypatch.setenv("CODEX_THREAD_ID", "t-1")
    monkeypatch.setenv("CLAUDE_CODE_SESSION_ID", "cl-1")

    assert _capture_parent_edge() == (None, None, "/parent/working/dir")


def test_capture_falls_back_to_one_family_edge(monkeypatch):
    """The measured amber-vale shape: a refused identity whose env still
    carries one harness family. The capture layer resolves the edge from the
    family's own stamp (fno's record of this session); claims stay strict."""
    import fno.claims.self_identity as self_identity
    from fno.agents.spawn_lineage import _capture_parent_edge

    class _Refused:
        harness = None
        session_id = None
        disposition = "contradiction"
        markers_present = (("CODEX_THREAD_ID", "codex", "t-9"),)

    monkeypatch.setattr(self_identity, "resolve_self_identity", lambda: _Refused())
    monkeypatch.setenv("PWD", "/parent/working/dir")
    monkeypatch.delenv("CLAUDE_CODE_SESSION_ID", raising=False)
    monkeypatch.delenv("CLAUDE_SESSION_ID", raising=False)
    monkeypatch.setenv("FNO_HARNESS_NAME", "codex")
    monkeypatch.setenv("FNO_HARNESS_SESSION_ID", "a-stamp")
    monkeypatch.setenv("CODEX_THREAD_ID", "t-9")

    assert _capture_parent_edge() == ("a-stamp", "codex", "/parent/working/dir")


def test_same_family_parent_edge_marker_only(monkeypatch):
    """No stamp: the first marker in precedence order (the durable one)
    carries the edge."""
    from fno.agents.spawn_lineage import _same_family_parent_edge

    for _m in (
        "CODEX_THREAD_ID",
        "CODEX_SESSION_ID",
        "CLAUDE_CODE_SESSION_ID",
        "CLAUDE_SESSION_ID",
        "GEMINI_SESSION_ID",
        "OPENCODE_SESSION_ID",
        "FNO_HARNESS_NAME",
        "FNO_HARNESS_SESSION_ID",
    ):
        monkeypatch.delenv(_m, raising=False)
    monkeypatch.setenv("CODEX_THREAD_ID", "t-9")
    monkeypatch.setenv("CODEX_SESSION_ID", "root-1")

    assert _same_family_parent_edge() == ("t-9", "codex")


def test_same_family_parent_edge_refuses_cross_family_and_no_markers(monkeypatch):
    """Two families, or none: no edge, mirroring the Rust writer."""
    from fno.agents.spawn_lineage import _same_family_parent_edge

    for _m in (
        "CODEX_THREAD_ID",
        "CODEX_SESSION_ID",
        "CLAUDE_CODE_SESSION_ID",
        "CLAUDE_SESSION_ID",
        "GEMINI_SESSION_ID",
        "OPENCODE_SESSION_ID",
        "FNO_HARNESS_NAME",
        "FNO_HARNESS_SESSION_ID",
    ):
        monkeypatch.delenv(_m, raising=False)
    monkeypatch.setenv("CODEX_THREAD_ID", "t-9")
    monkeypatch.setenv("CLAUDE_CODE_SESSION_ID", "cl-9")
    assert _same_family_parent_edge() is None

    monkeypatch.delenv("CODEX_THREAD_ID")
    monkeypatch.delenv("CLAUDE_CODE_SESSION_ID")
    assert _same_family_parent_edge() is None
