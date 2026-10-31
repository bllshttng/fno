"""x-ae2d + x-d285: a routed worker's route and account survive every
re-entry, or the re-entry refuses.

Two mechanisms. The spawn seams record the PATH of the route-settings file a
worker was launched with plus its launch account (never the file's contents,
which include a live ``ANTHROPIC_AUTH_TOKEN``), and every re-entry door either
restores that whole context or refuses non-zero naming what is missing.

The falsified premise this file used to carry - that ``claude attach`` opens a
running session so attach cannot lose a route - is deleted. The operator
showed the opposite on 2026-08-23: every re-entry door shells a fresh claude
process, and a fresh process re-resolves its account namespace from ambient
env, so an attach or resume launched from the wrong shell lands in the wrong
config namespace. The transition matrix below is the replacement: one explicit
row per door, each pinned to the same contract.
"""
from __future__ import annotations

import json
import subprocess

import pytest

from fno.paths_testing import use_tmpdir

class _FakeRunner:
    """Mux pane runner: a pane spawn returns pane 7 with child pid 4242."""

    def __init__(self) -> None:
        self.calls: list[list[str]] = []

    def __call__(self, argv, **kwargs):
        self.calls.append(list(argv))
        if argv[:4] == ["codex", "app-server", "daemon", "start"]:
            return subprocess.CompletedProcess(argv, 0, '{"status":"alreadyRunning"}', "")
        if argv[1:4] == ["mux", "pane", "run"]:
            return subprocess.CompletedProcess(argv, 0, "7\n", "")
        if argv[1:4] == ["mux", "pane", "ls"]:
            out = json.dumps(
                [{"pane_id": 7, "squad_id": 1, "tab_id": 1, "cwd": "/w", "child_pid": 4242}]
            )
            return subprocess.CompletedProcess(argv, 0, out, "")
        # A codex pane spawn probes liveness and retains the pane's output while
        # it waits for a session binding. 11 = the pane is up.
        if argv[1:4] == ["mux", "pane", "wait"]:
            return subprocess.CompletedProcess(argv, 11, "", "")
        if argv[1:4] == ["mux", "pane", "read"]:
            return subprocess.CompletedProcess(argv, 0, "", "")
        raise AssertionError(f"unexpected invocation: {argv}")


# --- 1.2: the spawn seam records the route path ------------------------------


# --- 1.3: the revive door restores the route, or refuses ---------------------


requires_rust = pytest.mark.dev_build


@requires_rust
def test_ac4_a_routed_codex_pane_records_identity_but_never_a_route_file(
    tmp_path, monkeypatch
) -> None:
    """AC4, flipped by x-3954: a routed codex pane records its route IDENTITY
    on the row (`route_provider_id` + `model_name`, identifiers only) and still
    records no route-settings file. The endpoint lives in no artifact at all:
    every relaunch door re-resolves the route from today's config in Rust
    (`codex_route.rs`) or refuses, so a half restore onto codex's default
    provider stays impossible rather than merely avoided.

    The provider record is pinned via a TOML config (re-pinned AFTER
    use_tmpdir, which points FNO_CONFIG at a YAML file the Rust reader cannot
    parse), so the REAL resolution path runs - no stubbed resolver.
    """
    from fno.agents.registry import load_registry

    use_tmpdir(monkeypatch, tmp_path)
    cfg = tmp_path / "config.toml"
    cfg.write_text(
        f'[config]\nstate_dir = "{tmp_path / ".fno"}/"\n'
        "[model_routing]\n"
        '[model_routing.roles]\ncodex-verify = "zai-openai,glm-5.3-flash[1m]"\n'
        "[model_routing.providers.zai-openai]\n"
        'protocol = "openai"\n'
        'base_url = "https://api.z.ai/api/coding/paas/v4"\n'
        'api_key_env = "ZAI_API_KEY"\n',
        encoding="utf-8",
    )
    monkeypatch.setenv("FNO_CONFIG", str(cfg))
    monkeypatch.setenv("ZAI_API_KEY", "zai-secret-token")
    for var in ("FNO_SESSION", "CLAUDE_CODE_SESSION_ID", "CODEX_SESSION_ID", "GEMINI_SESSION_ID"):
        monkeypatch.delenv(var, raising=False)

    from fno.agents import mux_spawn

    monkeypatch.setattr(
        mux_spawn,
        "_backfill_codex_session_id",
        lambda *a, **k: "019fb024-2327-75f3-8b80-06e9d5ade05f",
    )
    mux_spawn.dispatch_spawn_pane(
        name="router",
        message="go",
        provider="codex",
        cwd=tmp_path,
        runner=_FakeRunner(),
        role="codex-verify",
    )
    row = load_registry()[0]
    assert row.route_settings_path is None
    assert row.route_provider_id == "zai-openai"
    assert row.model_name == "glm-5.3-flash[1m]"


# --- x-d285 task 1.3: the transition matrix ----------------------------------
#
# One row per door that re-enters or re-launches a Claude session:
#
#   door                 owned by                      wired in
#   attach (inline)      crates/fno-agents client_commands run_attach (reentry.rs plan; no Python leg)
#   resume live wake     resume_cli wake arm           2.1
#   resume dead          resume_cli --resume arm       2.1
#   mux attach           crates/fno server AttachAgent 2.2 (cargo matrix)
#   mux ResumeAgent      crates/fno server ResumeAgent 2.2 (cargo matrix)
#   spawn --resume       dispatch_spawn revive         route: x-ae2d, account: 1.1
#   bg fresh spawn       dispatch_spawn picker         2.3
#   loop dispatch        fno/dispatch launch_account   2.3 (test_dispatch_one)
#   background observe   record_session_observation    3.1
#   fork observe         record_session_observation    3.1
#
# The contract per door: the provider invocation carries BOTH the recorded
# account namespace and the recorded route, or refuses naming the missing
# evidence. A bare ``claude attach``/``--resume`` observed on a routed row is
# exactly the defect the row exists to catch.


# --- v23 (x-2019): the requested axis, verbatim beside the effect ------------


# --- a foreign endpoint is never offered the Artifact tool -------------------
