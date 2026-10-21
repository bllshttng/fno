"""Validate the hook manifests: every registered path resolves, and each guard
that must reach both harnesses is wired exactly once on each."""
from __future__ import annotations

import json
import os
import re
import shlex
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[3]
HOOKS_JSON = REPO_ROOT / "hooks" / "hooks.json"
CODEX_HOOKS_JSON = REPO_ROOT / "hooks" / "codex-hooks.json"


def _command(command: str) -> str:
    command = command.removeprefix("PYTHONDONTWRITEBYTECODE=1 ").removeprefix("FNO_HOOK_CONTEXT=1 ")
    return " ".join(shlex.split(command))


def test_every_hook_command_runs_in_hook_context() -> None:
    # A hook's fno call must never join its session (crates/fno/src/join_marker.rs).
    for manifest in (HOOKS_JSON, CODEX_HOOKS_JSON):
        data = json.loads(manifest.read_text(encoding="utf-8"))
        for registrations in data["hooks"].values():
            for registration in registrations:
                for hook in registration.get("hooks", []):
                    assert "FNO_HOOK_CONTEXT=1 " in hook.get("command", ""), (manifest.name, hook)


def test_codex_hooks_use_supported_event_names_and_existing_commands() -> None:
    data = json.loads(CODEX_HOOKS_JSON.read_text(encoding="utf-8"))
    supported = {
        "PreToolUse",
        "PostToolUse",
        "PreCompact",
        "PostCompact",
        "SessionStart",
        "UserPromptSubmit",
        "SubagentStart",
        "SubagentStop",
        "Stop",
    }
    assert set(data["hooks"]).issubset(supported)

    copied_claude_only = {
        "WorktreeCreate",
        "CwdChanged",
        "FileChanged",
        "SessionEnd",
        "StopFailure",
    }
    assert set(data["hooks"]).isdisjoint(copied_claude_only)

    assert "${CODEX_PLUGIN_ROOT}" not in CODEX_HOOKS_JSON.read_text(encoding="utf-8")
    placeholder = re.compile(r"\$\{PLUGIN_ROOT(:-[^}]*)?\}")
    failures: list[str] = []
    for event, registrations in data["hooks"].items():
        for reg in registrations:
            for hook in reg.get("hooks", []):
                substituted = placeholder.sub(str(REPO_ROOT), hook.get("command", ""))
                m = re.search(rf"{re.escape(str(REPO_ROOT))}\S+", substituted)
                if m is None:
                    continue
                path = Path(m.group(0).rstrip(';"'))
                if not path.exists():
                    failures.append(f"{event}: missing path {path}")
    if failures:
        pytest.fail("codex-hooks.json references missing files:\n  " + "\n  ".join(failures))


def test_codex_stop_attester_precedes_target_loop() -> None:
    data = json.loads(CODEX_HOOKS_JSON.read_text(encoding="utf-8"))
    stop_hooks = [
        hook
        for registration in data["hooks"]["Stop"]
        for hook in registration.get("hooks", [])
    ]
    attesters = [
        hook for hook in stop_hooks if "hooks/code-review-attest.sh" in hook["command"]
    ]
    assert len(attesters) == 1
    assert attesters[0]["type"] == "command"
    assert not attesters[0].get("async", False)
    attester_index = stop_hooks.index(attesters[0])
    target_indices = [
        index
        for index, hook in enumerate(stop_hooks)
        if "hooks/target-stop-hook.sh" in hook["command"]
    ]
    assert len(target_indices) == 1
    assert attester_index < target_indices[0]


@pytest.mark.parametrize(
    ("config_path", "root_var"),
    [(HOOKS_JSON, "CLAUDE_PLUGIN_ROOT"), (CODEX_HOOKS_JSON, "PLUGIN_ROOT")],
)
def test_write_guard_is_registered_once_per_harness(
    config_path: Path, root_var: str
) -> None:
    """One location policy, reachable from both harnesses.

    A guard wired on only one manifest reads as protection and ships green
    while the other harness writes to canonical unchecked. Exactly one, so a
    second registration cannot double-judge a payload or hide a divergent
    classifier behind the same matcher.
    """
    data = json.loads(config_path.read_text(encoding="utf-8"))
    command = "bash ${%s}/hooks/worktree-write-protect.sh" % root_var
    registrations = [
        registration
        for registration in data["hooks"]["PreToolUse"]
        if registration.get("matcher") == "Edit|Write"
        and any(_command(hook.get("command", "")) == command for hook in registration.get("hooks", []))
    ]
    assert len(registrations) == 1, (
        f"{config_path.name} must route Edit and Write through the worktree "
        f"guard exactly once (found {len(registrations)})"
    )

    others = [
        hook.get("command", "")
        for registration in data["hooks"]["PreToolUse"]
        for hook in registration.get("hooks", [])
        if "worktree-write-protect.sh" in hook.get("command", "")
    ]
    assert len(others) == 1, f"{config_path.name} registers a second location classifier"


def test_plan_location_guard_wired_into_claude_and_codex_pretooluse() -> None:
    """The plan-save-location gate must fire on BOTH harnesses.

    A guard registered on one of N reachable paths is decorative: a codex
    session writing a plan never loads claude's hooks.json, and vice versa.
    Nothing else asserts these registrations, so dropping either one would
    leave the guard's own harness green while silently disabling it for half
    the fleet.
    """
    guard = "hooks/plan-location-guard.sh"
    assert (REPO_ROOT / guard).is_file(), f"guard missing at {guard}"

    for path, root_var, matcher in (
        (HOOKS_JSON, "CLAUDE_PLUGIN_ROOT", "Write"),
        (CODEX_HOOKS_JSON, "PLUGIN_ROOT", "Edit|Write"),
    ):
        registrations = json.loads(path.read_text(encoding="utf-8"))["hooks"][
            "PreToolUse"
        ]
        wired = [
            registration
            for registration in registrations
            if registration.get("matcher") == matcher
            and any(
                _command(hook.get("command", "")) == f"bash ${{{root_var}}}/{guard}"
                for hook in registration.get("hooks", [])
            )
        ]
        assert len(wired) == 1, (
            f"{path.name} must carry exactly one PreToolUse registration "
            f"routing matcher {matcher!r} through {guard}"
        )


def test_every_manifest_hook_is_wired_and_pretooluse_launches() -> None:
    """Every manifest command must be wired (script exists), and the read-only
    PreToolUse gates must start under the real ``$SHELL -lc`` launch path.

    A hook whose command cannot resolve (PLUGIN_ROOT empty/unset, or a script
    removed) fails OPEN in Codex: it exits 127/2 and the guard was silently
    absent. Verifying the hand-expanded absolute path always passes, because that
    path cannot reproduce a placeholder-expansion failure; only the configured
    string through the shell Codex actually invokes (codex-rs/hooks engine
    command_runner.rs) can.

    Only PreToolUse gates are LAUNCHED: SessionStart/Stop and other stateful
    hooks mutate ~/.fno when executed (inject-mail-drain advances the real mail
    cursor), so running them from a test would damage live state. Stateful
    events still get an existence check. This is the CI-catchable half; the
    live-install half is `fno doctor`. Reuses the doctor launcher so the
    isolated-temp-cwd and event-scope invariants live in one place.
    """
    shell = os.environ.get("SHELL")
    if not shell:
        pytest.skip("$SHELL unset; cannot reproduce the real launch path")

    from fno.doctor import (
        _PROBE_LAUNCH_EVENTS,
        _PROBE_MANIFESTS,
        _is_hook_launch_failure,
        _launch_plugin_hook,
        _manifest_event_commands,
        _referenced_hook_scripts,
    )

    # The probe must only execute read-only gate events. Pin the scope here so a
    # future change that adds SessionStart (stateful, mutates ~/.fno) to the
    # launch set fails this test instead of silently running it.
    assert _PROBE_LAUNCH_EVENTS == ("PreToolUse",), (
        "probe may only launch read-only PreToolUse gates; adding a stateful "
        "event here would execute it against the probe env"
    )

    failures: list[str] = []
    samples: dict[str, tuple[str, str]] = {}  # manifest name -> (command, root_var)
    for name, rel, root_var in _PROBE_MANIFESTS:
        data = json.loads((REPO_ROOT / rel).read_text(encoding="utf-8"))
        for event, command in _manifest_event_commands(data, source=name):
            for script in _referenced_hook_scripts(command, REPO_ROOT):
                if not script.is_file():
                    failures.append(f"{name}/{event}: missing script {script}")
            if event in _PROBE_LAUNCH_EVENTS:
                samples.setdefault(name, (command, root_var))
                result = _launch_plugin_hook(
                    command, root_value=str(REPO_ROOT), shell=shell, root_var=root_var
                )
                if _is_hook_launch_failure(result):
                    failures.append(
                        f"{name}/{event}: rc={result['rc']} cmd={result['resolved'][:70]}"
                    )
    assert not failures, (
        "manifest hooks missing or failing to launch through $SHELL -lc:\n  "
        + "\n  ".join(failures)
    )

    # The advertised fail-open itself: an UNRESOLVED root must make every
    # manifest's PreToolUse launch fail. Asserted per manifest so a broken root
    # on one harness cannot be masked by another; requiring every manifest to
    # contribute a sample means dropping PreToolUse from one manifest fails here
    # instead of being hidden by the other.
    expected = {name for name, _rel, _root in _PROBE_MANIFESTS}
    assert set(samples) == expected, (
        f"every manifest must carry a PreToolUse gate to probe; got {sorted(samples)}"
    )
    for name, (command, root_var) in samples.items():
        empty_root = _launch_plugin_hook(
            command, root_value="", shell=shell, root_var=root_var
        )
        assert _is_hook_launch_failure(empty_root), (
            f"{name}: empty plugin root must fail the launch (the fail-open), "
            f"got rc={empty_root['rc']}"
        )

    # Placeholder fidelity: a command using the OTHER harness's root variable
    # must fail when only this manifest's var is set, because the real harness
    # does not set the other var. Pins that the probe is not masking a
    # wrong-placeholder manifest into a false green.
    wrong = _launch_plugin_hook(
        "bash ${CLAUDE_PLUGIN_ROOT}/hooks/write-gate.sh",
        root_value=str(REPO_ROOT),
        shell=shell,
        root_var="PLUGIN_ROOT",
    )
    assert _is_hook_launch_failure(wrong), (
        "a command using ${CLAUDE_PLUGIN_ROOT} must fail when only PLUGIN_ROOT "
        f"is set (real Codex sets no CLAUDE_PLUGIN_ROOT); got rc={wrong['rc']}"
    )


def test_bg_process_guard_wired_beside_git_protection_on_both_harnesses() -> None:
    """One shared dispatcher must own the complete Bash guard chain on BOTH lanes.

    A concurrent branch rewriting either manifest can drop a registration
    and still merge clean, so the assertion pins one command per harness.
    Guard ordering is owned inside the dispatcher and covered at that boundary.
    """
    # The chain lives inside the dispatcher now; these are its guards.
    for guard in (
        "hooks/git-protection.py",
        "hooks/bin-install-guard.sh",
        "hooks/bg-process-guard.py",
        "hooks/pipe-guard.sh",
        "hooks/recursive-grep-guard.py",
        "hooks/test-run-guard.sh",
    ):
        assert (REPO_ROOT / guard).is_file(), f"guard missing at {guard}"

    for path, root_var, matcher in (
        (HOOKS_JSON, "CLAUDE_PLUGIN_ROOT", "Bash"),
        (CODEX_HOOKS_JSON, "PLUGIN_ROOT", "^Bash$"),
    ):
        registrations = [
            registration
            for registration in json.loads(path.read_text(encoding="utf-8"))[
                "hooks"
            ]["PreToolUse"]
            if registration.get("matcher") == matcher
        ]
        assert len(registrations) == 1, (
            f"{path.name} must carry exactly one PreToolUse {matcher!r} "
            f"registration, found {len(registrations)}"
        )
        commands = [
            _command(hook.get("command", "")) for hook in registrations[0].get("hooks", [])
        ]
        expected = [f"bash ${{{root_var}}}/hooks/pretooluse-bash-dispatch.sh"]
        assert commands == expected, (
            f"{path.name} PreToolUse {matcher!r} chain drifted: {commands}"
        )


def _declaration_groups() -> dict:
    return json.loads(
        (REPO_ROOT / "hooks" / "context-hooks.json").read_text(encoding="utf-8")
    )["groups"]


def test_context_hooks_declaration() -> None:
    """The runner declaration satisfies the contract the manifests relied on.

    Every group a manifest names through context-run.sh exists; producer ids
    are unique within a group; every argv[0] resolves to an executable script
    under the repo (the exec-bit gate scans this file too)."""
    groups = _declaration_groups()
    named = set()
    for path in (HOOKS_JSON, CODEX_HOOKS_JSON):
        manifest_named = set()
        for registrations in json.loads(path.read_text(encoding="utf-8"))["hooks"].values():
            for registration in registrations:
                for hook in registration.get("hooks", []):
                    match = re.search(r"context-run\.sh\s+([\w-]+)", _command(hook["command"]))
                    if match:
                        manifest_named.add(match.group(1))
        assert manifest_named, f"{path.name} has no context runner registration"
        named.update(manifest_named)
    assert named <= set(groups), f"manifest names unknown groups: {sorted(named - set(groups))}"
    for name, group in groups.items():
        ids = [producer["id"] for producer in group["producers"]]
        assert len(ids) == len(set(ids)), f"group {name} has duplicate producer ids"
        for producer in group["producers"]:
            argv0 = producer["argv"][0]
            resolved = REPO_ROOT / argv0.replace("${PLUGIN_ROOT}/", "")
            assert resolved.is_file(), f"group {name}: missing script {resolved}"
            assert os.access(resolved, os.X_OK), (
                f"group {name}: not executable {resolved}"
            )
