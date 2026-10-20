"""The spawn seam's end-to-end contract over the real compose.

The composition is characterized by the Rust goldens
(crates/fno-agents/tests/fixtures/spawn_compose/); the seam-visibility tests
that walked the old Python body live there now. What stays here is the path
no golden can pin: a real `fno-agents` binary answering the transport's one
verb call under a pinned FNO_CONFIG.
"""
from __future__ import annotations

import io
from pathlib import Path

import pytest

import fno.agents.spawn_defaults as sd

requires_rust = pytest.mark.dev_build


def test_retired_bg_substrate_refuses_with_the_redirect(capsys):
    """The bg spelling is retired, not deprecated: one line, the replacement,
    exit 2. thread keeps working and canonicalizes to the internal selector."""
    import pytest

    from fno.agents.spawn_defaults import resolve_spawn_gates

    with pytest.raises(SystemExit) as e:
        resolve_spawn_gates("bg", None, once=False, harness="claude")
    assert e.value.code == 2
    assert "substrate 'bg' was retired" in capsys.readouterr().err
    assert resolve_spawn_gates("thread", None, once=False, harness="claude") == "bg"


def _pin_world(tmp_path: Path, monkeypatch: pytest.MonkeyPatch, config: str) -> Path:
    """The real path: this checkout's binary, one config, one journal."""
    from fno.rust_binary import find_dev_binary

    binary = find_dev_binary()
    if binary is None:
        pytest.skip("no fno-agents dev build (cargo build -p fno-agents)")
    monkeypatch.setenv("FNO_AGENTS_BIN", str(binary))
    config_path = tmp_path / "config.toml"
    config_path.write_text(config, encoding="utf-8")
    monkeypatch.setenv("FNO_CONFIG", str(config_path))
    journal = tmp_path / "events.jsonl"
    monkeypatch.setenv("FNO_EVENTS_PATH", str(journal))
    monkeypatch.setenv("FNO_NO_CANONICAL_CONFIG", "1")
    return journal


@requires_rust
def test_config_provider_is_inert_end_to_end(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """One config field, one binary round trip: the retired profile scalar
    injects no --harness, stderr stays quiet, and the journal row lands."""
    journal = _pin_world(
        tmp_path, monkeypatch, '[agents.profiles.target]\nprovider = "codex"\n'
    )
    err = io.StringIO()
    out = sd.compose_spawn_argv(
        ["spawn", "--name", "w", "/fno:target x-1"], stderr=err
    )
    assert "--harness" not in out, "the profile provider scalar is inert"
    printed = err.getvalue()
    assert ": applied " not in printed, printed
    assert len([ln for ln in printed.splitlines() if ln.strip()]) <= 2, printed
    from fno.events.log import read_events

    rows = read_events(journal)
    applied = [r for r in rows if r.get("type") == "spawn_defaults_applied"]
    assert applied, "exactly the compose wrote the row"
    assert applied[-1]["data"]["verb"] == "target"
    err = io.StringIO()
    sd.compose_spawn_argv(
        ["spawn", "--name", "w", "--verbose", "/fno:target x-1"], stderr=err
    )
    assert "applied harness=" not in err.getvalue(), "even --verbose names no inert scalar"
