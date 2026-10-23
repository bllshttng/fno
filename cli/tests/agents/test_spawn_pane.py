"""Tests for the mux-pane spawn back half (4a-G2, task 4.5).

``fno agents spawn --substrate pane`` hosts the agent as a mux pane via
``fno mux pane run`` and writes the registry row with the ``mux`` ref. The
mux subprocess is faked at the ``runner`` seam (the G1 e2e drives the real
socket); these tests pin the Python contract:

- AC1-HP  spawn -> `pane run --session --cwd -- env <mesh> <argv>`; row
          carries mux:{session, pane_id} + claude_session_uuid + child pid.
- AC1-ERR mux failure -> no half-created row, error names the mux session,
          no daemon fallback.
- AC1-FR  a claude argv carrying -p/--print is refused BEFORE any pane run.
- AC1-EDGE is Rust-side (pane run self-spawns the server; G1 e2e covers it).
- Routing: pane-substrate spawns never auto-route to the Rust client.
"""
from __future__ import annotations

import json
import os
import subprocess
from pathlib import Path
from typing import Optional

from types import SimpleNamespace

import pytest

from fno.paths_testing import use_tmpdir
from fno.agents.mux_spawn import MuxSpawnResult
from tests.agents._fake_claude import stub_codex_sandbox_probe  # noqa: F401  re-exported for test_spawn_pane_codex_receipt


from tests.agents._route_overlay_stub import claude_route_overlay_stub


@pytest.fixture(autouse=True)
def _graphless_seed_resolver(monkeypatch):
    """The seed-word gate fails open on an unreadable graph; pin that here so
    the plumbing tests' fake node words never depend on the ambient store."""

    def _unreadable(path=None, **_):
        raise ValueError("graph store unreadable")

    monkeypatch.setattr("fno.graph.load.load_graph", _unreadable)


CODEX_HARNESS = "codex"


class FakeRunner:
    """Record `fno mux ...` invocations; script the replies per command."""

    def __init__(
        self,
        run_returncode: int = 0,
        run_stdout: str = "7\n",
        run_stderr: str = "",
        ls_stdout: Optional[str] = None,
        db_stdout: str = "",
        # x-6928 interactive-readiness gate probes.
        wait_returncode: int = 11,
        read_stdout: str = "ready\n",
        read_returncode: int = 0,
        read_stderr: str = "",
        placement: Optional[dict] = None,
        tabs_stdout: Optional[str] = None,
        tabs_returncode: int = 0,
        tabs_stderr: str = "",
        tab_stdout: str = "[]",
        tab_create_stdout: str = '{"tab_id": 12}',
        kill_returncode: int = 0,
        kill_stderr: str = "",
        kill_exception: Optional[Exception] = None,
        kill_removes_pane: bool = False,
    ) -> None:
        self.calls: list[list[str]] = []
        self.run_returncode = run_returncode
        self.run_stdout = run_stdout
        self.run_stderr = run_stderr
        self.ls_stdout = ls_stdout
        self.db_stdout = db_stdout
        self.wait_returncode = wait_returncode
        self.read_stdout = read_stdout
        self.read_returncode = read_returncode
        self.read_stderr = read_stderr
        self.placement = placement
        self.tabs_stdout = tabs_stdout
        self.tabs_returncode = tabs_returncode
        self.tabs_stderr = tabs_stderr
        self.tab_stdout = tab_stdout
        self.tab_create_stdout = tab_create_stdout
        self.kill_calls: list[list[str]] = []
        self.kill_returncode = kill_returncode
        self.kill_stderr = kill_stderr
        self.kill_exception = kill_exception
        self.kill_removes_pane = kill_removes_pane

    def __call__(self, argv, **kwargs):
        self.calls.append(list(argv))
        if len(argv) > 1 and argv[1] == "manifest-eval":
            harness = argv[argv.index("--harness") + 1]
            title = argv[argv.index("--osc-title") + 1] if "--osc-title" in argv else ""
            if harness == "claude" and title.startswith("⠋"):
                payload = {
                    "matched": True, "rule_id": "osc_title_working", "state": "working",
                }
            else:
                rule = {"claude": "live_prompt_box", "codex": "idle_prompt"}.get(harness)
                payload = {"matched": bool(rule), "rule_id": rule, "state": "idle"}
            return subprocess.CompletedProcess(argv, 0, json.dumps(payload), "")
        # The opencode spawn path reads opencode's session store through this
        # same seam (x-830c); default empty output = "no session captured", the
        # live-only row every non-opencode test already expects.
        if argv[:2] == ["opencode", "db"]:
            return subprocess.CompletedProcess(argv, 0, self.db_stdout, "")
        if argv[1:4] == ["mux", "tab", "ls"]:
            return subprocess.CompletedProcess(
                argv, self.tabs_returncode,
                self.tabs_stdout if self.tabs_stdout is not None else self.tab_stdout,
                self.tabs_stderr,
            )
        if argv[1:4] == ["mux", "tab", "create"]:
            return subprocess.CompletedProcess(argv, 0, self.tab_create_stdout, "")
        if argv[1:4] == ["mux", "pane", "run"]:
            if "--json" in argv:
                payload = {"pane_id": 7}
                if self.placement is not None:
                    payload["placement"] = self.placement
                return subprocess.CompletedProcess(
                    argv, self.run_returncode, json.dumps(payload), self.run_stderr
                )
            return subprocess.CompletedProcess(
                argv, self.run_returncode, self.run_stdout, self.run_stderr
            )
        if argv[1:4] == ["mux", "pane", "ls"]:
            out = self.ls_stdout
            if out is None:
                out = json.dumps(
                    [{"pane_id": 7, "squad_id": 1, "tab_id": 1, "cwd": "/w", "child_pid": 4242}]
                )
            return subprocess.CompletedProcess(argv, 0, out, "")
        if argv[1:4] == ["mux", "tab", "rename"]:
            return subprocess.CompletedProcess(argv, 0, "", "")
        if argv[1:4] == ["mux", "pane", "wait"]:
            return subprocess.CompletedProcess(argv, self.wait_returncode, "", "")
        if argv[1:4] == ["mux", "pane", "read"]:
            return subprocess.CompletedProcess(
                argv, self.read_returncode, self.read_stdout, self.read_stderr
            )
        if argv[1:4] in (
            ["mux", "pane", "claim"],
            ["mux", "pane", "send"],
            ["mux", "pane", "release"],
        ):
            return subprocess.CompletedProcess(argv, 0, "", "")
        if argv[1:4] == ["mux", "pane", "kill"]:
            self.kill_calls.append(list(argv))
            if self.kill_exception is not None:
                raise self.kill_exception
            result = subprocess.CompletedProcess(
                argv, self.kill_returncode, "", self.kill_stderr
            )
            if self.kill_returncode == 0 and self.kill_removes_pane:
                self.ls_stdout = "[]"
            return result
        if argv[:4] == ["codex", "app-server", "daemon", "start"]:
            return subprocess.CompletedProcess(
                argv, 0, '{"status":"alreadyRunning"}', ""
            )
        raise AssertionError(f"unexpected fno invocation: {argv}")


def _spawn(monkeypatch, tmp_path, **kwargs):
    use_tmpdir(monkeypatch, tmp_path)
    monkeypatch.delenv("FNO_SESSION", raising=False)
    from fno import rust_binary
    from fno.agents.mux_spawn import dispatch_spawn_pane

    monkeypatch.setattr(
        rust_binary, "resolve_installed_binary", lambda: Path("/fake/fno-agents")
    )

    harness = kwargs.get("provider")
    if harness is None:
        harness = "claude"
    codex_binding = kwargs.pop("codex_binding", True)
    if harness == "codex" and codex_binding:
        import fno.agents.mux_spawn as mux_spawn

        monkeypatch.setattr(
            mux_spawn,
            "_backfill_codex_session_id",
            lambda *_args, **_kwargs: "019fb024-2327-75f3-8b80-06e9d5ade05f",
        )

    runner = kwargs.pop("runner", FakeRunner())
    provider = kwargs.pop("provider", "claude")
    name = kwargs.pop("name", "peer")
    # Routed Claude fixtures in this module are z.ai routes. Supply the vendor
    # axis explicitly so the production seam never has to infer it from env.
    if provider == "claude" and kwargs.get("route_env") is not None:
        kwargs.setdefault("route_provider", "zai")
    if kwargs.get("route_provider") is not None:
        from fno.agents.model_routing import bind_route_provider
        from fno.agents.spawn_gate import run_gate

        monkeypatch.setenv("FNO_SPAWN_GATE", "0")
        if kwargs.get("route_env") is not None:
            kwargs["route_env"] = bind_route_provider(
                kwargs["route_env"], kwargs["route_provider"]
            )
        kwargs.setdefault(
            "provider_gate",
            run_gate(name, "pane", route_provider=kwargs["route_provider"]),
        )
    result = dispatch_spawn_pane(
        name=name,
        message=kwargs.pop("message", "hello"),
        provider=provider,
        cwd=kwargs.pop("cwd", tmp_path),
        runner=runner,
        **kwargs,
    )
    return result, runner


def test_opencode_spawn_stamps_the_captured_session_id(
    tmp_path: Path, monkeypatch
) -> None:
    """x-830c: a unique store match lands on the row as harness_session_id.

    This is what makes the opencode resume lane reachable at all - the mapping
    and the resume argv both key on this field.
    """
    from fno.agents.registry import load_registry

    ses = "ses_09679f284ffeJv7NdBAoLQLnLZ"
    result, _ = _spawn(
        monkeypatch, tmp_path,
        provider="opencode",
        runner=FakeRunner(db_stdout=f"id\n{ses}\n"),
    )
    rows = load_registry()
    assert [r.harness_session_id for r in rows] == [ses]
    # US8: opencode resumes off harness_session_id, so short_id stays empty -
    # the jobId population is claude-only. Assert BOTH the row and the
    # receipt-facing result: a regressed claude guard would otherwise hand out a
    # non-hex `ses_0967` result handle while the row-only check still passed.
    assert rows[0].short_id == ""
    assert result.short_id == ""


def test_codex_spawn_without_capture_fails_and_reaps_unaddressable_pane(
    tmp_path: Path, monkeypatch
) -> None:
    """A required Codex binding never returns an unusable successful spawn."""
    from fno.agents.dispatch import DispatchAskError
    from fno.agents.registry import load_registry

    runner = FakeRunner()
    with pytest.raises(DispatchAskError, match="session binding.*reaped"):
        _spawn(
            monkeypatch, tmp_path, provider="codex", runner=runner,
            codex_binding=False,
        )
    assert load_registry() == []
    assert runner.kill_calls


def test_ac1_hp_spawn_pane_runs_mux_and_writes_mux_ref_row(
    no_state_grant: None, tmp_path: Path, monkeypatch
) -> None:
    result, runner = _spawn(monkeypatch, tmp_path)

    # The hosting call is the G1 script API with the resolved session + cwd.
    run_call = runner.calls[0]
    assert run_call[1:4] == ["mux", "pane", "run"]
    assert "--claim" in run_call  # agent panes opt into the writer claim
    assert run_call[run_call.index("--server") + 1] == "main"
    assert run_call[run_call.index("--cwd") + 1] == str(tmp_path)
    # Mesh identity rides the env(1) wrapper after `--`.
    tail = run_call[run_call.index("--") + 1 :]
    assert tail[0] == "env"
    assert "FNO_AGENT_SELF=peer" in tail
    assert "FNO_AGENT_HARNESS=claude" in tail
    assert "FNO_HARNESS_NAME=claude" in tail
    assert f"FNO_HARNESS_SESSION_ID={result.session_uuid}" in tail
    assert "CLAUDE_CODE_FORCE_SESSION_PERSISTENCE=1" in tail
    # The provider argv is interactive claude with the pinned session id and the
    # worker's own display name (x-c028: without `--name` claude inherits one
    # from the launching session's lineage and every pane worker looks alike).
    claude_at = tail.index("claude")
    assert tail[claude_at + 1] == "--session-id"
    assert tail[claude_at + 2] == result.session_uuid
    assert tail[claude_at + 3] == "--name"
    assert tail[claude_at + 4] == "peer"
    # The seed rides behind its own `--` fence: a leading-flag seed must reach
    # claude as the prompt positional, never its flag parser.
    assert tail[claude_at + 5] == "--"
    seed = tail[claude_at + 6]
    assert seed.startswith("hello\n\n")
    assert seed.count("<fno_relay_compression>") == 1

    assert result.pane_id == 7
    assert result.session == "main"
    assert result.child_pid == 4242

    from fno.agents.registry import load_registry

    rows = load_registry()
    assert len(rows) == 1
    row = rows[0]
    assert row.mux == {"session": "main", "pane_id": 7}
    assert row.harness_session_id == result.session_uuid
    assert row.pid == 4242
    assert row.status == "live"
    # The row keeps short_id empty: it is the worker/bg transport slot, and a mux
    # row holds exactly one live ref (validate_single_live_ref).
    assert row.short_id == ""
    # The pane receipt carries the generated mailbox handle, not a provider
    # transport job id, so the caller can mail the pane from the receipt.
    assert result.short_id == result.session_uuid[:8]
    # That canonical handle resolves back to this exact row.
    from fno.agents.registry import resolve_agent_in

    resolved = resolve_agent_in(rows, result.short_id)
    assert resolved.entry.name == "peer"
    assert resolved.matched_by == "canonical_handle"


def test_ac1_err_mux_failure_leaves_no_row_and_names_session(
    tmp_path: Path, monkeypatch
) -> None:
    from fno.agents.dispatch import DispatchAskError

    with pytest.raises(DispatchAskError) as exc_info:
        _spawn(
            monkeypatch,
            tmp_path,
            runner=FakeRunner(run_returncode=1, run_stdout="", run_stderr="no pty"),
        )
    msg = str(exc_info.value)
    assert "'main'" in msg, f"error must name the mux session: {msg}"
    assert "no pty" in msg
    assert "fallback" in msg  # explicitly no daemon-PTY fallback

    from fno.agents.registry import load_registry

    assert load_registry() == [], "a failed spawn must not leave a half-created row"


def test_collision_refused_before_any_pane_spawn(tmp_path: Path, monkeypatch) -> None:
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents.dispatch import DispatchAskError
    from fno.agents.mux_spawn import dispatch_spawn_pane
    from fno.agents.registry import AgentEntry, write_registry

    write_registry(
        [AgentEntry(name="peer", harness="claude", cwd="/p", log_path="/l")]
    )
    runner = FakeRunner()
    with pytest.raises(DispatchAskError, match="already exists") as exc_info:
        dispatch_spawn_pane(
            name="peer", message="", provider="claude", cwd=tmp_path, runner=runner
        )
    assert exc_info.value.exit_code == 2
    assert runner.calls == [], "collision must refuse before any mux subprocess"


def test_ac1_fr_billing_guard_refuses_print_argv_before_pane(
    tmp_path: Path, monkeypatch
) -> None:
    use_tmpdir(monkeypatch, tmp_path)
    import fno.agents.mux_spawn as mux_spawn

    # The builder never emits -p by construction; force it to prove the guard
    # sits between argv resolution and the pane spawn.
    monkeypatch.setattr(
        mux_spawn, "build_pane_argv", lambda *a, **k: ["claude", "-p", "hi"]
    )
    runner = FakeRunner()
    from fno.agents.dispatch import DispatchAskError

    with pytest.raises(DispatchAskError, match="-p/--print"):
        mux_spawn.dispatch_spawn_pane(
            name="peer", message="hi", provider="claude", cwd=tmp_path, runner=runner
        )
    assert runner.calls == [], "the guard must fire BEFORE any pane exists"

    # The predicate itself, both spellings.
    assert not mux_spawn.claude_argv_is_interactive(["claude", "-p"])
    assert not mux_spawn.claude_argv_is_interactive(["claude", "--print", "x"])
    assert mux_spawn.claude_argv_is_interactive(["claude", "--session-id", "u", "msg"])


def test_build_pane_argv_provider_forms(no_state_grant: None, tmp_path: Path) -> None:
    from fno.agents.mux_spawn import build_pane_argv

    claude = build_pane_argv("claude", "task", tmp_path, "uuid-1")
    assert claude == ["claude", "--session-id", "uuid-1", "--", "task"]

    codex = build_pane_argv("codex", "task", tmp_path, None)
    assert codex[:5] == ["codex", "--remote", "unix://", "-C", str(tmp_path)]
    assert "--sandbox" in codex and codex[-1] == "task"
    # The codex seed rides behind clap's own end-of-options fence.
    assert codex[-2] == "--"

    gemini = build_pane_argv("gemini", "task", tmp_path, None)
    assert gemini[:2] == ["gemini", "--skip-trust"]
    assert "-i" in gemini
    # Bare interactive session: no -i without a message.
    assert "-i" not in build_pane_argv("gemini", "", tmp_path, None)

    # agy is never-prompt and stateless. The seed is delivered after readiness.
    agy = build_pane_argv("agy", "task", tmp_path, "ignored-uuid")
    assert agy == ["agy", "--dangerously-skip-permissions"]
    assert "-p" not in agy and "--session-id" not in agy
    assert build_pane_argv("agy", "", tmp_path, None) == [
        "agy",
        "--dangerously-skip-permissions",
    ]

    # x-51f6 US2: bare `opencode` is the TUI; the message rides --prompt in
    # the EQUAL form (yargs misparses the split form on a flag-shaped value;
    # the positional is a PROJECT PATH, not a prompt), and never the headless `run` subcommand.
    # x-c772: opencode is always launched with a model (the valid
    # zai-coding-plan/glm-5.3 default).
    opencode = build_pane_argv("opencode", "task", tmp_path, "ignored")
    assert opencode == ["opencode", "--prompt=task", "--model", "zai-coding-plan/glm-5.3"]
    assert build_pane_argv("opencode", "", tmp_path, None) == [
        "opencode",
        "--model",
        "zai-coding-plan/glm-5.3",
    ]
    assert "run" not in opencode and "--session-id" not in opencode


def test_generic_pane_arm_hosts_an_undeclared_harness(
    no_state_grant: None, tmp_path: Path
) -> None:
    """AC4-HP: a harness with no capability row builds the VIEWPORT argv - the
    bare binary plus the operator's own `--` passthrough. No identity flag, no
    model, no effort, no permission mapping: each is a per-vendor spelling and
    inventing one is the guess this lane exists to refuse. The seed stays OUT
    of argv so `_submit_spawn_seed` types it after readiness (the agy path)."""
    from fno.agents.mux_spawn import build_pane_argv, seed_rode_in_argv

    argv = build_pane_argv(
        "nanoclaw", "say ready", tmp_path, False, None, passthrough=["--foo", "bar"]
    )
    assert argv == ["nanoclaw", "--foo", "bar"]
    assert argv[0] == "nanoclaw"
    assert seed_rode_in_argv("say ready", argv) is False
    assert build_pane_argv("nanoclaw", "", tmp_path, False, None) == ["nanoclaw"]


def test_build_pane_argv_normalizes_direct_slash_commands(tmp_path: Path) -> None:
    """Direct ``fno agents spawn`` calls bypass resolve_dispatch.

    The pane argv builder is therefore a load-bearing normalization choke point,
    including for the advertised plugin-qualified command spelling.
    """
    from fno.agents.mux_spawn import build_pane_argv

    assert build_pane_argv(
        "codex", "/fno:target x-81ad", tmp_path, False, None
    )[-1] == "$fno:target x-81ad"
    assert build_pane_argv(
        "opencode", "/fno:target x-81ad", tmp_path, False, None
    )[1] == "--prompt=/fno:target x-81ad"
    assert build_pane_argv(
        "claude", "/fno:target x-81ad", tmp_path, False, "uuid"
    )[-1] == "/fno:target x-81ad"



@pytest.fixture
def no_state_grant(monkeypatch: pytest.MonkeyPatch) -> None:
    """Neutralize fno's computed writable-dir grant for exact-argv assertions.

    The grant's CONTENT is machine-dependent (the resolved state root, and the
    vault only when this project's plans live under one), so a test comparing a
    literal argv cannot carry it. Tests about the grant itself assert it by name;
    these assert provider argv FORM, which is a different axis.
    """
    monkeypatch.setattr(
        "fno.agents.mux_spawn.worker_writable_dirs", lambda *a, **k: []
    )


def test_build_pane_argv_codex_strip_drops_the_user_grant_too(tmp_path: Path) -> None:
    """x-0a75, composition axis inverted: nothing composes on the codex pane
    any more, including a grant the operator typed. The spawn must still reach
    ready on the seeded pane, so the strip removes the pair whole and keeps
    the seed behind `--`."""
    from fno.agents.mux_spawn import build_pane_argv

    repo = _pane_repo(tmp_path)
    argv = build_pane_argv(
        "codex", "seed text", repo, False, None, passthrough=["--add-dir", "/extra"]
    )

    assert argv.count("--add-dir") == 0
    assert "/extra" not in argv
    assert argv[-2:] == ["--", "seed text"]


def _stub_harness_bin(monkeypatch, tmp_path: Path, name: str) -> Path:
    """A stub CLI binary on a tmp PATH under a name no roster knows."""
    bin_dir = tmp_path / "stub-bin"
    bin_dir.mkdir(exist_ok=True)
    stub = bin_dir / name
    stub.write_text("#!/bin/sh\nsleep 60\n")
    stub.chmod(0o755)
    monkeypatch.setenv("PATH", str(bin_dir))
    return stub


def test_undeclared_token_shape_refused_before_any_path_lookup(
    tmp_path: Path, monkeypatch
) -> None:
    """AC10-ERR: a malformed harness token refuses naming the token, before
    shutil.which and before any argv is composed - a shell-hostile string never
    reaches a PATH lookup."""
    import shutil

    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents.dispatch import DispatchAskError
    from fno.agents.mux_spawn import dispatch_spawn_pane

    def _boom(_name):
        raise AssertionError("shutil.which ran for a malformed harness token")

    monkeypatch.setattr(shutil, "which", _boom)
    for bad in ("", "9goose", "Goose", "bad name", "bad/name", "a" * 33):
        with pytest.raises(DispatchAskError) as caught:
            dispatch_spawn_pane(
                name="peer", message="", provider=bad, cwd=tmp_path, runner=FakeRunner()
            )
        assert caught.value.exit_code == 2, bad
        assert bad in str(caught.value) or not bad, bad
        assert "PATH lookup" in str(caught.value), bad


def test_undeclared_harness_not_on_path_refused_naming_path(
    tmp_path: Path, monkeypatch
) -> None:
    """AC7-ERR: a well-formed name with no binary on PATH refuses naming PATH
    and the resolved name, exit 2, before any mux pane is created."""
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents.dispatch import DispatchAskError
    from fno.agents.mux_spawn import dispatch_spawn_pane

    empty_bin = tmp_path / "empty-bin"
    empty_bin.mkdir()
    monkeypatch.setenv("PATH", str(empty_bin))
    runner = FakeRunner()
    with pytest.raises(DispatchAskError) as caught:
        dispatch_spawn_pane(
            name="peer", message="", provider="notonpath", cwd=tmp_path, runner=runner
        )
    msg = str(caught.value)
    assert caught.value.exit_code == 2
    assert "notonpath" in msg and "PATH" in msg
    assert runner.calls == [], "the refusal must fire before any pane exists"


def test_undeclared_spawn_journey_states_the_lane(
    tmp_path: Path, monkeypatch, capsys
) -> None:
    """AC1/AC9 journey, positive markers only: the registry row exists and
    carries name+harness, the posture answers declared=False BY NAME, and the
    receipt carries the UNDECLARED block - what the lane gives, what it
    withholds, and submit_keys=enter named as an unmeasured default. AC3's
    pane-to-Drive step (the [harness.<name>.attach] declaration reaching
    thread_reach == Drive with no code change) is proven Rust-side by
    crates/fno/tests/attach_override_ac1.rs against a made-up name; this
    change must not touch crates/."""
    _stub_harness_bin(monkeypatch, tmp_path, "nanoclaw")
    result, runner = _spawn(monkeypatch, tmp_path, provider="nanoclaw", name="clawwork")
    assert runner.calls[0][1:4] == ["mux", "pane", "run"]
    # No capability row means no pinned readiness marker, and the seed rode in
    # no argv, so it must be typed and submitted after the probe.
    assert result.readiness == "live"
    assert result.seed == "submitted"

    from fno.agents.harness_map import capabilities_or_undeclared

    assert capabilities_or_undeclared("nanoclaw")["declared"] is False

    from fno.agents.registry import load_registry, resolve_agent_in

    rows = load_registry()
    assert len(rows) == 1
    assert rows[0].name == "clawwork"
    assert rows[0].harness == "nanoclaw"
    # Addressable by name - the mail lane's resolution for a row with no
    # canonical handle.
    resolved = resolve_agent_in(rows, "clawwork")
    assert resolved.entry.name == "clawwork"

    receipt = capsys.readouterr().err
    for marker in (
        "nanoclaw",
        "UNDECLARED",
        "submit_keys=enter",
        "[harness.nanoclaw.attach]",
        "thread lane",
        "steering",
        "ask",
    ):
        assert marker in receipt, f"receipt must state {marker!r}:\n{receipt}"


def test_mesh_env_wrapper_scrubs_inherited_session_identity() -> None:
    """AC6-HP: a spawned child inherits its parent's ROUTE but never its
    parent's IDENTITY. An ambient session marker riding through this seam is
    exactly how a claude worker spawned from a codex parent comes to carry a
    foreign CODEX_THREAD_ID and resolves as the wrong harness. Each harness
    re-mints its own marker for the child, so scrubbing the inherited set is
    lossless.

    Every name in AMBIENT_IDENTITY_ENV must be unset (the direct-read legacy
    markers like CLAUDECODE_SESSION_ID are included, not just the resolver
    tuple), while a routing var survives untouched.
    """
    from fno.agents.mux_spawn import _mesh_env_wrapper
    from fno.harness_identity import AMBIENT_IDENTITY_ENV

    wrapper = _mesh_env_wrapper(
        "child-1",
        "claude",
        role=None,
        argv=["claude", "--print", "hi"],
        account_env={"CLAUDE_CONFIG_DIR": "/acct"},
        route_env={"ANTHROPIC_API_KEY": "sk-route"},
    )
    assert wrapper[0] == "env"
    unset_names = {
        wrapper[i + 1] for i, token in enumerate(wrapper) if token == "-u"
    }
    # Every ambient identity name is unset, whatever the harness family.
    for name in AMBIENT_IDENTITY_ENV:
        assert name in unset_names, f"identity marker {name} not scrubbed"

    # No identity name is re-assigned to the child.
    assignments = {
        token.split("=", 1)[0]
        for token in wrapper
        if isinstance(token, str) and "=" in token and not token.startswith("-")
    }
    # Two names ARE re-assigned, with the wrapper's own values rather than the
    # parent's: the harness this pane runs, and the substrate it runs on. The
    # scrub above is what keeps them the wrapper's to state.
    assert "FNO_HARNESS_NAME=claude" in wrapper
    assert "FNO_AGENT_SUBSTRATE=pane" in wrapper
    re_minted = {"FNO_HARNESS_NAME", "FNO_AGENT_SUBSTRATE"}
    for name in AMBIENT_IDENTITY_ENV:
        if name in re_minted:
            continue
        assert name not in assignments, f"identity marker {name} re-exported"

    # Routing survives the scrub: route and account vars are untouched.
    assert "ANTHROPIC_API_KEY=sk-route" in wrapper
    assert "CLAUDE_CONFIG_DIR=/acct" in wrapper


# ---------------------------------------------------------------------------
# x-3e38 pane placement: squad/split on the outer pane-run transport
# ---------------------------------------------------------------------------


def test_placement_directives_ride_outer_pane_run_before_separator(
    tmp_path: Path, monkeypatch
) -> None:
    # AC1-HP/AC2-HP: placement rides the OUTER pane-run argv, before the `--`
    # fencing the provider argv; build_pane_argv stays placement-blind.
    _result, runner = _spawn(monkeypatch, tmp_path, squad="review", split="left")
    run_call = runner.calls[0]
    sep = run_call.index("--")
    outer = run_call[:sep]
    assert outer[outer.index("squad") + 1] == "review"
    assert outer[outer.index("split") + 1] == "left"
    tail = run_call[sep + 1 :]
    assert "squad" not in tail and "split" not in tail
    assert "claude" in tail  # provider argv unchanged


def test_bounded_placement_sends_fit_and_skips_tab_reads(
    tmp_path: Path, monkeypatch
) -> None:
    # AC6-HP: with no explicit geometry the bounded lane sends --fit and
    # never asks the server about tabs - the server picks the tab now.
    from fno.agents.mux_spawn import dispatch_spawn_bounded_pane

    use_tmpdir(monkeypatch, tmp_path)
    post_panes = [
        {"pane_id": 7, "squad_id": 1, "tab_id": 12, "cwd": str(tmp_path), "child_pid": 4242}
    ]
    runner = FakeRunner(ls_stdout=json.dumps(post_panes))
    harness = "claude"

    dispatch_spawn_bounded_pane(
        name="bounded-fit", message="hello", provider=harness,
        cwd=tmp_path, placement_holder="test:fit",
        claims_root=tmp_path, runner=runner,
    )

    run_call = next(call for call in runner.calls if call[1:4] == ["mux", "pane", "run"])
    assert "--fit" in run_call[: run_call.index("--")]
    assert "--tab" not in run_call[: run_call.index("--")]
    assert not any(call[1:4] == ["mux", "tab", "ls"] for call in runner.calls)
    assert not any(call[1:4] == ["mux", "tab", "create"] for call in runner.calls)


def test_bounded_placement_lease_loser_spawns_nothing(tmp_path: Path, monkeypatch) -> None:
    from fno.agents.mux_spawn import DispatchAskError, dispatch_spawn_bounded_pane
    from fno.claims.core import acquire_claim

    use_tmpdir(monkeypatch, tmp_path)
    acquire_claim("placement:main:global", "other-holder", root=tmp_path)
    runner = FakeRunner()
    harness = "claude"
    with pytest.raises(DispatchAskError, match="other-holder"):
        dispatch_spawn_bounded_pane(
            name="loser", message="hello", provider=harness, cwd=tmp_path,
            placement_holder="loser-holder", claims_root=tmp_path, runner=runner,
        )
    assert runner.calls == []


@pytest.mark.parametrize(
    ("listing", "reason"),
    [
        ([{"pane_id": 7, "squad_id": 1, "tab_id": 13}], "landed in tab 13, expected tab 12"),
    ],
)
def test_bounded_post_spawn_failure_reaps_pane_and_writes_no_row(
    tmp_path: Path, monkeypatch, listing: list[dict], reason: str
) -> None:
    from fno.agents.mux_spawn import DispatchAskError
    from fno.agents.registry import load_registry

    runner = FakeRunner(ls_stdout=json.dumps(listing))
    with pytest.raises(DispatchAskError, match=reason):
        _spawn(monkeypatch, tmp_path, tab_id="id:12", runner=runner)
    assert runner.kill_calls
    assert load_registry() == []


def test_cmd_spawn_pane_uses_global_bounded_dispatch(monkeypatch) -> None:
    from typer.testing import CliRunner

    import fno.agents.cli as agents_cli
    import fno.agents.mux_spawn as mux_spawn

    captured = {}

    def fake_bounded(**kwargs):
        captured.update(kwargs)
        return MuxSpawnResult(
            name=kwargs["name"], provider=kwargs["provider"], session="main",
            pane_id=2, child_pid=None, session_uuid="claude-session",
        )

    monkeypatch.setattr(mux_spawn, "dispatch_spawn_bounded_pane", fake_bounded)
    monkeypatch.setattr(
        mux_spawn, "dispatch_spawn_pane",
        lambda **_kwargs: pytest.fail("default pane bypassed placement lease"),
    )
    monkeypatch.setenv("FNO_AGENTS_RUNTIME", "python")

    result = CliRunner().invoke(
        agents_cli.agents_app,
        ["spawn", "--name", "ordinary", "--as", "pane", "--split", "left", "do work"],
    )

    assert result.exit_code == 0, result.output
    assert captured["workspace"] is None


class SquadAwareRunner(FakeRunner):
    """A fake whose two commands DISAGREE about which squad they answer for.

    This is the whole point of the fixture. The old fake's ``pane ls`` payload
    carried ``pane_id``, ``squad_id`` and ``tab_id`` in one object and its
    ``tab ls`` stub returned a canned list correlated with nothing, so pane and
    tab agreed on squad by construction and the bug could not reproduce.

    Here the spawned pane lands in squad 1 and an UNSCOPED ``tab ls`` answers
    with squad 5's tabs - the live shape, where the pane run resolves its squad
    from the spawn's cwd and the tab commands resolve theirs from the operator's
    gaze. A resolver that reads unscoped gets squad 5's tabs for a squad-1 pane
    and places nothing where it meant to.
    """

    def __init__(self, *, squads: dict, pane_squad: int, pane_tab: int, **kw) -> None:
        super().__init__(
            ls_stdout=json.dumps(
                [
                    {
                        "pane_id": 7,
                        "squad_id": pane_squad,
                        "tab_id": pane_tab,
                        "cwd": "/w",
                        "child_pid": 4242,
                    }
                ]
            ),
            **kw,
        )
        self.squads = squads
        self.gazed_at = 5
        self.tab_ls_scopes: list[str] = []

    def __call__(self, argv, **kwargs):
        if list(argv[1:4]) == ["mux", "tab", "ls"]:
            self.calls.append(list(argv))
            if "--workspace" in argv:
                scope = argv[argv.index("--workspace") + 1]
            else:
                scope = "UNSCOPED"
            self.tab_ls_scopes.append(scope)
            if self.tabs_returncode != 0:
                return subprocess.CompletedProcess(
                    argv, self.tabs_returncode, "", self.tabs_stderr
                )
            if scope == "UNSCOPED":
                # CurrentRoute on the tab commands = the squad being LOOKED at.
                rows = self.squads.get(self.gazed_at, [])
            elif scope.startswith("id:"):
                sid = int(scope[3:])
                if sid not in self.squads:
                    return subprocess.CompletedProcess(
                        argv, 1, "", f"no such workspace id: {sid}"
                    )
                rows = self.squads[sid]
            else:
                return subprocess.CompletedProcess(
                    argv, 1, "", f"no such workspace: {scope}"
                )
            return subprocess.CompletedProcess(argv, 0, json.dumps(rows), "")
        if list(argv[1:4]) == ["mux", "tab", "join"]:
            self.calls.append(list(argv))
            return subprocess.CompletedProcess(argv, 0, "", "")
        return super().__call__(argv, **kwargs)


def _squad_fake(**kw) -> SquadAwareRunner:
    """Squad 1 holds the pane and a `codex` tab; squad 5 is what gaze answers."""
    return SquadAwareRunner(
        squads={
            1: [{"tab_id": 9, "name": "codex", "pane_ids": [1, 2, 3], "active": True}],
            5: [{"tab_id": 77, "name": "codex", "pane_ids": [50], "active": True}],
        },
        pane_squad=1,
        pane_tab=10,
        **kw,
    )


def test_pane_group_scopes_every_tab_read_to_the_pane_own_squad(
    tmp_path: Path, monkeypatch
) -> None:
    """The measured defect: `pane ls` reported panes at squad 1 while `tab ls`
    listed squad 5's tabs, so `tab join --src 67 --at 88` answered "no tab at
    index 67" for a pane id `pane ls` had just handed over. One enum value
    (`PaneTarget::CurrentRoute`) resolved by CWD on the pane path and by GAZE on
    the tab path.
    """
    import fno.agents.mux_spawn as mux_spawn

    monkeypatch.setattr(mux_spawn, "_pane_group_max", lambda: 4)
    _result, runner = _spawn(monkeypatch, tmp_path, tab="codex", runner=_squad_fake())

    assert runner.tab_ls_scopes, "the group placement must read the tab list"
    assert "UNSCOPED" not in runner.tab_ls_scopes
    assert set(runner.tab_ls_scopes) == {"id:1"}

    # And it joined squad 1's codex tab (anchored on one of ITS panes), never
    # squad 5's tab 77 that an unscoped read would have offered. The anchor
    # pane carries the squad, so the join names no --workspace of its own.
    join = next(call for call in runner.calls if call[1:4] == ["mux", "tab", "join"])
    assert join[join.index("--at") + 1] == "1"
    assert join[join.index("--src") + 1] == "id:10"
    assert "--workspace" not in join
    assert not [call for call in runner.calls if call[1:4] == ["mux", "tab", "rename"]]

    # Act, then place: no pre-spawn `--tab` prediction, and every tab read
    # happens AFTER the pane exists.
    run_call = next(call for call in runner.calls if call[1:4] == ["mux", "pane", "run"])
    assert "--tab" not in run_call
    run_at = runner.calls.index(run_call)
    tab_reads = [i for i, c in enumerate(runner.calls) if c[1:4] == ["mux", "tab", "ls"]]
    assert tab_reads and min(tab_reads) > run_at


# -- x-1499: the visible ordinal contract -------------------------------------


def test_tab_ordinal_explicit_forms_split_against_bare_names() -> None:
    """The selector/group split: every explicit form (id:, name:, ordinal:,
    digits, active, new) is a placement selector; only a bare name is a pane
    group, so ``ordinal:6`` never becomes a group named ``ordinal:6``."""
    from fno.agents.mux_spawn import _resolve_group_tab

    for selector in ("6", "ordinal:6", "id:6", "name:targets", "active", "new"):
        assert _resolve_group_tab(selector) == (selector, None)
    assert _resolve_group_tab("targets") == (None, "targets")


# ---------------------------------------------------------------------------
# _mesh_env_wrapper: a routed pane scrubs the parent's Anthropic creds (x-db50)
# ---------------------------------------------------------------------------


def test_mesh_env_wrapper_routed_pane_scrubs_anthropic_creds(monkeypatch):
    """A routed role must prefix `env -u ANTHROPIC_API_KEY -u
    CLAUDE_CODE_OAUTH_TOKEN` so a parent API key / subscription OAuth token
    cannot override the routed AUTH_TOKEN."""
    from fno.agents import mux_spawn
    from fno.agents import model_routing

    monkeypatch.setattr(
        model_routing,
        "resolve_route",
        lambda role, **kw: {
            "ANTHROPIC_BASE_URL": "https://api.z.ai/api/anthropic",
            "ANTHROPIC_AUTH_TOKEN": "zk",
            "ANTHROPIC_MODEL": "glm-5.2",
            # Every tier: the wrapper's internal lane now crosses
            # resolve_spawn_route, which refuses an endpoint without the full
            # model set.
            "ANTHROPIC_DEFAULT_OPUS_MODEL": "glm-5.2",
            "ANTHROPIC_DEFAULT_SONNET_MODEL": "glm-5.2",
            "ANTHROPIC_DEFAULT_HAIKU_MODEL": "glm-5.2",
            "ANTHROPIC_DEFAULT_FABLE_MODEL": "glm-5.2",
        },
    )
    wrapped = mux_spawn._mesh_env_wrapper("w", "claude", "coordinate", ["claude"])
    assert wrapped[0] == "env"
    # -u flags precede any KEY=VAL assignment (env parses options first).
    assert "-u" in wrapped
    ui = wrapped.index("-u")
    first_assign = next(i for i, t in enumerate(wrapped) if "=" in t)
    unset_region = wrapped[ui:first_assign]
    assert "ANTHROPIC_API_KEY" in unset_region
    assert "CLAUDE_CODE_OAUTH_TOKEN" in unset_region
    assert ui < first_assign  # unsets before assignments
    assert "ANTHROPIC_AUTH_TOKEN=zk" in wrapped


def test_mesh_env_wrapper_clears_inherited_provenance(monkeypatch):
    """A pane must not inherit its spawner's node.

    dispatch_spawn_pane hands the ambient environment to the self-spawning mux
    process, so without an explicit clear an ad-hoc pane carries the server's
    FNO_NODE and a plan-less child carries its FNO_PLAN - which ambient origin
    capture then persists into every node that pane files.
    """
    from fno.agents import mux_spawn

    adhoc = mux_spawn._mesh_env_wrapper("w", "claude", None, ["claude"])
    for key in mux_spawn.PROVENANCE_KEYS:
        assert ["-u", key] == adhoc[adhoc.index(key) - 1 : adhoc.index(key) + 1]
        assert not any(t.startswith(f"{key}=") for t in adhoc), key

    # A resolved key is set, not cleared; the unresolved rest are still cleared.
    bound = mux_spawn._mesh_env_wrapper(
        "w", "claude", None, ["claude"], provenance={"FNO_NODE": "x-aaaa"}
    )
    assert "FNO_NODE=x-aaaa" in bound
    assert "-u" in bound and "FNO_PLAN" in bound
    assert "FNO_NODE" not in bound[: bound.index("FNO_NODE=x-aaaa")]


# -- x-6928: agents spawn --at current + interactive-readiness gate ----------


def test_exact_at_current_forwards_token_runs_json_and_reads_receipt(
    tmp_path: Path, monkeypatch
) -> None:
    """AC1-HP / AC1-UI: --at current forwards the token before the `--` fence,
    requests --json, and reads the SERVER-authored placement receipt - never
    synthesized from the requested flags. A painted, alive pane writes the row.
    """
    from fno.agents.registry import load_registry
    import fno.agents.mux_spawn as mux_spawn

    monkeypatch.setattr(mux_spawn, "_pane_group_max", lambda: 4)

    placement = {
        "anchor": 4,
        "direction": "Down",
        "fallback": "refuse",
        "squad": 1,
        "tab": 10,
    }
    result, runner = _spawn(
        monkeypatch,
        tmp_path,
        split="down",
        at="current",
        runner=FakeRunner(placement=placement, read_stdout="claude\n"),
    )
    run_call = runner.calls[0]
    assert run_call[1:4] == ["mux", "pane", "run"]
    assert "at" in run_call and "current" in run_call, "token forwarded verbatim"
    assert "--json" in run_call, "exact placement requests the server receipt"
    assert run_call[run_call.index("--max-panes") + 1] == "4"
    assert run_call.index("at") < run_call.index("--"), "token rides before the argv fence"
    assert result.placement == placement, "receipt is server-authored, not synthesized"
    # The readiness gate probes the spawn's own session, not the default.
    wait_call = next(c for c in runner.calls if c[1:4] == ["mux", "pane", "wait"])
    assert "--server" in wait_call, "readiness probe targets the spawn's server"
    assert [r.name for r in load_registry()] == ["peer"], "a ready spawn writes the row"
    assert runner.kill_calls == [], "no reap on a successful readiness gate"


def test_exact_at_current_kills_pane_and_writes_no_row_on_early_exit(
    tmp_path: Path, monkeypatch
) -> None:
    """AC5-ERR: a provider that exits before readiness is reaped (pane kill),
    the split collapses via tree normalization, and NO registry row is written.
    """
    from fno.agents.mux_spawn import DispatchAskError, dispatch_spawn_pane
    from fno.agents.registry import load_registry

    use_tmpdir(monkeypatch, tmp_path)
    monkeypatch.delenv("FNO_SESSION", raising=False)
    runner = FakeRunner(placement={"anchor": 4}, wait_returncode=12)
    with pytest.raises(DispatchAskError):
        dispatch_spawn_pane(
            name="peer",
            message="hi",
            provider="claude",
            cwd=tmp_path,
            split="down",
            at="current",
            runner=runner,
        )
    assert len(runner.kill_calls) == 1, "the transaction-owned pane was reaped"
    kill = runner.kill_calls[0]
    assert kill[1:4] == ["mux", "pane", "kill"]
    assert "--server" in kill and "main" in kill, "cleanup targets the spawn's server"
    assert "7" in kill, "the placed pane id is reaped"
    assert load_registry() == [], "no registry row on launch failure"


@pytest.mark.parametrize("cleanup_failure", ["timeout"])
def test_exact_readiness_failure_never_claims_unconfirmed_reap(
    tmp_path: Path, monkeypatch, cleanup_failure: str
) -> None:
    from fno.agents.mux_spawn import DispatchAskError, dispatch_spawn_pane
    from fno.agents.registry import load_registry

    use_tmpdir(monkeypatch, tmp_path)
    monkeypatch.delenv("FNO_SESSION", raising=False)
    runner = FakeRunner(
        placement={"anchor": 4},
        wait_returncode=12,
        kill_returncode=1,
        kill_stderr="permission denied",
        kill_exception=(
            subprocess.TimeoutExpired(["fno", "mux", "pane", "kill"], 30)
            if cleanup_failure == "timeout"
            else None
        ),
    )

    with pytest.raises(DispatchAskError) as exc:
        dispatch_spawn_pane(
            name="peer",
            message="hi",
            provider="claude",
            cwd=tmp_path,
            split="down",
            at="current",
            runner=runner,
        )

    message = str(exc.value)
    assert "pane 7 may still exist" in message
    assert "session 'main'" in message
    assert "pane 7 reaped" not in message
    assert load_registry() == []


def test_registry_write_failure_reaps_exact_spawned_pane(
    tmp_path: Path, monkeypatch
) -> None:
    """A pane without its registry identity is rolled back before failure."""
    from fno.agents import mux_spawn
    from fno.agents.mux_spawn import DispatchAskError, dispatch_spawn_pane
    from fno.agents.registry import load_registry

    failure = OSError("disk full")

    use_tmpdir(monkeypatch, tmp_path)
    monkeypatch.delenv("FNO_SESSION", raising=False)
    runner = FakeRunner()
    monkeypatch.setattr(
        mux_spawn, "update_registry",
        lambda *_args, **_kwargs: (_ for _ in ()).throw(failure),
    )
    monkeypatch.setattr(
        mux_spawn,
        "_backfill_codex_session_id",
        lambda *_args, **_kwargs: "019fb024-2327-75f3-8b80-06e9d5ade05f",
    )

    with pytest.raises(DispatchAskError, match=r"registry write failed.*pane 7 reaped"):
        dispatch_spawn_pane(
            name="peer", message="hi", provider="codex", cwd=tmp_path, runner=runner,
        )

    assert runner.kill_calls == [
        ["fno", "mux", "pane", "kill", "--server", "main", "7"]
    ]
    assert load_registry() == []


def test_working_osc_title_prevents_idle_composer_from_reporting_ready(monkeypatch) -> None:
    from fno import rust_binary
    from fno.agents.mux_spawn import _await_interactive_readiness

    monkeypatch.setattr(
        rust_binary, "resolve_installed_binary", lambda: Path("/fake/fno-agents")
    )

    runner = FakeRunner(
        wait_returncode=11,
        read_stdout="╭─╮\n│ prompt │\n╰─╯",
        ls_stdout=json.dumps([{"pane_id": 7, "title": "⠋ Working"}]),
    )
    readiness, detail = _await_interactive_readiness("claude", "main", 7, runner)
    assert readiness == "live"
    assert "observed osc_title_working" in detail
    manifest_call = next(call for call in runner.calls if "manifest-eval" in call)
    assert manifest_call[manifest_call.index("--osc-title") + 1] == "⠋ Working"


def test_permission_prompt_is_blocked_not_live_without_authorization() -> None:
    from fno.agents.mux_spawn import _await_interactive_readiness

    readiness, detail = _await_interactive_readiness(
        "codex",
        "main",
        7,
        FakeRunner(wait_returncode=11, read_stdout="approval"),
        manifest_evaluator=lambda _h, _screen: {
            "matched": True, "rule_id": "approval_prompt", "state": "blocked",
        },
    )
    assert readiness == "blocked"
    assert detail == "blocked-rule=approval_prompt"


def test_permission_sender_refuses_when_fingerprint_advanced() -> None:
    from fno.agents.mux_spawn import _send_permission_response

    answerable = {
        "fingerprint": [7] * 32,
        "region_lines": 20,
        "options": [{"idx": "1", "label": "once", "keystroke": [49]}],
    }
    verdict = {
        "matched": True, "rule_id": "approval_prompt", "state": "blocked",
        "answerable": answerable,
    }
    advanced = {**verdict, "answerable": {**answerable, "fingerprint": [8] * 32}}
    runner = FakeRunner(read_stdout="advanced prompt")
    assert not _send_permission_response(
        "codex", "main", 7, "approval_prompt", ["1"], verdict, runner,
        lambda _h, _screen: advanced,
    )
    assert [call[3] for call in runner.calls] == ["claim", "read", "release"]


# What a LIVE zai route actually carries, not just its endpoint half: the two
# connection keys plus `resolve_route`'s ANTHROPIC_MODEL, the four tier maps
# from MODEL_ENV_KEYS (haiku separately remapped to the provider's cheaper
# model), and the 1M compact window a `[1m]` model needs.
#
# The fixture used to hold only the two connection keys, so every happy test
# proved the endpoint reaches the pane and proved nothing about the model. A
# change that filtered keys anywhere along the carry would have kept the suite
# green while a happy pane ran on whatever model the ambient environment
# resolved -- or, quieter still, on the right model with a 200K window.
_ROUTE = {
    "ANTHROPIC_BASE_URL": "https://api.z.ai/api/anthropic",
    "ANTHROPIC_AUTH_TOKEN": "zai-secret",
    "ANTHROPIC_MODEL": "glm-5.2[1m]",
    "ANTHROPIC_DEFAULT_OPUS_MODEL": "glm-5.2[1m]",
    "ANTHROPIC_DEFAULT_SONNET_MODEL": "glm-5.2[1m]",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL": "glm-4.5-air",
    "ANTHROPIC_DEFAULT_FABLE_MODEL": "glm-5.2[1m]",
    "CLAUDE_CODE_AUTO_COMPACT_WINDOW": "1000000",
}


def _claude_env_pairs(argv: list[str]) -> dict:
    """The route as happy would receive it: every ``--claude-env KEY=VALUE``."""
    return dict(
        argv[i + 1].split("=", 1)
        for i, token in enumerate(argv)
        if token == "--claude-env"
    )


def _env1_assignments(wrapped: list[str]) -> dict:
    """The ``NAME=VALUE`` run of an ``env(1)`` wrapper, as the child receives it.

    Handles the exact shape ``_mesh_env_wrapper`` emits: a leading ``env``, a run
    of ``-u NAME`` pairs, then assignments, then the command. Deliberately NOT a
    full mirror of the mux server's ``env_assignments_start`` (server.rs), which
    also steps over ``--unset NAME``, other flags, and a ``--`` terminator. If
    the wrapper ever grows one of those, this stops at it and the union
    assertions fail loudly rather than silently skipping a key.
    """
    assert wrapped[0] == "env", wrapped[:3]
    pairs: dict[str, str] = {}
    i = 1
    while i < len(wrapped):
        tok = wrapped[i]
        if tok == "-u":
            i += 2
            continue
        if "=" in tok and not tok.startswith("-"):
            key, value = tok.split("=", 1)
            pairs[key] = value
            i += 1
            continue
        break
    return pairs


def _wrapped_happy_argv(route: dict) -> list[str]:
    """The argv that actually executes for a happy pane: env(1) -> happy -> claude."""
    import fno.agents.mux_spawn as mux_spawn

    inner = mux_spawn.happy_pane_argv(["claude", "--model", "glm-5.2", "go"], route)
    return mux_spawn._mesh_env_wrapper("wk", "claude", None, inner, None, None, route)


@pytest.mark.dev_build
def test_happy_pane_wrapped_argv_holds_the_union_invariant(monkeypatch) -> None:
    """Every route key reaches the child, and no credential reaches ``ps``.

    Asserted on the FULLY WRAPPED argv, not on happy_pane_argv's output, and
    that is the whole point of the test. happy builds its child env as
    ``{...process.env, ...claudeEnvVars}``, so a key is delivered whether it
    rides the env(1) run or --claude-env -- but only --claude-env publishes it
    to `ps`. A test that saw happy_pane_argv alone could not distinguish "the
    secret moved to the wrapper" (correct) from "the secret was dropped"
    (a 401 in production, and silent here).
    """
    import fno.agents.mux_spawn as mux_spawn
    from fno.agents.spawn_overlay_client import compose_credentials

    monkeypatch.setattr(mux_spawn.shutil, "which", lambda b: "/opt/homebrew/bin/happy")
    wrapped = _wrapped_happy_argv(_ROUTE)

    env1 = _env1_assignments(wrapped)
    claude_env = _claude_env_pairs(wrapped)
    secrets = [k for k in _ROUTE if k in compose_credentials(None, None, {})["secret"]]
    assert secrets, "fixture must carry a credential or this proves nothing"

    for key, value in _ROUTE.items():
        assert env1.get(key) == value or claude_env.get(key) == value, (
            f"{key} reaches the child on neither channel"
        )
    for key in secrets:
        assert key not in claude_env, f"{key} is a world-readable ps token"
        assert env1.get(key) == _ROUTE[key], f"{key} dropped, not moved"
    assert set(claude_env) == set(_ROUTE) - set(secrets), "none dropped, none invented"

    assert "--settings" not in wrapped
    assert wrapped[wrapped.index("happy") - 1] != "env", "env(1) must set, not just exec"
    assert wrapped[-3:] == ["--model", "glm-5.2", "go"]


def test_inject_route_model_argv_pins_the_route_before_the_seed_fence() -> None:
    """A routed pane pins ANTHROPIC_MODEL on argv, ahead of settings env.

    The user settings env block outranks process env, so env alone let a
    global model pin win. The token lands flags-first, before the ``--``
    fence, so it can never become prompt text, and an argv that already names
    a model is caller intent that wins.
    """
    import fno.agents.mux_spawn as mux_spawn

    route = {"ANTHROPIC_MODEL": "glm-5.3-flash[1m]"}
    pinned = mux_spawn.inject_route_model_argv(["claude", "--name", "w", "go"], route)
    assert pinned == ["claude", "--model", "glm-5.3-flash[1m]", "--name", "w", "go"]
    fenced = mux_spawn.inject_route_model_argv(["claude", "go", "--", "--model x"], route)
    assert fenced == ["claude", "--model", "glm-5.3-flash[1m]", "go", "--", "--model x"]
    # An existing model token (explicit flag or passthrough) is never doubled.
    explicit = mux_spawn.inject_route_model_argv(["claude", "--model", "glm-5.2"], route)
    assert explicit == ["claude", "--model", "glm-5.2"]
    # No route model, or a non-claude head: byte-identical.
    assert mux_spawn.inject_route_model_argv(["claude", "go"], {}) == ["claude", "go"]
    assert mux_spawn.inject_route_model_argv(["codex", "go"], route) == ["codex", "go"]


def test_explicit_happy_monitor_refuses_when_happy_is_absent_before_runner(
    tmp_path: Path, monkeypatch
) -> None:
    import fno.agents.mux_spawn as mux_spawn
    from fno.agents.dispatch import DispatchAskError
    from fno.agents.model_routing import resolve_explicit_route

    monkeypatch.setenv("ZAI_API_KEY", "zai-secret")
    monkeypatch.setattr(mux_spawn.shutil, "which", lambda binary: None)
    route = resolve_explicit_route("zai", "glm-5.2")
    assert route is not None
    runner = FakeRunner()

    with pytest.raises(DispatchAskError, match="--monitor happy") as exc:
        _spawn(
            monkeypatch,
            tmp_path,
            monitor="happy",
            route_provider="zai",
            route_env=route,
            runner=runner,
        )

    assert exc.value.exit_code == 127
    assert runner.calls == []


@pytest.mark.parametrize(
    ("provider", "route_provider", "message"),
    [("codex", "zai", "claude")],
)
def test_explicit_happy_monitor_refuses_in_process_incompatible_route(
    tmp_path: Path,
    monkeypatch,
    provider: str,
    route_provider: str,
    message: str,
) -> None:
    from fno.agents.dispatch import DispatchAskError

    runner = FakeRunner()
    with pytest.raises(DispatchAskError, match=message) as exc:
        _spawn(
            monkeypatch,
            tmp_path,
            provider=provider,
            monitor="happy",
            route_provider=route_provider,
            route_env=dict(_ROUTE),
            runner=runner,
        )

    assert exc.value.exit_code == 2
    assert runner.calls == []


def test_explicit_happy_monitor_refuses_partial_zai_route_before_runner(
    tmp_path: Path, monkeypatch
) -> None:
    from fno.agents.dispatch import DispatchAskError

    runner = FakeRunner()
    with pytest.raises(DispatchAskError, match="resolved zai route") as exc:
        _spawn(
            monkeypatch,
            tmp_path,
            monitor="happy",
            route_provider="zai",
            route_env={"ANTHROPIC_MODEL": "glm-5.2"},
            runner=runner,
        )

    assert exc.value.exit_code == 2
    assert runner.calls == []


def test_explicit_happy_monitor_refuses_conflicting_account_credential(
    tmp_path: Path, monkeypatch
) -> None:
    from fno.agents.dispatch import DispatchAskError

    monkeypatch.setenv("ZAI_API_KEY", "zai-secret")
    runner = FakeRunner()
    with pytest.raises(DispatchAskError, match="conflicting Anthropic credential") as exc:
        _spawn(
            monkeypatch,
            tmp_path,
            monitor="happy",
            route_provider="zai",
            route_env={**_ROUTE, "ANTHROPIC_MODEL": "glm-5.2"},
            account_env={"CLAUDE_CODE_OAUTH_TOKEN": "anthropic-secret"},
            runner=runner,
        )

    assert exc.value.exit_code == 2
    assert runner.calls == []


def _zai_route(monkeypatch):
    from fno.agents.model_routing import resolve_explicit_route

    monkeypatch.setenv("ZAI_API_KEY", "zai-secret")
    route = resolve_explicit_route("zai", "glm-5.2[1m]")
    assert route is not None
    return route


def _happy_spawn(monkeypatch, tmp_path, **kwargs):
    """A happy-monitored claude pane spawn.

    No real SessionStart hook fires under the fake runner, so the bounded
    registration wait is scripted: by default the worker "restamps" a known id
    (the success path); pass ``registration=(None, reason)`` to drive the
    failure path (reap + raise). The argv is built before that wait, so argv
    assertions are unaffected.
    """
    import fno.agents.mux_spawn as mux_spawn

    monkeypatch.setattr(mux_spawn.shutil, "which", lambda b: "/opt/homebrew/bin/happy")
    registration = kwargs.pop("registration", ("happy-proven-sid", ""))
    monkeypatch.setattr(
        mux_spawn, "_await_pane_registration", lambda name, mux, r, *a, **k: registration
    )
    return _spawn(
        monkeypatch,
        tmp_path,
        monitor="happy",
        route_provider="zai",
        route_env=_zai_route(monkeypatch),
        **kwargs,
    )


def test_happy_spawn_never_pins_a_session_id(tmp_path: Path, monkeypatch) -> None:
    """happy discards --session-id, so fno must not mint one and report it."""
    result, runner = _happy_spawn(monkeypatch, tmp_path)

    run_call = next(c for c in runner.calls if c[1:4] == ["mux", "pane", "run"])
    provider_argv = run_call[run_call.index("--") + 1 :]
    assert "--session-id" not in provider_argv
    assert "happy" in provider_argv
    # fno did not mint an id; the one on the receipt came from the restamp,
    # and that proven session earns a `live` receipt with a real short_id.
    assert result.session_uuid == "happy-proven-sid"
    assert result.status == "live"
    assert result.short_id, "a live receipt must carry a non-empty short_id"


def test_happy_routed_panes_malformed_config_file_refuses(
    tmp_path: Path, monkeypatch
) -> None:
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents.dispatch import DispatchAskError
    from fno.agents.mux_spawn import dispatch_spawn_pane

    config_path = tmp_path / "config.toml"
    config_path.write_text("[agents\nhappy_routed_panes = true\n", encoding="utf-8")
    monkeypatch.setenv("FNO_CONFIG", str(config_path))
    runner = FakeRunner()

    with pytest.raises(DispatchAskError, match="silently launching"):
        dispatch_spawn_pane(
            name="peer",
            message="hello",
            provider="claude",
            cwd=tmp_path,
            route_env=dict(_ROUTE),
            runner=runner,
        )
    assert runner.calls == []


def _pane_run_argv(runner: "FakeRunner") -> list[str]:
    for call in runner.calls:
        if call[1:4] == ["mux", "pane", "run"]:
            return call[call.index("--") + 1 :]
    raise AssertionError("no `mux pane run` call recorded")


def test_codex_pane_applies_business_role_route_before_launch(tmp_path: Path, monkeypatch) -> None:
    from fno.agents import model_routing

    monkeypatch.setattr(
        model_routing,
        "resolve_codex_route",
        lambda role, **kwargs: model_routing.CodexRoute(
            env={"OPENAI_API_KEY": "business-key"},
            config_args=["-c", "model='gpt-business'"],
        ),
    )

    _, runner = _spawn(
        monkeypatch,
        tmp_path,
        provider="codex",
        role="publisher",
    )

    argv = _pane_run_argv(runner)
    assert "OPENAI_API_KEY=business-key" in argv
    codex_index = argv.index("codex")
    assert argv[codex_index : codex_index + 3] == [
        "codex",
        "-c",
        "model='gpt-business'",
    ]


def test_routed_claude_pane_launches_through_happy_when_enabled(
    tmp_path: Path, monkeypatch
) -> None:
    import fno.agents.mux_spawn as mux_spawn

    monkeypatch.setattr(mux_spawn, "happy_routed_panes_enabled", lambda: True)
    monkeypatch.setattr(mux_spawn.shutil, "which", lambda b: "/opt/homebrew/bin/happy")
    monkeypatch.setattr(
        mux_spawn, "_await_pane_registration", lambda name, mux, r, *a, **k: ("happy-sid", "")
    )
    # The route env the Rust owner would answer for -P zai --model glm-5.2
    # with ZAI_API_KEY: the canned stub's answer.
    route_env = claude_route_overlay_stub(
        {"kind": "claude-route", "env": {"ZAI_API_KEY": "zai-secret"}, "model": "glm-5.2"}
    )["route"]
    _, runner = _spawn(monkeypatch, tmp_path, route_env=route_env)

    argv = _pane_run_argv(runner)
    assert argv[0] == "env", "the mesh env wrapper must stay outermost"
    happy_argv = argv[argv.index("happy") :]
    pairs = [
        happy_argv[i + 1]
        for i, token in enumerate(happy_argv)
        if token == "--claude-env"
    ]
    assert "ANTHROPIC_BASE_URL=https://api.z.ai/api/anthropic" in pairs
    # Credential on the env(1) run only -- see the union invariant test.
    assert not any(p.startswith("ANTHROPIC_AUTH_TOKEN=") for p in pairs)
    assert "ANTHROPIC_AUTH_TOKEN=zai-secret" in argv[: argv.index("happy")]
    assert "--settings" not in argv


# A happy-hosted claude pane is created id-less (`spawning`) because
# happy owns the session id and restamps the row later via the worker's
# SessionStart hook. If that restamp never lands, the row strands `spawning`
# forever and the receipt reads as a soft success - a silent no-op. The fix
# waits for the restamp within a bounded window, then either hands back a
# `live` receipt or reaps the pane and fails loud.


def _reg_row(name: str = "peer", hsid: Optional[str] = None):
    from fno.agents.registry import AgentEntry

    return AgentEntry(
        name=name,
        harness="claude",
        cwd="/w",
        log_path="",
        status="spawning",
        harness_session_id=hsid,
        mux={"session": "main", "pane_id": 7},
    )


def test_await_pane_registration_times_out_when_alive_but_unregistered(monkeypatch) -> None:
    import fno.agents.mux_spawn as mux_spawn

    monkeypatch.setattr(mux_spawn, "load_registry", lambda *a, **k: [_reg_row(hsid=None)])
    monkeypatch.setattr(mux_spawn, "_PANE_REGISTRATION_DEADLINE_S", 0.01)
    monkeypatch.setattr(mux_spawn, "_PANE_REGISTRATION_POLL_S", 0.001)
    # Alive (11) but never registered: the reproduced failure shape.
    sid, reason = mux_spawn._await_pane_registration(
        "peer", {"session": "main", "pane_id": 7}, FakeRunner(wait_returncode=11)
    )
    assert sid is None
    assert "no session id" in reason


def test_happy_pane_failure_reaps_and_raises_not_silent(
    tmp_path: Path, monkeypatch
) -> None:
    """The headline: a stranded happy pane must fail loud, not return `spawning`."""
    import fno.agents.mux_spawn as mux_spawn
    from fno.agents.dispatch import DispatchAskError
    from fno.agents.registry import load_registry

    monkeypatch.setattr(mux_spawn, "happy_routed_panes_enabled", lambda: True)
    monkeypatch.setattr(mux_spawn.shutil, "which", lambda b: "/usr/local/bin/happy")
    monkeypatch.setattr(
        mux_spawn,
        "_await_pane_registration",
        lambda name, mux, r, *a, **k: (None, "pane process exited before registering"),
    )
    runner = FakeRunner()
    with pytest.raises(DispatchAskError) as ei:
        _spawn(monkeypatch, tmp_path, route_env=dict(_ROUTE), runner=runner)

    assert ei.value.exit_code == 1
    assert "did not register" in str(ei.value)
    assert runner.kill_calls, "the stranded pane must be reaped"
    assert load_registry() == [], "the stranded spawning row must be removed"


def test_a_terminal_row_does_not_own_its_name(tmp_path: Path, monkeypatch) -> None:
    """x-cdca: a dead pane must not deadlock the node that spawned it.

    A failed spawn releases its family-2 claims so a retry can run under the
    SAME deterministic worker name. A status-blind collision guard
    therefore turned one dead pane into a permanently failed node until a human
    ran `fno agents rm`. A terminal row will never act again, so it does not
    hold the name; its evidence lives in a timestamped file on disk, not in the
    registry row.
    """
    from dataclasses import replace as _replace

    from fno.agents.registry import load_registry, update_registry

    first, _ = _spawn(monkeypatch, tmp_path, name="w1")
    assert first.name == "w1"

    # Make the existing row terminal, the way the death branch does.
    update_registry(lambda rows: [_replace(r, status="failed") for r in rows])

    second, _ = _spawn(monkeypatch, tmp_path, name="w1")
    assert second.name == "w1"
    rows = [r for r in load_registry() if r.name == "w1"]
    assert len(rows) == 1, "the corpse must be dropped in the same transaction"
    assert rows[0].status != "failed"


# --- x-1caa: `--` passthrough to the provider CLI ----------------------------


def test_ac1_passthrough_splices_into_every_pane_arm(tmp_path: Path) -> None:
    """AC1-HP: tokens after `--` ride each arm's own flag zone, upstream of the
    composed-argv refusals, so they inherit the guards rather than bypassing."""
    from fno.agents.mux_spawn import build_pane_argv

    tok = ["--verbose", "--setting", "x"]
    # claude/codex: before the end-of-options fence carrying the seed.
    argv = build_pane_argv("claude", "task", tmp_path, "u1", passthrough=tok)
    fence = argv.index("--")
    assert argv[fence - 3 : fence] == tok
    assert argv[fence + 1] == "task"
    argv = build_pane_argv("codex", "task", tmp_path, None, passthrough=tok)
    fence = argv.index("--")
    assert argv[fence - 3 : fence] == tok
    # agy: the seed is delivered after readiness, so passthrough owns the tail.
    argv = build_pane_argv("agy", "task", tmp_path, None, passthrough=tok)
    assert argv[-3:] == tok and "task" not in argv
    # gemini: before the -i <message> pair.
    argv = build_pane_argv("gemini", "task", tmp_path, None, passthrough=tok)
    i = argv.index("-i")
    assert argv[i - 3 : i] == tok
    # opencode: the tail, after the model pair (the positional is a project path).
    argv = build_pane_argv("opencode", "task", tmp_path, None, passthrough=tok)
    assert argv[-3:] == tok


def test_ac3_passthrough_hits_the_happy_refusals(
    tmp_path: Path, monkeypatch
) -> None:
    """AC3-ERR: the happy-lane refusals get their first real callers - a
    passthrough is the only way those tokens can enter the composed argv.
    happy discards a pinned --session-id, so the receipt would name a session
    that never exists; the --settings refusal is pinned on happy_pane_argv."""
    tok = ["--session-id", "u"]
    import fno.agents.mux_spawn as mux_spawn
    from fno.agents.dispatch import DispatchAskError
    from fno.agents.model_routing import resolve_explicit_route

    use_tmpdir(monkeypatch, tmp_path)
    monkeypatch.setenv("ZAI_API_KEY", "zai-secret")
    monkeypatch.setattr(mux_spawn.shutil, "which", lambda _b: "/opt/homebrew/bin/happy")
    route = resolve_explicit_route("zai", "glm-5.2")
    assert route is not None
    runner = FakeRunner()
    with pytest.raises(DispatchAskError, match=tok[0]) as exc:
        _spawn(
            monkeypatch,
            tmp_path,
            monitor="happy",
            route_provider="zai",
            route_env=route,
            passthrough=tok,
            runner=runner,
        )
    assert exc.value.exit_code == 2
    assert runner.calls == []



@pytest.mark.parametrize(
    ("provider", "tok"),
    [
        ("opencode", ["run"]),
    ],
)
def test_ac5_headless_form_tokens_refused_on_the_composed_argv(
    tmp_path: Path, monkeypatch, provider: str, tok: list[str]
) -> None:
    """AC5-ERR: the agy/opencode headless-form comments are guards now. The
    bare `run` token must match too - it is a subcommand, not a flag."""
    from fno.agents.dispatch import DispatchAskError

    runner = FakeRunner()
    with pytest.raises(DispatchAskError, match=provider) as exc:
        _spawn(
            monkeypatch, tmp_path, provider=provider, passthrough=tok, runner=runner
        )
    assert exc.value.exit_code == 2
    assert "headless" in str(exc.value)
    assert runner.calls == []


def test_ac1_cli_passthrough_reaches_dispatch(tmp_path: Path, monkeypatch) -> None:
    """AC1 at the public surface: the tokens parse into the variadic positional
    and travel to the bounded default pane dispatcher as `passthrough`."""
    from typer.testing import CliRunner

    import fno.agents.cli as agents_cli
    import fno.agents.mux_spawn as mux_spawn

    use_tmpdir(monkeypatch, tmp_path)
    captured: dict = {}

    def fake_dispatch(**kwargs):
        captured.update(kwargs)
        return MuxSpawnResult(
            name=kwargs["name"],
            provider=kwargs["provider"],
            session="main",
            pane_id=1,
            child_pid=None,
            session_uuid="u",
        )

    monkeypatch.setattr(mux_spawn, "dispatch_spawn_bounded_pane", fake_dispatch)
    monkeypatch.setenv("FNO_AGENTS_RUNTIME", "python")
    monkeypatch.setenv("FNO_REPO_ROOT", os.getcwd())

    result = CliRunner().invoke(
        agents_cli.agents_app,
        ["spawn", "--name", "peer", "--harness", "claude", "--as", "pane", "hi", "--", "--verbose"],
    )
    assert result.exit_code == 0, result.output
    assert captured["message"] == "hi"
    assert captured["passthrough"] == ["--verbose"]


def test_ac7_claude_off_pane_forwards_fenced_tokens(
    tmp_path: Path, monkeypatch
) -> None:
    """The off-pane lanes forward fenced tokens; the pane lane never runs."""
    import typer
    from typer.testing import CliRunner

    import fno.agents.cli as agents_cli
    import fno.agents.mux_spawn as mux_spawn
    from fno.agents import dispatch as _dispatch

    use_tmpdir(monkeypatch, tmp_path)
    monkeypatch.setenv("FNO_AGENTS_RUNTIME", "python")
    monkeypatch.setattr(
        mux_spawn,
        "dispatch_spawn_pane",
        lambda **_kwargs: pytest.fail("pane dispatch must not run"),
    )
    sent: dict = {}

    def recorder(**kwargs):
        sent.update(kwargs)
        raise typer.Exit(code=0)

    monkeypatch.setattr(_dispatch, "dispatch_spawn", recorder)
    result = CliRunner().invoke(
        agents_cli.agents_app,
        ["spawn", "--name", "peer", "--once", "hi", "--", "--verbose", "x"],
    )
    assert sent.get("passthrough") == ["--verbose", "x"], (
        f"the fenced tokens must ride the off-pane dispatch: {result.output}"
    )
    assert "pane-only" not in result.output, result.output


# -- an unanswered control read is reconciled, never asserted absent --
#
# `mux pane run` exiting `_MUX_CONTROL_UNANSWERED` means the command reached the
# server and the reply never came back - not that no pane was created. These
# pin the Python side of that reconciliation: adopt the one pane that proves
# it is this spawn, refuse (writing no row) on zero/many/unknown, and never
# let a recovered pane earn its row without proving it actually started.


def _patch_process_create_time_far_future(monkeypatch) -> None:
    """Make a fake pane child read as newer without changing liveness reads."""
    import fno.agents.mux_spawn as mux_spawn

    monkeypatch.setattr(mux_spawn, "_pid_started_at_or_after", lambda *_args: True)


def test_unanswered_run_with_no_matching_pane_refuses_cleanly(
    tmp_path: Path, monkeypatch
) -> None:
    from fno.agents.mux_spawn import DispatchAskError, _MUX_CONTROL_UNANSWERED
    from fno.agents.registry import load_registry

    ls_stdout = json.dumps(
        [{"pane_id": 3, "squad_id": 1, "tab_id": 1, "cwd": "/somewhere/else", "child_pid": 4242}]
    )
    runner = FakeRunner(run_returncode=_MUX_CONTROL_UNANSWERED, ls_stdout=ls_stdout)

    with pytest.raises(DispatchAskError) as exc:
        _spawn(monkeypatch, tmp_path, runner=runner)

    message = str(exc.value)
    assert "no pane was created" in message
    assert "--substrate thread" in message
    assert load_registry() == []


def test_unanswered_run_rejects_a_stale_pid_by_real_wall_clock(
    tmp_path: Path, monkeypatch
) -> None:
    """The freshness filter must compare real epoch-seconds process start
    times, not `spawn_gate._process_start_time`'s opaque incarnation-token
    units - a stale pid whose wall-clock start predates the spawn must still
    be rejected even though it shares this spawn's cwd."""
    import psutil

    from fno.agents.mux_spawn import DispatchAskError, _MUX_CONTROL_UNANSWERED
    from fno.agents.registry import load_registry

    class _StaleProc:
        def __init__(self, _pid: int) -> None:
            pass

        def create_time(self) -> float:
            return 1.0  # long before any spawn_started_ms this test samples

    monkeypatch.setattr(psutil, "Process", _StaleProc)
    ls_stdout = json.dumps(
        [{"pane_id": 9, "squad_id": 1, "tab_id": 1, "cwd": str(tmp_path), "child_pid": 4242}]
    )
    runner = FakeRunner(run_returncode=_MUX_CONTROL_UNANSWERED, ls_stdout=ls_stdout)

    with pytest.raises(DispatchAskError) as exc:
        _spawn(monkeypatch, tmp_path, runner=runner)

    assert "no pane was created" in str(exc.value)
    assert load_registry() == []


def test_unanswered_run_claimed_pane_id_is_scoped_to_this_mux_session(
    tmp_path: Path, monkeypatch
) -> None:
    """A registry row's pane_id is only 'claimed' within ITS OWN mux session:
    pane ids are allocated independently per server starting at 1, so a row
    parked in a different session must never suppress a same-numbered,
    same-cwd candidate in the session actually being reconciled."""
    from fno.agents.mux_spawn import _MUX_CONTROL_UNANSWERED
    from fno.agents.registry import AgentEntry, load_registry, update_registry

    _patch_process_create_time_far_future(monkeypatch)
    ls_stdout = json.dumps(
        [{"pane_id": 9, "squad_id": 1, "tab_id": 1, "cwd": str(tmp_path), "child_pid": 4242}]
    )
    runner = FakeRunner(
        run_returncode=_MUX_CONTROL_UNANSWERED,
        ls_stdout=ls_stdout,
        # Non-empty read -> readiness "ready" (LD4's bar for a recovered pane).
        read_stdout="$ ",
    )

    use_tmpdir(monkeypatch, tmp_path)
    # A live row already owns pane_id 9, but in a DIFFERENT mux session -
    # this must not be read as "pane 9 in *this* session is claimed".
    update_registry(
        lambda rows: rows
        + [
            AgentEntry(
                name="other-worker",
                cwd=str(tmp_path),
                log_path=str(tmp_path / "other-worker.log"),
                harness="claude",
                mux={"session": "some-other-session", "pane_id": 9},
            )
        ]
    )

    result, _ = _spawn(monkeypatch, tmp_path, runner=runner)

    assert result.pane_id == 9
    assert result.recovered is True
    assert any(r.name == "peer" and r.mux == {"session": "main", "pane_id": 9} for r in load_registry())


def test_unanswered_run_with_empty_listing_is_unknown_not_absent(
    tmp_path: Path, monkeypatch
) -> None:
    """An empty `pane ls` after an unanswered run must never read as proof no
    pane exists (`_pane_absent_from_listing`'s trap): the session socket also
    prints `[]` and exits 0 when it is refused or absent."""
    from fno.agents.mux_spawn import DispatchAskError, _MUX_CONTROL_UNANSWERED
    from fno.agents.registry import load_registry

    runner = FakeRunner(run_returncode=_MUX_CONTROL_UNANSWERED, ls_stdout="[]")

    with pytest.raises(DispatchAskError) as exc:
        _spawn(monkeypatch, tmp_path, runner=runner)

    message = str(exc.value)
    assert "no pane was created" not in message
    assert load_registry() == []


def test_unanswered_run_recovered_pane_without_session_id_is_reaped(
    tmp_path: Path, monkeypatch
) -> None:
    """LD4: a recovered pane earns its row only by proving it started. This
    one has no session id to show for itself (the default FakeRunner db/codex
    seams both miss), so it must be reaped rather than written live-only the
    way a NORMAL (non-recovered) codex miss is."""
    from fno.agents.mux_spawn import DispatchAskError, _MUX_CONTROL_UNANSWERED
    from fno.agents.registry import load_registry

    _patch_process_create_time_far_future(monkeypatch)
    ls_stdout = json.dumps(
        [
            {
                "pane_id": 9,
                "squad_id": 1,
                "tab_id": 1,
                "cwd": str(tmp_path),
                "child_pid": 2_000_000_000,
            }
        ]
    )
    runner = FakeRunner(
        run_returncode=_MUX_CONTROL_UNANSWERED,
        ls_stdout=ls_stdout,
        read_stdout="",
        kill_removes_pane=True,
    )
    from fno.agents import spawn_gate

    monkeypatch.setattr(
        spawn_gate,
        "_process_start_time",
        lambda _pid: None if runner.kill_calls else "incarnation-before-kill",
    )

    with pytest.raises(DispatchAskError) as exc:
        _spawn(monkeypatch, tmp_path, provider="codex", runner=runner)

    message = str(exc.value)
    assert "pane 9" in message
    assert "reaped" in message
    assert load_registry() == []
    assert runner.kill_calls, "an unproven recovered pane must be reaped"


def test_group_placement_failure_leaves_the_worker_alive(
    tmp_path: Path, monkeypatch, capsys
) -> None:
    """The pane is created with the seed already in its argv, so by the time
    grouping runs the worker is live and may have started. Grouping is cosmetic
    by place_pane_in_group_tab's own account, so a transient `tab ls` non-zero
    must not kill a running worker."""
    import fno.agents.mux_spawn as mux_spawn

    monkeypatch.setattr(mux_spawn, "_pane_group_max", lambda: 4)
    runner = SquadAwareRunner(squads={1: [], 5: []}, pane_squad=1, pane_tab=1)
    runner.tabs_returncode = 1
    runner.tabs_stderr = "transient server hiccup"

    # A failing tab read must not raise, and must not reap.
    result, runner = _spawn(monkeypatch, tmp_path, tab="codex", runner=runner)

    assert not runner.kill_calls, "a live worker was killed over tab grouping"
    err = capsys.readouterr().err
    assert "left in its own tab" in err
    assert "codex" in err


def test_pane_run_env_does_not_latch_the_writable_dir_grant(
    tmp_path: Path, monkeypatch
) -> None:
    """The mux server latches this env at birth, so a grant left in it would pin
    ONE spawn's directory list onto every later pane shell - including workers in
    other projects. The pane lane carries its grant in the argv instead."""
    import fno.agents.mux_spawn as mux_spawn
    from fno.agents.writable_dirs import WORKER_ADD_DIRS_ENV

    monkeypatch.setenv(WORKER_ADD_DIRS_ENV, "/some/other/project")
    seen: dict[str, object] = {}
    real = mux_spawn._run_mux

    def spy(args, runner, **kw):
        if list(args[:3]) == ["mux", "pane", "run"]:
            seen["env"] = kw.get("env")
        return real(args, runner, **kw)

    monkeypatch.setattr(mux_spawn, "_run_mux", spy)
    _spawn(monkeypatch, tmp_path)

    env = seen["env"]
    assert env is not None
    assert WORKER_ADD_DIRS_ENV not in env


# --- An unconfirmed seed is a failed spawn, not a ready worker --------------
#
# `_submit_spawn_seed` used to return "unconfirmed" on five paths. The call site
# folded all five into a rewritten `readiness_detail` and carried on, so the pane
# got a `ready` row and `spawn_gate.LIVE_STATUSES` counted it live while it sat
# at an empty prompt having executed nothing. That is a slot held by a worker
# that never started.
#
# Those five split on two questions, which is three answers plus the happy path.
#
#   unattempted  nothing was sent - an unreadable or blank frame. An alive
#                unpainted child is still a live worker, so keep the row. This
#                is the ONLY retryable state, because it is the only one where
#                nothing can already be sitting in the pane's buffer.
#   unconfirmed  the send was REFUSED - a non-zero return code, or a trust
#                modal still on screen after the clearing submit. Positive
#                evidence the worker will never start, so fail the spawn on the
#                first answer.
#   unknown      mux did not ANSWER - the RPC timed out. The keystroke may have
#                landed, so reaping would kill a worker already running and a
#                retry would type the seed twice. Keep the row, say so.
#
# The distinction between the last two is the whole point: a timeout is not a
# rejection, and folding it into one reaped live panes.


def _seed_script(monkeypatch, *states: str) -> list[tuple]:
    """Drive `_submit_spawn_seed` through a scripted sequence of outcomes."""
    from fno.agents import mux_spawn

    # The retry's real delay exists for a TUI that has not painted; a scripted
    # sequence has no TUI, so paying it would just add a second of wall clock
    # per test for nothing.
    monkeypatch.setattr(mux_spawn, "_SEED_RETRY_DELAY_S", 0)

    calls: list[tuple] = []
    seq = iter(states)

    # **_kw absorbs seed_in_argv: the real _submit_spawn_seed takes it as a
    # keyword-only argument, and this stub stands in for it.
    def fake_submit(provider, session, pane_id, seed, runner, **_kw):
        calls.append((provider, session, pane_id, seed))
        state = next(seq)
        detail = {
            "submitted": "",
            "unknown": "mux did not answer the submit; the keystroke may have landed",
        }.get(state, "text delivered, submission unconfirmed")
        return state, detail, "delivered", "painted"

    monkeypatch.setattr(mux_spawn, "_submit_spawn_seed", fake_submit)
    return calls


def test_a_seed_that_submits_on_the_retry_still_spawns_one_worker(
    tmp_path: Path, monkeypatch
) -> None:
    """A pane that painted late is the common transient. It lands in
    `unattempted` - nothing was sent - so one retry recovers it, and it must not
    produce a second pane or a second row."""
    from fno.agents.registry import load_registry

    calls = _seed_script(monkeypatch, "unattempted", "submitted")

    result, runner = _spawn(monkeypatch, tmp_path)

    assert result.seed == "submitted"
    assert len(calls) == 2, "the retry must actually re-submit"
    assert len(load_registry()) == 1
    assert runner.kill_calls == []


def test_a_transient_read_failure_on_an_argv_seed_is_re_probed_not_condemned(
    tmp_path: Path, monkeypatch
) -> None:
    """One timed-out read must not permanently condemn a healthy pane.

    Before the two-field split this case self-healed: the raised read reported
    `unattempted`, the seed retry fired, and a pane that answered a second later
    produced a clean receipt. The split correctly stops that retry for an argv
    seed, because it re-enters the SUBMIT path and can type a payload the pane is
    already running. What it must not also drop is the second LOOK. Machine load
    is exactly when a read times out, and it is the condition that produced this
    family's original specimens.

    So the re-probe is read-only. It upgrades the observation and never touches
    the seed, which was settled at exec time."""
    from fno.agents import mux_spawn
    from fno.agents.registry import load_registry

    monkeypatch.setattr(mux_spawn, "_SEED_RETRY_DELAY_S", 0)
    submits: list = []

    def fake_submit(provider, session, pane_id, seed, runner, **_kw):
        submits.append(seed)
        return (*mux_spawn._ARGV_SEED_RECEIPT, "unreadable")

    monkeypatch.setattr(mux_spawn, "_submit_spawn_seed", fake_submit)

    result, runner = _spawn(monkeypatch, tmp_path)

    assert result.seed == "submitted"
    assert result.pane_observation == "painted", (
        "the second look succeeded, so the doubt it was carrying is resolved"
    )
    assert len(submits) == 1, "the re-probe must NOT re-enter the submit path"
    assert len(load_registry()) == 1
    assert runner.kill_calls == []


def test_a_pane_that_stays_unreadable_keeps_its_doubt(
    tmp_path: Path, monkeypatch
) -> None:
    """The negative control on the re-probe, and the rest of the pair's contract.

    A re-probe that always upgraded would pass the test above and quietly delete
    the dead-pane protection, which is the whole reason this clause exists. So
    the observation must survive as `unreadable` onto `MuxSpawnResult`, where
    every downstream reader branches on it. The pane also keeps its row: an
    unreadable frame is a doubt, not a death."""
    from fno.agents import mux_spawn
    from fno.agents.registry import load_registry

    monkeypatch.setattr(mux_spawn, "_SEED_RETRY_DELAY_S", 0)
    submits: list = []

    def fake_submit(provider, session, pane_id, seed, runner, **_kw):
        submits.append(seed)
        return (*mux_spawn._ARGV_SEED_RECEIPT, "unreadable")

    monkeypatch.setattr(mux_spawn, "_submit_spawn_seed", fake_submit)
    # The re-probe keeps failing. `_run_mux` turns a non-zero code into a clean
    # return, so it comes back `unreadable` rather than raising. Readiness is
    # stubbed because it fails the SAME read, and this test is about what happens
    # after readiness has already passed.
    monkeypatch.setattr(
        mux_spawn,
        "_await_interactive_readiness",
        lambda *a, **k: ("ready", "ready-marker=x"),
    )

    result, runner = _spawn(
        monkeypatch, tmp_path, runner=FakeRunner(read_returncode=1, read_stdout="")
    )

    assert result.seed == "submitted"
    assert result.seed_source == "argv"
    assert result.pane_observation == "unreadable"
    assert len(submits) == 1, "a delivered seed must never be re-submitted"
    assert len(load_registry()) == 1
    assert runner.kill_calls == [], "an unreadable frame is a doubt, not a death"


def test_a_seed_unconfirmed_twice_reaps_the_pane_and_writes_no_row(
    tmp_path: Path, monkeypatch
) -> None:
    """The measured failure: readiness reported live for a worker that never
    started. A retry that still cannot get the seed in means a pane that will
    not accept the payload, so it takes the existing readiness-failure path."""
    from fno.agents import spawn_gate
    from fno.agents.mux_spawn import DispatchAskError
    from fno.agents.registry import load_registry

    # The reap confirms a kill by watching the child's start time change, and
    # this fixture's child_pid is the literal 4242. Unpatched, the probe asks
    # the HOST whether pid 4242 is alive: absent on a dev laptop (reaped, green)
    # and frequently present on a CI runner (reads as "the worker survived
    # teardown", red). The assertion below was therefore decided by the machine
    # rather than by the code. Pin the sequence the way the sibling reap tests
    # in tests/unit/test_mux_pane_guarded.py already do: alive, then gone.
    # First read per pid is the pre-kill sample; every read after it reports the
    # process gone. A fixed-length iter would StopIteration here, because this
    # flow reaps more than once.
    seen: set[int] = set()

    def _start_time(pid: int) -> object:
        if pid in seen:
            return None
        seen.add(pid)
        return 7

    monkeypatch.setattr(spawn_gate, "_process_start_time", _start_time)

    calls = _seed_script(monkeypatch, "unattempted", "unconfirmed")

    with pytest.raises(DispatchAskError) as exc:
        _spawn(monkeypatch, tmp_path)

    assert len(calls) == 2, "exactly one retry, then stop holding the slot"
    message = str(exc.value)
    assert "submission unconfirmed" in message
    assert "reaped" in message
    assert load_registry() == [], "an unstarted worker must not hold a live row"


def test_an_agy_trust_timeout_fails_the_spawn_rather_than_holding_a_slot():
    """A timeout at the trust gate is not the timeout at the submit, and the
    modal is the whole difference.

    At the submit the seed was already typed into a RUNNING pane, so an
    unanswered RPC leaves a worker that may be doing its job. At the trust gate
    the modal may still be up, and a pane behind an unanswered modal executes
    NOTHING while holding a slot against max_live. That is the leak this
    function exists to close, so both trust arms fail the spawn.

    The source must not say `trust-cleared` either. Nothing read back that the
    gate cleared, and claiming it makes a wedged pane and a healthy one produce
    identical receipts."""
    from fno.agents.mux_spawn import DispatchAskError, _submit_spawn_seed

    modal = "Do you trust this folder?"

    def runner_that_times_out_after_the_modal(argv, **kw):
        # argv[0] is the fno binary `_run_mux` prepends. The first read paints
        # the modal; every later RPC times out, which is what `_run_mux` turns
        # into DispatchAskError.
        if argv[1:4] == ["mux", "pane", "read"] and not calls:
            calls.append(argv)
            return SimpleNamespace(returncode=0, stdout=modal, stderr="")
        raise DispatchAskError("mux did not answer", exit_code=1)

    calls: list = []
    state, detail, source, pane = _submit_spawn_seed(
        "agy", "sess", 3, "seed", runner_that_times_out_after_the_modal
    )

    assert state == "unconfirmed", "a pane behind a live modal must not keep a row"
    assert source != "trust-cleared", "nothing read back that the gate cleared"
    assert "modal may still be up" in detail
    # The modal WAS read on the first call, so the instrument worked. The doubt
    # here is about the gate, which is what `unconfirmed` already says.
    assert pane == "painted"


def test_codex_project_trust_prompt_refuses_before_seed_and_reaps(
    tmp_path: Path, monkeypatch
) -> None:
    from fno.agents.mux_spawn import DispatchAskError
    from fno.agents.registry import load_registry

    modal = "Do you trust the contents of this directory?"
    runner = FakeRunner(read_stdout=modal)

    with pytest.raises(DispatchAskError) as exc:
        _spawn(monkeypatch, tmp_path, provider="codex", runner=runner)

    detail = str(exc.value)
    assert "Codex project trust required" in detail
    assert str(tmp_path) in detail
    assert "trust_level = \"trusted\"" in detail
    assert not any(call[1:4] == ["mux", "pane", "send"] for call in runner.calls)
    assert runner.kill_calls
    assert load_registry() == []


def test_codex_hook_review_prompt_refuses_before_seed_and_reaps(
    tmp_path: Path, monkeypatch
) -> None:
    from fno.agents.mux_spawn import DispatchAskError
    from fno.agents import mux_spawn

    # A codex too old or unknown for the hook-trust flag still meets the
    # modal, and the spawn must refuse it.
    monkeypatch.setattr(mux_spawn, "_codex_cli_version", lambda: None)
    from fno.agents.registry import load_registry

    runner = FakeRunner(read_stdout="Hooks need review")

    with pytest.raises(DispatchAskError) as exc:
        _spawn(monkeypatch, tmp_path, provider="codex", runner=runner)

    detail = str(exc.value)
    assert "Codex hooks need review" in detail
    assert "/hooks" in detail
    assert "binding" not in detail.lower()
    assert not any(call[1:4] == ["mux", "pane", "send"] for call in runner.calls)
    assert runner.kill_calls
    assert load_registry() == []


def test_codex_hook_review_bypass_rides_the_bounded_pane(
    tmp_path: Path, monkeypatch
) -> None:
    """The hook-trust bypass rides every codex pane. The bounded posture keeps
    the seed off argv, so the pane delivers it once the TUI is ready."""
    from fno.agents import mux_spawn

    monkeypatch.setattr(mux_spawn, "_codex_cli_version", lambda: (0, 148, 0))
    runner = FakeRunner(read_stdout="Hooks need review")

    result, _ = _spawn(monkeypatch, tmp_path, provider="codex", runner=runner)

    run_call = next(call for call in runner.calls if call[1:4] == ["mux", "pane", "run"])
    assert "--dangerously-bypass-hook-trust" in run_call
    assert result.seed == "submitted"
    assert result.seed_source == "delivered"


def test_a_timed_out_submit_keeps_the_row_and_never_retries(
    tmp_path: Path, monkeypatch
) -> None:
    """A mux RPC timeout says the instrument did not answer, never that the
    keystroke failed to land. `mux pane send --submit` can deliver the Enter and
    then miss its own reply, so the worker is already running its task.

    Reaping there kills a live worker and leaves no row saying it existed.
    Retrying there types the seed a second time into a pane that already has it.
    So this state does neither: the row survives, one send was made, and the
    receipt carries the uncertainty."""
    from fno.agents.registry import load_registry

    calls = _seed_script(monkeypatch, "unknown")  # a retry would raise StopIteration

    _spawn(monkeypatch, tmp_path)

    assert len(calls) == 1, "an unanswered submit must not be re-sent"
    rows = load_registry()
    assert len(rows) == 1, "a worker that may be running must keep its row"


@pytest.mark.parametrize("provider", ["claude", "codex", "agy", "opencode", "pi"])
def test_a_fenced_model_lands_on_the_row_for_every_harness(
    tmp_path: Path, monkeypatch, provider: str
) -> None:
    """A grid pick with no route rides the fence as `--model`. The row records
    it as the request, or it reads null while the harness runs that model."""
    from fno.agents.registry import load_registry

    _spawn(monkeypatch, tmp_path, provider=provider, passthrough=["--model", "m-low"])

    (row,) = load_registry()
    assert (row.model, row.model_basis, row.requested_model) == ("m-low", "requested", "m-low")


def test_explicit_happy_monitor_refuses_separate_model_before_runner(
    tmp_path: Path, monkeypatch
) -> None:
    from fno.agents.dispatch import DispatchAskError
    from fno.agents.model_routing import resolve_explicit_route

    monkeypatch.setenv("ZAI_API_KEY", "zai-secret")
    route = resolve_explicit_route("zai", "glm-5.2")
    assert route is not None
    runner = FakeRunner()
    with pytest.raises(DispatchAskError, match="separate --model") as exc:
        _spawn(
            monkeypatch,
            tmp_path,
            monitor="happy",
            route_provider="zai",
            route_env=route,
            model="claude-opus-4-1",
            runner=runner,
        )

    assert exc.value.exit_code == 2
    assert runner.calls == []


def test_explicit_happy_monitor_refuses_noncanonical_route_override(
    tmp_path: Path, monkeypatch
) -> None:
    from fno.agents.dispatch import DispatchAskError

    monkeypatch.setenv("ZAI_API_KEY", "zai-secret")
    runner = FakeRunner()
    with pytest.raises(DispatchAskError, match="resolved zai route") as exc:
        _spawn(
            monkeypatch,
            tmp_path,
            monitor="happy",
            route_provider="zai",
            route_env={
                **_ROUTE,
                "ANTHROPIC_MODEL": "glm-5.2",
                "ANTHROPIC_DEFAULT_OPUS_MODEL": "claude-opus-4-1",
            },
            runner=runner,
        )

    assert exc.value.exit_code == 2
    assert runner.calls == []


def test_explicit_happy_monitor_refuses_route_missing_canonical_tiers(
    tmp_path: Path, monkeypatch
) -> None:
    from fno.agents.dispatch import DispatchAskError

    monkeypatch.setenv("ZAI_API_KEY", "zai-secret")
    runner = FakeRunner()
    with pytest.raises(DispatchAskError, match="resolved zai route") as exc:
        _spawn(
            monkeypatch,
            tmp_path,
            monitor="happy",
            route_provider="zai",
            route_env={**_ROUTE, "ANTHROPIC_MODEL": "glm-5.2"},
            runner=runner,
        )

    assert exc.value.exit_code == 2
    assert runner.calls == []


def test_explicit_happy_monitor_refuses_route_that_does_not_match_zai(
    tmp_path: Path, monkeypatch
) -> None:
    from fno.agents.dispatch import DispatchAskError

    monkeypatch.setenv("ZAI_API_KEY", "zai-secret")
    runner = FakeRunner()
    with pytest.raises(DispatchAskError, match="resolved zai route") as exc:
        _spawn(
            monkeypatch,
            tmp_path,
            monitor="happy",
            route_provider="zai",
            route_env={
                "ANTHROPIC_BASE_URL": "https://api.anthropic.com",
                "ANTHROPIC_AUTH_TOKEN": "anthropic-secret",
                "ANTHROPIC_MODEL": "claude-opus-4-1",
            },
            runner=runner,
        )

    assert exc.value.exit_code == 2
    assert runner.calls == []


def test_cmd_spawn_refuses_a_monitor_other_than_happy(tmp_path: Path, monkeypatch) -> None:
    from typer.testing import CliRunner

    import fno.agents.cli as agents_cli

    use_tmpdir(monkeypatch, tmp_path)
    monkeypatch.setenv("FNO_AGENTS_RUNTIME", "python")
    args = [
        "spawn", "--name", "peer", "--harness", "claude", "--monitor", "other", "hello",
    ]
    result = CliRunner().invoke(agents_cli.agents_app, args)

    assert result.exit_code == 2, result.output
    assert "happy" in result.output


def test_an_unconfirmed_send_is_never_retried(tmp_path: Path, monkeypatch) -> None:
    """`unconfirmed` means text was delivered and the submission was not
    confirmed, so the pane's buffer may already hold the seed. A retry there
    re-derives the payload from a frame containing a partial seed and submits
    fragment+seed into a live pane. Failing the spawn is the safer answer."""
    from fno.agents.mux_spawn import DispatchAskError
    from fno.agents.registry import load_registry

    calls = _seed_script(monkeypatch, "unconfirmed")  # a second call raises StopIteration

    with pytest.raises(DispatchAskError):
        _spawn(monkeypatch, tmp_path)

    assert len(calls) == 1, "one attempt, then fail - never a second send"
    assert load_registry() == []


def test_opencode_spawn_never_claims_another_rows_session_id(
    tmp_path: Path, monkeypatch
) -> None:
    """Two panes racing in one cwd must not both stamp the same session id.

    The second pane's session may not exist yet when both backfills query, so
    each sees the SAME lone candidate and the ambiguity rule cannot fire. The
    loser drops to live-only rather than pointing resume at the other pane.
    """
    from fno.agents.registry import load_registry

    ses = "ses_09679f284ffeJv7NdBAoLQLnLZ"
    _spawn(
        monkeypatch, tmp_path, name="oc-a",
        provider="opencode", runner=FakeRunner(db_stdout=f"{ses}\n"),
    )
    # Second pane, same cwd, backfill returns the SAME id (the race).
    _spawn(
        monkeypatch, tmp_path, name="oc-b",
        provider="opencode", runner=FakeRunner(db_stdout=f"{ses}\n"),
    )
    rows = {r.name: r.harness_session_id for r in load_registry()}
    assert rows["oc-a"] == ses, "the first pane to land owns the id"
    assert rows["oc-b"] is None, "the loser must not share the id"


def test_unanswered_run_with_two_matching_panes_refuses_naming_both(
    tmp_path: Path, monkeypatch
) -> None:
    from fno.agents.mux_spawn import DispatchAskError, _MUX_CONTROL_UNANSWERED
    from fno.agents.registry import load_registry

    _patch_process_create_time_far_future(monkeypatch)
    ls_stdout = json.dumps(
        [
            {"pane_id": 9, "squad_id": 1, "tab_id": 1, "cwd": str(tmp_path), "child_pid": 4242},
            {"pane_id": 10, "squad_id": 1, "tab_id": 1, "cwd": str(tmp_path), "child_pid": 4343},
        ]
    )
    runner = FakeRunner(run_returncode=_MUX_CONTROL_UNANSWERED, ls_stdout=ls_stdout)

    with pytest.raises(DispatchAskError) as exc:
        _spawn(monkeypatch, tmp_path, runner=runner)

    message = str(exc.value)
    assert "9" in message and "10" in message
    assert load_registry() == []


def test_undeclared_capability_flags_refused_with_dashed_advice(
    tmp_path: Path, monkeypatch
) -> None:
    """AC6-ERR: capability-keyed flags refuse naming the flag and the harness,
    advising the vendor's own flag after `--`. No value is inferred and no
    declared harness's mapping is borrowed."""
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents.dispatch import DispatchAskError
    from fno.agents.mux_spawn import dispatch_spawn_pane

    _stub_harness_bin(monkeypatch, tmp_path, "nanoclaw")
    runner = FakeRunner()
    with pytest.raises(DispatchAskError) as caught:
        dispatch_spawn_pane(
            name="peer", message="", provider="nanoclaw", cwd=tmp_path,
            runner=runner, model="gpt-x",
        )
    msg = str(caught.value)
    assert caught.value.exit_code == 2
    assert "nanoclaw" in msg
    assert "--model" in msg, msg
    assert "capability row" in msg
    assert "'--'" in msg
    assert runner.calls == []


def test_pane_refusal_names_headless_when_the_harness_has_a_one_shot_form(tmp_path: Path) -> None:
    from fno.agents.dispatch import DispatchAskError
    from fno.agents.mux_spawn import build_pane_argv

    with pytest.raises(DispatchAskError, match="zcode' has no interactive pane form; --substrate headless"):
        build_pane_argv("zcode", "", tmp_path, False, None)


def test_stable_tab_id_refuses_ordinal_before_spawn(tmp_path: Path, monkeypatch) -> None:
    from fno.agents.mux_spawn import DispatchAskError

    runner = FakeRunner()
    with pytest.raises(DispatchAskError, match="stable-id"):
        _spawn(monkeypatch, tmp_path, tab_id="2", runner=runner)
    assert runner.calls == []


def _pane_repo(tmp_path: Path) -> Path:
    import subprocess

    repo = tmp_path / "repo"
    repo.mkdir()
    subprocess.run(["git", "init", "-q"], cwd=repo, check=True)
    return repo
