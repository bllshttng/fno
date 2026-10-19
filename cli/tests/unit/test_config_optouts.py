from __future__ import annotations

import json
import logging
import subprocess
import sys
from types import SimpleNamespace

import pytest

from fno.config import settings_from_files
from fno.config import writer
from fno.claims import acquire_claim
from fno.claims import optout_lease
from fno.claims.optout_lease import restore_reaped_optouts


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


def test_second_holder_cannot_rewrite_a_live_opt_out(tmp_path, monkeypatch):
    monkeypatch.setenv("FNO_GLOBAL_SETTINGS_PATH", str(tmp_path / "settings.yaml"))
    monkeypatch.setenv("FNO_CLAIMS_ROOT", str(tmp_path / "global"))
    monkeypatch.setattr(optout_lease, "_resolve_optout_holder", lambda: "session-a")
    optout_lease.set_config_value("review.self_review_required", "false")

    monkeypatch.setattr(optout_lease, "_resolve_optout_holder", lambda: "session-b")
    with pytest.raises(writer.ConfigSetError, match="session-a"):
        optout_lease.set_config_value("review.self_review_required", "false")

    assert "self_review_required = false" in (
        tmp_path / "config.toml"
    ).read_text(encoding="utf-8")


def test_bare_writer_refuses_merge_gating_keys_fail_closed(tmp_path, monkeypatch):
    # The lease lane lives in fno.claims.optout_lease; fno.config may not
    # import the claims layer, so a plain writer set/unset without the
    # injected ops must refuse the key instead of writing an unleased opt-out.
    monkeypatch.setenv("FNO_GLOBAL_SETTINGS_PATH", str(tmp_path / "settings.yaml"))

    with pytest.raises(writer.ConfigSetError, match="claims lane"):
        writer.set_config_value("review.self_review_required", "false")
    with pytest.raises(writer.ConfigSetError, match="claims lane"):
        writer.unset_config_value("review.self_review_required")

    assert not (tmp_path / "settings.yaml").exists()


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


def test_restore_removes_the_recorded_prior_value(tmp_path):
    config = tmp_path / "config.toml"
    config.write_text(
        "[review]\nself_review_required = false\n",
        encoding="utf-8",
    )
    claim = SimpleNamespace(
        key="config-optout:review.self_review_required",
        metadata={
            "config_key": "review.self_review_required",
            "config_path": str(config),
            "scope": "global",
            "prior_present": False,
            "prior_value": None,
        },
    )

    assert restore_reaped_optouts([claim]) == []
    assert "self_review_required = false" not in config.read_text(encoding="utf-8")
    assert "self_review_required = true" not in config.read_text(encoding="utf-8")


def test_optout_restore_module_fails_loud_on_stderr():
    # The Rust reap leaf folds this module's stderr into its reap_failed
    # list; exit 1 plus that prefix is the seam's only error channel.
    claim = {
        "key": "config-optout:review.self_review_required",
        "metadata": {"config_key": "not.a.registered.optout", "config_path": ""},
    }
    proc = subprocess.run(
        [
            sys.executable,
            "-m",
            "fno.claims._optout_restore",
            "--claim",
            json.dumps(claim),
        ],
        capture_output=True,
        text=True,
        check=False,
    )

    assert proc.returncode == 1
    assert proc.stderr.startswith("opt-out restore failed:")
