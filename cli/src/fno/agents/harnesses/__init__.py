"""Harness adapters for fno agents (Phase 1 substrate).

Each maintained headless harness module (claude, codex) owns its subprocess
adapter. Gemini remains a readable/pane-hosted legacy identity, but has no
Python ask adapter.

The exported roster names below keep their ``PROVIDER`` spelling on purpose:
they are the wire vocabulary of the ``provider`` config field and the registry
rows, which the four-axis ruling deliberately leaves in place. Renaming the
package fixed the container; the field is a separate, ruled-out surface.
"""


def __getattr__(name: str) -> tuple[str, ...]:
    """PEP 562: the roster-backed names resolve on first access, never at import."""
    if name == "SPAWN_HARNESSES":
        from fno.harness_names import SPAWN_HARNESSES as spawn_set
        return spawn_set
    if name in ("READABLE_PROVIDERS", "KNOWN_PROVIDERS"):
        from fno.harness_names import known_providers
        return known_providers()
    raise AttributeError(name)
