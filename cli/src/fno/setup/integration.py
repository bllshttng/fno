"""The pi and agy integration legs left in Python.

``fno config setup auto-wire`` (Rust, setup_autowire.rs) installs every other
CLI natively and reads two lookups from here: the pi extension source and the
agy adapter paths. ``_pi_install`` and ``_agy_install`` drive the same
fno-agents doors for the live spawn journeys.
"""
from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Optional


@dataclass
class IntegrationResult:
    """Outcome of one CLI's integration install."""

    cli: str  # "claude" | "gemini" | "codex" | "opencode" | "agy"
    label: str  # human name, e.g. "Claude Code"
    status: str  # "installed" | "already-installed" | "manual" | "failed"
    note: str = ""  # detail (e.g. "skills-dir", a manual step, a failure reason)

    @property
    def ok(self) -> bool:
        # "manual" is NOT ok: a step succeeded but the integration is not yet
        # wired up, so it must never print as "installed".
        return self.status in ("installed", "already-installed")


# --- pi ---------------------------------------------------------------------
# pi is an extension harness (like opencode, not a plugin-marketplace CLI),
# measured 2026-08-28 as shipping no shell hook surface at all: its lifecycle
# boundary is the in-process `pi.on("agent_settled")` extension event. The
# integration is a local-file TypeScript extension copied into pi's global
# extension dir (~/.pi/agent/extensions/ - pi auto-discovers *.ts there, one
# install covers every project). Unlike codex, the installed state is
# verifiable (the file exists and matches the shipped source), so we can
# claim "installed" honestly.

def _pi_extension_src() -> Path:
    return Path(__file__).parent / "assets" / "pi" / "footnote.ts"


def _pi_is_installed() -> bool:
    from fno.rust_binary import call_binary_json

    _err, payload = call_binary_json(
        "plugin-install",
        ["pi", "--status", "--extension-src", str(_pi_extension_src()), "--json"],
    )
    return bool(payload and payload.get("installed"))


def _pi_install() -> IntegrationResult:
    from fno.rust_binary import call_binary_json

    _err, payload = call_binary_json(
        "plugin-install", ["pi", "--extension-src", str(_pi_extension_src()), "--json"]
    )
    if payload and payload.get("status"):
        return IntegrationResult(
            payload["cli"], payload["label"], payload["status"], note=payload["note"]
        )
    note = _err or "the fno-agents binary is missing or failed; run fno doctor update --rust"
    return IntegrationResult("pi", "pi", "failed", note=note)


# --- agy (Antigravity CLI) --------------------------------------------------
# agy is a native Stop-hook harness (like opencode, not a plugin-marketplace CLI).
# Its hooks are Claude-shaped event names with a Gemini-family wire format, so the
# integration registers footnote's Stop adapter in agy's hooks.json customization
# file (~/.gemini/config/hooks.json, the global dir - one install covers every
# project). The command references the adapter that ships in the plugin
# (hooks/footnote-agy-target-stop-hook.sh), resolved via the plugin-root pointer; a CLI-only
# install (uv/curl) carries no hooks/, so it degrades to a "manual" finish rather
# than wiring a path that does not exist.

def _agy_hooks_json() -> Path:
    return Path.home() / ".gemini" / "config" / "hooks.json"


def _agy_adapter_path() -> "Optional[Path]":
    # The adapter ships in the plugin (hooks/), which the uv/curl wheel does NOT
    # carry. The durable resolver always returns a path (last fallback may not
    # exist), so gate on is_file(): None means "not in this install" -> manual.
    from fno.paths import resolve_plugin_script_durable

    p = resolve_plugin_script_durable("hooks/footnote-agy-target-stop-hook.sh")
    return p if p.is_file() else None


def _agy_role_adapter_path() -> "Optional[Path]":
    # Same load-shape as _agy_adapter_path for the PreInvocation role adapter.
    # agy has no session-start event, so the role line rides PreInvocation
    # gated on invocationNum == 0 (first model call == session start).
    from fno.paths import resolve_plugin_script_durable

    p = resolve_plugin_script_durable("hooks/agy-team-inject.sh")
    return p if p.is_file() else None


def _agy_guard_adapter_path() -> "Optional[Path]":
    # Same load-shape for the PreToolUse lead guard shim; it translates agy's
    # payload to the shared lead guard through `lead-guard --wire agy`.
    from fno.paths import resolve_plugin_script_durable

    p = resolve_plugin_script_durable("hooks/agy-lead-guard.sh")
    return p if p.is_file() else None


def _agy_is_installed() -> bool:
    from fno.rust_binary import call_binary_json

    adapter = _agy_adapter_path()
    role = _agy_role_adapter_path()
    args = [
        "agy",
        "--hooks-status",
        "--hooks-file",
        str(_agy_hooks_json()),
        "--json",
    ]
    if adapter is not None:
        args += ["--adapter", str(adapter)]
    if role is not None:
        args += ["--promote", str(role)]
    guard = _agy_guard_adapter_path()
    if guard is not None:
        args += ["--guard", str(guard)]
    error, payload = call_binary_json("plugin-install", args)
    if error is not None or not isinstance(payload, dict):
        return False
    return bool(payload.get("installed"))


def _agy_install() -> IntegrationResult:
    label = "Antigravity CLI"
    adapter = _agy_adapter_path()
    if adapter is None:
        return IntegrationResult(
            "agy",
            label,
            "manual",
            note="adapter ships in the plugin (not this CLI-only install); wire "
            "hooks/footnote-agy-target-stop-hook.sh into ~/.gemini/config/hooks.json by hand",
        )
    from fno.rust_binary import call_binary_json

    # Probe first: a stale fno-agents binary IGNORES unknown flags and would
    # fall through to the old full plugin install. A current one answers
    # --hooks-status with a JSON status object.
    hooks_file = _agy_hooks_json()
    error, probe = call_binary_json(
        "plugin-install",
        ["agy", "--hooks-status", "--hooks-file", str(hooks_file), "--json"],
    )
    if error is not None or not isinstance(probe, dict) or "file" not in probe:
        return IntegrationResult(
            "agy",
            label,
            "failed",
            note="the fno-agents binary does not answer --hooks-status; run "
            "`fno doctor update --rust`",
        )
    args = [
        "agy",
        "--hooks",
        "--adapter",
        str(adapter),
        "--hooks-file",
        str(hooks_file),
        "--json",
    ]
    role = _agy_role_adapter_path()
    if role is not None:
        args += ["--promote", str(role)]
    guard = _agy_guard_adapter_path()
    if guard is not None:
        args += ["--guard", str(guard)]
    error, payload = call_binary_json("plugin-install", args)
    if error is not None:
        if "not found" in error:
            note = "fno-agents binary not found; run `fno doctor update --rust`"
        else:
            note = error
        return IntegrationResult("agy", label, "failed", note=note)
    note = "Stop hook installed"
    if isinstance(payload, dict):
        note = payload.get("note") or note
    return IntegrationResult("agy", label, "installed", note=note)
