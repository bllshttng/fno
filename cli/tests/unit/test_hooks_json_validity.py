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


def test_every_manifest_hook_references_an_existing_script() -> None:
    """Every manifest command must reference a script that exists in the repo.

    A hook whose command cannot resolve (PLUGIN_ROOT empty/unset, or a script
    removed) fails OPEN in Codex: it exits 127/2 and the guard was silently
    absent. The launch-path half (PreToolUse-only launching, the fail-open on
    an unresolved root, the wrong-placeholder refusal) is native now: the
    doctor family's Rust probe owns it (see
    crates/fno/src/doctor/report/hooks_probe.rs); the manifest-existence half
    stays here because it needs only the checked-in manifests."""
    import json as _json
    import re as _re

    failures = []
    for rel in ("hooks/hooks.json", "hooks/codex-hooks.json"):
        data = _json.loads((REPO_ROOT / rel).read_text(encoding="utf-8"))
        for _event, groups in (data.get("hooks") or {}).items():
            if not isinstance(groups, list):
                continue
            for group in groups:
                if not isinstance(group, dict):
                    continue
                for hook in group.get("hooks") or []:
                    command = hook.get("command", "") if isinstance(hook, dict) else ""
                    for tail in _re.findall(
                        r"\$\{(?:CLAUDE_)?PLUGIN_ROOT\}(\S+)", command
                    ):
                        if not (REPO_ROOT / tail).is_file():
                            failures.append(f"{rel}: missing script {tail}")
    assert not failures, (
        "manifest hooks reference missing scripts:\n  " + "\n  ".join(failures)
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
