"""Rust-door contracts the Python test tree still pins end to end.

The agents client commands (resume, attach, logs, trace) are owned by the
compiled ``fno-agents`` client; their byte-parity rows against the retired
Python fallback were deleted once the port was complete. What stays is the
cross-language seam (a registry written by Python's real ``write_registry``
must be readable by Rust) and the attach refusals no Rust unit test reaches,
because they need a fixture state tree and a fake mux portal on PATH.
Skipped when the Rust binary is absent (sdist test env).
"""
from __future__ import annotations

import functools
import json
import os
import shutil
import subprocess
import tempfile
from pathlib import Path

import pytest

from fno.rust_binary import find_dev_binary


@pytest.fixture(autouse=True)
def _scratch_cwds():
    """Create the hardcoded ``/tmp/proj`` cwd: Rust's resume refuses a gone cwd,
    and macOS periodic /tmp cleanup removes it on a clean machine."""
    Path("/tmp/proj").mkdir(parents=True, exist_ok=True)


@functools.lru_cache(maxsize=1)
def _fno_shim_dir() -> str | None:
    """A PATH dir providing ``fno``, or ``None`` when the real one is present.

    The Rust client resolves a short token by shelling out to
    ``fno agents heal-token --all-sources`` (``client_commands.rs``). ``fno`` is the
    cargo-installed RUST binary; the Python package installs only ``fno-py``
    (``[project.scripts]``). So a checkout that has never run ``cargo install``
    -- CI, and any dev box that only built ``fno-agents`` -- has no ``fno`` on
    PATH and the shellout dies with ENOENT for a purely environmental reason.

    The shim resolves ``fno-py`` at exec time rather than install time, because
    it is the surrounding ``uv run`` that puts the venv bin on PATH.

    ``mux`` never forwards: the Python front execs whichever ``fno`` is on
    PATH for that command, which here is this shim again, and the recursion runs
    until the runner kills it. The Rust attach command reaches the mux through
    ``fno mux thread``, so the shim answers as a box with no live mux server.
    """
    if shutil.which("fno"):
        return None
    d = tempfile.mkdtemp(prefix="fno-parity-shim-")
    shim = Path(d) / "fno"
    shim.write_text(
        "#!/bin/sh\n"
        'if [ "$1" = mux ]; then\n'
        '  echo "fno mux: no live mux server (parity shim)" >&2\n'
        "  exit 24\n"
        "fi\n"
        'exec fno-py "$@"\n'
    )
    shim.chmod(0o755)
    return d


RUST_BIN = find_dev_binary()
requires_rust = pytest.mark.skipif(
    RUST_BIN is None,
    reason="compiled fno-agents binary not present (build with `cargo build -p fno-agents`)",
)


@pytest.fixture(autouse=True)
def _provider_clis_on_path(tmp_path, monkeypatch):
    """Presence checks need codex/gemini/opencode resolvable.

    The full smoke packet stubs these on PATH before running this file; the
    changed packet selects the same file with no such stub, so a PR touching it
    failed on `codex CLI not on PATH` instead of any parity claim. Stub them
    here so both packets exercise the same PATH, and keep them `exit 0` no-ops:
    nothing under test executes a provider. A per-test fake that prepends its
    own dir still wins.
    """
    fake_bin = tmp_path / "fake-provider-bin"
    fake_bin.mkdir()
    for name in ("codex", "gemini", "opencode"):
        stub = fake_bin / name
        stub.write_text("#!/bin/sh\nexit 0\n")
        stub.chmod(0o755)
    monkeypatch.setenv("PATH", str(fake_bin) + os.pathsep + os.environ.get("PATH", ""))


def _run_rust(args: list[str], home: Path) -> subprocess.CompletedProcess:
    """Run the Rust client with FNO_AGENTS_HOME pointed at ``home`` (the agents dir).

    The full environment (notably ``PATH``) is inherited so the Rust client's
    provider-CLI PATH check sees the same PATH as Python's ``shutil.which`` --
    otherwise resume/attach would diverge purely on which PATH each side was
    handed, not on behavior.
    """
    source_path = str(Path(__file__).resolve().parents[2] / "src")
    inherited_pythonpath = os.environ.get("PYTHONPATH")
    pythonpath = (
        source_path + os.pathsep + inherited_pythonpath
        if inherited_pythonpath
        else source_path
    )
    env = {
        **os.environ,
        "FNO_AGENTS_HOME": str(home),
        "PYTHONPATH": pythonpath,
    }
    # The smoke setup exports the fresh Rust front through FNO_RUST_FRONT. The
    # front's verification notice is setup noise, not part of the client command
    # contract, and would make Rust stderr differ from the Python oracle.
    env.pop("FNO_RUST_FRONT", None)
    # Only when nothing on the PATH handed to the client provides fno, so a
    # machine that HAS fno keeps exercising the genuine article, and a test
    # that put its own fno shim first on PATH keeps that shim in front.
    shim_dir = _fno_shim_dir()
    if shim_dir and shutil.which("fno", path=env.get("PATH", "")) is None:
        env["PATH"] = shim_dir + os.pathsep + env.get("PATH", "")
    # A short-token parity case asks the Rust client to invoke the Python
    # all-source resolver. Keep that test hermetic instead of scanning the
    # operator's real Claude/OpenCode corpora; individual tests can still
    # override these seams through monkeypatch before calling this helper.
    isolated_stores = {
        "FNO_CLAUDE_PROJECTS_DIR": home.parent / "empty-claude-projects",
        "FNO_CODEX_SESSIONS_DIR": home.parent / "empty-codex-sessions",
        "FNO_OPENCODE_STORAGE_DIR": home.parent / "empty-opencode" / "storage",
        # attach tries the mux thread portal first; an empty mux dir means
        # no socket, so the command falls through to the path under test
        # instead of driving the operator's live server.
        "FNO_MUX_DIR": home.parent / "empty-mux",
    }
    for key, path in isolated_stores.items():
        if key not in env:
            path.mkdir(parents=True, exist_ok=True)
            env[key] = str(path)
    return subprocess.run(
        [str(RUST_BIN), *args],
        env=env,
        capture_output=True,
        text=True,
    )


def _seed_registry(agents: Path, entries: list[dict]) -> None:
    agents.mkdir(parents=True, exist_ok=True)
    (agents / "registry.json").write_text(json.dumps({"schema_version": 3, "agents": entries}))


# --------------------------------------------------------------------------- #
# attach (refusal paths the Rust door owns end to end)
# --------------------------------------------------------------------------- #

@requires_rust
def test_attach_missing_agent_exit2(tmp_path) -> None:
    # attach uses the lifecycle convention: missing agent is exit 2 (NOT 13,
    # which is resume's convention). The shared resolver (x-1b1e) prints the
    # accepted-forms not-found message; the exit code is unchanged.
    agents = tmp_path / "agents"
    _seed_registry(agents, [])
    rust = _run_rust(["attach", "ghost"], agents)
    assert rust.stderr == (
        "no agent matching 'ghost'; "
        "accepted forms: name, canonical handle, transport short id, or full session id\n"
    )
    assert rust.returncode == 2


@requires_rust
def test_attach_codex_thread_headless_names_the_terminal_not_one_shot(tmp_path) -> None:
    # A codex THREAD row (x-6678 shape: interactive, no short id, no mux) left
    # the one-shot refusal when its attach form was declared (x-296f). This
    # run is headless, so the door refuses naming the terminal requirement -
    # never the vendor's bare "stdin is not a terminal", and never "one-shot",
    # which is false for a durable thread.
    agents = tmp_path / "agents"
    _seed_registry(
        agents,
        [{"name": "cxt", "provider": "codex", "cwd": "/tmp/x", "log_path": "/x/l",
          "short_id": "", "project_root": "/x", "status": "live", "created_at": "t",
          "host_mode": "interactive", "harness_session_id": "01a04546-28b2-7a41-ae4c-892bbeb8e295"}],
    )
    rust = _run_rust(["attach", "cxt"], agents)
    assert rust.returncode == 13
    assert "one-shot" not in rust.stderr
    assert "terminal" in rust.stderr
    assert "peek cxt --follow" in rust.stderr


def _mux_thread_shim(tmp_path: Path, monkeypatch, script: str) -> None:
    """Put a fake ``fno`` first on PATH whose ``mux thread`` arm runs ``script``.

    The attach command reaches the mux thread portal by shelling ``fno mux
    thread <name>``; the shim stands in for a live server so the portal's
    success and refusal arms are testable without one.
    """
    d = tmp_path / "shim"
    d.mkdir()
    shim = d / "fno"
    shim.write_text('#!/bin/sh\nif [ "$1" = mux ] && [ "$2" = thread ]; then\n' + script + '\nfi\nexec fno-py "$@"\n')
    shim.chmod(0o755)
    monkeypatch.setenv("PATH", str(d) + os.pathsep + os.environ.get("PATH", ""))


@requires_rust
def test_attach_surfaces_a_portal_refusal_instead_of_double_attaching(tmp_path, monkeypatch) -> None:
    # Any exit the portal owns (not no-server, usage, or unanswered) is the
    # server refusing: the command repeats it verbatim and never falls through
    # to an attach the server just refused.
    agents = tmp_path / "agents"
    _seed_registry(
        agents,
        [{"name": "gm", "provider": "gemini", "cwd": "/tmp/x", "log_path": "/x/l",
          "short_id": "", "project_root": "/x", "status": "live", "created_at": "t"}],
    )
    _mux_thread_shim(tmp_path, monkeypatch, 'echo "fno mux thread: no such agent: $3" >&2; exit 1')
    rust = _run_rust(["attach", "gm"], agents)
    assert rust.returncode == 1, rust.stderr
    assert rust.stderr == "fno mux thread: no such agent: gm\n"


# --------------------------------------------------------------------------- #
# Contract: the Rust client must read a registry written by Python's REAL
# write_registry (top-level key "agents"), not a hand-built fixture. This is the
# round-trip that would have caught the entries-vs-agents key bug -- the earlier
# fixtures wrote the key the Rust code happened to read.
# --------------------------------------------------------------------------- #

@requires_rust
def test_rust_reads_real_python_written_registry(tmp_path) -> None:
    from fno.agents.registry import AgentEntry, write_registry

    agents = tmp_path / "agents"
    agents.mkdir(parents=True)
    write_registry(
        [
            AgentEntry(
                name="cx",
                harness="codex",
                cwd="/tmp/proj",
                log_path=str(agents / "cx.jsonl"),
                harness_session_id="uuid-9",
            )
        ],
        agents / "registry.json",
    )
    # resume resolves the agent (codex resume argv) -> exit 0 with the snippet,
    # proving the Rust reader found the agent under the real "agents" key.
    rust = _run_rust(["resume", "cx", "--print-command"], agents)
    assert rust.returncode == 0, rust.stderr
    # The resume form's declared pre_exec (the shared-daemon ownership
    # assertion) composes ahead of the resume exec, so the snippet is a
    # quoted `sh -c` script rather than a bare `codex` invocation.
    assert rust.stdout.startswith("cd /tmp/proj && exec sh -c ")
    # codex 0.156.1 refuses the writable_roots override on the declared
    # form's --remote lane, so the grant is absent from the snippet.
    assert "writable_roots=" not in rust.stdout
    assert "app-server" in rust.stdout and "daemon" in rust.stdout
    # The Rust reader resolved the agent and rendered ITS session id, and the
    # daemon-attach flag trails it: codex's globals (-c, --cd) sit before the
    # subcommand, nothing trails the positional except --remote unix://.
    assert rust.stdout.strip().endswith("'\"'\"'unix://'\"'\"''")
    # attach found the agent: a codex row with no thread shape reads the
    # native features.attach claim and names the daemon-kept lane (exit 24),
    # never "not found" (exit 2).
    att = _run_rust(["attach", "cx"], agents)
    assert att.returncode == 24, att.stderr
    assert 'features.attach = "native"' in att.stderr
