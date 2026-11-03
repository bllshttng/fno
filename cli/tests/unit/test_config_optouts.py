"""Read-time revocation of merge-gating opt-outs. The write-time lease and
the reaper's restore are native (crates/fno/src/config_cli/write/), frozen
by the config_set parity goldens and the claim_reap parity case."""

from __future__ import annotations

import logging
import os
import subprocess

from fno.config import settings_from_files
from fno.rust_binary import resolve_binary


def test_unbacked_self_review_opt_out_reverts_to_default(
    tmp_path, monkeypatch, caplog
):
    config = tmp_path / "config.toml"
    config.write_text(
        "[review]\nself_review_required = false\n",
        encoding="utf-8",
    )
    monkeypatch.setenv("FNO_CLAIMS_ROOT", str(tmp_path / "claims-root"))
    caplog.set_level(logging.WARNING)

    settings = settings_from_files([config])

    assert settings.review.self_review_required is True
    assert "review.self_review_required" in caplog.text
    assert "free" in caplog.text


def test_explicit_empty_optional_apps_requires_a_live_claim(tmp_path, monkeypatch):
    config = tmp_path / "config.toml"
    config.write_text("[review]\noptional_apps = []\n", encoding="utf-8")
    monkeypatch.setenv("FNO_CLAIMS_ROOT", str(tmp_path / "global"))

    revoked = settings_from_files([config])
    assert revoked.review.optional_apps is None

    binary = resolve_binary()
    assert binary is not None, "the fno-agents binary is required"
    subprocess.run(
        [str(binary), "claim", "acquire", "config-optout:review.optional_apps",
         "--holder", "session-a", "--pid", str(os.getpid()), "--ttl", "30m"],
        check=True,
        capture_output=True,
    )
    honored = settings_from_files([config])
    assert honored.review.optional_apps == []


def test_unreadable_claim_instrument_revokes_and_names_instrument(
    tmp_path, monkeypatch, caplog
):
    config = tmp_path / "config.toml"
    config.write_text(
        "[review]\nself_review_required = false\n",
        encoding="utf-8",
    )
    monkeypatch.setenv("FNO_CLAIMS_ROOT", str(tmp_path / "global"))
    monkeypatch.setattr(
        "fno.rust_binary.call_binary_json",
        lambda *args, **kwargs: ("permission denied", None),
    )

    with caplog.at_level(logging.WARNING):
        settings = settings_from_files([config])

    assert settings.review.self_review_required is True
    assert "unreadable" in caplog.text
