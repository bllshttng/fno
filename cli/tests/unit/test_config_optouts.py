"""Read-time revocation of merge-gating opt-outs. The write-time lease and
the reaper's restore are native (crates/fno/src/config_cli/write/), frozen
by the config_set parity goldens and the claim_reap parity case."""

from __future__ import annotations

import logging

from fno.config import settings_from_files
from fno.claims import acquire_claim
from fno.claims import optout_lease


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

    acquire_claim(
        "config-optout:review.optional_apps",
        "session-a",
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
        optout_lease,
        "claim_status",
        lambda *args, **kwargs: (_ for _ in ()).throw(OSError("permission denied")),
    )

    with caplog.at_level(logging.WARNING):
        settings = settings_from_files([config])

    assert settings.review.self_review_required is True
    assert "unreadable" in caplog.text
