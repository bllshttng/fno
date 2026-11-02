from __future__ import annotations

from pathlib import Path

import pytest

from fno.paths_testing import use_tmpdir


BREVITY_MARKER = "<fno_relay_compression>"
FIXTURE = Path(__file__).resolve().parents[3] / "schemas" / "spawn-brevity.json"
ORIGINAL_PAYLOAD = """Report why `fno mail send` did not run.

```bash
fno mail send worker --raw '/fno:review'
```

Keep 80 words, not 81."""


def _setup_tmp_home(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    use_tmpdir(monkeypatch, tmp_path)
    home = tmp_path / "home"
    home.mkdir(exist_ok=True)
    monkeypatch.setenv("HOME", str(home))
    for key in ("FNO_AGENT_SELF", "FNO_AGENT_HARNESS", "FNO_AGENT_SESSION"):
        monkeypatch.delenv(key, raising=False)


def test_python_payload_enrichment_is_idempotent_and_preserves_empty_spawn() -> None:
    from fno.agents.spawn_payload import enrich_spawn_payload

    assert enrich_spawn_payload("") == ""
    enriched = enrich_spawn_payload(ORIGINAL_PAYLOAD)
    assert enrich_spawn_payload(enriched) == enriched
    assert enriched[: len(ORIGINAL_PAYLOAD)] == ORIGINAL_PAYLOAD


