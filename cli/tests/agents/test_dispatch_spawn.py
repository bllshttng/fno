"""Spawn verb contracts on the Python leg.

  codex --once: creates and exchanges, stdout carries the reply, the teardown
          receipt names the peer, and the registry has no row afterward;
          a failed create exits nonzero and writes no row.
  name collision: refuses exit 2 and leaves the existing row untouched.
  claude thread spawn: exact JSON receipt on stdout, registry row present.
  harness seam: undeclared, unmeasured and pane-only harnesses refuse cleanly.
  opencode thread: delegates to the serve lane; --once and --resume refuse.
"""
from __future__ import annotations

import json
import re
from pathlib import Path
from unittest.mock import MagicMock

import pytest
from typer.testing import CliRunner

from fno.paths_testing import use_tmpdir
from fno.agents.harnesses import codex as codex_mod
from fno.agents.harnesses.codex import (
    CodexInvocationError,
    CodexResult,
)
from fno.agents.registry import (
    AgentEntry,
    load_registry,
    write_registry,
)


@pytest.fixture(autouse=True)
def _graphless_seed_resolver(monkeypatch):
    """The seed-word gate fails open on an unreadable graph; pin that here so
    the plumbing tests' fake node words never depend on the ambient store."""

    def _unreadable(path=None, **_):
        raise ValueError("graph store unreadable")

    monkeypatch.setattr("fno.graph.load.load_graph", _unreadable)


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _receipt_line(output: str) -> str:
    """The line that IS the JSON receipt. CliRunner mixes stderr into output,
    so a stderr notice (the inherited tier-remap drop warning) can precede or
    follow the receipt on machines carrying ANTHROPIC_DEFAULT_*_MODEL."""
    for line in output.splitlines():
        line = line.strip()
        if line.startswith("{"):
            return line
    raise AssertionError(f"no JSON receipt line in output: {output!r}")


def _make_runner() -> CliRunner:
    return CliRunner()


def _write_existing_entry(name: str, provider: str, session_id: str) -> None:
    """Seed the registry with one entry for collision tests."""
    write_registry([
        AgentEntry(
            name=name,
            harness=provider,
            cwd="/tmp",
            log_path="/tmp/a.log",
            harness_session_id=session_id if provider == "codex" else None,
            short_id=session_id if provider == "claude" else "",
        )
    ])


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


@pytest.fixture
def workdir(tmp_path, monkeypatch):
    """Isolated fno home with codex marked available on PATH."""
    use_tmpdir(monkeypatch, tmp_path)
    # Pin the repo root and stand inside the sandbox: the spawn default-move
    # compares caller cwd to the canonical root, and the receipt is only
    # byte-stable when every resolution branch short-circuits to tmp.
    monkeypatch.setenv("FNO_REPO_ROOT", str(tmp_path))
    monkeypatch.chdir(tmp_path)
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    (bin_dir / "codex").write_text("#!/bin/sh\necho fake\n", encoding="utf-8")
    (bin_dir / "codex").chmod(0o755)
    monkeypatch.setenv("PATH", str(bin_dir))
    return tmp_path


@pytest.fixture
def workdir_claude(tmp_path, monkeypatch):
    """Isolated fno home with claude marked available on PATH."""
    from tests.agents._fake_claude import install_fake_claude
    use_tmpdir(monkeypatch, tmp_path)
    # Same pin as workdir: keep the default-move resolver inside the sandbox.
    monkeypatch.setenv("FNO_REPO_ROOT", str(tmp_path))
    monkeypatch.chdir(tmp_path)
    bin_dir = tmp_path / "bin"
    install_fake_claude(bin_dir)
    monkeypatch.setenv("PATH", str(bin_dir))
    return tmp_path


@pytest.fixture
def fake_codex_create_once(monkeypatch):
    """Replace codex_mod.create with a mock returning a successful CodexResult."""
    mock = MagicMock(return_value=CodexResult(
        exit_code=0,
        session_id="codex-once-sid",
        last_msg="hello from codex",
        duration_ms=42,
    ))
    monkeypatch.setattr(codex_mod, "create", mock)
    return mock


# ---------------------------------------------------------------------------
# AC2-HP: codex --once happy path
# ---------------------------------------------------------------------------


def test_spawn_once_codex_happy_path(workdir, fake_codex_create_once, monkeypatch) -> None:
    """AC2-HP: codex --once creates+exchanges, reply on stdout, teardown on stderr,
    no registry row afterward, exit 0."""
    from fno.agents.cli import agents_app

    runner = _make_runner()
    result = runner.invoke(
        agents_app,
        ["spawn", "--name", "tmp1", "--once", "summarize X"],
        env={"CODEX_THREAD_ID": "019f0000-0000-7000-8000-00000000000a"},
        catch_exceptions=False,
    )

    assert result.exit_code == 0, f"expected exit 0, got {result.exit_code}\noutput: {result.output}"
    # stdout+stderr combined is result.output in Typer CliRunner
    assert "hello from codex" in result.output
    # Teardown receipt in output
    assert "tmp1" in result.output
    assert "codex" in result.output
    assert "torn down" in result.output or "teardown" in result.output.lower()
    # Registry must have NO row for tmp1 after teardown
    entries = load_registry()
    assert not any(e.name == "tmp1" for e in entries), (
        f"Expected no tmp1 row after --once teardown, got: {entries}"
    )


def test_spawn_once_create_failure_no_registry_entry(workdir, monkeypatch) -> None:
    """codex create fails -> nonzero exit and no registry row (helpers write
    the registry only after subprocess success)."""
    monkeypatch.setattr(codex_mod, "create", MagicMock(side_effect=CodexInvocationError(1)))
    from fno.agents.cli import agents_app

    result = _make_runner().invoke(
        agents_app, ["spawn", "--name", "tmp2", "-H", "codex", "--once", "hello"]
    )

    assert result.exit_code != 0, result.output
    assert not any(e.name == "tmp2" for e in load_registry())


# ---------------------------------------------------------------------------
# AC2-EDGE: name collision refuses, existing row untouched
# ---------------------------------------------------------------------------


def test_spawn_collision_refuses(workdir) -> None:
    """AC2-EDGE: pre-seeded registry entry -> spawn refuses exit 2, row untouched."""
    _write_existing_entry("existing-agent", "codex", "oldses-123")

    from fno.agents.cli import agents_app

    runner = _make_runner()
    result = runner.invoke(
        agents_app,
        ["spawn", "--name", "existing-agent", "-H", "codex", "--once", "hello"],
    )

    assert result.exit_code == 2, (
        f"expected exit 2 for name collision, got {result.exit_code}\n"
        f"output: {result.output}"
    )
    # Message must mention the name and hint
    assert "existing-agent" in result.output
    assert "rm" in result.output
    # Row must remain intact
    entries = load_registry()
    existing = next((e for e in entries if e.name == "existing-agent"), None)
    assert existing is not None, "existing row must not be deleted on collision"
    assert existing.harness_session_id == "oldses-123"


# ---------------------------------------------------------------------------
# claude plain spawn: compact JSON receipt, registry row present
# ---------------------------------------------------------------------------


def test_spawn_claude_plain(workdir_claude) -> None:
    """claude bg-substrate spawn: compact JSON receipt on stdout (4a-G2: the
    plain/pane default is mux-hosted; the bg thread lane keeps this shape)."""
    from fno.agents.cli import agents_app

    runner = _make_runner()
    result = runner.invoke(
        agents_app,
        ["spawn", "--name", "myagent-c", "hello"],
        catch_exceptions=False,
    )

    assert result.exit_code == 0, (
        f"expected exit 0, got {result.exit_code}\noutput: {result.output}"
    )
    # output is stdout+stderr combined in Typer CliRunner; the JSON receipt
    # is the FIRST line (on stdout), before any stderr teardown notes.
    first_line = result.output.split("\n")[0].strip()
    receipt = json.loads(first_line)
    assert receipt["name"] == "myagent-c"
    assert receipt["harness"] == "claude"
    # AC5: no -P on this spawn -> provider (vendor) and model are absent, not
    # defaulted to the harness. A provider key holding "claude" is the defect.
    assert "provider" not in receipt
    assert "model" not in receipt
    assert receipt["status"] == "live"
    assert "short_id" in receipt
    # jq .short_id must work (i.e. it's a plain string value)
    assert isinstance(receipt["short_id"], str)
    assert len(receipt["short_id"]) == 8

    # Verify the exact format: hand-rolled, keys in order name/short_id/harness/status.
    # No -P -> provider/model absent (AC5); harness carries the harness literal.
    assert first_line == (
        f'{{"name": "{receipt["name"]}", "short_id": "{receipt["short_id"]}", '
        f'"harness": "claude", "status": "live"}}'
    )

    # Registry row must be present
    entries = load_registry()
    entry = next((e for e in entries if e.name == "myagent-c"), None)
    assert entry is not None, "registry row must exist after claude spawn"
    assert entry.harness == "claude"
    assert entry.short_id == receipt["short_id"]


def test_spawn_refuses_a_payload_whose_verb_the_shell_ate(workdir) -> None:
    """zsh turns a double-quoted `$fno:target x-1` into `arget x-1`. The spawn
    refuses it before launching anything, instead of seeding a worker with prose."""
    from fno.agents.cli import agents_app

    result = _make_runner().invoke(
        agents_app,
        ["spawn", "--name", "eaten-c", "-H", "codex", "arget x-1"],
        catch_exceptions=False,
    )

    assert result.exit_code == 2, result.output
    assert "Single-quote the payload: '$fno:target ...'" in result.output
    assert not any(e.name == "eaten-c" for e in load_registry())


# ---------------------------------------------------------------------------
# codex plain spawn (no --once): Rust runtime is required
# ---------------------------------------------------------------------------


def test_spawn_codex_plain_no_once_requires_runtime(workdir, monkeypatch) -> None:
    """Codex thread spawn refuses on a missing runtime before any dispatch.

    The gate is a binary verb now: with no fno-agents on the machine the gate
    transport cannot even be asked, and the spawn refuses fail-closed at the
    gate (87, gate_unavailable) instead of reaching the lane's own exit-13
    check. 13 remains the lane's own code for a readable gate whose worker
    binary is missing.
    """
    from fno import rust_binary
    from fno.agents.cli import agents_app

    # A smoke run preserves a real fno-agents for the claim door and may leave
    # it reachable; this test pins the missing-runtime refusal, so force the
    # fallback by stubbing both resolvers.
    monkeypatch.setattr(rust_binary, "resolve_binary", lambda: None)
    monkeypatch.setattr(rust_binary, "resolve_installed_binary", lambda: None)
    monkeypatch.setattr(rust_binary, "find_dev_binary", lambda: None)

    runner = _make_runner()
    result = runner.invoke(
        agents_app,
        ["spawn", "--name", "ptagent", "-H", "codex", "hello"],
    )

    assert result.exit_code == 87, (
        f"expected exit 87 for a codex spawn with no runtime at all, got {result.exit_code}\n"
        f"output: {result.output}"
    )
    assert "gate_unavailable" in result.output
    assert "fno-agents" in result.output


def test_spawn_thread_refusal_renders_from_accept_set(monkeypatch) -> None:
    from fno.agents import dispatch

    from fno import harness_names

    dispatch._check_spawn_harness("opencode")
    # Patch the ONE cache the message builder reads. The spawn set is
    # roster-served now, so patching dispatch's imported binding would prove
    # nothing about where the rendered text comes from; the module reads the
    # _ROSTER cache through _roster() on every refusal.
    monkeypatch.setattr(
        harness_names,
        "_ROSTER",
        {
            "known": harness_names.known_harnesses(),
            "providers": harness_names.known_providers(),
            "spawn": (*harness_names._roster("spawn"), "future"),
        },
    )

    with pytest.raises(dispatch.DispatchAskError) as caught:
        # A DECLARED harness outside the roster's spawn set renders the accept
        # list; an undeclared name takes the undeclared arm instead, so the
        # set is asserted through a declared pane-only harness (gemini; pi and
        # agy sit in the set by their contract rows).
        dispatch._check_spawn_harness("gemini")

    expected = ", ".join(harness_names._roster("spawn"))
    assert f"accepted here: {expected}" in str(caught.value)
    assert expected.endswith("future"), "the patched roster must be rendered"
    # The pane sentence derives too: it names the refused harness, never a
    # hardcoded roster that the set can outgrow.
    assert "gemini has no measured thread lane yet; use --substrate pane." in str(caught.value)


def test_spawn_pi_passes_the_seam_on_both_lanes_and_unmeasured_headless_refuses(
    tmp_path: Path,
) -> None:
    """x-43bd arm 1: the seam is substrate-aware. pi passes on thread (its
    keeper lane is journey-proven) and on headless (measured by running it),
    while a harness whose headless stance reads `unmeasured` (grok) refuses,
    naming the stance - not the old `unknown harness` contradiction. The
    Python spawn path has no pi one-shot, so it names the Rust lane instead
    of falling to the retired-harness error."""
    from fno.agents import dispatch

    # Positive marker: the accepted name RETURNS rather than raising.
    dispatch._check_spawn_harness("pi", headless=False)
    dispatch._check_spawn_harness("pi")
    dispatch._check_spawn_harness("pi", headless=True)

    with pytest.raises(dispatch.DispatchAskError, match="Rust runtime's headless lane"):
        dispatch.dispatch_spawn("pi-once", "hi", "pi", tmp_path, headless=True)

    with pytest.raises(dispatch.DispatchAskError) as caught:
        dispatch._check_spawn_harness("grok", headless=True)
    message = str(caught.value)
    assert "unmeasured" in message
    assert "headless" in message
    assert "unattended journey" in message
    assert "unknown harness" not in message


def test_spawn_agy_headless_refuses_by_name(workdir, monkeypatch) -> None:
    from fno.agents import dispatch

    monkeypatch.setattr(
        dispatch,
        "_lane_b_thread_spawn",
        lambda **kw: pytest.fail("headless must never reach the keeper lane"),
    )
    with pytest.raises(dispatch.DispatchAskError) as caught:
        dispatch.dispatch_spawn(
            name="wkagyh", message="hello", harness="agy", cwd=workdir, headless=True
        )
    message = str(caught.value)
    assert "unmeasured" in message
    assert "a bare spawn" in message
    assert "no measured thread lane" not in message, (
        "the merged measurement disproved that sentence for agy"
    )


def test_spawn_agy_thread_branch_drives_the_keeper_lane(workdir, monkeypatch) -> None:
    """`dispatch_spawn -H agy --substrate thread` reaches
    `_lane_b_thread_spawn` and returns its minted conversation id - the
    refusal text `unknown harness 'agy' on the thread substrate` never
    renders. The seed rides the keeper paste, keyed to that id and agy's own
    composer-ready marker."""
    from fno.agents import dispatch

    calls: list[dict] = []
    seeds: list[dict] = []

    def _fake_lane_b(**kwargs):
        calls.append(kwargs)
        return {
            "name": kwargs["name"],
            "harness": kwargs["harness"],
            "session_id": "c5661b28-bcba-4690-8b2e-4a4a88541e8c",
            "keeper_socket": "/tmp/does-not-matter.sock",
            "keeper_pid": 1,
            "child_pid": 2,
            "argv": ["agy"],
        }

    def _fake_seed(*, name, session_id, sock, message, ready_marker, clear_modal):
        seeds.append(
            {
                "session_id": session_id,
                "message": message,
                "marker": ready_marker,
                "modal": clear_modal,
            }
        )

    monkeypatch.setattr(dispatch, "_lane_b_thread_spawn", _fake_lane_b)
    monkeypatch.setattr(dispatch, "_keeper_seed_submit", _fake_seed)

    result = dispatch.dispatch_spawn(
        name="wkagy", message="hello", harness="agy", cwd=workdir
    )

    assert result.kind == "created"
    assert result.provider == "agy"
    assert result.short_id == "c5661b28-bcba-4690-8b2e-4a4a88541e8c"
    assert len(calls) == 1, "the agy branch must drive the keeper lane exactly once"
    assert calls[0]["harness"] == "agy"
    assert len(seeds) == 1, f"exactly one seed paste, got {seeds!r}"
    assert seeds[0]["session_id"] == "c5661b28-bcba-4690-8b2e-4a4a88541e8c"
    assert seeds[0]["message"].startswith("hello")
    assert seeds[0]["marker"] == b"? for shortcuts"
    # A keeper has nobody to answer agy's folder-trust modal, and a TUI behind
    # one runs nothing while holding a live row, so the seed carries the answer.
    assert seeds[0]["modal"] is not None
    pattern, keys = seeds[0]["modal"]
    assert re.search(pattern, "Do you trust the contents of this project?", re.I)
    assert keys == b"\r"


def test_undeclared_harness_thread_and_headless_refuse_pane_only() -> None:
    """AC5-ERR: thread AND headless both refuse an undeclared harness, naming
    it, saying it declares no capability row, and naming pane as its only
    substrate - never gemini's retirement, never agy the successor."""
    from fno.agents import dispatch

    for kwargs in ({}, {"once": True}):
        with pytest.raises(dispatch.DispatchAskError) as caught:
            dispatch.dispatch_spawn(
                name="peer",
                message="hi",
                harness="nanoclaw",
                cwd="/tmp",
                **kwargs,
            )
        msg = str(caught.value)
        assert caught.value.exit_code == 2
        assert "nanoclaw" in msg
        assert "no capability row" in msg
        assert "pane" in msg
        assert "only lane" in msg
        assert "gemini" not in msg
        assert "agy" not in msg


# ---------------------------------------------------------------------------
# opencode bg: delegation to the Rust serve lane
# ---------------------------------------------------------------------------


def test_spawn_opencode_bg_delegates_to_serve_lane(workdir, monkeypatch) -> None:
    """A node-bearing opencode bg spawn reaches the serve lane.

    --node forces spawn onto the Python parser (x-84a8: the Rust client cannot
    parse provenance flags), and before this arm the provider fell through to
    the retired-gemini refusal. The helper is stubbed; the real lane is proven
    live by the Rust journey test."""
    from fno.agents import dispatch as dispatch_mod
    from fno.agents.cli import agents_app

    calls: list[dict] = []

    def _fake_serve(**kwargs):
        calls.append(kwargs)
        return "ses_deleg1"

    monkeypatch.setattr(dispatch_mod, "_opencode_serve_spawn", _fake_serve)
    monkeypatch.setattr(
        "fno.graph.store.read_nodes_by_ids",
        lambda path, tokens: {"entries": [{"id": "x-abcd", "slug": "serve", "dispatch_verb": "/target", "difficulty": "low"}], "missing": []},
    )
    # The composed node seed is target-family; this machine may lack the
    # opencode loop extension. The gate has its own test file.
    monkeypatch.setattr(
        "fno.agents.harness_map.check_loop_participation", lambda *a, **k: None
    )

    runner = _make_runner()
    result = runner.invoke(
        agents_app,
        [
            "spawn",
            "--name",
            "wkoc",
            "-H",
            "opencode",
            "--node",
            "x-abcd",
            "--cwd",
            str(workdir),
            "--session-phase",
            "do",
            "run the node",
        ],
    )

    assert result.exit_code == 0, (
        f"expected exit 0, got {result.exit_code}\noutput: {result.output}"
    )
    assert "retired" not in result.output
    assert calls, "serve delegation never ran"
    assert calls[0]["name"] == "wkoc"
    assert "run the node" in calls[0]["message"]
    receipt = json.loads(_receipt_line(result.output))
    assert receipt["short_id"] == "ses_deleg1"
    assert receipt["harness"] == "opencode"


def test_spawn_opencode_bg_resume_refused(workdir) -> None:
    """Resume over the serve API is a filed follow-up; the spawn refuses
    instead of pretending to resume."""
    from fno.agents.cli import agents_app

    runner = _make_runner()
    result = runner.invoke(
        agents_app,
        [
            "spawn",
            "--name",
            "wkoc",
            "-H",
            "opencode",
            "--resume",
            "ses_old123",
            "--cwd",
            str(workdir),
            "hello",
        ],
    )

    assert result.exit_code == 2, (
        f"expected exit 2, got {result.exit_code}\noutput: {result.output}"
    )
    assert "--resume" in result.output


def test_opencode_serve_spawn_without_binary_exits_13(workdir, monkeypatch) -> None:
    """No fno-agents runtime on the box -> the same exit-13 shape as codex
    plain spawn, naming the pane escape."""
    from fno import rust_binary
    from fno.agents import dispatch as dispatch_mod
    from fno.agents.dispatch import DispatchAskError, _opencode_serve_spawn

    monkeypatch.setattr(rust_binary, "resolve_binary", lambda: None)
    with pytest.raises(DispatchAskError) as exc_info:
        _opencode_serve_spawn(
            name="wkoc",
            message="hello",
            cwd=workdir,
            from_name="",
        )
    assert exc_info.value.exit_code == 13
    assert "pane lane (--split)" in str(exc_info.value)
    assert dispatch_mod._opencode_serve_spawn is _opencode_serve_spawn


def test_keeper_lane_child_env_never_inherits_the_spawn_trigger(
    workdir, monkeypatch
) -> None:
    """The keeper lane captures the spawn cause BEFORE its env snapshot: a
    machine dispatch routed through a keeper thread must leave no
    FNO_SPAWN_TRIGGER in the worker's ambient env, or the worker's own ask
    events mislabel as dispatcher and its own spawns inherit the cause."""
    from fno.agents import dispatch

    monkeypatch.setattr(dispatch, "_lane_b_worker_binary", lambda: Path("/fake/worker"))
    monkeypatch.setattr(dispatch, "_mint_thread_session_id", lambda *a, **k: "minted-1")
    monkeypatch.setattr(
        "fno.agents.harness_map.render_session_argv", lambda *a, **k: ["pi"]
    )
    monkeypatch.setattr("fno.agents.spawn_axes_client.keeper_posture", lambda *a, **k: [])

    captured: dict = {}

    class _Boom(Exception):
        pass

    real_popen = dispatch.subprocess.Popen

    def _fake_popen(*args, **kwargs):
        if "env" not in kwargs:  # the registry door, not the keeper launch
            return real_popen(*args, **kwargs)
        captured["env"] = kwargs["env"]
        raise _Boom("stop before the keeper starts")

    monkeypatch.setattr(dispatch.subprocess, "Popen", _fake_popen)
    monkeypatch.setenv("FNO_SPAWN_TRIGGER", "dispatch:ac")

    with pytest.raises(_Boom):
        dispatch._lane_b_thread_spawn(
            name="probe-trigger", harness="pi", cwd=str(workdir), lock_timeout=1.0
        )

    assert "FNO_SPAWN_TRIGGER" not in captured["env"]


@pytest.fixture(autouse=True)
def _python_door(monkeypatch):
    """These tests pin the PYTHON door's receipts and refusals. The axis
    flags that used to route their argvs to Python are retired, so an argv
    with no Python-only marker would exec the Rust client instead; the
    sanctioned fallback pins the door under test. The gate stays live."""
    monkeypatch.setenv("FNO_AGENTS_RUNTIME", "python")
