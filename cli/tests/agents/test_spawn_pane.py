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
import subprocess
from pathlib import Path
from typing import Optional


import pytest



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


# ---------------------------------------------------------------------------
# x-3e38 pane placement: squad/split on the outer pane-run transport
# ---------------------------------------------------------------------------


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


# -- x-1499: the visible ordinal contract -------------------------------------


# ---------------------------------------------------------------------------
# _mesh_env_wrapper: a routed pane scrubs the parent's Anthropic creds (x-db50)
# ---------------------------------------------------------------------------


# -- x-6928: agents spawn --at current + interactive-readiness gate ----------


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


def _pane_run_argv(runner: "FakeRunner") -> list[str]:
    for call in runner.calls:
        if call[1:4] == ["mux", "pane", "run"]:
            return call[call.index("--") + 1 :]
    raise AssertionError("no `mux pane run` call recorded")


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


# -- an unanswered control read is reconciled, never asserted absent --
#
# `mux pane run` exiting `_MUX_CONTROL_UNANSWERED` means the command reached the
# server and the reply never came back - not that no pane was created. These
# pin the Python side of that reconciliation: adopt the one pane that proves
# it is this spawn, refuse (writing no row) on zero/many/unknown, and never
# let a recovered pane earn its row without proving it actually started.


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


def _pane_repo(tmp_path: Path) -> Path:
    import subprocess

    repo = tmp_path / "repo"
    repo.mkdir()
    subprocess.run(["git", "init", "-q"], cwd=repo, check=True)
    return repo
