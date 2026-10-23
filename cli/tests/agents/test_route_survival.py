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
from pathlib import Path

import pytest

from fno.paths_testing import use_tmpdir
from fno.registry_door import read_registry_document

ROUTE_ENV = {
    "ANTHROPIC_BASE_URL": "https://api.z.ai/api/anthropic",
    "ANTHROPIC_AUTH_TOKEN": "zai-secret-token",
    "ANTHROPIC_MODEL": "glm-5.2",
    # Every tier: the restore relaunch goes through resolve_spawn_route, which
    # refuses a foreign endpoint without the full model set.
    "ANTHROPIC_DEFAULT_OPUS_MODEL": "glm-5.2",
    "ANTHROPIC_DEFAULT_SONNET_MODEL": "glm-5.2",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL": "glm-5.2",
    "ANTHROPIC_DEFAULT_FABLE_MODEL": "glm-5.2",
}

# The provider registry is the binary's answer; pytest shards run without it,
# so the refresh tests pin this canned zai row (today's haiku default).
_ZAI_PROVIDERS_ANSWER = {
    "providers": {
        "zai": {
            "protocol": "anthropic",
            "base_url": "https://api.z.ai/api/anthropic",
            "api_key_env": "ZAI_API_KEY",
            "api_key_file": "~/.fno/.env",
            "tier_models": {"haiku": "glm-4.7"},
            "key": {"satisfying": None, "checked": ["ZAI_API_KEY", "~/.fno/.env"]},
        }
    }
}


def _pin_zai_registry(monkeypatch) -> None:
    monkeypatch.setattr(
        "fno.agents.spawn_overlay_client.spawn_overlay_call",
        lambda payload: _ZAI_PROVIDERS_ANSWER,
    )


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


def _admitted_zai(monkeypatch, name: str = "router"):
    from fno.agents.spawn_gate import run_gate

    monkeypatch.setenv("FNO_SPAWN_GATE", "0")
    return run_gate(name, "bg", route_provider="zai")


def _spawn_pane(monkeypatch, tmp_path, provider="claude", name="router", **kwargs):
    use_tmpdir(monkeypatch, tmp_path)
    for var in ("FNO_SESSION", "CLAUDE_CODE_SESSION_ID", "CODEX_SESSION_ID", "GEMINI_SESSION_ID"):
        monkeypatch.delenv(var, raising=False)
    from fno.agents import mux_spawn

    if provider == "codex":
        monkeypatch.setattr(
            mux_spawn,
            "_backfill_codex_session_id",
            lambda *a, **k: "019fb024-2327-75f3-8b80-06e9d5ade05f",
        )

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
            run_gate("router", "pane", route_provider=kwargs["route_provider"]),
        )

    return mux_spawn.dispatch_spawn_pane(
        name=name,
        message="go",
        provider=provider,
        cwd=tmp_path,
        runner=_FakeRunner(),
        **kwargs,
    )


# --- 1.2: the spawn seam records the route path ------------------------------


def _spawn_routed_bg(monkeypatch, tmp_path, name: str, **kwargs):
    """Run one claude bg spawn through dispatch_spawn with the fake claude."""
    from tests.agents._fake_claude import install_fake_claude

    use_tmpdir(monkeypatch, tmp_path)
    bin_dir = tmp_path / "bin"
    install_fake_claude(bin_dir)
    monkeypatch.setenv("PATH", str(bin_dir))
    for var in ("FNO_SESSION", "CLAUDE_CODE_SESSION_ID", "CODEX_SESSION_ID", "GEMINI_SESSION_ID"):
        monkeypatch.delenv(var, raising=False)

    from fno.agents import dispatch as dispatch_mod
    from fno.agents.model_routing import bind_route_provider
    from fno.agents.spawn_gate import run_gate

    monkeypatch.setenv("FNO_SPAWN_GATE", "0")
    return dispatch_mod.dispatch_spawn(
        name=name,
        message="go",
        harness="claude",
        cwd=tmp_path,
        route_env=bind_route_provider(dict(ROUTE_ENV), "zai"),
        route_provider="zai",
        provider_gate=run_gate(name, "bg", route_provider="zai"),
        **kwargs,
    )


def test_routed_bg_row_stamps_the_route_model(tmp_path, monkeypatch) -> None:
    """The bg receipt names the route's model; the row must match.

    This is the measured shape that stayed None on every routed claude bg
    spawn while the receipt printed it (the receipt-can-lie half).
    """
    from fno.agents.registry import load_registry

    _spawn_routed_bg(
        monkeypatch, tmp_path, "router-bg", route_model="glm-5.3-flash[1m]"
    )
    row = next(r for r in load_registry() if r.name == "router-bg")
    assert row.provider == "zai"
    assert row.model == "glm-5.3-flash[1m]"
    assert row.model_basis == "requested"


def test_unrouted_spawn_records_no_route_path(tmp_path, monkeypatch) -> None:
    """Neither a plain nor an --account worker records a route to restore.

    An account's settings file is the scrub floor plus the account's
    non-profile env, which read_route_settings reads back as "no route" (or,
    for an api-key account, an endpoint-without-model-tiers unit the
    composition guard refuses). Stamping it on the row turned every
    --account worker's revive into an exit-2 refusal telling the operator to
    re-spawn with --route."""
    from fno.agents.registry import load_registry

    _spawn_pane(monkeypatch, tmp_path)
    assert load_registry()[0].route_settings_path is None
    _spawn_pane(
        monkeypatch, tmp_path, name="router-2",
        account_env={"CLAUDE_CONFIG_DIR": "/x/.claude"},
    )
    rows = {r.name: r for r in load_registry()}
    assert rows["router-2"].route_settings_path is None


def test_ac7_registry_stores_the_path_never_the_route_contents(tmp_path, monkeypatch) -> None:
    """AC7: the registry file carries a path and no credential."""

    _spawn_pane(
        monkeypatch,
        tmp_path,
        route_env=dict(ROUTE_ENV),
        route_provider="zai",
    )
    raw = json.dumps(read_registry_document()[0])
    assert "route-settings" in raw  # the path IS recorded
    assert "ANTHROPIC_AUTH_TOKEN" not in raw
    assert "zai-secret-token" not in raw
    assert "ANTHROPIC_BASE_URL" not in raw


# --- 1.3: the revive door restores the route, or refuses ---------------------


def _routed_claude_row(
    tmp_path, monkeypatch, *, provider="zai", session_id="sess-1"
):
    """A registry holding one routed claude worker, plus its route file path."""
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents.model_routing import _write_settings_env_file
    from fno.agents.account_env import SCRUB_AUTH_VARS

    env = {var: "" for var in SCRUB_AUTH_VARS}
    env.update(ROUTE_ENV)
    path = _write_settings_env_file(env)
    from fno.agents.registry import AgentEntry, write_registry

    write_registry(
        [
            AgentEntry(
                name="router",
                cwd=str(tmp_path),
                log_path="",
                harness="claude",
                provider=provider,
                short_id="deadbeef",
                harness_session_id=session_id,
                route_settings_path=path,
            )
        ]
    )
    return path


def test_ac2_hp_revive_restores_the_recorded_route(tmp_path, monkeypatch) -> None:
    """AC2-HP: a relaunch comes back on the route it was launched with."""
    from fno.agents.dispatch import restore_route_for_relaunch
    from fno.agents.registry import load_registry

    _routed_claude_row(tmp_path, monkeypatch)
    _pin_zai_registry(monkeypatch)
    got = restore_route_for_relaunch(load_registry()[0])
    # Exactly the route's own keys. The stored file also carries the auth-scrub
    # floor as empty strings, which means "unset" only to claude reading a
    # settings FILE - replaying it as process env would hand the revived worker
    # an ANTHROPIC_API_KEY="" the original launch never had.
    # The one key that does NOT replay verbatim is the provider-DEFAULT haiku
    # tier: it re-resolves against today's registry (x-5cc5), so the fixture's
    # glm-5.2 comes back as the current zai default.
    assert got == {**ROUTE_ENV, "ANTHROPIC_DEFAULT_HAIKU_MODEL": "glm-4.7"}


def test_ac3_err_revive_refuses_when_the_route_file_is_gone(tmp_path, monkeypatch) -> None:
    """AC3-ERR: refuse non-zero, name the route, start nothing."""
    from fno.agents.dispatch import restore_route_for_relaunch
    from fno.agents.dispatch_errors import RouteRestoreRefused
    from fno.agents.registry import load_registry

    path = _routed_claude_row(tmp_path, monkeypatch)
    Path(path).unlink()
    with pytest.raises(RouteRestoreRefused) as exc:
        restore_route_for_relaunch(load_registry()[0])
    assert path in str(exc.value)
    assert exc.value.exit_code == 2


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


def test_an_explicit_account_composes_with_a_restored_route(
    tmp_path, monkeypatch
) -> None:
    """--account and a route compose; the revive path must not refuse the pair.

    Nothing in `fno agents spawn` refuses --account alongside --route: the route
    wins endpoint+auth+model through the settings file while the account's
    CLAUDE_CONFIG_DIR rides the spawn env to pick the per-account daemon
    (x-5ed4). A restored route is a route like any other, so the revive path
    inherits that contract rather than inventing a refusal for it.
    """
    from fno.agents.dispatch import DispatchAskError, dispatch_spawn

    _routed_claude_row(tmp_path, monkeypatch)
    seen: dict = {}

    def _fake_bg_create(**kwargs):
        seen.update(kwargs)
        raise DispatchAskError("stop here", exit_code=99)

    from fno.agents.harnesses import claude as claude_mod

    monkeypatch.setattr(claude_mod, "bg_create", _fake_bg_create)
    with pytest.raises(DispatchAskError) as exc:
        dispatch_spawn(
            name="router",
            message="go",
            harness="claude",
            cwd=tmp_path,
            resume_session_id="sess-1",
            account_env={"CLAUDE_CONFIG_DIR": "/cfg"},
            route_provider="zai",
            provider_gate=_admitted_zai(monkeypatch),
        )
    assert exc.value.exit_code == 99, "the pair must reach the launch, not be refused"
    assert seen["account_env"] == {"CLAUDE_CONFIG_DIR": "/cfg"}, "account survives"
    # The restored route reaches the launch carrying its provider stamp: the
    # relaunch re-binds the provider, and that binding is what a running worker
    # reads to name its own account (x-c703). The haiku tier is the one key
    # refreshed to today's provider default rather than replayed (x-5cc5).
    _pin_zai_registry(monkeypatch)
    assert seen["route_env"] == {
        **ROUTE_ENV,
        "FNO_ROUTE_PROVIDER": "zai",
        "ANTHROPIC_DEFAULT_HAIKU_MODEL": "glm-4.7",
    }, "and the restored route rides with it"


def test_a_legacy_routed_row_without_provider_refuses_relaunch(
    tmp_path, monkeypatch
) -> None:
    from fno.agents.dispatch import DispatchAskError, dispatch_spawn

    _routed_claude_row(tmp_path, monkeypatch, provider=None)
    harness = "claude"
    with pytest.raises(DispatchAskError) as exc:
        dispatch_spawn(
            name="router",
            message="go",
            harness=harness,
            cwd=tmp_path,
            resume_session_id="sess-1",
        )
    assert exc.value.exit_code == 2
    assert "no model-provider axis" in str(exc.value)
    assert "cap cannot be evaluated" in str(exc.value)


def test_a_restored_route_without_prior_provider_admission_refuses(
    tmp_path, monkeypatch
) -> None:
    from fno.agents.dispatch import DispatchAskError, dispatch_spawn

    _routed_claude_row(tmp_path, monkeypatch)
    harness = "claude"
    with pytest.raises(DispatchAskError) as exc:
        dispatch_spawn(
            name="router",
            message="go",
            harness=harness,
            cwd=tmp_path,
            resume_session_id="sess-1",
        )
    assert exc.value.exit_code == 2
    assert "provider admission was not evaluated" in str(exc.value)


def test_direct_resume_refuses_an_incomplete_forward_registry(
    tmp_path, monkeypatch
) -> None:
    import fno.agents.dispatch as dispatch_mod
    from fno.agents.dispatch import DispatchAskError, dispatch_spawn
    from fno.agents.registry import LoadedRegistry, load_registry

    _routed_claude_row(tmp_path, monkeypatch)
    partial = LoadedRegistry(load_registry(), complete=False)
    monkeypatch.setattr(dispatch_mod, "load_registry", lambda: partial)
    harness = "claude"
    with pytest.raises(DispatchAskError, match="registry forward read is incomplete"):
        dispatch_spawn(
            name="router",
            message="go",
            harness=harness,
            cwd=tmp_path,
            resume_session_id="sess-1",
        )


def test_a_route_file_holding_only_the_scrub_floor_refuses(tmp_path, monkeypatch) -> None:
    """Readable but routeless is the same silent fallback as missing."""
    from fno.agents.dispatch import DispatchAskError, restore_route_for_relaunch
    from fno.agents.account_env import SCRUB_AUTH_VARS
    from fno.agents.registry import load_registry

    path = _routed_claude_row(tmp_path, monkeypatch)
    Path(path).write_text(
        json.dumps({"env": {v: "" for v in SCRUB_AUTH_VARS}}), encoding="utf-8"
    )
    with pytest.raises(DispatchAskError) as exc:
        restore_route_for_relaunch(load_registry()[0])
    assert exc.value.exit_code == 2


def test_the_account_picker_never_fires_on_a_revive(tmp_path, monkeypatch, capsys) -> None:
    """An auto-picked account must not turn a revive into an exit-2 refusal.

    `pick_on_launch` fills in `--account` for a spawn that named none, on BOTH
    seams: the argv seam (`_pick_account_at_seam`) and the in-process one here.
    Left running on a revive it prints an `account: <id> (picked)` receipt for a
    destination the operator never chose, and composes that overlay with the
    restored route - two axes merged by an advisory guess. A revive continues an
    existing transcript, and that transcript lives under the config dir it was
    created in, so a picked CLAUDE_CONFIG_DIR resumes into a directory where the
    uuid does not exist.
    """
    import fno.agents.rust_runtime as rr
    from fno.agents.dispatch import DispatchAskError, dispatch_spawn

    path = _routed_claude_row(tmp_path, monkeypatch)
    monkeypatch.setattr(
        "fno.agents.dispatch.pick_account_id",
        lambda *a, **k: pytest.fail("picked an account for a revive"),
    )

    # Seam 1: the argv rewrite the CLI applies before either runtime.
    args = ["spawn", "router", "--resume", "sess-1", "--substrate", "thread"]
    assert rr._pick_account_at_seam(args) == args

    # Seam 2: the in-process caller that bypasses argument parsing. The restore
    # runs (proved by the refusal naming the missing file), and no pick fires.
    Path(path).unlink()
    with pytest.raises(DispatchAskError) as exc:
        dispatch_spawn(
            name="router",
            message="go",
            harness="claude",
            cwd=tmp_path,
            resume_session_id="sess-1",
        )
    assert path in str(exc.value)
    assert "--account" not in str(exc.value), "no refusal about a flag nobody typed"


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


def _routed_glm_row(tmp_path, monkeypatch, *, launch_account="makers"):
    """The operator's failing shape: routed GLM worker on an isolated account."""
    path = _routed_claude_row(tmp_path, monkeypatch)
    if launch_account is not None:
        from fno.agents.registry import update_registry

        def _stamp(rows):
            for r in rows:
                if r.name == "router":
                    r.launch_account = launch_account
            return rows

        update_registry(_stamp)
    return path


def test_python_resume_refuses_a_routed_claude_row_without_waking(tmp_path) -> None:
    """The Python fallback cannot own Claude's account-pinned resume route."""
    from types import SimpleNamespace

    from fno.agents.resume_cli import resume_logic

    entry = SimpleNamespace(
        name="router",
        harness="claude",
        cwd=str(tmp_path),
        short_id="deadbeef",
        harness_session_id="sess-1",
        route_settings_path="/route/settings.json",
        launch_account="makers",
        provider="zai",
    )
    launches: list[tuple] = []

    res = resume_logic(
        name="router",
        registry_loader=lambda: [entry],
        path_checker=lambda b: True,
        cwd_checker=lambda c: True,
        execvp=lambda *args, **kwargs: launches.append(args),
    )
    assert res.exit_code == 13
    assert "resumes through the fno-agents runtime" in res.stderr
    assert launches == []


def test_matrix_spawn_resume_inherits_the_recorded_account(tmp_path, monkeypatch) -> None:
    """Door: spawn --resume (revive). The revived row inherits the source
    row's launch account - the transcript lives under the config dir it was
    created in, so the account is a fact about the transcript. The recorded
    account's overlay rides the spawn, so the process BILLS there too."""

    from fno.agents import dispatch
    from fno.agents.harnesses.base import ProviderResult
    from fno.agents.registry import load_registry

    _routed_glm_row(tmp_path, monkeypatch)
    monkeypatch.setattr(
        "fno.agents.harnesses.claude.session_is_live", lambda short: False
    )
    monkeypatch.setattr(
        "fno.agents.harnesses.claude.resolve_session_uuid_at_spawn",
        lambda home, short: "sess-1",
    )

    seen = {}

    def _fake_bg_create(**kwargs):
        seen.update(kwargs)
        return ProviderResult(0, "", "", 1, session_id_out="feedface")

    monkeypatch.setattr(
        "fno.agents.harnesses.claude.bg_create", _fake_bg_create
    )

    class _Overlay:
        account_id = "makers"
        env = {"CLAUDE_CONFIG_DIR": "/acct/makers/.claude"}
        lane = "config-dir"

    monkeypatch.setattr(
        "fno.agents.account_env.resolve_account_overlay",
        lambda account_id, **kwargs: _Overlay(),
    )
    result = dispatch.dispatch_spawn(
        name="router",
        message="go",
        harness="claude",
        cwd=tmp_path,
        resume_session_id="sess-1",
        route_provider="zai",
        provider_gate=_admitted_zai(monkeypatch),
    )
    assert result.kind == "created"
    row = load_registry()[0]
    assert row.launch_account == "makers", (
        "a revive inherits the source row's account axis"
    )
    assert seen["account_env"] == {"CLAUDE_CONFIG_DIR": "/acct/makers/.claude"}, (
        "the recorded account's overlay must ride the resume spawn"
    )


def _fake_bg_seam(monkeypatch):
    """The bg spawn seam: no live session, a resolvable spawn uuid, and a
    bg_create that succeeds without launching anything."""
    from fno.agents.harnesses.base import ProviderResult

    monkeypatch.setattr(
        "fno.agents.harnesses.claude.session_is_live", lambda short: False
    )
    monkeypatch.setattr(
        "fno.agents.harnesses.claude.resolve_session_uuid_at_spawn",
        lambda *a: "sess-2",
    )
    monkeypatch.setattr(
        "fno.agents.harnesses.claude.bg_create",
        lambda **kwargs: ProviderResult(0, "", "", 1, session_id_out="feedface"),
    )


def test_matrix_bg_fresh_spawn_default_control_stamps_default(
    tmp_path, monkeypatch
) -> None:
    """Door: bg spawn, default-Anthropic control. No picker fires, so the
    fresh row positively records "default" - never an absence that a later
    read could mistake for legacy-unknown."""
    from fno.agents import dispatch
    from fno.agents.registry import load_registry

    use_tmpdir(monkeypatch, tmp_path)
    _fake_bg_seam(monkeypatch)
    result = dispatch.dispatch_spawn(
        name="fresh-default",
        message="go",
        harness="claude",
        cwd=tmp_path,
    )
    assert result.kind == "created"
    rows = {r.name: r for r in load_registry()}
    assert rows["fresh-default"].launch_account == "default", (
        "a fresh spawn with no account pick positively records default"
    )


def test_matrix_background_observe_records_the_second_id(tmp_path, monkeypatch) -> None:
    """Door: native background observation. A SessionStart reporting a
    different id fills related_session_id without replacing the primary."""
    from fno.agents.registry import load_registry, record_session_observation

    _routed_glm_row(tmp_path, monkeypatch)
    entry, outcome = record_session_observation(
        name="router", harness="claude", session_id="sess-fork"
    )
    assert outcome == "related"
    row = load_registry()[0]
    assert row.harness_session_id == "sess-1", "the primary is never replaced"
    assert row.related_session_id == "sess-fork"


def test_a_restored_route_is_announced(tmp_path, monkeypatch, capsys) -> None:
    """A relaunch that changes destination without saying so is the failure this
    path exists to remove; a restore that says nothing is the same silence."""
    from fno.agents.dispatch import dispatch_spawn

    path = _routed_claude_row(tmp_path, monkeypatch)

    seen = {}

    def _stop(**kw):
        seen.update(kw)
        raise RuntimeError(f"stop before launch; route={sorted(kw['route_env'] or {})}")

    monkeypatch.setattr("fno.agents.dispatch._claude_create_path", _stop)
    with pytest.raises(RuntimeError) as exc:
        dispatch_spawn(
            name="router",
            message="go",
            harness="claude",
            cwd=tmp_path,
            resume_session_id="sess-1",
            route_provider="zai",
            provider_gate=_admitted_zai(monkeypatch),
        )
    assert "ANTHROPIC_BASE_URL" in str(exc.value), "the restored route reaches the launch"
    assert seen["route_provider"] == "zai", "the recorded provider rides with it"
    assert path in capsys.readouterr().err


# --- v23 (x-2019): the requested axis, verbatim beside the effect ------------


def test_routed_bg_row_stamps_the_request_verbatim(tmp_path, monkeypatch) -> None:
    """requested_* carries the request as typed - [1m] suffix included.

    The observed `model` axis may later flip to a verified reading; the
    requested stamps never do, so requested-vs-observed stays diffable.
    """
    from fno.agents.registry import load_registry

    _spawn_routed_bg(
        monkeypatch,
        tmp_path,
        "requester-bg",
        model="glm-5.3[1m]",
        effort="high",
        route_model="glm-5.3-flash[1m]",
    )
    row = next(r for r in load_registry() if r.name == "requester-bg")
    assert row.requested_model == "glm-5.3[1m]"
    assert row.requested_provider == "zai"
    assert row.requested_effort == "high"
    # The explicit --model won the argv and the observed axis; the request
    # records the SAME value here, verbatim - not the route's token.
    assert row.model == "glm-5.3[1m]"


# --- a foreign endpoint is never offered the Artifact tool -------------------


def test_foreign_route_settings_deny_artifact(tmp_path, monkeypatch):
    """A route off api.anthropic.com writes ``permissions.deny: ["Artifact"]``.

    claude 2.1.265 gave the Artifact tool an ``input_schema`` pattern holding
    ``\\p{Cc}``. z.ai's Anthropic-compatible endpoint refuses any tools pattern
    with a ``\\p{...}`` escape and answers ``400 [1210]``, so every fresh
    interactive routed worker died on its first turn. The tool needs Anthropic's
    own backend, so a routed worker loses nothing when it is not offered. The
    whole-host table's owner is the Rust writer's decision test; this pins the
    file's permissions block through the door.
    """
    use_tmpdir(monkeypatch, tmp_path)
    from fno.agents.model_routing import _write_settings_env_file

    path = Path(_write_settings_env_file(dict(ROUTE_ENV)))
    payload = json.loads(path.read_text(encoding="utf-8"))
    assert payload["permissions"] == {"deny": ["Artifact"]}
    kept = dict(ROUTE_ENV)
    kept["ANTHROPIC_BASE_URL"] = "https://api.anthropic.com"
    payload = json.loads(Path(_write_settings_env_file(kept)).read_text(encoding="utf-8"))
    assert "permissions" not in payload
