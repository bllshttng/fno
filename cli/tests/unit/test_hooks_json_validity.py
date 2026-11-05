"""Validate the hook manifests: every registered path resolves, and each guard
that must reach both harnesses is wired exactly once on each."""
from __future__ import annotations

import json
import os
import re
import subprocess
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[3]
CODEX_HOOKS_JSON = REPO_ROOT / "hooks" / "codex-hooks.json"


CODEX_EXIT_127_TAIL = " || { rc=$?; [ $rc -eq 127 ] || exit $rc; echo '"


def test_codex_hook_names_itself_when_its_file_is_gone(tmp_path: Path) -> None:
    # Codex pins a session to one plugin version folder, and an update deletes
    # that folder. The command text in memory is then the only code that can run.
    data = json.loads(CODEX_HOOKS_JSON.read_text(encoding="utf-8"))
    commands = [
        hook["command"]
        for registrations in data["hooks"].values()
        for registration in registrations
        for hook in registration["hooks"]
    ]
    assert commands
    env = {**os.environ, "PLUGIN_ROOT": str(tmp_path / "deleted-version")}
    for command in commands:
        assert CODEX_EXIT_127_TAIL in command, command
        run = subprocess.run(
            ["bash", "-c", command], env=env, input="{}", capture_output=True, text=True
        )
        message = json.loads(run.stdout)["systemMessage"]
        script = re.findall(r"([\w.-]+\.(?:sh|py))\"", command)[-1]
        assert run.returncode == 0, command
        assert f"fno hook {script}" in message, command

    # Any other exit stays the hook's own answer: 2 is a block in Codex.
    tail = commands[0].split(CODEX_EXIT_127_TAIL)[1]
    blocked = subprocess.run(
        ["bash", "-c", "(exit 2)" + CODEX_EXIT_127_TAIL + tail], capture_output=True, text=True
    )
    assert blocked.returncode == 2
    assert blocked.stdout == ""


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
                        # The command string quotes its path, and the
                        # placeholder replacement is root-anchored.
                        script = tail.rstrip("\"';").lstrip("/")
                        if not (REPO_ROOT / script).is_file():
                            failures.append(f"{rel}: missing script {script}")
    assert not failures, (
        "manifest hooks reference missing scripts:\n  " + "\n  ".join(failures)
    )
