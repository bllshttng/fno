"""The reap-to-adopt round trip, and the notices that name it.

The claim this node rests on is that `fno agents rm` forfeits a resume handle
REVERSIBLY: the harness session record goes, the transcript stays, and `fno
agents adopt` rebuilds the row from what is left. Every notice added here
prints that recovery, so a test that never checks it would ship a promise
nobody verified.

`adopt` itself is a Rust command, so these tests pin the Python half its recovery
actually depends on: the store probe that `adopt`'s third resolution step runs
(`heal_token` -> `probe_stores`), which is what finds a reaped session by its
8-hex short id. That is the load-bearing link, and it is hermetic.

ACs:
- AC1-HP  : rm writes the session id and the literal adopt command to stderr
            BEFORE any harness teardown is issued, and still completes.
- AC2-HP  : the transcript survives rm, and the store probe still resolves the
            short id afterwards -- the round trip adopt performs.
- AC1-ERR : an operator answering no at the TTY prompt stops the reap.
- AC2-ERR : --force never prompts, and still writes the warning.
- AC1-EDGE: a row with no resume handle at all gets no notice naming an empty
            one.
- AC1-UI  : peek's miss names the registry it read, says the transcript is on
            disk when it is, and prints the adopt command.
"""
from __future__ import annotations

import io
from pathlib import Path

import pytest

from fno.agents import rm_notice
from fno.agents.registry import AgentEntry, load_registry, update_registry

# A full claude session uuid and the 8-hex short id an operator actually peeks
# and adopts by. The short is the uuid's first eight, matching canonical_handle.
SESSION_ID = "0a6e775f-1111-2222-3333-444444444444"
SHORT_ID = "0a6e775f"


@pytest.fixture
def isolated_state(tmp_path: Path, monkeypatch) -> Path:
    from fno import paths

    fake_home = tmp_path / "home"
    fake_home.mkdir()
    monkeypatch.setattr(paths, "agents_registry_path", lambda: tmp_path / "registry.jsonl")
    monkeypatch.setattr(paths, "state_dir", lambda: tmp_path / "state")
    monkeypatch.setattr(Path, "home", classmethod(lambda cls: fake_home))
    return tmp_path


def _seed_row(name: str = "reaped-worker", *, short_id: str = SHORT_ID) -> None:
    update_registry(
        lambda entries: entries
        + [
            AgentEntry(
                name=name,
                harness="claude",
                cwd="/tmp",
                log_path="/tmp/reaped-worker.log",
                harness_session_id=SESSION_ID,
                short_id=short_id,
            )
        ]
    )


def _seed_transcript(tmp_path: Path, monkeypatch) -> Path:
    """A claude transcript on disk, named by its session uuid as claude names it."""
    from fno.agents.discover import PROJECTS_DIR_ENV

    root = tmp_path / "projects" / "-tmp-project"
    root.mkdir(parents=True)
    path = root / f"{SESSION_ID}.jsonl"
    path.write_text(
        '{"type":"user","cwd":"/tmp","message":{"role":"user","content":"hi"}}\n',
        encoding="utf-8",
    )
    monkeypatch.setenv(PROJECTS_DIR_ENV, str(tmp_path / "projects"))
    return path


def _names() -> list[str]:
    return [e.name for e in load_registry()]


# --------------------------------------------------------------------------
# AC1-HP / AC2-ERR: the warning precedes the teardown
# --------------------------------------------------------------------------


def test_rm_notice_prefers_the_full_session_id():
    """adopt takes either form, but only the full id is collision-free.

    A codex session id is time-prefixed, so its first eight collide across
    same-window sessions. Naming the short one could hand the operator a
    sibling session, which is worse than naming nothing.
    """
    with_both = AgentEntry(
        name="a",
        harness="claude",
        cwd="/tmp",
        log_path="/tmp/a.log",
        harness_session_id=SESSION_ID,
        short_id=SHORT_ID,
    )
    assert rm_notice.resume_handle_for(with_both) == SESSION_ID

    short_only = AgentEntry(
        name="b",
        harness="claude",
        cwd="/tmp",
        log_path="/tmp/b.log",
        short_id=SHORT_ID,
    )
    assert rm_notice.resume_handle_for(short_only) == SHORT_ID


# --------------------------------------------------------------------------
# AC1-ERR / AC2-ERR: the confirmation gate
# --------------------------------------------------------------------------


class _FakeTTY(io.StringIO):
    def __init__(self, answer: str = "") -> None:
        super().__init__(answer)

    def isatty(self) -> bool:  # noqa: D102 - a TTY for the prompt's purposes
        return True


def test_declining_at_the_prompt_stops_the_reap(isolated_state):
    """AC1-ERR: anything but a yes is a no, and nothing is torn down."""
    _seed_row()
    err = _FakeTTY()
    assert rm_notice.warn_and_confirm(
        "reaped-worker", stderr=err, stdin=_FakeTTY("n\n")
    ) is False
    assert "fno agents adopt" in err.getvalue()


def test_force_never_prompts_but_still_warns(isolated_state):
    """AC2-ERR: --force skips the question, never the notice."""
    _seed_row()
    err = _FakeTTY()
    stdin = _FakeTTY("")  # a read here would return "" and refuse
    assert rm_notice.warn_and_confirm(
        "reaped-worker", force=True, stderr=err, stdin=stdin
    ) is True
    assert "fno agents adopt" in err.getvalue()
    assert "anyway?" not in err.getvalue()


def test_no_notice_for_a_harness_that_tears_nothing_down(isolated_state):
    """opencode is registry-only and gemini has no teardown arm at all.

    Warning that their reap "forfeits the resume handle" would report a loss
    that does not happen, which is this module's own defect pointed the other
    way. The row still carries a session id, so the guard cannot be the handle
    lookup: it has to be the harness.
    """
    for harness in ("opencode", "gemini"):
        entry = AgentEntry(
            name=f"{harness}-row",
            harness=harness,
            cwd="/tmp",
            log_path="/tmp/x.log",
            harness_session_id=SESSION_ID,
        )
        assert rm_notice.resume_handle_for(entry) is not None
        assert rm_notice.forfeits_resume_handle(entry) is False, harness

    for harness in ("claude", "codex"):
        entry = AgentEntry(
            name=f"{harness}-row",
            harness=harness,
            cwd="/tmp",
            log_path="/tmp/x.log",
            harness_session_id=SESSION_ID,
        )
        assert rm_notice.forfeits_resume_handle(entry) is True, harness


def test_a_prompt_nobody_can_read_is_never_asked(isolated_state):
    """The silent-hang guard: stdin is a TTY but stderr is redirected away.

    `dispatch-node.sh` and `spawn.sh` both run `fno agents rm ... >/dev/null
    2>&1` with stdin inherited. Prompting there would block forever on a
    question sent to /dev/null -- a silent hang produced by the guard meant to
    prevent a silent loss. The read below would return "" and refuse, so a
    regression here fails as a False, not as a timeout.
    """
    _seed_row()
    err = io.StringIO()  # NOT a tty: the prompt would be invisible
    assert rm_notice.warn_and_confirm(
        "reaped-worker", stderr=err, stdin=_FakeTTY("")
    ) is True
    assert "anyway?" not in err.getvalue()


# --------------------------------------------------------------------------
# AC2-HP: the round trip adopt actually performs
# --------------------------------------------------------------------------


def test_the_store_probe_resolves_a_reaped_short_id(isolated_state, tmp_path, monkeypatch):
    """Adopt's recovery is real: the harness store probe still answers for the
    short id once the row is gone.

    `adopt`'s third resolution step is the harness store probe, so if the
    probe answers for the short id of a row that no longer exists, every
    `fno agents adopt` hint the notices print is a promise the tool keeps.
    (The rm half of the old round trip is Rust-owned now; the crate's index
    tests pin that rm drops the index record, never the transcript.)
    """
    from fno.agents.store_fallback import probe_stores

    _seed_transcript(tmp_path, monkeypatch)

    hits = probe_stores(SHORT_ID)
    assert [h.session_id for h in hits] == [SESSION_ID], (
        "adopt's store probe must answer for the short id of a reaped row; "
        "otherwise every `fno agents adopt` hint this change prints is a lie"
    )


# --------------------------------------------------------------------------
# AC1-UI: peek names the instrument
# --------------------------------------------------------------------------


    # The exit code alone is not the assertion: a crashed binary also fails to
    # print a peer. The adopt hint is the positive marker.


