"""ReachabilityProbeError: every provider's inconclusive probe raises the
shared base class with its ``provider`` discriminator.
"""
from __future__ import annotations

import pytest

from fno.agents.harnesses.base import ReachabilityProbeError


@pytest.mark.parametrize("provider", ["claude", "codex"])
def test_provider_reachability_probes_raise_shared_base_class(
    provider: str,
    tmp_path,
    monkeypatch,
) -> None:
    """Contract-drift guard: every provider's inconclusive probe path
    raises the shared base class and sets the provider discriminator.
    """
    if provider == "claude":
        from fno.agents.harnesses import claude as claude_mod

        def _raise_os_error(*args, **kwargs):
            raise OSError("simulated probe failure")

        monkeypatch.setattr(claude_mod, "_subprocess_run", _raise_os_error)

        def probe():
            return claude_mod.claude_logs_reachable("abc12345", timeout=0.01)
    elif provider == "codex":
        from fno.agents.harnesses import codex as codex_mod

        unreadable_index = tmp_path / "session_index.jsonl"
        unreadable_index.mkdir()

        def probe():
            return codex_mod.load_known_session_ids(session_index_path=unreadable_index)
    with pytest.raises(ReachabilityProbeError) as exc_info:
        probe()

    assert exc_info.value.provider == provider
