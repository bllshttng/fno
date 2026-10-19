"""The lane-B thread spawn (x-889a): a pane-less keeper hosts the thread.

A lane-B harness (transcript on disk, no daemon to hold the session) gets
its thread from fno holding the pty master itself: the spawn mints the
harness session id BEFORE launch, renders the harness's create argv
through the capability seam, and runs it under the SAME keeper the mux
hosts panes on. These tests pin the argv shape, the registry row, the
lane-A refusal, and - when a worker binary is resolvable - the real
keeper journey end to end against a stub provider.
"""
from __future__ import annotations

import inspect
import os
import shutil
import socket
import subprocess
import threading
import time
from pathlib import Path

import pytest

from fno.agents import dispatch as dispatch_mod
from fno.agents.dispatch import (
    DispatchAskError,
    _lane_b_thread_spawn,
    _mint_thread_session_id,
)
from fno.agents.harness_map import (
    render_session_argv,
    thread_lane,
)
from fno.agents.harnesses.pi import pi_model, pi_provider
from fno.agents.registry import load_registry
from fno.paths_testing import use_tmpdir
from tests._afunix import short_bind_root

# The autouse _rust_posture_door reaches every test here, so the
# whole module runs with the dev build; the conftest hook applies
# the skip where this checkout has none.
pytestmark = pytest.mark.dev_build


@pytest.fixture(autouse=True)
def _rust_posture_door(monkeypatch):
    """Pin the launch-posture door to THIS checkout's fno-agents build.

    complete_launch_argv resolves the posture through the Rust owner, so a
    stale installed binary must not answer for this tree; skip where the
    checkout has none (the same contract conftest.native_backlog_door
    implements)."""
    from fno.rust_binary import find_dev_binary

    binary = find_dev_binary()
    if binary is None:
        pytest.skip("no fno-agents dev build (cargo build -p fno-agents)")
    monkeypatch.setenv("FNO_AGENTS_BIN", str(binary))


def _fake_keeper(monkeypatch, tmp_path):
    """Stub the keeper launch + Identify so the spawn runs without a binary.

    Records the Popen argv and answers Identify with the session id the
    argv carries, the same read off the wire the real keeper does.
    """
    recorded: dict[str, object] = {}

    real_popen = subprocess.Popen

    class _FakeProc:
        pid = 4242

        args: list[str] = []

        def kill(self) -> None:  # pragma: no cover - failure paths only
            recorded["killed"] = True

        def __enter__(self) -> "_FakeProc":
            return self

        def __exit__(self, *exc: object) -> bool:
            return False

        def wait(self, timeout: object = None) -> int:
            return 0

        def poll(self) -> int:
            return 0

        def communicate(self, input: object = None, timeout: object = None):
            return ("", "")

    def _fake_popen(argv, **kwargs):  # noqa: ANN001, ANN202
        # The keeper stub sits on the shared subprocess module, so the launch-
        # posture door (the fno-agents binary behind complete_launch_argv)
        # must pass through to the real Popen: faking it would break
        # subprocess.run's own context-manager use and starve the door.
        if argv and str(argv[0]).endswith("fno-agents"):
            return real_popen(argv, **kwargs)
        # Only the keeper launch is the behavior under test. Native event
        # commits (subprocess.run -> Popen with the fno CLI) ride the real
        # binary, or the emit is starved and the capture clobbered.
        if "--keeper" not in [str(part) for part in argv]:
            return real_popen(argv, **kwargs)
        recorded["argv"] = argv
        proc = _FakeProc()
        proc.args = list(argv)
        return proc

    def _fake_identify(sock, timeout_sec=10.0):  # noqa: ANN001
        argv = list(recorded["argv"])  # type: ignore[arg-type]
        session_id = argv[argv.index("--pane-key") + 1]
        return {
            "v": 1,
            "keeper_pid": 4242,
            "child_pid": 555,
            "session_id": session_id,
            "argv": argv[argv.index("--") + 1 :],
            "cwd": str(tmp_path),
        }

    monkeypatch.setattr(dispatch_mod.subprocess, "Popen", _fake_popen)
    monkeypatch.setattr(dispatch_mod, "_keeper_identify", _fake_identify)
    monkeypatch.setattr(
        dispatch_mod, "_lane_b_worker_binary", lambda: Path("/fake/fno-agents-worker")
    )
    return recorded


@pytest.fixture
def lane_b_home(tmp_path, monkeypatch):
    """Isolated fno home; the provider renders as pi but never runs."""
    use_tmpdir(monkeypatch, tmp_path)
    return tmp_path


# ---------------------------------------------------------------------------
# AC4-ERR: the public dispatch surface still refuses
# ---------------------------------------------------------------------------


def test_lane_b_spawn_wiring_names_every_wired_keeper_harness() -> None:
    """The public dispatch_spawn body reaches the keeper entry point, and the
    table it dispatches through carries every wired arm - cursor-agent
    (callee-minted), pi and grok (caller-assigned) and agy (callee-minted) -
    so no wiring is dropped by accident. Each row is asserted through the
    field that makes it a real arm rather than a stub: the refusal an
    operator reads when the lane has no one-shot form.
    """
    from fno.agents.harness_map import known_harnesses
    from fno.agents.keeper_thread import keeper_arm

    source = inspect.getsource(dispatch_mod.dispatch_spawn)
    assert "keeper_thread_spawn" in source
    rowed = {h for h in known_harnesses() if keeper_arm(h) is not None}
    assert rowed == {"cursor-agent", "pi", "grok", "agy"}
    for harness in sorted(rowed):
        arm = keeper_arm(harness)
        assert arm and arm.get("once_refusal"), f"{harness} declares no one-shot refusal"


# ---------------------------------------------------------------------------
# AC3-EDGE: lane A keeps its harness-owned path
# ---------------------------------------------------------------------------

def test_lane_a_harnesses_are_refused_by_the_lane_b_entry_point(
    lane_b_home, monkeypatch
) -> None:
    """claude and codex resolve to the attach lane, and the lane-B entry
    point refuses them: no keeper is ever created for lane A."""
    assert thread_lane("claude") == "attach"
    assert thread_lane("codex") == "attach"
    for harness in ("claude", "codex"):
        recorded = _fake_keeper(monkeypatch, lane_b_home)
        with pytest.raises(DispatchAskError) as exc_info:
            _lane_b_thread_spawn(name="wk-a", harness=harness, cwd=lane_b_home)
        assert exc_info.value.exit_code == 2
        assert "keeper-lane" in str(exc_info.value)
        assert "argv" not in recorded, "no keeper argv is built for lane A"
        assert not list(load_registry()), "no row is written for lane A"


# ---------------------------------------------------------------------------
# AC1-HP / AC2-HP: the spawn, the argv, the row
# ---------------------------------------------------------------------------

def test_lane_b_spawn_renders_the_contract_argv_and_registers_the_row(
    lane_b_home, monkeypatch
) -> None:
    """The minted id rides the rendered create argv AND the keeper argv, and
    the row carries the socket + the id the viewport's pi thread arm reads."""
    from fno.mail.seed_provenance import SEED_PROVENANCE_KEYS

    recorded = _fake_keeper(monkeypatch, lane_b_home)
    real_popen = dispatch_mod.subprocess.Popen

    keeper_env: dict = {}

    def _recording_popen(argv, **kwargs):  # noqa: ANN001, ANN202
        if "--keeper" in [str(part) for part in argv]:
            keeper_env.update(kwargs.get("env") or {})
        return real_popen(argv, **kwargs)

    monkeypatch.setattr(dispatch_mod.subprocess, "Popen", _recording_popen)
    for _seed_key in SEED_PROVENANCE_KEYS:
        monkeypatch.setenv(_seed_key, "stale-parent-value")
    receipt = _lane_b_thread_spawn(name="wk-pi", harness="pi", cwd=lane_b_home)

    argv = list(recorded["argv"])  # type: ignore[arg-type]
    assert argv[0] == "/fake/fno-agents-worker"
    assert argv[1] == "--keeper", "the pane-less spelling is canonical"
    worker_tail = argv[argv.index("--") + 1 :]
    session_id = receipt["session_id"]
    # The contract's render plus the provider+model completion the pane lane's
    # build_pane_argv has always appended for pi (bare pi defaults to provider
    # google, and `--provider` without `--model` falls to Bedrock).
    expected_tail = [
        *render_session_argv("pi", "interactive_create", session_id),
        "--provider",
        pi_provider(),
        "--model",
        pi_model(),
    ]
    assert worker_tail == expected_tail, (
        "the provider argv is the contract's render plus the pinned "
        "provider/model, never hand-assembled"
    )
    assert argv[argv.index("--pane-key") + 1] == session_id
    assert argv[argv.index("--sock") + 1] == receipt["keeper_socket"]
    assert argv[argv.index("--cwd") + 1] == str(lane_b_home)

    entries = load_registry()
    assert [e.name for e in entries] == ["wk-pi"]
    row = entries[0]
    assert row.harness == "pi"
    assert row.host_mode == "interactive"
    assert row.harness_session_id == session_id
    assert row.messaging_socket_path == receipt["keeper_socket"]
    assert row.pid == 4242
    assert row.mux is None, "a thread row is pane-less: no mux ref"
    # The row's id is its own mint now, never the keeper session id beside it.
    assert row.fno_id
    assert row.fno_id != session_id
    assert row.origin == "spawn"
    # The keeper passes its env through to the harness child, so a stale seed
    # block inherited from the dispatcher must not survive the spawn floor.
    assert "argv" in recorded, "the keeper launch is the behavior under test"
    for _seed_key in SEED_PROVENANCE_KEYS:
        assert _seed_key not in keeper_env, f"{_seed_key} reached the keeper child env"


def test_lane_b_spawn_refuses_a_name_collision(lane_b_home, monkeypatch) -> None:
    """A second spawn under the same name is refused before any keeper
    launch - the collision check runs inside the per-agent flock."""
    _fake_keeper(monkeypatch, lane_b_home)
    _lane_b_thread_spawn(name="wk-pi", harness="pi", cwd=lane_b_home)
    recorded = _fake_keeper(monkeypatch, lane_b_home)
    with pytest.raises(DispatchAskError) as exc_info:
        _lane_b_thread_spawn(name="wk-pi", harness="pi", cwd=lane_b_home)
    assert exc_info.value.exit_code == 2
    assert "already exists" in str(exc_info.value)
    assert "argv" not in recorded
    assert len(load_registry()) == 1


def test_lane_b_spawn_without_worker_binary_exits_13(lane_b_home, monkeypatch) -> None:
    """No fno-agents runtime -> the same exit-13 shape as the codex lane."""
    monkeypatch.setattr(dispatch_mod, "_lane_b_worker_binary", lambda: None)
    with pytest.raises(DispatchAskError) as exc_info:
        _lane_b_thread_spawn(name="wk-nobin", harness="pi", cwd=lane_b_home)
    assert exc_info.value.exit_code == 13
    assert "--substrate pane" in str(exc_info.value)


def test_lane_b_spawn_kills_the_keeper_when_identify_disagrees(
    lane_b_home, monkeypatch
) -> None:
    """A keeper answering a session id other than the minted one is killed,
    never registered: the row must not point at a session fno did not mint.
    A LIVE keeper answering wrong is an identity bug; a keeper that already
    EXITED means our spawn never bound and the socket belongs to someone
    else - the error names the collision, not the identity."""
    recorded = _fake_keeper(monkeypatch, lane_b_home)

    class _LiveProc:
        pid = 4242

        def poll(self) -> None:
            return None  # alive: the answering keeper is ours

        def kill(self) -> None:
            recorded["killed"] = True

    def _wrong_identify(sock, timeout_sec=10.0):  # noqa: ANN001
        return {"v": 1, "keeper_pid": 4242, "child_pid": 555, "session_id": "other"}

    # Same pass-through as _fake_keeper: the launch-posture door must reach
    # the real binary; only the KEEPER Popen is faked.
    real_popen = subprocess.Popen

    def _live_popen(argv, **kwargs):  # noqa: ANN001, ANN202
        if argv and str(argv[0]).endswith("fno-agents"):
            return real_popen(argv, **kwargs)
        return _LiveProc()

    monkeypatch.setattr(dispatch_mod.subprocess, "Popen", _live_popen)
    monkeypatch.setattr(dispatch_mod, "_keeper_identify", _wrong_identify)
    with pytest.raises(DispatchAskError) as exc_info:
        _lane_b_thread_spawn(name="wk-wrong", harness="pi", cwd=lane_b_home)
    assert "expected the minted" in str(exc_info.value)
    assert recorded.get("killed") is True
    assert not list(load_registry())

    class _DeadProc(_LiveProc):
        def poll(self) -> int:
            return 2  # exited at the bind refusal: the socket is not ours

    recorded.clear()

    def _dead_popen(argv, **kwargs):  # noqa: ANN001, ANN202
        if argv and str(argv[0]).endswith("fno-agents"):
            return real_popen(argv, **kwargs)
        return _DeadProc()

    monkeypatch.setattr(dispatch_mod.subprocess, "Popen", _dead_popen)
    with pytest.raises(DispatchAskError) as exc_info:
        _lane_b_thread_spawn(name="wk-collide", harness="pi", cwd=lane_b_home)
    assert "another keeper still holds" in str(exc_info.value)
    assert not list(load_registry())


# ---------------------------------------------------------------------------
# AC1-HP journey: the real keeper, a stub provider, no daemon anywhere
# ---------------------------------------------------------------------------


def _test_worker_bin() -> Path | None:
    """Prefer the worktree's cargo-built worker: the journey must run the
    code under test, not whatever deployed fno-agents happens to sit on
    PATH (a deployed binary can predate the --keeper lane)."""
    repo = Path(__file__).resolve().parents[3]
    for profile in ("debug", "release"):
        built = repo / "crates" / "fno-agents" / "target" / profile / "fno-agents-worker"
        if built.is_file() and os.access(built, os.X_OK):
            return built
    return dispatch_mod._lane_b_worker_binary()


_worker_bin = _test_worker_bin()


@pytest.mark.skipif(_worker_bin is None, reason="no fno-agents-worker binary resolvable")
def test_lane_b_journey_real_keeper_hosts_the_thread(lane_b_home, monkeypatch) -> None:
    """End to end: a real keeper hosts a stub pi carrying the minted
    --session-id, answers Identify with it, and its parent is the spawning
    process - never fno-agents-daemon and never the mux server."""
    # A keeper socket must fit AF_UNIX's 104-byte sun_path, and the pytest
    # basetemp does not: rewrite the isolated state root to a short tmp dir
    # (use_tmpdir's docstring invites overwriting the settings file). That
    # root sits outside TMPDIR by construction, so the journey declares
    # ambient-on-purpose - the hermetic guard's own escape hatch - instead
    # of fighting the socket-length limit the guard cannot know about.
    monkeypatch.setenv("FNO_TEST_HERMETIC", "0")
    settings = lane_b_home / ".fno" / "settings.yaml"
    short_state = short_bind_root("fno-laneb-")  # lifetime is this one journey test
    settings.write_text(
        f"schema_version: 1\nconfig:\n  state_dir: {short_state}/\n",
        encoding="utf-8",
    )
    # The keeper socket follows FNO_AGENTS_HOME's parent, not state_dir
    # (_lane_b_keeper_socket), so the fixture's deep pin defeats the short
    # root exactly when the journey needs it. Pin it short too.
    monkeypatch.setenv("FNO_AGENTS_HOME", str(short_state / ".fno"))
    bin_dir = lane_b_home / "bin"
    bin_dir.mkdir(exist_ok=True)
    stub = bin_dir / "pi"
    stub.write_text("#!/bin/sh\nexec /usr/bin/tee /dev/null\n", encoding="utf-8")
    stub.chmod(0o755)
    # Prepend, never replace: ps/pgrep below still need the system PATH.
    monkeypatch.setenv("PATH", str(bin_dir) + os.pathsep + os.environ["PATH"])
    monkeypatch.setenv("FNO_AGENTS_WORKER_BIN", str(_worker_bin))

    receipt = _lane_b_thread_spawn(name="wk-journey", harness="pi", cwd=lane_b_home)
    keeper_pid = int(receipt["keeper_pid"])
    child_pid = int(receipt["child_pid"])

    def _ps_field(field: str, pid: int) -> str:
        out = subprocess.run(
            ["ps", "-o", f"{field}=", "-p", str(pid)],
            capture_output=True,
            text=True,
        )
        return out.stdout.strip()

    try:
        # The keeper's IDENTITY is already proven the strong way: the
        # Identify round trip in the receipt answered the minted session id,
        # and only fno-agents-worker speaks that protocol. A ps name check
        # adds nothing - Linux truncates both comm (15 chars) and args (80
        # cols headless) - so it is deliberately absent.
        keeper_ppid = _ps_field("ppid", keeper_pid)
        daemon_pids = {
            line.split()[0]
            for line in subprocess.run(
                ["pgrep", "-f", "fno-agents-daemon"], capture_output=True, text=True
            ).stdout.splitlines()
            if line.strip()
        }
        assert keeper_ppid not in daemon_pids, (
            f"the keeper's parent {keeper_ppid} must not be the daemon"
        )
        assert _ps_field("ppid", child_pid) == str(keeper_pid), (
            "the harness child is parented by the keeper"
        )
        # The row's socket is live: a fresh Identify over the ROW's field
        # answers the same minted session id.
        again = dispatch_mod._keeper_identify(Path(receipt["keeper_socket"]))
        assert again["session_id"] == receipt["session_id"]
        assert Path(receipt["keeper_socket"]).exists()
    finally:
        subprocess.run(["kill", str(keeper_pid)], capture_output=True)

    row = next(e for e in load_registry() if e.name == "wk-journey")
    assert row.messaging_socket_path == receipt["keeper_socket"]
    shutil.rmtree(short_state, ignore_errors=True)


# ---------------------------------------------------------------------------
# cursor-agent: the callee-minted keeper harness
# ---------------------------------------------------------------------------

def test_lane_b_cursor_agent_mints_through_create_chat(
    lane_b_home, monkeypatch
) -> None:
    """The row's id is CALLEE-minted: create-chat returns it before launch,
    and it is never fno's uuid4."""
    _fake_keeper(monkeypatch, lane_b_home)
    minted = "fadad56b-8008-45f5-b809-f9fab7074534"

    from fno.agents.harnesses import cursor_agent

    monkeypatch.setattr(
        cursor_agent, "create_chat", lambda cwd: minted
    )
    receipt = _lane_b_thread_spawn(
        name="wk-cursor", harness="cursor-agent", cwd=lane_b_home
    )
    assert receipt["session_id"] == minted
    row = next(e for e in load_registry() if e.name == "wk-cursor")
    assert row.harness == "cursor-agent"
    assert row.harness_session_id == minted


def test_lane_b_cursor_agent_refuses_a_truncated_resume_id(lane_b_home) -> None:
    """spawn --resume with a head-8 handle: the pinned refusal names the
    condition, because the wrong value has an obvious source."""
    with pytest.raises(DispatchAskError) as caught:
        _mint_thread_session_id("cursor-agent", lane_b_home, requested="74db359a")
    message = str(caught.value)
    assert "74db359a" in message
    assert "8 hex characters" in message
    assert "an fno session handle, not a chat id" in message


# ---------------------------------------------------------------------------
# agy: the second callee-minted keeper harness
# ---------------------------------------------------------------------------


def test_lane_b_agy_trusts_the_cwd_before_the_keeper_launches(
    lane_b_home, monkeypatch
) -> None:
    """A folder agy does not trust puts a modal in front of the composer, and
    the keeper has nobody to answer it. The upsert runs on the launch path, not
    only inside the mint, so a caller-supplied resume id gets it too."""
    _fake_keeper(monkeypatch, lane_b_home)
    trusted: list = []

    from fno.agents import mux_spawn

    monkeypatch.setattr(
        mux_spawn, "_ensure_agy_folder_trusted", lambda cwd: trusted.append(cwd) or True
    )
    _lane_b_thread_spawn(
        name="wk-agy-trust",
        harness="agy",
        cwd=lane_b_home,
        resume_session_id="8e2c0b41-19a7-4c3d-b0f5-6d7e2a3b9c10",
    )
    assert lane_b_home in trusted


# ---------------------------------------------------------------------------
# The seed submit's modal answer must not desync the frame decoder
# ---------------------------------------------------------------------------

def _serve_frames(sock_path: Path, chunks: list[bytes]) -> threading.Thread:
    """A fake keeper: accept one connection, write ``chunks`` in order, record
    every Input frame the client sends, and ECHO each one back the way a real
    TUI repaints a submitted line - that repaint is the seed's landing proof.
    """
    received: list[bytes] = []
    server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    server.bind(str(sock_path))
    server.listen(1)

    def _run() -> None:
        conn, _ = server.accept()
        try:
            for chunk in chunks:
                conn.sendall(chunk)
                time.sleep(0.15)
            deadline = time.monotonic() + 10
            conn.settimeout(0.3)
            pending = bytearray()
            while time.monotonic() < deadline:
                try:
                    data = conn.recv(65536)
                except socket.timeout:
                    continue
                if not data:
                    break
                received.append(data)
                pending.extend(data)
                while len(pending) >= 5:
                    length = int.from_bytes(pending[1:5], "little")
                    if len(pending) < 5 + length:
                        break
                    payload = bytes(pending[5 : 5 + length])
                    del pending[: 5 + length]
                    if payload != b"\r":
                        conn.sendall(_frame(1, payload))
        finally:
            conn.close()
            server.close()

    thread = threading.Thread(target=_run, daemon=True)
    thread.start()
    thread.received = received  # type: ignore[attr-defined]
    return thread


def _frame(tag: int, payload: bytes) -> bytes:
    return bytes([tag]) + len(payload).to_bytes(4, "little") + payload


def test_answering_a_modal_keeps_the_frame_decoder_in_sync() -> None:
    """The modal answer must not drop either buffer.

    `raw_pending` can hold the HEAD of a partial frame. Clearing it makes the
    next recv parse a tag and length out of mid-payload, and one garbage length
    over the 1 MiB cap breaks the decode loop for good - a wedge that reads
    exactly like a TUI that never painted its composer. `text` matters for a
    quieter reason: agy repaints the composer right behind the trust dialog, so
    the marker can arrive in the SAME recv as the modal.

    The stream here is split mid-frame on purpose: the modal frame's payload is
    delivered in two writes, and the composer marker rides the second.
    """
    from fno.agents.dispatch import _keeper_seed_submit

    # AF_UNIX caps sun_path at 104 bytes and the pytest basetemp does not fit,
    # the same short-path move the keeper spawn itself makes.
    short = short_bind_root("fnok-")
    sock_path = short / "k.sock"
    modal = _frame(1, b"Do you trust the contents of this project?\n")
    ready = _frame(1, b"? for shortcuts")
    # The split point is the whole point: the modal frame arrives COMPLETE (so
    # the regex matches and the answer fires) carrying the first three bytes of
    # the NEXT frame behind it. Those three bytes are what `raw_pending` holds
    # when the answer fires, and dropping them makes the marker's tail parse as
    # a header. A split inside the modal frame instead would leave the buffer
    # empty at that moment and the bug would be a no-op - which is how this
    # test first passed against the defect it exists to catch.
    server = _serve_frames(sock_path, [modal + ready[:3], ready[3:]])

    _keeper_seed_submit(
        name="wk-modal",
        session_id="7a64a981-b33c-4e94-861f-bb66a13c9f01",
        sock=sock_path,
        message="hello",
        ready_marker=b"? for shortcuts",
        clear_modal=(r"trust (?:this )?folder|do you trust", b"\r"),
    )
    server.join(timeout=10)

    shutil.rmtree(short, ignore_errors=True)
    sent = b"".join(server.received)  # type: ignore[attr-defined]
    # The modal answer AND the seed both landed: a decoder that desynced on the
    # partial frame would never have seen the marker and would have raised.
    assert sent.count(_frame(1, b"\r")) >= 1, "the modal was never answered"
    assert b"hello" in sent, "the seed never reached the composer"


def test_a_callee_minted_row_with_no_mint_is_refused_not_fabricated(monkeypatch) -> None:
    """The fallback for a row fno has no mint for is its own UUIDv4, and a
    callee-minted harness never adopts one: the keeper would launch on an id
    the harness does not know, and Identify would read that fabricated id back
    off the argv and report it as truth. grok stands in for the next row to
    declare the strategy before its mint is written."""
    import fno.agents.harness_map as harness_map
    from fno.agents.keeper_thread import mint_session_id

    real = harness_map.capabilities
    monkeypatch.setattr(
        harness_map,
        "capabilities",
        lambda h: {**real(h), "session_binding": {"strategy": "callee-minted-read-back"}},
    )
    with pytest.raises(DispatchAskError) as caught:
        mint_session_id("grok", Path("/tmp"), None)
    assert "callee-minted-read-back" in str(caught.value)
    assert "never adopts" in str(caught.value)
    # The measured rows are unaffected: grok really declares caller-assigned.
    monkeypatch.undo()
    assert mint_session_id("grok", Path("/tmp"), "abc") == "abc"


def test_a_modal_arriving_with_the_marker_is_still_answered() -> None:
    """One repaint can deliver the composer paint and the dialog over it.

    The marker is then already in the buffer when the modal matches, so a loop
    that tests the marker FIRST breaks out with the dialog unanswered. The seed
    goes to a TUI showing the modal: its submit CR answers the dialog, the
    payload is swallowed, and the spawn returns a live registry row with no
    orders. The modal check runs first for exactly this stream.
    """
    from fno.agents.dispatch import _keeper_seed_submit

    short = short_bind_root("fnok-")
    sock_path = short / "k.sock"
    modal = _frame(1, b"Do you trust the contents of this project?\n")
    ready = _frame(1, b"? for shortcuts")
    # ONE chunk, both complete: the marker is matchable the first time the loop
    # looks, which is what makes the ordering load-bearing.
    server = _serve_frames(sock_path, [ready + modal])

    _keeper_seed_submit(
        name="wk-modal-same-recv",
        session_id="0d1a5c33-6b90-4d02-9f77-2a1c5e88b410",
        sock=sock_path,
        message="hello",
        ready_marker=b"? for shortcuts",
        clear_modal=(r"trust (?:this )?folder|do you trust", b"\r"),
    )
    server.join(timeout=10)

    shutil.rmtree(short, ignore_errors=True)
    sent = b"".join(server.received)  # type: ignore[attr-defined]
    answer = sent.index(_frame(1, b"\r"))
    assert b"hello" in sent, "the seed never reached the composer"
    assert answer < sent.index(b"hello"), "the seed was pasted before the modal was answered"


def test_lane_b_agy_refuses_a_truncated_resume_id(lane_b_home) -> None:
    """agy resumes by EXACT id, so a partial one silently opens a different
    conversation under a name fno believes is occupied."""
    with pytest.raises(DispatchAskError) as caught:
        _mint_thread_session_id("agy", lane_b_home, requested="c5661b28")
    assert "c5661b28" in str(caught.value)
    assert "not a full UUID" in str(caught.value)


def test_lane_b_agy_runs_no_mint_and_binds_at_the_report(lane_b_home, monkeypatch) -> None:
    """agy's id binds at the session report: a spawn carries no conversation
    id, runs no mint turn, and the row's id is fno's provisional uuid until
    the report promotes the harness's own."""
    _fake_keeper(monkeypatch, lane_b_home)


    def _no_mint(payload):
        raise AssertionError(f"no door ask may run at agy spawn: {payload}")

    monkeypatch.setattr("fno.agents.spawn_axes_client.spawn_axes_call", _no_mint)
    receipt = _lane_b_thread_spawn(name="wk-agy", harness="agy", cwd=lane_b_home)

    assert receipt["session_id"]
    row = next(e for e in load_registry() if e.name == "wk-agy")
    assert row.harness == "agy"
    assert row.harness_session_id == receipt["session_id"]
