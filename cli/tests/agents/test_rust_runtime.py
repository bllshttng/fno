"""Tests for the ``fno agents`` Rust routing seam.

Covers gating (``FNO_AGENTS_RUNTIME``), argv forwarding, the missing-binary
refusals, which spawns stay on Python, the codex git-grant and sandbox-probe
refusals, and the command-set drift guards against client.rs. The actual
``os.execv`` is always stubbed so the test process is never replaced.
"""
from __future__ import annotations
from tests.fixtures.graph_seed import seed_graph

import json
import os
import stat
import subprocess
from pathlib import Path

import pytest
from typer.testing import CliRunner

from fno import rust_binary
from fno.agents import rust_runtime as rr


@pytest.fixture(autouse=True)
def _open_node_row(monkeypatch, tmp_path):
    """The x-8d88 seam derives the node from a /target-family seed, so every
    x-f370 seed here resolves against one open, /target-bearing row instead
    of the real graph, where the historical node is long done and the
    tightened guard would refuse the spawn before the behavior under test."""
    (tmp_path / "p.md").write_text("---\n---\n", encoding="utf-8")
    row = {
        "id": "x-f370",
        "slug": "codex-seed",
        "dispatch_command": "/target",
        "difficulty": "medium",
        "status": "in_progress",
        "plan_path": str(tmp_path / "p.md"),
        "cwd": str(tmp_path),
    }
    monkeypatch.setattr("fno.graph.store.read_nodes_by_ids", lambda path, tokens: {"entries": [row], "missing": []})
    # resolve_seed_node reads through fno.graph.load.load_graph, a second
    # reader the stub above does not cover: where a real store answers (CI's
    # changed-smoke env), the seed lookup missed and refused the spawn before
    # the behavior under test. Answer the loader with the same row.
    monkeypatch.setattr(
        "fno.graph.load.load_graph", lambda *a, **k: [{"id": "x-f370", "slug": "codex-seed"}]
    )
    # The command reads the store itself for the nodeless derive: point its
    # FNO_HOME at a fixture graph naming the same row.
    monkeypatch.setenv("FNO_HOME", str(tmp_path))
    seed_graph(tmp_path / "graph.json", json.dumps({"entries": [{"id": "x-f370"}]}))
    monkeypatch.setattr(
        "fno.agents.cli._spawn_guard_decision",
        lambda *a, **k: (
            {
                "verdict": "dispatchable",
                "reservation_key": "dispatch:x-f370",
                "reservation_holder": "test-holder",
            },
            0,
        ),
    )

# Declared journal isolation: the conftest pin keys on this module's name
# (see _PLAN_JOURNAL_PINNED_MODULES) and the guard
# scripts/ci/check-tests-hermetic-events.sh proves this marker did not rot.
FNO_EVENTS_PATH = "hermetic: this module's journal is pinned per test"


@pytest.fixture(autouse=True)
def _isolated_registry(monkeypatch, tmp_path_factory):
    """Default the agents registry to a missing path so tests don't pick up
    the developer's real ``~/.fno/agents/registry.json``. The routing
    tests stub ``route_to_rust`` so the Rust client (which reads the registry to
    decide create-vs-resume) never runs, but isolating the path keeps the suite
    deterministic on a dev machine with real registered agents.

    Tests that need a specific registry override this with their own
    ``monkeypatch.setattr(_paths, "agents_registry_path", ...)`` -- the
    test-local setattr wins because monkeypatch records overrides in order.
    """
    import fno.paths as _paths

    missing = tmp_path_factory.mktemp("registry-isolated") / "missing.json"
    monkeypatch.setattr(_paths, "agents_registry_path", lambda: missing)


def _make_exe(path: Path) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("#!/bin/sh\nexit 0\n")
    path.chmod(path.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
    return path


# --------------------------------------------------------------------------- #
# runtime_mode: rust | python | auto
# --------------------------------------------------------------------------- #

@pytest.mark.parametrize(
    "value,expected",
    [
        (" RUST ", "rust"),
        ("python", "python"),
        # Unset / empty / unrecognized all collapse to the default: auto.
        ("rs", "auto"),
    ],
)
def test_runtime_mode_resolution(monkeypatch, value, expected) -> None:
    monkeypatch.setenv(rr.RUNTIME_ENV, value)
    assert rr.runtime_mode() == expected


# --------------------------------------------------------------------------- #
# route_to_rust: argv forwarding + missing-binary error
# --------------------------------------------------------------------------- #

def test_route_forwards_command_and_args(monkeypatch, tmp_path) -> None:
    binary = _make_exe(tmp_path / rust_binary.BINARY_NAME)
    calls: list = []

    def fake_exec(path, argv):
        calls.append((path, list(argv)))
        raise SystemExit(0)  # mimic process replacement terminating the test path

    with pytest.raises(SystemExit) as exc:
        rr.route_to_rust(
            ["ask", "worker-A", "hello", "--harness", "codex"],
            _exec=fake_exec,
            _resolve=lambda: binary,
        )
    assert exc.value.code == 0
    assert calls == [(str(binary), [str(binary), "ask", "worker-A", "hello", "--harness", "codex"])]


def test_route_missing_binary_exits_127(capsys) -> None:
    with pytest.raises(SystemExit) as exc:
        rr.route_to_rust(["list"], _resolve=lambda: None)
    assert exc.value.code == rr.BIN_NOT_FOUND_EXIT
    err = capsys.readouterr().err
    assert rust_binary.BINARY_NAME in err
    assert "not found" in err


# --------------------------------------------------------------------------- #
# Integration: fno agents short-circuits ONLY when opted in
# --------------------------------------------------------------------------- #

def test_spawn_seam_injects_config_defaults(
    monkeypatch, tmp_path
) -> None:
    """x-de9d US8, as the axis retirement left it: the seam composes a bare
    spawn through the real binary against a real config TOML. The launch
    values moved to the lane template, so the composed argv carries the
    seam's own marker and no harness axis of its own."""
    import pytest

    from fno.rust_binary import find_dev_binary

    binary = find_dev_binary()
    if binary is None:
        pytest.skip("no fno-agents dev build (cargo build -p fno-agents)")
    monkeypatch.setenv("FNO_AGENTS_BIN", str(binary))
    config_path = tmp_path / "config.toml"
    config_path.write_text(
        '[agents.defaults]\nprovider = "codex"\nmodel = "gpt-5.6-sol"\n',
        encoding="utf-8",
    )
    monkeypatch.setenv("FNO_CONFIG", str(config_path))
    monkeypatch.setenv("FNO_NO_CANONICAL_CONFIG", "1")
    monkeypatch.setenv("FNO_EVENTS_PATH", str(tmp_path / "events.jsonl"))

    from fno.cli import app

    captured: list = []

    def fake_route(args, **kw):
        captured.append(list(args))
        raise SystemExit(0)

    # --once keeps the spawn on the Rust route; every other lane resolves in
    # the Python body.
    monkeypatch.setenv(rr.RUNTIME_ENV, "rust")
    monkeypatch.setattr(rr, "route_to_rust", fake_route)
    result = CliRunner().invoke(
        app, ["agents", "spawn", "--name", "worker-A", "hi", "--once"]
    )
    assert result.exit_code == 0, result.output
    argv = captured[0]
    assert argv[0] == "spawn"
    assert "--defaults-applied=unenforced" in argv
    assert "--harness" not in argv, "the lane template owns the launch axis"
    # positionals still resolvable after the composed flags
    assert "worker-A" in argv


def test_codex_code_spawn_refuses_when_git_grant_is_unresolved(
    monkeypatch, tmp_path
) -> None:
    """A bounded code worker is refused before the route can mint a row."""
    from fno.cli import app

    called: list[list[str]] = []
    monkeypatch.setenv(rr.RUNTIME_ENV, "rust")
    monkeypatch.setattr(rr, "route_to_rust", lambda args, **kw: called.append(list(args)))
    result = CliRunner().invoke(
        app,
        [
            "agents",
            "spawn",
            "$fno:target x-f370",
            "--harness",
            "codex",
            "--cwd",
            str(tmp_path),
            "--node",
            "x-f370",
        ],
    )

    assert result.exit_code == 2
    assert str(tmp_path) in result.output
    assert "git rev-parse" in result.output
    assert called == []


def test_codex_code_payload_after_provider_fence_is_checked() -> None:
    assert rr._is_codex_code_payload(
        ["spawn", "--", "$fno:target x-f370"]
    )
    assert rr._is_codex_code_payload(
        ["spawn", "--message=$fno:target x-f370"]
    )


@pytest.mark.parametrize(
    "verdict,blocked,routed,marker",
    [
        ("blocked", [("gh", "error connecting to api.github.com")], False, "sandbox-probe: gh is unreachable"),
        ("unknown", [], True, "launching unprobed"),
    ],
)
def test_bounded_codex_code_spawn_is_probed_before_it_routes(
    monkeypatch, tmp_path, verdict, blocked, routed, marker, loop_admission_ready
) -> None:
    """x-8d88: the derived node takes the Python dispatch lane, so the
    routed branch is observed at the codex thread terminus."""
    from fno.agents import sandbox_probe
    from fno.cli import app

    spawned: list[dict] = []

    def fake_thread_spawn(**kw):
        spawned.append(dict(kw))
        return "fake-thread-id"

    probed: list[Path] = []

    def fake_probe(cwd, **kw):
        probed.append(cwd)
        return sandbox_probe.SandboxProbe(verdict, blocked, "codex: command not found")

    repo = tmp_path / "repo"
    subprocess.run(["git", "init", "-q", str(repo)], check=True)
    monkeypatch.setenv(rr.RUNTIME_ENV, "rust")
    monkeypatch.setattr(rr, "route_to_rust", lambda args, **kw: None)
    monkeypatch.setattr(sandbox_probe, "probe_codex_sandbox", fake_probe)
    monkeypatch.setattr("fno.agents.dispatch._codex_thread_spawn", fake_thread_spawn)
    result = CliRunner().invoke(
        app,
        [
            "agents",
            "spawn",
            "$fno:target x-f370",
            "--harness",
            "codex",
            "--cwd",
            str(repo),
        ],
    )

    assert probed == [repo]
    assert bool(spawned) is routed
    if routed:
        assert result.exit_code == 0, result.output
    else:
        assert result.exit_code == sandbox_probe.EXIT_SANDBOX_UNREACHABLE
        assert "remedy:" in result.output
    if marker:
        assert marker in result.output


def test_sandbox_probe_runs_only_for_bounded_codex_code_spawns(monkeypatch, tmp_path) -> None:
    from fno.agents import sandbox_probe

    probed: list[Path] = []
    monkeypatch.setattr(
        sandbox_probe,
        "probe_codex_sandbox",
        lambda cwd, **kw: probed.append(cwd) or sandbox_probe.SandboxProbe("reachable"),
    )
    base = ["spawn", "$fno:target x-f370", "--harness", "codex", "--substrate", "thread", "--cwd", str(tmp_path)]
    rr._refuse_codex_spawn_with_unreachable_tools(base)
    assert probed == [tmp_path]

    for argv in (
        [*base, "--yolo"],
        [*base, "--once"],
        [*base, "--permission-mode", "danger-full-access:never"],
        ["spawn", "hello", *base[2:]],
        [*base[:3], "claude", *base[4:]],
    ):
        rr._refuse_codex_spawn_with_unreachable_tools(argv)
    assert probed == [tmp_path]


def test_agents_help_falls_through_when_opted_in(monkeypatch) -> None:
    """`fno agents --help` stays on the Python help even with the env set."""
    from fno.cli import app

    def boom(args):
        raise AssertionError("should not exec for --help")

    monkeypatch.setenv(rr.RUNTIME_ENV, "rust")
    monkeypatch.setattr(rr, "route_to_rust", boom)
    result = CliRunner().invoke(app, ["agents", "--help"])
    assert result.exit_code == 0, result.output
    assert "agent" in result.output.lower()


# --------------------------------------------------------------------------- #
# Default flip: auto mode routes only the daemon-native commands (no Python
# contract to regress) to an *installed* binary; commands Python owns stay on
# Python. `<command> [--help]` exercises the routing decision in make_context
# without running a real command body or a spawn (route_to_rust is stubbed).
# A `called[]` list records routing so fall-through cases assert on routing,
# not on whatever exit code the Python dispatch happens to produce.
# --------------------------------------------------------------------------- #

def test_auto_routes_daemon_native_command_when_installed(monkeypatch, tmp_path) -> None:
    """Default (auto): a Rust-only command (no Python contract) routes to the binary."""
    from fno.cli import app

    binary = _make_exe(tmp_path / rust_binary.BINARY_NAME)
    captured: list = []

    def fake_route(args, **kw):
        captured.append((list(args), kw.get("binary")))
        raise SystemExit(99)

    monkeypatch.delenv(rr.RUNTIME_ENV, raising=False)
    monkeypatch.setattr(rust_binary, "resolve_installed_binary", lambda: binary)
    monkeypatch.setattr(rr, "route_to_rust", fake_route)
    # `status` exists only in the Rust client, so routing it regresses nothing.
    result = CliRunner().invoke(app, ["agents", "status"])
    assert result.exit_code == 99
    assert captured == [(["status"], binary)]


def test_rust_mode_forces_even_shared_command(monkeypatch) -> None:
    """=rust forces the binary for every command, including the shared/Python-only
    ones (the caller explicitly accepts the Rust surface; the binary owns the
    flag/output behavior)."""
    from fno.cli import app

    captured: list = []

    def fake_route(args, **kw):
        captured.append(list(args))
        raise SystemExit(99)

    monkeypatch.setenv(rr.RUNTIME_ENV, "rust")
    monkeypatch.setattr(rr, "route_to_rust", fake_route)
    result = CliRunner().invoke(app, ["agents", "list", "worker-A"])
    assert result.exit_code == 99
    assert captured == [["list", "worker-A"]]


# --------------------------------------------------------------------------- #
# Command-set invariants + cross-source drift guards.
# --------------------------------------------------------------------------- #

def test_python_agent_commands_match_registered_commands() -> None:
    """PYTHON_AGENT_COMMANDS mirrors the Python-owned @agents_app.command registrations.

    Every Python ``@agents_app.command`` is preserved as fallback dispatch for
    ``FNO_AGENTS_RUNTIME=python`` mode and no-binary environments. Rust-backed
    commands auto-route; ``role`` remains Python-owned because no Rust client
    port exists. The guard keeps any future Python-only command from silently
    escaping the explicit set.
    """
    from fno.agents.cli import agents_app

    registered = {cmd.name for cmd in agents_app.registered_commands}
    # Every registered command has a Rust client port and auto-routes; they
    # remain registered as the =python fallback but are no longer carved out.
    # ``ask`` joined this set with the gemini port + unconditional flip.
    rust_parity_commands = {
        "stop",
        "rm",
        "list",
        "reconcile",
        "drive-authority",
        "trace",
        "resume",
        "attach",
        "logs",
        "ask",
        # x-1b75: ported to the Rust client beside trace (same daemon-free,
        # client-side dispatch shape). The Python command stays registered as
        # the FNO_AGENTS_RUNTIME=python / no-binary refusal.
        "registry-json",
        # Task 1.2: spawn gains a Python implementation (--once / claude plain
        # spawn) but stays in RUST_CLIENT_COMMANDS + AUTO_ROUTE_COMMANDS so the
        # daemon PTY worker path (codex/gemini without --once) still auto-routes
        # to Rust. It therefore belongs in this parity set: registered AND has
        # a Rust client port.
        "spawn",
    }
    # Folded single-command leaves whose command has NO Rust client port: they
    # left ``registered_commands`` when they moved out of cli.py, so the
    # RUST_CLIENT_COMMANDS intersection below cannot see them. Naming them here
    # keeps the guard's teeth: a new folded leaf must be added to
    # PYTHON_AGENT_COMMANDS and named here, or the assert fires. (`reseat` left
    # when the whole command moved to the native `fno mux thread reseat`.)
    folded_python_leaves: set[str] = set()
    lazy_python_owned = (
        set(rr.FOLDED_AGENT_SUBCOMMANDS) & set(rr.RUST_CLIENT_COMMANDS)
    ) | folded_python_leaves
    python_owned = (registered - rust_parity_commands) | lazy_python_owned
    assert python_owned == set(rr.PYTHON_AGENT_COMMANDS), (
        "PYTHON_AGENT_COMMANDS is out of sync with the agents_app commands.\n"
        f"  only registered (excl. rust-parity): {sorted(python_owned - set(rr.PYTHON_AGENT_COMMANDS))}\n"
        f"  only in PYTHON_AGENT_COMMANDS: {sorted(set(rr.PYTHON_AGENT_COMMANDS) - python_owned)}"
    )
    # All rust-parity commands must auto-route (not be in PYTHON_AGENT_COMMANDS).
    for v in rust_parity_commands:
        assert v not in rr.PYTHON_AGENT_COMMANDS, f"{v} should not be in PYTHON_AGENT_COMMANDS after parity"
        assert v in rr.AUTO_ROUTE_COMMANDS, f"{v} must be in AUTO_ROUTE_COMMANDS after parity"


def _find_repo_root(start: Path) -> Path | None:
    """Walk up from ``start`` to the first ancestor containing ``crates/`` (the
    workspace root). Robust to the test file moving between ``cli/tests/...``
    depths -- unlike a fixed ``parents[N]`` index."""
    for parent in [start, *start.parents]:
        if (parent / "crates" / "fno-agents" / "src" / "bin" / "client.rs").is_file():
            return parent
    return None


def test_rust_client_commands_match_client_rs() -> None:
    """RUST_CLIENT_COMMANDS mirrors the routable commands in client.rs.

    Parses the Rust client source and compares its dispatchable commands to the
    Python-side allowlist so a new command on either side fails CI instead of
    silently mis-routing. Skipped when the crate source is absent (e.g. an
    installed sdist test env without the workspace).
    """
    import re

    repo_root = _find_repo_root(Path(__file__).resolve().parent)
    if repo_root is None:
        pytest.skip("crates/fno-agents/src/bin/client.rs not present in this checkout")
    client_rs = repo_root / "crates" / "fno-agents" / "src" / "bin" / "client.rs"

    # Strip `//` line comments before matching so a commented-out or
    # illustrative `"x" =>` in a comment can't inject a phantom command (gemini).
    # client.rs has no string literal containing `//`, so a plain split is safe
    # here; revisit if that ever changes.
    src = "\n".join(line.split("//", 1)[0] for line in client_rs.read_text().splitlines())
    # build_request match arms: `"verb" =>`  (drive flags are `--…`, excluded by
    # [a-z] start). Scope the scan to build_request's body: since Task 1.3a the
    # file also contains a PROVIDER match (`"claude" =>` inside maybe_run_spawn)
    # whose arms are not commands and must not leak into this set.
    build_request_body = src.split("fn build_request", 1)[1].split("\nfn ", 1)[0]
    arms = set(re.findall(r'"([a-z][a-z0-9-]*)"\s*=>', build_request_body))
    # Commands dispatched before build_request via `command == "…"` (drive, status; the
    # `--emit-schema` flag starts with `-` so it is excluded by the same anchor).
    specials = set(re.findall(r'command == "([a-z][a-z0-9-]*)"', src))
    # Hidden direct-dispatch arms: the binary serves them before build_request,
    # but Python never routes them. `board` is reached only through
    # `fno inbox board` shelling to the binary via resolve_binary (d-e11b2b3e);
    # adding it to RUST_CLIENT_COMMANDS would mint a second surface for it.
    # `notify-watch` is the same shape: the pr-watch tick phase and the arms
    # readout are its only callers, both through resolve_binary.
    # The three territory doors are the same shape: `active-backlog-receipt`
    # and `territory-rows` are reached through the config passthroughs, and
    # `territory-verdict` through the spawn gate's delegate - every caller
    # uses resolve_binary directly, never `fno agents <command>` routing.
    # `node-route` is shelled by the squad prune's cascade fold for a verdict
    # per unknown member; `roster-reap` is the roster sweep's own door. Both
    # are binary-first surfaces, never `fno` auto-routes.
    # `reclaim` is `fno doctor reclaim`'s door and `plugin-install` is
    # `fno config plugin install`'s: both thin Python wrappers shell the
    # binary through resolve_binary, never auto-route.
    # `publish-review` is the reviewer lane's bot-identity producer: the emit
    # chokepoint and the hidden `fno do pr publish-review` command shell it through
    # command_call, never auto-route.
    # `canonical-check` (x-a150) is the same shape: the post-merge sync shells
    # it through command_call with a stdin JSON payload; an argv route would break.
    # `sync-canonical` (x-85df) is the same shape again: the post-merge sync,
    # its catch-up sweep and its staleness alarm all shell through command_call
    # with a stdin JSON payload, never an argv route.
    routable = arms | (
        specials
        - {
            "board",
            "notify-watch",
            "active-backlog-receipt",
            "territory-rows",
            "territory-verdict",
            "node-route",
            "roster-reap",
            "reclaim",
            "plugin-install",
            "publish-review",
            "canonical-check",
            "sync-canonical",
            # The patch door (x-665f): resolve_binary-only, like board - the
            # client action list is shrink-only (d-fe66560a), so this arm is
            # never registered and never routed.
            "backlog-update",
            # The provenance fold: `fno doctor intel` shells the binary
            # through resolve_binary, never `fno agents intel` routing; the
            # arm stays unregistered for the same shrink law.
            "intel",
            # The stall-cause table: `fno doctor stalls` shells the binary
            # through resolve_binary, the same shape as intel above.
            "stalls",
            # `fno backlog session backfill` shells the binary through
            # resolve_binary, never `fno agents` routing.
            "session-backfill",
            # `release-notes` is the update modal's notes builder: Python's
            # `fno doctor update --check` resolver shells it through
            # command_call with a stdin JSON payload, never an argv route.
            "release-notes",
            # `update-journal` is the update lifecycle's transport-only door:
            # crates/fno/src/doctor_update.rs execs the binary with the command
            # as the first token (like `backlog`), so it dispatches on the
            # `command ==` arm and is never routed or help-listed.
            "update-journal",
            # `chats` is the conversation store's read model: the bus log and
            # reply resolver shell it through command_call with a stdin JSON
            # payload, never an argv route.
            "chats",
            # `mail-threads` is the messages tab's projection over the chats
            # store: the fno client's gather and details door shell it
            # directly, never an argv route.
            "mail-threads",
            # The agents fold took these off the user CLI. The binary keeps
            # its arms for the callers that exec it directly, so `fno agents
            # <command>` reaches the tombstone, never the binary.
            "team-fold",
            "team-orphans",
            "term-ledger",
        }
    )

    # d-fe66560a keeps the binary's action list shrink-only, so these leaves
    # have no `command ==` arm of their own: the router rewrites each into an
    # argument of the `status` action before the exec. They stay routable
    # commands, and the rewrite target's flag must exist in client.rs or the
    # route dies as an unknown command at the binary.
    for leaf, flag in sorted(rr._STATUS_ARG_LEAVES.items()):
        assert f'"{flag}"' in src, (
            f"rewrite target {flag} for routed leaf {leaf} is missing from client.rs"
        )
    routable |= set(rr._STATUS_ARG_LEAVES)

    assert routable == set(rr.RUST_CLIENT_COMMANDS), (
        "RUST_CLIENT_COMMANDS is out of sync with client.rs routable commands.\n"
        f"  only in client.rs: {sorted(routable - set(rr.RUST_CLIENT_COMMANDS))}\n"
        f"  only in RUST_CLIENT_COMMANDS: {sorted(set(rr.RUST_CLIENT_COMMANDS) - routable)}"
    )


def test_harness_markers_match_rust_consumers() -> None:
    """Rust marker tables mirror Python HARNESS_SESSION_MARKERS, order included.

    The Rust unit test only checks the Rust const against a hard-coded Rust
    literal, so a drift on the Python side leaves both suites green while the two
    runtimes infer different harnesses. This reads the actual Rust source and
    asserts the (env_var, harness) sequences equal Python's canonical table --
    order is load-bearing (it is the priority list). Skipped when the crate
    source is absent (installed sdist test env without the workspace).
    """
    import re

    from fno.harness_identity import HARNESS_SESSION_MARKERS

    repo_root = _find_repo_root(Path(__file__).resolve().parent)
    if repo_root is None:
        pytest.skip("crates/fno-agents/src/bin/client.rs not present in this checkout")
    rust_root = repo_root / "crates" / "fno-agents" / "src"
    expected = [tuple(p) for p in HARNESS_SESSION_MARKERS]
    for path, const_name in (
        (rust_root / "bin" / "client.rs", "HARNESS_MARKERS"),
        (rust_root / "claims.rs", "HARNESS_SESSION_MARKERS"),
    ):
        src = path.read_text()
        block = re.search(
            rf"const {const_name}[^=]*=\s*&\[(.*?)\];", src, re.DOTALL
        )
        assert block, f"{const_name} const not found in {path.name}"
        rust_pairs = re.findall(
            r'\(\s*"([^"]+)"\s*,\s*"([^"]+)"\s*\)', block.group(1)
        )
        assert rust_pairs == expected, (
            f"{const_name} is out of sync between Rust and Python.\n"
            f"  {path.name}: {rust_pairs}\n"
            f"  harness_identity.HARNESS_SESSION_MARKERS: {list(HARNESS_SESSION_MARKERS)}"
        )


# --------------------------------------------------------------------------- #
# `ask` (and every command whose Python leg was deleted) refuses without the binary.
# --------------------------------------------------------------------------- #


def test_ask_without_installed_binary_refuses(monkeypatch) -> None:
    """With no installed binary, `ask` refuses legibly: the Python ask
    runtime was ported to Rust, so there is no dispatch to fall back to."""
    from fno.cli import app

    called: list = []

    monkeypatch.delenv(rr.RUNTIME_ENV, raising=False)
    monkeypatch.setattr(rust_binary, "resolve_installed_binary", lambda: None)
    monkeypatch.setattr(rr, "route_to_rust", lambda args, **kw: called.append(list(args)))
    result = CliRunner().invoke(app, ["agents", "ask", "newagent", "hi", "--harness", "codex"])
    assert called == []
    assert result.exit_code == rr.BIN_NOT_FOUND_EXIT
    assert "ported" in result.stderr


def test_python_mode_refuses_ask(monkeypatch, tmp_path) -> None:
    """`FNO_AGENTS_RUNTIME=python` cannot force ask onto Python anymore: the
    legs were ported and deleted. The refusal names the flag so the operator
    learns the contract changed rather than staring at a missing command."""
    from fno.cli import app

    called: list = []

    monkeypatch.setenv(rr.RUNTIME_ENV, "python")
    monkeypatch.setattr(
        rust_binary, "resolve_installed_binary", lambda: tmp_path / "bin"
    )
    monkeypatch.setattr(rr, "route_to_rust", lambda args, **kw: called.append(list(args)))
    result = CliRunner().invoke(app, ["agents", "ask", "any", "hi", "--harness", "codex"])
    assert called == []
    assert result.exit_code == rr.BIN_NOT_FOUND_EXIT
    assert rr.RUNTIME_ENV in result.stderr


def test_anycast_ask_routes_verbatim_to_the_binary(monkeypatch, tmp_path) -> None:
    """`ask --to-project` takes the plain ask route: the client resolves the
    one live peer itself, so the seam never detours it through Python."""
    from fno.cli import app

    binary = _make_exe(tmp_path / rust_binary.BINARY_NAME)
    called: list = []

    def fake_route(args, **kw):
        called.append(list(args))
        raise SystemExit(0)

    monkeypatch.delenv(rr.RUNTIME_ENV, raising=False)
    monkeypatch.setattr(rust_binary, "resolve_installed_binary", lambda: binary)
    monkeypatch.setattr(rr, "route_to_rust", fake_route)
    CliRunner().invoke(app, ["agents", "ask", "--to-project", "proj", "--any", "hi"])
    assert len(called) == 1
    routed = called[0]
    assert routed[0] == "ask"
    assert routed[routed.index("--to-project") + 1] == "proj"
    assert "--any" in routed and "hi" in routed


# --------------------------------------------------------------------------- #
# Help completeness for the Rust-only commands.
#
# A bare ``fno agents --help`` always renders the *Python* group help (it never
# execs the binary), so without the RUST_ONLY_COMMAND_HELP injection it would omit
# every Rust-only command. These tests pin the no-drift guard and the legible
# fallback when the command is reached without an installed binary.
# --------------------------------------------------------------------------- #

def test_rust_only_command_help_covers_unregistered_commands() -> None:
    """RUST_ONLY_COMMAND_HELP keys == RUST_CLIENT_COMMANDS minus Python-registered commands.

    This is the drift guard that keeps the help complete forever: a future
    Rust-only command added to RUST_CLIENT_COMMANDS (and client.rs, per
    test_rust_client_commands_match_client_rs) cannot land without a help entry,
    so it can never silently vanish from ``fno agents --help`` again.
    """
    from fno.agents.cli import agents_app

    registered = {cmd.name for cmd in agents_app.registered_commands}
    expected = set(rr.RUST_CLIENT_COMMANDS) - registered - set(rr.PYTHON_AGENT_COMMANDS)
    assert set(rr.RUST_ONLY_COMMAND_HELP) == expected, (
        "RUST_ONLY_COMMAND_HELP is out of sync with the Rust-only command set.\n"
        f"  missing a help entry: {sorted(expected - set(rr.RUST_ONLY_COMMAND_HELP))}\n"
        f"  help entry for a non-Rust-only command: {sorted(set(rr.RUST_ONLY_COMMAND_HELP) - expected)}"
    )


def test_rust_only_command_no_binary_emits_install_hint(monkeypatch) -> None:
    """Auto mode + no installed binary for a Rust-only command points at the install path."""
    from fno.cli import app

    monkeypatch.delenv(rr.RUNTIME_ENV, raising=False)
    monkeypatch.setattr(rust_binary, "resolve_installed_binary", lambda: None)
    result = CliRunner().invoke(app, ["agents", "status"])
    assert result.exit_code == rr.BIN_NOT_FOUND_EXIT
    assert "Rust runtime" in result.output
    assert "No such command" not in result.output


# --------------------------------------------------------------------------- #
# Spawns carrying a Python-only flag (--role, --promote,
# --dispatch-account) never route to the binary: the Rust client cannot
# parse them. A --resume spawn DOES route: the binary's grammar knows the
# flag and refuses it with the retired-axis line. --monitor and
# --output-format left this list in row 9: the binary parses and
# refuses/launches them natively. `--help` exercises the make_context
# routing decision without running a real spawn.
# --------------------------------------------------------------------------- #


def test_role_bearing_spawn_not_routed_in_forced_rust_mode(monkeypatch, tmp_path) -> None:
    """Even FNO_AGENTS_RUNTIME=rust must not route a --role spawn to the binary."""
    from fno.cli import app

    called: list = []
    monkeypatch.setenv(rr.RUNTIME_ENV, "rust")
    monkeypatch.setattr(rr, "route_to_rust", lambda args, **kw: called.append(list(args)))
    CliRunner().invoke(app, ["agents", "spawn", "--role=tidy", "--help"])
    assert called == []


def test_monitor_bearing_spawn_routes_in_rust_mode(monkeypatch) -> None:
    """Row 9: a non-pane --monitor spawn crosses to the binary (the stub
    records the routing; the Python body still answers exit 2 with the same
    line for this once-shaped argv). The Rust door's own golden lives in
    spawn_axes' monitor_seam tests."""
    from fno.cli import app

    called: list = []
    monkeypatch.setenv(rr.RUNTIME_ENV, "rust")
    monkeypatch.setattr(rr, "route_to_rust", lambda args, **kw: called.append(list(args)))

    result = CliRunner().invoke(
        app,
        [
            "agents",
            "spawn",
            "--name",
            "peer",
            "--harness",
            "claude",
            "--monitor",
            "happy",
            "--once",
            "hello",
        ],
    )

    assert result.exit_code == 2, result.output
    assert "needs --as pane" in result.output
    assert len(called) == 1 and "--monitor" in called[0]


def test_output_format_spawn_routes_to_installed_rust(monkeypatch, tmp_path) -> None:
    """Row 9: PR-watch's JSON-envelope spawn routes to the binary, whose
    output_format_seam admits the claude-headless-json shape natively."""
    from fno.cli import app

    binary = _make_exe(tmp_path / rust_binary.BINARY_NAME)
    called: list = []
    monkeypatch.delenv(rr.RUNTIME_ENV, raising=False)
    monkeypatch.setattr(rust_binary, "resolve_installed_binary", lambda: binary)
    monkeypatch.setattr(rr, "route_to_rust", lambda args, **kw: called.append(list(args)))

    result = CliRunner().invoke(
        app,
        [
            "agents",
            "spawn",
            "--once",
            "--harness",
            "claude",
            "--output-format",
            "json",
            "--help",
        ],
    )

    assert result.exit_code == 0, result.output
    assert len(called) == 1 and "--output-format" in called[0]


def test_plain_spawn_stays_python_bg_spawn_auto_routes(monkeypatch, tmp_path) -> None:
    """A plain spawn resolves its lane in the Python body and must NOT route to
    the binary; a --once spawn still auto-routes."""
    from fno.cli import app

    binary = _make_exe(tmp_path / rust_binary.BINARY_NAME)
    captured: list = []

    def fake_route(args, **kw):
        captured.append(list(args))
        raise SystemExit(99)

    monkeypatch.delenv(rr.RUNTIME_ENV, raising=False)
    monkeypatch.setattr(rust_binary, "resolve_installed_binary", lambda: binary)
    monkeypatch.setattr(rr, "route_to_rust", fake_route)

    # Plain spawn: stays Python (the pane back half). It will fail inside the
    # Python dispatch (no mux in this test env), but must never exec the binary.
    CliRunner().invoke(app, ["agents", "spawn", "--name", "worker", "--harness", "claude"])
    assert captured == [], "a pane-substrate spawn must not route to the binary"

    # --once: still the binary's lane. The seam marker rides straight after
    # the command: it asserts the crossing upstream.
    result = CliRunner().invoke(
        app,
        ["agents", "spawn", "--name", "worker", "--harness", "claude", "--once", "hello"],
    )
    assert result.exit_code == 99
    assert len(captured) == 1
    argv = captured[0]
    assert argv[0] == "spawn"
    assert argv[1].startswith("--defaults-applied=")
    assert argv[2:] == ["--name", "worker", "--harness", "claude", "--once", "hello"]


def test_is_resume_bearing_spawn_predicate() -> None:
    # The predicate matches both spellings (the front-door normalizer rewrites
    # -r into --resume) and no longer routes: the binary's grammar knows the
    # flag and refuses a resume-bearing spawn itself.
    uuid = "0a1b2c3d-4e5f-6071-8293-a4b5c6d7e8f9"
    assert rr._is_resume_bearing_spawn("spawn", ["spawn", "w", "--resume", uuid])
    assert rr._is_resume_bearing_spawn("spawn", ["spawn", "--name", "w", f"--resume={uuid}"])
    assert rr._is_resume_bearing_spawn("spawn", ["spawn", "w", "-r", "6501096a"])
    assert not rr._is_resume_bearing_spawn("spawn", ["spawn", "w", "--substrate", "thread"])
    assert not rr._is_resume_bearing_spawn("spawn", ["spawn", "w", "--role", "tidy"])
    # A --resume flag on a non-spawn command never matches (resume is its own command).
    assert not rr._is_resume_bearing_spawn("ask", ["ask", "w", "--resume", uuid])


def test_pane_spawn_routes_to_the_binary(monkeypatch, tmp_path) -> None:
    """Every pane harness seats a Rust arm, so an explicit --as pane spawn
    routes to the binary for any harness. The old member-set carve-out is
    gone; the Python pane back half answers only FNO_AGENTS_RUNTIME=python.
    A bare spawn is NOT this shape: its default substrate still resolves in
    the Python door (see test_default_substrate_spawn_stays_python)."""
    from fno.cli import app

    binary = _make_exe(tmp_path / rust_binary.BINARY_NAME)
    captured: list = []

    def fake_route(args, **kw):
        captured.append(list(args))
        raise SystemExit(99)

    monkeypatch.delenv(rr.RUNTIME_ENV, raising=False)
    monkeypatch.setattr(rust_binary, "resolve_installed_binary", lambda: binary)
    monkeypatch.setattr(rr, "route_to_rust", fake_route)
    result = CliRunner().invoke(
        app,
        ["agents", "spawn", "--name", "w", "--as", "pane", "-H", "claude", "do x"],
    )
    assert result.exit_code == 99
    assert captured, "a pane spawn must route to the binary"
    assert captured[0][0] == "spawn"
    assert "--as" in captured[0]


def test_default_substrate_spawn_stays_python() -> None:
    """A bare spawn's default substrate (thread, with the thread-seatable
    refusal and the keeper thread lanes behind it) still resolves in the
    Python door. Only an explicit --as pane names the shape the binary's
    pane spine answers; the scan stops at --argv and the -- fence, and the
    --once headless spelling opts out."""
    assert rr._is_default_substrate_spawn("spawn", ["spawn", "w", "seed"])
    assert rr._is_default_substrate_spawn("spawn", ["spawn", "w"])
    assert not rr._is_default_substrate_spawn("spawn", ["spawn", "w", "--as", "pane"])
    assert not rr._is_default_substrate_spawn("spawn", ["spawn", "w", "--as=pane"])
    assert not rr._is_default_substrate_spawn("spawn", ["spawn", "w", "--as", "thread"])
    assert not rr._is_default_substrate_spawn("spawn", ["spawn", "w", "--as", "headless"])
    assert not rr._is_default_substrate_spawn("spawn", ["spawn", "w", "--once"])
    # A fenced or payload token is the child's, never fno's routing flag.
    assert rr._is_default_substrate_spawn("spawn", ["spawn", "w", "--", "--as", "pane"])
    assert rr._is_default_substrate_spawn("spawn", ["spawn", "w", "--argv", "--as=pane"])
    assert not rr._is_default_substrate_spawn("ask", ["ask", "w", "--as", "pane"])


def test_is_dispatch_account_bearing_spawn_predicate() -> None:
    # The cutover carrier is Python-only: the Rust spawn parser exits
    # "unknown flag: --dispatch-account" and the launch dies before it starts.
    # The rule lives in the router, not at one caller, so every caller is covered.
    assert rr._is_dispatch_account_bearing_spawn(
        "spawn", ["spawn", "w", "--dispatch-account", "ccr"]
    )
    assert rr._is_dispatch_account_bearing_spawn(
        "spawn", ["spawn", "--name", "w", "--dispatch-account=ccr"]
    )
    assert not rr._is_dispatch_account_bearing_spawn(
        "spawn", ["spawn", "w", "--account", "ccr"]
    )
    assert not rr._is_dispatch_account_bearing_spawn(
        "ask", ["ask", "w", "--dispatch-account", "ccr"]
    )


def test_routing_predicates_stop_at_argv_boundary() -> None:
    # codex P1: a provider payload's own flags after --argv must not force Python.
    assert not rr._is_resume_bearing_spawn(
        "spawn", ["spawn", "w", "--argv", "--", "tool", "--resume", "x"]
    )
    assert not rr._is_role_bearing_spawn(
        "spawn", ["spawn", "w", "--argv", "--", "tool", "--role", "x"]
    )
    assert not rr._is_route_bearing_spawn(
        "spawn", ["spawn", "w", "--argv", "--", "tool", "--route", "x"]
    )
    # a real fno flag BEFORE --argv still counts.
    assert rr._is_resume_bearing_spawn(
        "spawn", ["spawn", "w", "--resume", "u", "--argv", "--", "tool"]
    )


def test_the_account_picker_leaves_a_cutover_spawn_alone(monkeypatch) -> None:
    """A cutover already selected its account. Picking a second one merges the
    destination's overlay with the picked account's - the same mis-bill this
    seam already refuses for --route."""
    monkeypatch.setattr(
        "fno.agents.dispatch.pick_account_id",
        lambda *a, **k: pytest.fail("picked an account for a cutover spawn"),
    )
    args = ["spawn", "w", "--harness", "claude", "--dispatch-account", "ccr"]
    assert rr._pick_account_at_seam(args) == args


def test_the_seam_pick_fires_for_spawn_itself(monkeypatch) -> None:
    """The pick, scrub and remap refusal live in the seam's shared arm, so a
    spawn takes them like every worker command. Wiring them only into the elif
    made the whole provenance carrier decorative on the spawn path: a
    bg spawn exec'd the Rust client and minted its row off whatever stale
    parent env it inherited. The patch's exit code only surfaces if the seam
    actually called it."""
    from fno.cli import app

    monkeypatch.setattr(rr, "_pick_account_at_seam", lambda args: exit(99))
    result = CliRunner().invoke(app, ["agents", "spawn", "--name", "w-seam", "hi"])
    assert result.exit_code == 99, result.output

    # rm owns its own elif arm: the account seam must not leak into it.
    monkeypatch.setattr(rust_binary, "resolve_installed_binary", lambda: None)
    result = CliRunner().invoke(app, ["agents", "rm", "--name", "w-seam"])
    assert result.exit_code != 99, result.output


def test_exec_lane_scrubs_ambient_identity_before_rust_spawn(monkeypatch, tmp_path) -> None:
    """A --substrate bg spawn execs the Rust client, which launches the worker
    from its own inherited env; the Python identity floor never runs on that
    lane. The scrub at the exec seam is what closes it: the client (and the
    worker it spawns) must not inherit the parent's identity markers."""
    from fno.cli import app

    binary = _make_exe(tmp_path / rust_binary.BINARY_NAME)
    seen: dict = {}

    def fake_route(args, **kw):
        seen["markers"] = {
            name: os.environ.get(name)
            for name in ("CODEX_THREAD_ID", "CLAUDE_CODE_SESSION_ID", "CODEX_CI")
        }
        seen["path"] = os.environ.get("PATH")
        raise SystemExit(99)

    monkeypatch.delenv(rr.RUNTIME_ENV, raising=False)
    monkeypatch.setattr(rust_binary, "resolve_installed_binary", lambda: binary)
    monkeypatch.setattr(rr, "route_to_rust", fake_route)
    monkeypatch.setenv("CODEX_THREAD_ID", "parent-thread")
    monkeypatch.setenv("CLAUDE_CODE_SESSION_ID", "parent-claude")
    monkeypatch.setenv("CODEX_CI", "1")

    result = CliRunner().invoke(
        app,
        ["agents", "spawn", "--name", "worker", "--harness", "codex", "--once", "hi"],
    )
    assert result.exit_code == 99
    assert seen["markers"] == {
        "CODEX_THREAD_ID": None,
        "CLAUDE_CODE_SESSION_ID": None,
        "CODEX_CI": None,
    }, "identity markers must be scrubbed from the exec'd client's env"
    assert seen["path"], "the scrub must not touch non-identity env"


# --------------------------------------------------------------------------- #
# The seam marker: bridge carriage ahead of the message fence
# --------------------------------------------------------------------------- #

class _BridgeProc:
    returncode = 0
    stdout = json.dumps({"short_id": "s-1", "harness_session_id": "full-uuid"})
    stderr = ""


def _capture_bridge_run(captured: dict, monkeypatch) -> None:
    import fno.agents.rust_spawn as rust_spawn

    def _run(argv, **kw):
        captured["argv"] = [str(a) for a in argv]
        captured["env"] = kw.get("env")
        return _BridgeProc()

    monkeypatch.setattr(rust_binary, "resolve_binary", lambda: Path("/bin/true"))
    monkeypatch.setattr(rust_spawn.subprocess, "run", _run)


def test_opencode_bridge_carries_marker_before_the_message_fence(monkeypatch) -> None:
    import fno.agents.rust_spawn as rust_spawn

    captured: dict = {}
    _capture_bridge_run(captured, monkeypatch)
    rust_spawn._opencode_serve_spawn(
        name="w",
        message="/target x-90a9",
        cwd=Path("/tmp"),
        from_name="k",
        node="x-90a9",
    )
    argv = captured["argv"]
    assert argv[1] == "spawn"
    assert argv[2].startswith("--defaults-applied=")
    # No substrate flag: the binary refuses the retired axis and defaults
    # opencode to the thread lane.
    assert "--substrate" not in argv
    tail = argv[argv.index("--") + 1 :]
    assert tail == ["/target x-90a9"]


def test_codex_node_dispatch_refuses_without_a_command_prefixed_payload(
    monkeypatch, tmp_path
) -> None:
    """A --node dispatch is a code payload even when no token carries a
    /target-family prefix, so the grant refusal still holds."""
    from fno.cli import app

    called: list[list[str]] = []
    monkeypatch.setenv(rr.RUNTIME_ENV, "rust")
    monkeypatch.setattr(rr, "route_to_rust", lambda args, **kw: called.append(list(args)))
    result = CliRunner().invoke(
        app,
        [
            "agents",
            "spawn",
            "--harness",
            "codex",
            "--cwd",
            str(tmp_path),
            "--node",
            "x-f370",
        ],
    )

    assert result.exit_code == 2
    assert called == []


def test_python_forced_list_is_refused_with_no_table(monkeypatch) -> None:
    """There is no Python list lane: forcing it names the binary and exits 127."""
    from fno.cli import app

    monkeypatch.setenv(rr.RUNTIME_ENV, "python")
    result = CliRunner().invoke(app, ["agents", "list"])
    assert result.exit_code == rr.BIN_NOT_FOUND_EXIT
    assert f"requires the '{rust_binary.BINARY_NAME}' binary" in result.output
    assert "NAME" not in result.output


@pytest.mark.parametrize("binary_present", [True, False])
def test_rm_runtime_python_mode_refuses(monkeypatch, tmp_path, binary_present) -> None:
    """`FNO_AGENTS_RUNTIME=python` cannot force rm onto Python anymore: the
    twin was deleted. The refusal names the flag so the operator learns the
    contract changed rather than staring at a missing command."""
    from fno.cli import app

    called: list = []

    monkeypatch.setenv(rr.RUNTIME_ENV, "python")
    if binary_present:
        monkeypatch.setattr(
            rust_binary, "resolve_installed_binary", lambda: tmp_path / "bin"
        )
    else:
        monkeypatch.setattr(rust_binary, "resolve_installed_binary", lambda: None)
    monkeypatch.setattr(rr, "route_to_rust", lambda args, **kw: called.append(list(args)))
    result = CliRunner().invoke(app, ["agents", "rm", "any"])
    assert called == []
    assert result.exit_code == rr.BIN_NOT_FOUND_EXIT
    assert rr.RUNTIME_ENV in result.stderr


def test_rm_runtime_without_installed_binary_refuses(monkeypatch) -> None:
    """With no installed binary, `rm` refuses legibly: the Python rm twin was
    deleted (one command, one implementation), so there is no dispatch to fall
    back to."""
    from fno.cli import app

    called: list = []

    monkeypatch.delenv(rr.RUNTIME_ENV, raising=False)
    monkeypatch.setattr(rust_binary, "resolve_installed_binary", lambda: None)
    monkeypatch.setattr(rr, "route_to_rust", lambda args, **kw: called.append(list(args)))
    result = CliRunner().invoke(app, ["agents", "rm", "ghost"])
    assert called == []
    assert result.exit_code == rr.BIN_NOT_FOUND_EXIT
    assert "ported" in result.stderr
