"""``--check`` without ``--raw`` is a usage refusal, not a verdict.

The rest of this file's coverage moved with the raw-send leaf to the Rust
mail front, characterized by the parity goldens under
``crates/fno-agents/tests/golden/mail_send_raw/``.
"""
from __future__ import annotations

from typer.testing import CliRunner

from fno.mail.cli import mail_app
from fno.paths_testing import use_tmpdir


def test_check_without_raw_is_refused(tmp_path, monkeypatch):
    """Only the raw lane has a keystroke path to have or lack."""
    use_tmpdir(monkeypatch, tmp_path)
    result = CliRunner().invoke(mail_app, ["send", "me", "hello", "--check"])
    assert result.exit_code == 2
    combined = (result.stderr or "") + result.output
    assert "always has the durable fallback" in combined, combined
