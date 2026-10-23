"""Tests for the managed credential store (US1 register, US2 switch).

The slot backend (Keychain / credential file) is patched to a fake dict so the
orchestration - capture-before-overwrite, live-pin gate, verification+rollback,
atomic store writes - is exercised without touching the real Keychain/network.

Run: cd cli && uv run pytest src/fno/adapters/providers/test_managed.py -v
"""
from __future__ import annotations

import json
import subprocess

import pytest
from typer.testing import CliRunner

from fno.adapters.providers import managed
from fno.adapters.providers.cli import cli as providers_app
from fno.adapters.providers.model import _HARNESS_LITERAL, ProviderRecord, ProvidersConfig

runner = CliRunner()


def _blob(token: str) -> str:
    return json.dumps({"claudeAiOauth": {"accessToken": token}})


def _pids(root) -> tuple[int, ...]:
    """The pids recorded on the claude taint marker."""
    writers = managed.tainting_writers("claude", root)
    assert writers is not None, "no taint marker to read pids from"
    return tuple(pid for pid, _started in writers)


def _codex_blob(token: str) -> str:
    return json.dumps(
        {
            "auth_mode": "chatgpt",
            "tokens": {
                "access_token": token,
                "refresh_token": f"refresh-{token}",
                "id_token": "header.payload.signature",
            },
        }
    )


def _rec(id_: str, harness: _HARNESS_LITERAL = "claude") -> ProviderRecord:
    return ProviderRecord(id=id_, name=id_, harness=harness, auth="managed")


@pytest.fixture(autouse=True)
def no_profile_network(monkeypatch):
    """Principal resolution reaches ``/api/oauth/profile``; unit tests must not.

    Default every test to an unreachable endpoint, which ``slot_principal``
    classifies as ``profile-unavailable``. A test that cares about the profile
    overrides this - either by patching ``urlopen`` itself or by patching
    ``managed.slot_principal`` - so no test silently depends on a live account.
    """
    import urllib.error

    def _no_network(*_args, **_kwargs):
        raise urllib.error.URLError("network disabled in tests")

    monkeypatch.setattr(managed.urllib.request, "urlopen", _no_network)


@pytest.fixture(autouse=True)
def no_vault_network(monkeypatch):
    """Keep existing switch tests hermetic while the Rust actor owns decisions."""
    original = managed._vault
    monkeypatch.setattr(
        managed,
        "_vault",
        lambda action, *args: {
            "action": action,
            "verdict": "unchanged" if action == "sync" else "fresh",
        },
    )
    return original


@pytest.fixture()
def fake_slot(monkeypatch):
    """A fake credential slot: {cli: blob}. Patches the read/write seam and
    forces the live-pin gate clear by default."""
    slot: dict[str, str | None] = {}
    monkeypatch.setattr(managed, "_read_slot_blob", lambda cli, config_dir=None: slot.get(cli))
    monkeypatch.setattr(
        managed, "_write_slot_blob", lambda cli, blob, config_dir=None: slot.__setitem__(cli, blob)
    )
    monkeypatch.setattr(managed, "pinning_sessions", lambda config_dir=None: [])
    monkeypatch.setattr(
        managed, "canonical_slot_blobs",
        lambda cli: [slot[cli]] if slot.get(cli) else [],
    )
    return slot


# ---------------------------------------------------------------------------
# US1: register / snapshot
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# US2: switch (materialize)
# ---------------------------------------------------------------------------


def _register_two(fake_slot, tmp_path):
    """work-a stored from A0, work-b stored from B0, slot left holding B (active)."""
    a, b = _rec("work-a"), _rec("work-b")
    fake_slot["claude"] = _blob("A0")
    managed.snapshot_current(a, root=tmp_path)
    managed._atomic_write_private(managed._active_stamp_path("claude", tmp_path), "work-a")
    fake_slot["claude"] = _blob("B0")
    managed.snapshot_current(b, root=tmp_path)
    managed._atomic_write_private(managed._active_stamp_path("claude", tmp_path), "work-b")
    return {"work-a": a, "work-b": b}


class TestSwitch:
    def test_materializes_and_verifies(self, fake_slot, tmp_path):
        """AC2-HP: use work-a materializes A's blob into the slot and verifies."""
        by_id = _register_two(fake_slot, tmp_path)
        result = managed.switch(by_id["work-a"], by_id=by_id, root=tmp_path)
        assert result.active == "work-a"
        assert fake_slot["claude"] == _blob("A0")
        assert managed.active_slot_id("claude", tmp_path) == "work-a"

    def test_spent_target_refuses_before_writing_the_slot(self, fake_slot, tmp_path, monkeypatch):
        by_id = _register_two(fake_slot, tmp_path)
        before = fake_slot["claude"]
        monkeypatch.setattr(managed, "_vault", lambda action, *args: {"verdict": "dead"})
        monkeypatch.setattr(
            managed,
            "_write_slot_blob",
            lambda *_args, **_kwargs: pytest.fail("spent credential reached the slot writer"),
        )
        with pytest.raises(managed.ManagedStoreError, match=r"is spent \(invalid_grant\)"):
            managed.switch(by_id["work-a"], by_id=by_id, root=tmp_path)
        assert fake_slot["claude"] == before

    def test_missing_vault_binary_refuses_before_writing_the_slot(
        self, fake_slot, tmp_path, monkeypatch, no_vault_network
    ):
        from fno import rust_binary

        by_id = _register_two(fake_slot, tmp_path)
        before = fake_slot["claude"]
        monkeypatch.setattr(managed, "_vault", no_vault_network)
        monkeypatch.setattr(rust_binary, "find_dev_binary", lambda: None)
        monkeypatch.setattr(rust_binary, "resolve_binary", lambda: None)
        with pytest.raises(managed.ManagedStoreError, match="fno-agents binary not found"):
            managed.switch(by_id["work-a"], by_id=by_id, root=tmp_path)
        assert fake_slot["claude"] == before

    def test_refreshed_target_blob_is_the_one_materialized(self, fake_slot, tmp_path, monkeypatch):
        by_id = _register_two(fake_slot, tmp_path)
        refreshed = _blob("A_REFRESHED")

        def vault(action, *args):
            if action == "refresh":
                managed._atomic_write_private(tmp_path / "work-a" / "blob", refreshed)
                return {"verdict": "refreshed"}
            return {"verdict": "unchanged"}

        monkeypatch.setattr(managed, "_vault", vault)
        managed.switch(by_id["work-a"], by_id=by_id, root=tmp_path)
        assert fake_slot["claude"] == refreshed

    def test_already_active_is_noop(self, fake_slot, tmp_path):
        by_id = _register_two(fake_slot, tmp_path)  # active = work-b
        result = managed.switch(by_id["work-b"], by_id=by_id, root=tmp_path)
        assert result.active == "work-b"

    def test_stale_stamp_rematerializes_not_silent_noop(self, fake_slot, tmp_path):
        """codex P2: stamp names the target but the slot holds different creds
        (out-of-band /login) - re-materialize instead of a false no-op."""
        by_id = _register_two(fake_slot, tmp_path)  # stamp=work-b, slot=B0
        fake_slot["claude"] = _blob("SOMEONE_ELSE")  # slot changed out-of-band
        managed.switch(by_id["work-b"], by_id=by_id, root=tmp_path)
        assert fake_slot["claude"] == _blob("B0")  # re-materialized work-b's stored blob

    def test_emits_account_switched(self, fake_slot, tmp_path, monkeypatch):
        by_id = _register_two(fake_slot, tmp_path)
        events: list[tuple] = []
        monkeypatch.setattr(
            managed,
            "_codex_login_ok",
            lambda: pytest.fail("Claude switch must not probe Codex"),
        )
        managed.switch(
            by_id["work-a"], by_id=by_id, root=tmp_path,
            emit_fn=lambda kind, **d: events.append((kind, d)),
        )
        assert events == [
            (
                "account_switched",
                {"provider": "work-a", "account_id": "work-a", "outgoing": "work-b"},
            )
        ]


class TestSwitchGuards:
    def test_live_pin_defer_policy_refuses_and_leaves_slot_untouched(
        self, fake_slot, tmp_path, monkeypatch
    ):
        """pin_policy="defer" (the recovery path): a pinned slot defers, names the
        session, and mutates nothing."""
        by_id = _register_two(fake_slot, tmp_path)
        monkeypatch.setattr(
            managed, "pinning_sessions",
            lambda config_dir=None: [managed.PinningSession(4242, "claude")],
        )
        before = fake_slot["claude"]
        stored_b = (tmp_path / "work-b" / "blob").read_text()
        with pytest.raises(managed.SwitchDeferred) as exc:
            managed.switch(by_id["work-a"], by_id=by_id, root=tmp_path, pin_policy="defer")
        assert "4242" in str(exc.value)
        assert fake_slot["claude"] == before  # slot untouched
        assert (tmp_path / "work-b" / "blob").read_text() == stored_b  # store untouched

    def test_live_pin_default_warns_and_proceeds(self, fake_slot, tmp_path, monkeypatch):
        """Default policy: a pinned claude slot switches anyway (the same rewrite a
        manual /login performs) and reports the pinning pids on the result."""
        by_id = _register_two(fake_slot, tmp_path)
        monkeypatch.setattr(
            managed, "pinning_sessions",
            lambda config_dir=None: [managed.PinningSession(4242, "claude")],
        )
        result = managed.switch(by_id["work-a"], by_id=by_id, root=tmp_path)
        assert result.active == "work-a"
        assert result.pinned_by == (4242,)
        assert fake_slot["claude"] == (tmp_path / "work-a" / "blob").read_text()
        assert managed.active_slot_id("claude", tmp_path) == "work-a"

    def test_unpinned_switch_reports_no_pins(self, fake_slot, tmp_path):
        by_id = _register_two(fake_slot, tmp_path)
        result = managed.switch(by_id["work-a"], by_id=by_id, root=tmp_path)
        assert result.pinned_by == ()
        assert not managed.slot_tainted("claude", tmp_path)

    def test_invalid_pin_policy_rejected(self, fake_slot, tmp_path):
        by_id = _register_two(fake_slot, tmp_path)
        with pytest.raises(ValueError, match="invalid pin_policy"):
            managed.switch(by_id["work-a"], by_id=by_id, root=tmp_path, pin_policy="deferr")

    def test_pinned_switch_taints_stamp_and_skips_next_capture(
        self, fake_slot, tmp_path, monkeypatch
    ):
        """A warn-under-pin switch leaves an untrustworthy stamp: a pinned session
        may overwrite the slot afterward, so the NEXT switch must not capture the
        slot blob into the stamped account's snapshot (poisoning it)."""
        by_id = _register_two(fake_slot, tmp_path)
        monkeypatch.setattr(
            managed, "pinning_sessions",
            lambda config_dir=None: [managed.PinningSession(4242, "claude")],
        )
        managed.switch(by_id["work-a"], by_id=by_id, root=tmp_path)
        assert managed.slot_tainted("claude", tmp_path)

        # A pinned work-b session refreshes and overwrites the slot out-of-band.
        fake_slot["claude"] = _blob("B-rotated")
        snapshot_a = (tmp_path / "work-a" / "blob").read_text()

        # Pin-free switch back to work-b: capture must be skipped (work-a's
        # snapshot untouched by the foreign blob) and the taint cleared.
        monkeypatch.setattr(managed, "pinning_sessions", lambda config_dir=None: [])
        managed.switch(by_id["work-b"], by_id=by_id, root=tmp_path)
        assert (tmp_path / "work-a" / "blob").read_text() == snapshot_a
        assert not managed.slot_tainted("claude", tmp_path)

    def test_missing_snapshot_refuses(self, fake_slot, tmp_path):
        """Boundary: never materialize an account with no stored snapshot."""
        by_id = {"work-a": _rec("work-a")}
        managed._atomic_write_private(managed._active_stamp_path("claude", tmp_path), "work-b")
        with pytest.raises(managed.ManagedStoreError):
            managed.switch(by_id["work-a"], by_id=by_id, root=tmp_path)

    def test_failed_verify_rolls_back(self, fake_slot, tmp_path, monkeypatch):
        """AC3-ERR shape: a stale/revoked stored token fails verification and the
        slot rolls back to the captured outgoing blob."""
        by_id = _register_two(fake_slot, tmp_path)
        outgoing_blob = fake_slot["claude"]  # B0
        monkeypatch.setattr(managed, "verify_slot", lambda record, expected_blob: False)
        with pytest.raises(managed.ManagedStoreError):
            managed.switch(by_id["work-a"], by_id=by_id, root=tmp_path)
        assert fake_slot["claude"] == outgoing_blob  # rolled back to B
        assert managed.active_slot_id("claude", tmp_path) == "work-b"  # stamp not advanced

    def test_rollback_failure_reported_truthfully(self, fake_slot, tmp_path, monkeypatch):
        """When verify fails AND the rollback write also fails, the receipt says
        the slot is indeterminate - it never lies 'rolled back'."""
        by_id = _register_two(fake_slot, tmp_path)
        monkeypatch.setattr(managed, "verify_slot", lambda record, expected_blob: False)
        calls = {"n": 0}

        def _write(cli, blob, config_dir=None):
            calls["n"] += 1
            if calls["n"] >= 2:  # the rollback write
                raise managed.KeychainError("rollback write denied")
            fake_slot[cli] = blob

        monkeypatch.setattr(managed, "_write_slot_blob", _write)
        with pytest.raises(managed.ManagedStoreError) as exc:
            managed.switch(by_id["work-a"], by_id=by_id, root=tmp_path)
        assert "indeterminate" in str(exc.value)


# ---------------------------------------------------------------------------
# Model: managed auth strategy takes neither credentials_source nor env
# ---------------------------------------------------------------------------


class TestManagedRecordValidation:
    def test_managed_rejects_credentials_source(self):
        from pathlib import Path

        with pytest.raises(ValueError, match="auth=managed"):
            ProviderRecord(
                id="bad", name="bad", harness="claude", auth="managed",
                credentials_source=Path("/tmp/x"),
            )

    def test_managed_rejects_env(self):
        with pytest.raises(ValueError, match="auth=managed"):
            ProviderRecord(
                id="bad", name="bad", harness="claude", auth="managed",
                env={"ANTHROPIC_API_KEY": "x"},
            )

    def test_managed_bare_record_ok(self):
        rec = ProviderRecord(id="ok", name="ok", harness="claude", auth="managed")
        assert rec.auth == "managed" and rec.account_id == "ok"


# ---------------------------------------------------------------------------
# AC2-ERR: Keychain denial / timeout surfaces a receipt, never a hang
# ---------------------------------------------------------------------------


class TestKeychainErrors:
    def test_security_timeout_raises_receipt(self, monkeypatch):
        def _boom(*a, **k):
            raise subprocess.TimeoutExpired(cmd="security", timeout=5)

        monkeypatch.setattr(managed.subprocess, "run", _boom)
        with pytest.raises(managed.KeychainError):
            managed._run_security(["find-generic-password"])

    def test_security_oserror_raises_receipt(self, monkeypatch):
        monkeypatch.setattr(
            managed.subprocess, "run", lambda *a, **k: (_ for _ in ()).throw(OSError("nope"))
        )
        with pytest.raises(managed.KeychainError):
            managed._run_security(["add-generic-password"])


# ---------------------------------------------------------------------------
# AC1-FR: atomic store write leaves no partial on failure
# ---------------------------------------------------------------------------


class TestAtomicWrite:
    def test_no_partial_on_write_error(self, tmp_path, monkeypatch):
        target = tmp_path / "blob"
        target.write_text("original")

        def _boom(*a, **k):
            raise RuntimeError("disk full mid-write")

        monkeypatch.setattr(managed.os, "replace", _boom)
        with pytest.raises(RuntimeError):
            managed._atomic_write_private(target, "new-secret")
        assert target.read_text() == "original"  # untouched
        # No leftover temp files.
        assert list(tmp_path.glob(".blob.*.tmp")) == []

    def test_fchmod_failure_cleans_up_temp(self, tmp_path, monkeypatch):
        """A fchmod failure before fdopen takes the fd must still clean the temp
        (and not leak the fd - covered by closing it in the except)."""
        monkeypatch.setattr(
            managed.os, "fchmod", lambda *a, **k: (_ for _ in ()).throw(OSError("denied"))
        )
        with pytest.raises(OSError):
            managed._atomic_write_private(tmp_path / "blob", "secret")
        assert list(tmp_path.glob(".blob.*.tmp")) == []


# ---------------------------------------------------------------------------
# Real file-backed slot (codex auth.json) - exercises the un-mocked backend
# ---------------------------------------------------------------------------


def _register_codex_pair(tmp_path, monkeypatch):
    auth = tmp_path / ".codex" / "auth.json"
    monkeypatch.setattr(managed, "_codex_slot_auth_path", lambda: auth)
    monkeypatch.setattr(managed, "codex_pinning_sessions", lambda auth_path=None: [])
    a, b = _rec("cx-a", harness="codex"), _rec("cx-b", harness="codex")
    managed._write_slot_blob("codex", _codex_blob("A0"))
    managed.snapshot_current(a, root=tmp_path)
    managed._atomic_write_private(managed._active_stamp_path("codex", tmp_path), "cx-a")
    managed._write_slot_blob("codex", _codex_blob("B0"))
    managed.snapshot_current(b, root=tmp_path)
    managed._atomic_write_private(managed._active_stamp_path("codex", tmp_path), "cx-b")
    return a, b


class TestCodexFileBackend:
    def test_codex_switch_captures_and_materializes(self, tmp_path, monkeypatch):
        a, b = _register_codex_pair(tmp_path, monkeypatch)
        # switch to A: captures B's current slot, materializes A0, verifies.
        monkeypatch.setattr(
            managed,
            "_codex_login_ok",
            lambda: managed._CodexLoginResult(ok=True),
        )
        result = managed.switch(a, by_id={"cx-a": a, "cx-b": b}, root=tmp_path)
        assert managed._read_slot_blob("codex") == _codex_blob("A0")
        assert (tmp_path / "cx-b" / "blob").read_text() == _codex_blob("B0")
        assert result.slot_changed is True
        assert result.verification == "codex-recognized"
        assert result.reason is None

    def test_codex_switch_discloses_structural_fallback(self, tmp_path, monkeypatch):
        a, b = _register_codex_pair(tmp_path, monkeypatch)
        monkeypatch.setattr(
            managed,
            "_codex_login_ok",
            lambda: managed._CodexLoginResult(
                ok=None,
                reason="codex-login-status-missing",
            ),
        )

        events = []
        result = managed.switch(
            a,
            by_id={"cx-a": a, "cx-b": b},
            root=tmp_path,
            emit_fn=lambda kind, **data: events.append((kind, data)),
        )

        assert result.slot_changed is True
        assert result.verification == "structural"
        assert result.reason == "codex-login-status-missing"
        assert events[0][1]["reason"] == "codex-login-status-missing"

    def test_codex_login_rejection_rolls_back_without_event(self, tmp_path, monkeypatch):
        a, b = _register_codex_pair(tmp_path, monkeypatch)
        monkeypatch.setattr(
            managed,
            "_codex_login_ok",
            lambda: managed._CodexLoginResult(ok=False),
        )
        events = []

        with pytest.raises(managed.ManagedStoreError, match="not recognized"):
            managed.switch(
                a,
                by_id={"cx-a": a, "cx-b": b},
                root=tmp_path,
                emit_fn=lambda kind, **data: events.append((kind, data)),
            )

        assert managed._read_slot_blob("codex") == _codex_blob("B0")
        assert managed.active_slot_id("codex", tmp_path) == "cx-b"
        assert events == []

    def test_tokenless_codex_snapshot_rolls_back_before_native_probe(self, tmp_path, monkeypatch):
        a, b = _register_codex_pair(tmp_path, monkeypatch)
        (tmp_path / "cx-a" / "blob").write_text(json.dumps({"foo": "bar"}))
        monkeypatch.setattr(
            managed,
            "_codex_login_ok",
            lambda: pytest.fail("tokenless auth must fail structural verification"),
        )

        with pytest.raises(managed.ManagedStoreError, match="failed verification"):
            managed.switch(a, by_id={"cx-a": a, "cx-b": b}, root=tmp_path)

        assert managed._read_slot_blob("codex") == _codex_blob("B0")
        assert managed.active_slot_id("codex", tmp_path) == "cx-b"

    def test_codex_login_rejection_without_rollback_blob_reports_slot_state(
        self, tmp_path, monkeypatch
    ):
        auth = tmp_path / ".codex" / "auth.json"
        a, b = _register_codex_pair(tmp_path, monkeypatch)
        auth.unlink()
        monkeypatch.setattr(
            managed,
            "_codex_login_ok",
            lambda: managed._CodexLoginResult(ok=False),
        )
        events = []

        with pytest.raises(managed.ManagedStoreError, match="nothing to roll back"):
            managed.switch(
                a,
                by_id={"cx-a": a, "cx-b": b},
                root=tmp_path,
                emit_fn=lambda kind, **data: events.append((kind, data)),
            )

        assert managed._read_slot_blob("codex") == _codex_blob("A0")
        assert managed.active_slot_id("codex", tmp_path) is None
        assert (tmp_path / "cx-b" / "blob").read_text() == _codex_blob("B0")
        assert events == []

        monkeypatch.setattr(
            managed,
            "_codex_login_ok",
            lambda: managed._CodexLoginResult(ok=True),
        )
        managed.switch(a, by_id={"cx-a": a, "cx-b": b}, root=tmp_path)
        assert managed.active_slot_id("codex", tmp_path) == "cx-a"
        assert (tmp_path / "cx-b" / "blob").read_text() == _codex_blob("B0")

    def test_codex_structural_failure_with_failed_rollback_clears_stamp(
        self, tmp_path, monkeypatch
    ):
        a, b = _register_codex_pair(tmp_path, monkeypatch)
        original_verify = managed.verify_slot
        original_write = managed._write_slot_blob
        calls = {"count": 0}

        monkeypatch.setattr(managed, "verify_slot", lambda record, expected_blob: False)

        def _write(cli, blob, config_dir=None):
            calls["count"] += 1
            if calls["count"] == 2:
                raise managed.ManagedStoreError("rollback denied")
            return original_write(cli, blob, config_dir)

        monkeypatch.setattr(managed, "_write_slot_blob", _write)
        with pytest.raises(managed.ManagedStoreError, match="indeterminate"):
            managed.switch(a, by_id={"cx-a": a, "cx-b": b}, root=tmp_path)

        assert managed.active_slot_id("codex", tmp_path) is None
        assert (tmp_path / "cx-b" / "blob").read_text() == _codex_blob("B0")

        monkeypatch.setattr(managed, "verify_slot", original_verify)
        monkeypatch.setattr(
            managed,
            "_codex_login_ok",
            lambda: managed._CodexLoginResult(ok=True),
        )
        managed.switch(a, by_id={"cx-a": a, "cx-b": b}, root=tmp_path)
        assert managed.active_slot_id("codex", tmp_path) == "cx-a"
        assert (tmp_path / "cx-b" / "blob").read_text() == _codex_blob("B0")

    def test_codex_hard_probe_error_rolls_back(self, tmp_path, monkeypatch):
        a, b = _register_codex_pair(tmp_path, monkeypatch)

        def _raise():
            raise managed.ManagedStoreError("permission denied")

        monkeypatch.setattr(managed, "_codex_login_ok", _raise)
        with pytest.raises(managed.ManagedStoreError, match="permission denied"):
            managed.switch(a, by_id={"cx-a": a, "cx-b": b}, root=tmp_path)
        assert managed._read_slot_blob("codex") == _codex_blob("B0")
        assert managed.active_slot_id("codex", tmp_path) == "cx-b"

    def test_codex_probe_interrupt_rolls_back_then_reraises(self, tmp_path, monkeypatch):
        a, b = _register_codex_pair(tmp_path, monkeypatch)

        def _interrupt():
            raise KeyboardInterrupt

        monkeypatch.setattr(managed, "_codex_login_ok", _interrupt)
        with pytest.raises(KeyboardInterrupt) as caught:
            managed.switch(a, by_id={"cx-a": a, "cx-b": b}, root=tmp_path)
        assert caught.value.__notes__ == [
            "codex login verification interrupted; slot rolled back to the previous account"
        ]
        assert managed._read_slot_blob("codex") == _codex_blob("B0")
        assert managed.active_slot_id("codex", tmp_path) == "cx-b"

    def test_codex_probe_interrupt_reraises_even_when_rollback_fails(
        self, tmp_path, monkeypatch
    ):
        # A KeyboardInterrupt must propagate as itself even if best-effort
        # rollback fails - never downgrade a BaseException to a caught Exception.
        a, b = _register_codex_pair(tmp_path, monkeypatch)

        def _interrupt():
            raise KeyboardInterrupt

        monkeypatch.setattr(managed, "_codex_login_ok", _interrupt)
        original_write = managed._write_slot_blob
        calls = {"count": 0}

        def _write(cli, blob, config_dir=None):
            calls["count"] += 1
            if calls["count"] == 2:
                raise managed.ManagedStoreError("rollback denied")
            return original_write(cli, blob, config_dir)

        monkeypatch.setattr(managed, "_write_slot_blob", _write)
        with pytest.raises(KeyboardInterrupt) as caught:
            managed.switch(a, by_id={"cx-a": a, "cx-b": b}, root=tmp_path)
        assert "rollback ALSO failed (rollback denied)" in caught.value.__notes__[0]
        assert "active stamp cleared" in caught.value.__notes__[0]
        assert managed.active_slot_id("codex", tmp_path) is None
        assert (tmp_path / "cx-b" / "blob").read_text() == _codex_blob("B0")

    def test_codex_rejection_reports_rollback_failure(self, tmp_path, monkeypatch):
        a, b = _register_codex_pair(tmp_path, monkeypatch)
        monkeypatch.setattr(
            managed,
            "_codex_login_ok",
            lambda: managed._CodexLoginResult(ok=False),
        )
        original_write = managed._write_slot_blob
        calls = {"count": 0}

        def _write(cli, blob, config_dir=None):
            calls["count"] += 1
            if calls["count"] == 2:
                raise managed.ManagedStoreError("rollback denied")
            return original_write(cli, blob, config_dir)

        monkeypatch.setattr(managed, "_write_slot_blob", _write)
        with pytest.raises(managed.ManagedStoreError, match="indeterminate"):
            managed.switch(a, by_id={"cx-a": a, "cx-b": b}, root=tmp_path)
        assert managed.active_slot_id("codex", tmp_path) is None
        assert (tmp_path / "cx-b" / "blob").read_text() == _codex_blob("B0")

        monkeypatch.setattr(
            managed,
            "_codex_login_ok",
            lambda: managed._CodexLoginResult(ok=True),
        )
        managed.switch(a, by_id={"cx-a": a, "cx-b": b}, root=tmp_path)
        assert managed.active_slot_id("codex", tmp_path) == "cx-a"
        assert (tmp_path / "cx-b" / "blob").read_text() == _codex_blob("B0")

    def test_codex_switch_event_records_verification_strength(self, tmp_path, monkeypatch):
        a, b = _register_codex_pair(tmp_path, monkeypatch)
        monkeypatch.setattr(
            managed,
            "_codex_login_ok",
            lambda: managed._CodexLoginResult(ok=True),
        )
        events = []

        managed.switch(
            a,
            by_id={"cx-a": a, "cx-b": b},
            root=tmp_path,
            emit_fn=lambda kind, **data: events.append((kind, data)),
        )

        assert events == [
            (
                "account_switched",
                {
                    "provider": "cx-a",
                    "account_id": "cx-a",
                    "outgoing": "cx-b",
                    "slot_changed": True,
                    "verification": "codex-recognized",
                },
            )
        ]

    def test_codex_already_active_is_probe_free(self, tmp_path, monkeypatch):
        auth = tmp_path / ".codex" / "auth.json"
        monkeypatch.setattr(managed, "_codex_slot_auth_path", lambda: auth)
        target = _rec("cx-a", harness="codex")
        managed._write_slot_blob("codex", _codex_blob("A0"))
        managed.snapshot_current(target, root=tmp_path)
        managed._atomic_write_private(managed._active_stamp_path("codex", tmp_path), "cx-a")
        monkeypatch.setattr(
            managed,
            "_codex_login_ok",
            lambda: pytest.fail("already-active switch must not probe codex"),
        )

        result = managed.switch(target, by_id={"cx-a": target}, root=tmp_path)

        assert result.slot_changed is False
        assert result.verification == "structural"
        assert result.reason == "slot-already-active"

    def test_codex_switch_pin_defers(self, tmp_path, monkeypatch):
        """US6 Invariant: a live codex session pinning the slot defers the switch,
        names the session, and mutates nothing - claude parity for codex."""
        auth = tmp_path / ".codex" / "auth.json"
        monkeypatch.setattr(managed, "_codex_slot_auth_path", lambda: auth)
        a, b = _rec("cx-a", harness="codex"), _rec("cx-b", harness="codex")
        managed._write_slot_blob("codex", _codex_blob("A0"))
        managed.snapshot_current(a, root=tmp_path)
        managed._atomic_write_private(managed._active_stamp_path("codex", tmp_path), "cx-a")
        managed._write_slot_blob("codex", _codex_blob("B0"))
        managed.snapshot_current(b, root=tmp_path)
        managed._atomic_write_private(managed._active_stamp_path("codex", tmp_path), "cx-b")
        monkeypatch.setattr(
            managed, "codex_pinning_sessions",
            lambda auth_path=None: [managed.PinningSession(555, "codex exec")],
        )
        before = managed._read_slot_blob("codex")
        with pytest.raises(managed.SwitchDeferred) as exc:
            managed.switch(a, by_id={"cx-a": a, "cx-b": b}, root=tmp_path)
        assert "555" in str(exc.value) and "codex" in str(exc.value)
        assert managed._read_slot_blob("codex") == before  # slot untouched
        assert (tmp_path / "cx-a" / "blob").read_text() == _codex_blob("A0")  # store untouched

    def test_codex_session_launched_mid_switch_rolls_back(self, tmp_path, monkeypatch):
        """TOCTOU narrowing (cv-f578cbe7): the pre-write pin check is clear, but a
        codex session appears during the write. The post-write re-scan catches it,
        rolls the slot back to the outgoing creds, and defers - never leaving
        auth.json rewritten under the session that started mid-switch."""
        auth = tmp_path / ".codex" / "auth.json"
        monkeypatch.setattr(managed, "_codex_slot_auth_path", lambda: auth)
        a, b = _rec("cx-a", harness="codex"), _rec("cx-b", harness="codex")
        managed._write_slot_blob("codex", _codex_blob("A0"))
        managed.snapshot_current(a, root=tmp_path)
        managed._atomic_write_private(managed._active_stamp_path("codex", tmp_path), "cx-a")
        managed._write_slot_blob("codex", _codex_blob("B0"))  # slot holds outgoing B
        managed.snapshot_current(b, root=tmp_path)
        managed._atomic_write_private(managed._active_stamp_path("codex", tmp_path), "cx-b")
        # First scan (pre-write) clear; second scan (immediately post-write) finds
        # a session before the native probe can widen the rollback race.
        calls = {"n": 0}

        def _scan(auth_path=None):
            calls["n"] += 1
            return [] if calls["n"] == 1 else [managed.PinningSession(777, "codex")]

        monkeypatch.setattr(managed, "codex_pinning_sessions", _scan)
        monkeypatch.setattr(
            managed,
            "_codex_login_ok",
            lambda: pytest.fail("late pin must defer before probing codex"),
        )
        with pytest.raises(managed.SwitchDeferred) as exc:
            managed.switch(a, by_id={"cx-a": a, "cx-b": b}, root=tmp_path)
        assert "777" in str(exc.value) and "during the switch" in str(exc.value)
        assert managed._read_slot_blob("codex") == _codex_blob("B0")  # rolled back to outgoing
        assert managed.active_slot_id("codex", tmp_path) == "cx-b"  # stamp not advanced
        assert calls["n"] == 2  # both scans ran

    def test_codex_late_pin_without_rollback_blob_clears_stamp(self, tmp_path, monkeypatch):
        auth = tmp_path / ".codex" / "auth.json"
        a, b = _register_codex_pair(tmp_path, monkeypatch)
        auth.unlink()
        calls = {"count": 0}

        def _scan(auth_path=None):
            calls["count"] += 1
            return [] if calls["count"] == 1 else [managed.PinningSession(891, "codex")]

        monkeypatch.setattr(managed, "codex_pinning_sessions", _scan)
        monkeypatch.setattr(
            managed,
            "_codex_login_ok",
            lambda: pytest.fail("late pin must defer before probing codex"),
        )

        with pytest.raises(managed.SwitchDeferred, match="active stamp cleared"):
            managed.switch(a, by_id={"cx-a": a, "cx-b": b}, root=tmp_path)

        assert managed._read_slot_blob("codex") == _codex_blob("A0")
        assert managed.active_slot_id("codex", tmp_path) is None
        assert (tmp_path / "cx-b" / "blob").read_text() == _codex_blob("B0")

    def test_codex_session_started_during_successful_probe_keeps_target(self, tmp_path, monkeypatch):
        a, b = _register_codex_pair(tmp_path, monkeypatch)
        pin_active = {"value": False}
        scans = {"count": 0}

        def _scan(auth_path=None):
            scans["count"] += 1
            if pin_active["value"]:
                return [managed.PinningSession(888, "codex")]
            return []

        def _probe():
            pin_active["value"] = True
            return managed._CodexLoginResult(ok=True)

        monkeypatch.setattr(managed, "codex_pinning_sessions", _scan)
        monkeypatch.setattr(managed, "_codex_login_ok", _probe)

        result = managed.switch(a, by_id={"cx-a": a, "cx-b": b}, root=tmp_path)

        assert result.verification == "codex-recognized"
        assert managed._read_slot_blob("codex") == _codex_blob("A0")
        assert managed.active_slot_id("codex", tmp_path) == "cx-a"
        assert scans["count"] == 2

    def test_codex_rejection_with_pin_during_probe_withholds_rollback(
        self, tmp_path, monkeypatch
    ):
        a, b = _register_codex_pair(tmp_path, monkeypatch)
        pin_active = {"value": False}

        def _scan(auth_path=None):
            if pin_active["value"]:
                return [managed.PinningSession(889, "codex")]
            return []

        def _probe():
            pin_active["value"] = True
            return managed._CodexLoginResult(ok=False)

        monkeypatch.setattr(managed, "codex_pinning_sessions", _scan)
        monkeypatch.setattr(managed, "_codex_login_ok", _probe)

        with pytest.raises(managed.ManagedStoreError, match="rollback withheld.*pid 889"):
            managed.switch(a, by_id={"cx-a": a, "cx-b": b}, root=tmp_path)

        assert managed._read_slot_blob("codex") == _codex_blob("A0")
        assert managed.active_slot_id("codex", tmp_path) is None
        assert (tmp_path / "cx-b" / "blob").read_text() == _codex_blob("B0")

        pin_active["value"] = False
        monkeypatch.setattr(
            managed,
            "_codex_login_ok",
            lambda: managed._CodexLoginResult(ok=True),
        )
        result = managed.switch(a, by_id={"cx-a": a, "cx-b": b}, root=tmp_path)

        assert result.verification == "codex-recognized"
        assert managed._read_slot_blob("codex") == _codex_blob("A0")
        assert managed.active_slot_id("codex", tmp_path) == "cx-a"
        assert (tmp_path / "cx-b" / "blob").read_text() == _codex_blob("B0")

    def test_codex_interrupt_with_pin_during_probe_withholds_rollback(
        self, tmp_path, monkeypatch
    ):
        a, b = _register_codex_pair(tmp_path, monkeypatch)
        pin_active = {"value": False}

        def _scan(auth_path=None):
            if pin_active["value"]:
                return [managed.PinningSession(890, "codex")]
            return []

        def _probe():
            pin_active["value"] = True
            raise KeyboardInterrupt

        monkeypatch.setattr(managed, "codex_pinning_sessions", _scan)
        monkeypatch.setattr(managed, "_codex_login_ok", _probe)

        with pytest.raises(KeyboardInterrupt) as caught:
            managed.switch(a, by_id={"cx-a": a, "cx-b": b}, root=tmp_path)

        assert "rollback withheld" in caught.value.__notes__[0]
        assert "pid 890" in caught.value.__notes__[0]
        assert managed._read_slot_blob("codex") == _codex_blob("A0")
        assert managed.active_slot_id("codex", tmp_path) is None

    def test_claude_switch_has_single_pin_check(self, fake_slot, tmp_path, monkeypatch):
        """The post-write re-scan is codex-only: claude keeps G1's single pre-write
        check (byte-for-byte), so a clean claude switch scans exactly once."""
        by_id = _register_two(fake_slot, tmp_path)
        calls = {"n": 0}

        def _scan(config_dir=None):
            calls["n"] += 1
            return []

        monkeypatch.setattr(managed, "pinning_sessions", _scan)
        managed.switch(by_id["work-a"], by_id=by_id, root=tmp_path)
        assert calls["n"] == 1  # claude scanned once, not twice


class _FakeProc:
    """A psutil-proc stand-in for the pin matcher's process scan."""

    def __init__(self, pid, name, cmdline, environ=None, environ_raises=False):
        self.info = {"pid": pid, "name": name, "cmdline": cmdline}
        self._environ = environ or {}
        self._environ_raises = environ_raises

    def environ(self):
        if self._environ_raises:
            raise PermissionError("denied")
        return self._environ


# ---------------------------------------------------------------------------
# CLI surface (use)
# ---------------------------------------------------------------------------


def _cli_slot(monkeypatch):
    slot: dict[str, str | None] = {}
    monkeypatch.setattr(managed, "_read_slot_blob", lambda cli, config_dir=None: slot.get(cli))
    monkeypatch.setattr(
        managed, "_write_slot_blob", lambda cli, blob, config_dir=None: slot.__setitem__(cli, blob)
    )
    monkeypatch.setattr(managed, "pinning_sessions", lambda config_dir=None: [])
    monkeypatch.setattr(
        managed, "canonical_slot_blobs",
        lambda cli: [slot[cli]] if slot.get(cli) else [],
    )
    return slot


class TestCliSurface:
    def _invoke_codex_use(self, monkeypatch, result):
        config = ProvidersConfig(records=[_rec("cx-a", harness="codex")], active="cx-a")
        monkeypatch.setattr(
            "fno.adapters.providers.cli.load_providers",
            lambda repo_root=None: config,
        )
        monkeypatch.setattr(
            "fno.adapters.providers.cli.save_providers",
            lambda *args, **kwargs: None,
        )
        monkeypatch.setattr(managed, "switch", lambda *args, **kwargs: result)
        return runner.invoke(providers_app, ["use", "cx-a"], catch_exceptions=False)

    def _invoke_managed_use(self, monkeypatch, tmp_path, pins=()):
        """``use work-a`` over a store seeded with work-a and work-b, slot on work-b."""
        slot = _cli_slot(monkeypatch)
        monkeypatch.setattr(managed, "store_root", lambda: tmp_path)
        by_id = _register_two(slot, tmp_path)
        config = ProvidersConfig(records=list(by_id.values()), active="work-b")
        for name in ("load_providers", "load_scope_config"):
            monkeypatch.setattr(
                f"fno.adapters.providers.cli.{name}", lambda *args, **kwargs: config
            )
        monkeypatch.setattr(
            "fno.adapters.providers.cli.save_providers", lambda *args, **kwargs: None
        )
        monkeypatch.setattr(managed, "pinning_sessions", lambda config_dir=None: list(pins))
        return slot, runner.invoke(providers_app, ["use", "work-a"], catch_exceptions=False)

    def test_use_managed_materializes(self, tmp_path, monkeypatch):
        slot, r = self._invoke_managed_use(monkeypatch, tmp_path)
        assert r.exit_code == 0, r.output
        assert slot["claude"] == _blob("A0")
        assert "Materialized managed account 'work-a' into the slot (verified)." in r.output

    def test_use_codex_reports_native_verification(self, monkeypatch):
        result = managed.SwitchResult(
            active="cx-a",
            slot_changed=True,
            verification="codex-recognized",
        )
        response = self._invoke_codex_use(monkeypatch, result)
        assert response.exit_code == 0
        assert "Codex recognized" in response.output

    def test_use_codex_reports_structural_fallback(self, monkeypatch):
        result = managed.SwitchResult(
            active="cx-a",
            slot_changed=True,
            verification="structural",
            reason="codex-login-status-timeout",
        )
        response = self._invoke_codex_use(monkeypatch, result)
        assert response.exit_code == 0
        assert "structural fallback" in response.output
        assert "codex-login-status-timeout" in response.output

    def test_use_codex_reports_already_active_noop(self, monkeypatch):
        result = managed.SwitchResult(
            active="cx-a",
            slot_changed=False,
            verification="structural",
            reason="slot-already-active",
        )
        response = self._invoke_codex_use(monkeypatch, result)
        assert response.exit_code == 0
        assert "already materialized" in response.output
        assert "slot-already-active" in response.output

    def test_use_codex_interrupt_surfaces_rollback_receipt(self, monkeypatch):
        config = ProvidersConfig(records=[_rec("cx-a", harness="codex")], active="cx-a")
        monkeypatch.setattr(
            "fno.adapters.providers.cli.load_providers",
            lambda repo_root=None: config,
        )

        def _interrupt(*args, **kwargs):
            exc = KeyboardInterrupt()
            exc.add_note(
                "codex login verification interrupted; rollback ALSO failed "
                "(rollback denied); slot is in an indeterminate state"
            )
            raise exc

        monkeypatch.setattr(managed, "switch", _interrupt)
        response = runner.invoke(providers_app, ["use", "cx-a"])

        assert response.exit_code == 130
        assert "switch interrupted: codex login verification interrupted" in response.output
        assert "slot is in an indeterminate state" in response.output

    def test_use_managed_live_pin_proceeds_with_warning(self, tmp_path, monkeypatch):
        _, r = self._invoke_managed_use(
            monkeypatch, tmp_path, pins=[managed.PinningSession(99, "claude")]
        )
        assert r.exit_code == 0, r.output
        assert "Materialized managed account 'work-a'" in r.output
        assert "warning: swapped under 1 live claude session(s)" in r.output


# ---------------------------------------------------------------------------
# x-4b8d: principal reconciliation (prove the live slot's identity)
# ---------------------------------------------------------------------------


def _profile(uuid: str, email: str = "a@example.com", org: str = "org-1") -> dict:
    """The non-secret shape /api/oauth/profile returns (verified live 2026-08-03)."""
    return {
        "account": {"uuid": uuid, "email": email, "full_name": "JN"},
        "organization": {"uuid": org, "name": f"{email}'s Organization"},
        "application": {"slug": "claude-code"},
    }


def _store_state(root) -> dict[str, bytes]:
    """Every byte of the store that a refusal must leave untouched.

    The switch mutex is excluded: taking a lock is not a store mutation, and
    whether the lockfile survives release is a filelock implementation detail.
    """
    return {
        str(p.relative_to(root)): p.read_bytes()
        for p in sorted(root.rglob("*"))
        if p.is_file() and p.name != ".switch.lock"
    }


def _bind(record_id: str, uuid: str, root, email: str = "a@example.com") -> None:
    fingerprint = managed.principal_fingerprint(_profile(uuid, email))
    assert fingerprint is not None
    managed.write_record_principal(record_id, fingerprint, root)


class TestPrincipalFingerprint:
    def test_account_uuid_is_the_discriminator(self):
        """The smallest stable non-secret field set: account.uuid decides a match."""
        fp = managed.principal_fingerprint(_profile("acct-1", "jn@example.com", "org-9"))
        assert fp["account_uuid"] == "acct-1"
        assert fp["organization_uuid"] == "org-9"
        assert fp["email"] == "jn@example.com"
        assert not any("token" in k.lower() for k in fp)

    @pytest.mark.parametrize(
        "payload",
        [None, {}, {"account": {}}, {"account": {"uuid": ""}}, {"account": "nope"}, "text"],
    )
    def test_no_stable_uuid_is_not_a_fingerprint(self, payload):
        assert managed.principal_fingerprint(payload) is None

    def test_slot_principal_classifies_failures(self, monkeypatch):
        """A refusal must say WHICH way identity went unproven (US3), and a dead
        credential is not the same as an unanswered question."""
        import urllib.error

        def _raise(exc):
            def _urlopen(*a, **k):
                raise exc
            return _urlopen

        def _http(code):
            return _raise(
                urllib.error.HTTPError(managed._PROFILE_URL, code, "no", {}, None)
            )

        # The endpoint ANSWERED: this credential is dead.
        monkeypatch.setattr(managed.urllib.request, "urlopen", _http(401))
        assert managed.slot_principal(_blob("T")) == (None, "credential-rejected")
        monkeypatch.setattr(managed.urllib.request, "urlopen", _http(403))
        assert managed.slot_principal(_blob("T")) == (None, "credential-rejected")

        # The question went unanswered - a different thing entirely.
        monkeypatch.setattr(managed.urllib.request, "urlopen", _http(429))
        assert managed.slot_principal(_blob("T")) == (None, "profile-unavailable")
        monkeypatch.setattr(managed.urllib.request, "urlopen", _http(503))
        assert managed.slot_principal(_blob("T")) == (None, "profile-unavailable")
        monkeypatch.setattr(managed.urllib.request, "urlopen", _raise(TimeoutError()))
        assert managed.slot_principal(_blob("T")) == (None, "profile-unavailable")

        # A blob with no usable bearer never reaches the network, and is dead.
        assert managed.slot_principal("{}") == (None, "credential-rejected")

    def test_an_unanswered_candidate_blocks_the_whole_slot(self, monkeypatch):
        """A live account whose profile call merely timed out must not be skipped
        while the other candidate is stamped - claude may be reading the one we
        failed to ask about."""
        def _principal(blob):
            if blob == _blob("SILENT"):
                return None, "profile-unavailable"
            return managed.principal_fingerprint(_profile("acct-b")), None

        monkeypatch.setattr(managed, "slot_principal", _principal)
        assert managed.principal_of_blobs([_blob("SILENT"), _blob("B")]) == (
            None, None, "profile-unavailable"
        )


class TestReconcileSlot:
    def test_clears_false_taint_with_matching_principal(
        self, fake_slot, tmp_path, monkeypatch
    ):
        """AC1-HP: the stamped record IS the live principal - refresh, stamp, clear."""
        by_id = _register_two(fake_slot, tmp_path)  # stamp = work-b
        _bind("work-b", "acct-b", tmp_path)
        fake_slot["claude"] = _blob("B_ROTATED")
        managed._set_slot_taint("claude", tmp_path, True)
        monkeypatch.setattr(
            managed, "slot_principal", lambda blob: (managed.principal_fingerprint(_profile("acct-b")), None)
        )

        result = managed.reconcile_slot("claude", by_id=by_id, root=tmp_path)

        assert result.outcome == "matched" and result.record_id == "work-b"
        assert not managed.slot_tainted("claude", tmp_path)
        assert managed.active_slot_id("claude", tmp_path) == "work-b"
        assert (tmp_path / "work-b" / "blob").read_text() == _blob("B_ROTATED")

    def test_adopts_out_of_band_login_without_poisoning_the_other_store(
        self, fake_slot, tmp_path, monkeypatch
    ):
        """AC2-HP: stamp says work-b but `claude /login` put work-a in the slot."""
        by_id = _register_two(fake_slot, tmp_path)  # stamp = work-b
        _bind("work-a", "acct-a", tmp_path)
        _bind("work-b", "acct-b", tmp_path)
        fake_slot["claude"] = _blob("A_FRESH")  # out-of-band /login as work-a
        managed._set_slot_taint("claude", tmp_path, True)
        monkeypatch.setattr(
            managed, "slot_principal", lambda blob: (managed.principal_fingerprint(_profile("acct-a")), None)
        )

        result = managed.reconcile_slot("claude", by_id=by_id, root=tmp_path)

        assert result.outcome == "matched" and result.record_id == "work-a"
        assert managed.active_slot_id("claude", tmp_path) == "work-a"
        assert (tmp_path / "work-a" / "blob").read_text() == _blob("A_FRESH")
        # work-b's credential is NOT overwritten with work-a's token.
        assert (tmp_path / "work-b" / "blob").read_text() == _blob("B0")

    @pytest.mark.parametrize(
        "principal_fn,outcome",
        [
            (lambda blob: (None, "profile-unavailable"), "profile-unavailable"),
            (lambda blob: (None, "malformed-profile"), "malformed-profile"),
            (
                lambda blob: (managed.principal_fingerprint(_profile("acct-stranger")), None),
                "zero-match",
            ),
        ],
    )
    def test_unproven_identity_leaves_the_store_byte_identical(
        self, fake_slot, tmp_path, monkeypatch, principal_fn, outcome
    ):
        """AC3-ERR: no match, no mutation - stamp, snapshots and taint unchanged."""
        by_id = _register_two(fake_slot, tmp_path)
        _bind("work-a", "acct-a", tmp_path)
        _bind("work-b", "acct-b", tmp_path)
        managed._set_slot_taint("claude", tmp_path, True)
        fake_slot["claude"] = _blob("MYSTERY")
        monkeypatch.setattr(managed, "slot_principal", principal_fn)
        before = _store_state(tmp_path)

        result = managed.reconcile_slot("claude", by_id=by_id, root=tmp_path)

        assert result.outcome == outcome and result.record_id is None
        assert _store_state(tmp_path) == before
        assert managed.slot_tainted("claude", tmp_path)

    def test_ambiguous_match_refuses(self, fake_slot, tmp_path, monkeypatch):
        """AC3-ERR: two records fingerprinted to one principal is not proof."""
        by_id = _register_two(fake_slot, tmp_path)
        _bind("work-a", "acct-dup", tmp_path)
        _bind("work-b", "acct-dup", tmp_path)
        managed._set_slot_taint("claude", tmp_path, True)
        monkeypatch.setattr(
            managed, "slot_principal",
            lambda blob: (managed.principal_fingerprint(_profile("acct-dup")), None),
        )
        before = _store_state(tmp_path)

        result = managed.reconcile_slot("claude", by_id=by_id, root=tmp_path)

        assert result.outcome == "ambiguous-match"
        assert "work-a" in result.detail and "work-b" in result.detail
        assert _store_state(tmp_path) == before

    def test_config_dir_records_never_enter_shared_slot_reconciliation(
        self, fake_slot, tmp_path, monkeypatch
    ):
        """Boundaries: a record with its own dir is attributable without the slot."""
        by_id = _register_two(fake_slot, tmp_path)
        own = ProviderRecord(
            id="own-dir", name="own-dir", harness="claude", auth="managed",
            config_dir=tmp_path / "alt-home",
        )
        by_id["own-dir"] = own
        _bind("own-dir", "acct-x", tmp_path)
        managed._set_slot_taint("claude", tmp_path, True)
        monkeypatch.setattr(
            managed, "slot_principal",
            lambda blob: (managed.principal_fingerprint(_profile("acct-x")), None),
        )

        result = managed.reconcile_slot("claude", by_id=by_id, root=tmp_path)

        assert result.outcome == "zero-match"
        assert managed.slot_tainted("claude", tmp_path)

    def test_empty_slot_refuses(self, fake_slot, tmp_path):
        by_id = _register_two(fake_slot, tmp_path)
        fake_slot["claude"] = None
        result = managed.reconcile_slot("claude", by_id=by_id, root=tmp_path)
        assert result.outcome == "no-slot-credential"

    def test_codex_is_unsupported(self, fake_slot, tmp_path):
        """Only claude has a profile endpoint; guessing for codex is not proof."""
        result = managed.reconcile_slot("codex", by_id={}, root=tmp_path)
        assert result.outcome == "unsupported-harness"

class TestReconcileConcurrency:
    def test_held_switch_lock_yields_a_typed_refusal(self, fake_slot, tmp_path):
        """AC5-CON: reconciliation waits on the SAME mutex a switch takes."""
        import filelock

        by_id = _register_two(fake_slot, tmp_path)
        held = filelock.FileLock(str(managed._switch_lock_path(tmp_path)), timeout=1)
        held.acquire()
        try:
            result = managed.reconcile_slot(
                "claude", by_id=by_id, root=tmp_path, lock_timeout=0.2
            )
        finally:
            held.release()
        assert result.outcome == "lock-timeout"

    def test_racing_a_switch_never_mixes_stamp_and_snapshot(
        self, fake_slot, tmp_path, monkeypatch
    ):
        """AC5-CON: one complete identity transition commits before the other runs."""
        import threading

        by_id = _register_two(fake_slot, tmp_path)
        _bind("work-a", "acct-a", tmp_path)
        _bind("work-b", "acct-b", tmp_path)
        # The live principal always describes whatever blob is in the slot NOW.
        token_to_uuid = {_blob("A0"): "acct-a", _blob("B0"): "acct-b"}
        monkeypatch.setattr(
            managed, "slot_principal",
            lambda blob: (
                managed.principal_fingerprint(_profile(token_to_uuid.get(blob, "acct-?"))),
                None,
            ),
        )
        results: list[object] = []
        barrier = threading.Barrier(2)

        def _switch():
            barrier.wait()
            results.append(managed.switch(by_id["work-a"], by_id=by_id, root=tmp_path))

        def _reconcile():
            barrier.wait()
            results.append(managed.reconcile_slot("claude", by_id=by_id, root=tmp_path))

        threads = [threading.Thread(target=_switch), threading.Thread(target=_reconcile)]
        for t in threads:
            t.start()
        for t in threads:
            t.join(timeout=20)

        stamped = managed.active_slot_id("claude", tmp_path)
        assert stamped in ("work-a", "work-b")
        # The stamp and the slot describe the SAME account: never A's snapshot
        # under B's stamp.
        assert managed.read_blob(stamped, tmp_path) == fake_slot["claude"]


class TestReconcileRespectsLiveTaintWriters:
    """Proving the principal proves it NOW. A session that was pinning when the
    taint was written holds the PREVIOUS account's token and can flush a refresh
    of it into the slot afterwards, so clearing on its watch would trust a stamp
    the next refresh invalidates."""

    @staticmethod
    def _arm(fake_slot, tmp_path, monkeypatch, pids):
        by_id = _register_two(fake_slot, tmp_path)
        _bind("work-b", "acct-b", tmp_path)
        fake_slot["claude"] = _blob("B_ROTATED")
        managed._set_slot_taint("claude", tmp_path, True, pids)
        monkeypatch.setattr(
            managed, "slot_principal",
            lambda blob: (managed.principal_fingerprint(_profile("acct-b")), None),
        )
        return by_id

    def test_a_live_taint_writer_blocks_the_repair(
        self, fake_slot, tmp_path, monkeypatch
    ):
        import os

        by_id = self._arm(fake_slot, tmp_path, monkeypatch, [os.getpid()])
        before = _store_state(tmp_path)

        result = managed.reconcile_slot("claude", by_id=by_id, root=tmp_path)

        assert result.outcome == "slot-pinned"
        assert str(os.getpid()) in result.detail
        assert _store_state(tmp_path) == before
        assert managed.slot_tainted("claude", tmp_path)

    def test_a_dead_taint_writer_no_longer_blocks(
        self, fake_slot, tmp_path, monkeypatch
    ):
        """The common repair: the rotated-out session already exited."""
        by_id = self._arm(fake_slot, tmp_path, monkeypatch, [999_999])
        monkeypatch.setattr(managed.psutil, "pid_exists", lambda pid: False)

        result = managed.reconcile_slot("claude", by_id=by_id, root=tmp_path)

        assert result.outcome == "matched" and result.record_id == "work-b"
        assert not managed.slot_tainted("claude", tmp_path)

    def test_an_unrelated_live_session_does_not_block(
        self, fake_slot, tmp_path, monkeypatch
    ):
        """A session started AFTER the switch read the new credential, so it is
        not a risk - and on the shared slot it is usually the account being
        proven, including the very session running the repair."""
        by_id = self._arm(fake_slot, tmp_path, monkeypatch, [999_999])
        monkeypatch.setattr(managed.psutil, "pid_exists", lambda pid: False)
        monkeypatch.setattr(
            managed, "pinning_sessions",
            lambda config_dir=None: [managed.PinningSession(4242, "claude")],
        )
        assert managed.reconcile_slot("claude", by_id=by_id, root=tmp_path).ok

    def test_a_legacy_marker_falls_back_to_a_live_scan(
        self, fake_slot, tmp_path, monkeypatch
    ):
        """A marker written before pids were recorded cannot say who was live,
        so the conservative scan is the only honest answer."""
        by_id = self._arm(fake_slot, tmp_path, monkeypatch, [])
        managed._atomic_write_private(managed._slot_taint_path("claude", tmp_path), "1")
        assert managed.tainting_writers("claude", tmp_path) is None
        monkeypatch.setattr(
            managed, "pinning_sessions",
            lambda config_dir=None: [managed.PinningSession(4242, "claude")],
        )

        result = managed.reconcile_slot("claude", by_id=by_id, root=tmp_path)

        assert result.outcome == "slot-pinned" and "4242" in result.detail

    def test_switch_records_the_pins_it_proceeded_under(
        self, fake_slot, tmp_path, monkeypatch
    ):
        by_id = _register_two(fake_slot, tmp_path)
        monkeypatch.setattr(
            managed, "pinning_sessions",
            lambda config_dir=None: [managed.PinningSession(77, "claude")],
        )
        managed.switch(by_id["work-a"], by_id=by_id, root=tmp_path)
        assert _pids(tmp_path) == (77,)


class TestCanonicalSlotRead:
    def test_an_ambient_config_dir_never_redirects_the_identity_read(
        self, tmp_path, monkeypatch
    ):
        """A worker pinned to another account exports CLAUDE_CONFIG_DIR. Reading
        that dir would prove the PINNED account's identity and stamp it onto the
        canonical slot, which is the misattribution this whole node exists to
        kill - arriving through the repair meant to prevent it."""
        monkeypatch.setenv("HOME", str(tmp_path))
        monkeypatch.setenv("CLAUDE_CONFIG_DIR", str(tmp_path / "claude-alt"))
        monkeypatch.setattr(managed.sys, "platform", "linux")
        asked: list[tuple] = []

        def _record(cfg, *, shared):
            asked.append((cfg, shared))
            return _blob("CANONICAL")

        monkeypatch.setattr(managed, "_read_claude_blob", _record)

        assert managed.canonical_slot_blobs("claude") == [_blob("CANONICAL")]
        assert asked == [(tmp_path / ".claude", True)]

    def test_both_keychain_items_are_candidates(self, tmp_path, monkeypatch):
        """darwin keeps a scoped and an unscoped item for the canonical dir, and
        they can hold different accounts."""
        monkeypatch.setenv("HOME", str(tmp_path))
        monkeypatch.setattr(managed.sys, "platform", "darwin")
        items = {
            managed._claude_scoped_service(tmp_path / ".claude"): _blob("SCOPED"),
            managed._CLAUDE_KEYCHAIN_SERVICE: _blob("UNSCOPED"),
        }
        monkeypatch.setattr(
            managed, "_read_claude_keychain_item", lambda service: items.get(service)
        )
        assert managed.canonical_slot_blobs("claude") == [
            _blob("SCOPED"), _blob("UNSCOPED")
        ]

    def test_identical_items_collapse_to_one_candidate(self, tmp_path, monkeypatch):
        monkeypatch.setenv("HOME", str(tmp_path))
        monkeypatch.setattr(managed.sys, "platform", "darwin")
        monkeypatch.setattr(
            managed, "_read_claude_keychain_item", lambda service: _blob("SAME")
        )
        assert managed.canonical_slot_blobs("claude") == [_blob("SAME")]

    def test_two_accounts_in_one_slot_refuses_rather_than_picking(
        self, fake_slot, tmp_path, monkeypatch
    ):
        """Not a tie to break: whichever was stamped, some reader would get the
        other one."""
        by_id = _register_two(fake_slot, tmp_path)
        _bind("work-a", "acct-a", tmp_path)
        _bind("work-b", "acct-b", tmp_path)
        monkeypatch.setattr(
            managed, "canonical_slot_blobs",
            lambda cli: [_blob("SCOPED"), _blob("UNSCOPED")],
        )
        by_blob = {_blob("SCOPED"): "acct-a", _blob("UNSCOPED"): "acct-b"}
        monkeypatch.setattr(
            managed, "slot_principal",
            lambda blob: (managed.principal_fingerprint(_profile(by_blob[blob])), None),
        )
        before = _store_state(tmp_path)

        result = managed.reconcile_slot("claude", by_id=by_id, root=tmp_path)

        assert result.outcome == "ambiguous-slot"
        assert "no `/logout`" in result.detail
        assert "sign out" not in result.detail
        assert _store_state(tmp_path) == before

    def test_agreeing_items_reconcile_normally(
        self, fake_slot, tmp_path, monkeypatch
    ):
        by_id = _register_two(fake_slot, tmp_path)
        _bind("work-b", "acct-b", tmp_path)
        managed._set_slot_taint("claude", tmp_path, True, [])
        monkeypatch.setattr(
            managed, "canonical_slot_blobs",
            lambda cli: [_blob("SCOPED"), _blob("UNSCOPED")],
        )
        monkeypatch.setattr(
            managed, "slot_principal",
            lambda blob: (managed.principal_fingerprint(_profile("acct-b")), None),
        )

        result = managed.reconcile_slot("claude", by_id=by_id, root=tmp_path)

        assert result.outcome == "matched" and result.record_id == "work-b"


class TestReconcileCommitsOnlyWhatItProved:
    """Peer-review findings on PR #712: identity was proven about bytes that
    could already be gone, and a refusal could still touch disk."""

    def test_a_writer_that_rewrites_and_exits_mid_profile_blocks_the_commit(
        self, fake_slot, tmp_path, monkeypatch
    ):
        """The window the pid check alone cannot close: the recorded writer
        replaces the slot DURING the profile call and then exits, so a liveness
        check afterwards finds nothing and we would stamp a credential we never
        looked at."""
        by_id = _register_two(fake_slot, tmp_path)
        _bind("work-b", "acct-b", tmp_path)
        fake_slot["claude"] = _blob("B_AT_READ")
        managed._set_slot_taint("claude", tmp_path, True, [999_999])
        monkeypatch.setattr(managed.psutil, "pid_exists", lambda pid: False)

        def _profile_then_rewrite(blob):
            fake_slot["claude"] = _blob("SOMEONE_ELSE")  # the writer's last act
            return managed.principal_fingerprint(_profile("acct-b")), None

        monkeypatch.setattr(managed, "slot_principal", _profile_then_rewrite)
        before = _store_state(tmp_path)

        result = managed.reconcile_slot("claude", by_id=by_id, root=tmp_path)

        assert result.outcome == "slot-changed"
        assert _store_state(tmp_path) == before
        assert managed.slot_tainted("claude", tmp_path)

    def test_a_live_writer_is_refused_before_the_endpoint_is_touched(
        self, fake_slot, tmp_path, monkeypatch
    ):
        """Proving identity first would spend a network round trip to answer a
        question the pin gate already settles."""
        import os

        by_id = _register_two(fake_slot, tmp_path)
        _bind("work-b", "acct-b", tmp_path)
        managed._set_slot_taint("claude", tmp_path, True, [os.getpid()])
        monkeypatch.setattr(
            managed, "slot_principal",
            lambda blob: pytest.fail("resolved a principal for a pinned slot"),
        )

        assert managed.reconcile_slot(
            "claude", by_id=by_id, root=tmp_path
        ).outcome == "slot-pinned"

    def test_a_refusal_never_creates_the_store(self, fake_slot, tmp_path):
        """`matched` is the only outcome allowed to touch disk, and that has to
        include the store directory itself."""
        missing = tmp_path / "no-store-here"

        result = managed.reconcile_slot("claude", by_id={}, root=missing)

        assert result.outcome == "no-managed-store"
        assert not missing.exists()


class TestTaintWriterIdentity:
    """A pid is not an identity. Pids get reused, and a recycled one wearing the
    number of a long-gone session would hold the repair open forever - a
    permanent refusal, which is worse than the transient one it imitates."""

    @staticmethod
    def _arm(fake_slot, tmp_path, monkeypatch, pids):
        by_id = _register_two(fake_slot, tmp_path)
        _bind("work-b", "acct-b", tmp_path)
        fake_slot["claude"] = _blob("B_ROTATED")
        managed._set_slot_taint("claude", tmp_path, True, pids)
        monkeypatch.setattr(
            managed, "slot_principal",
            lambda blob: (managed.principal_fingerprint(_profile("acct-b")), None),
        )
        return by_id

    def test_a_recycled_pid_does_not_hold_the_repair(
        self, fake_slot, tmp_path, monkeypatch
    ):
        """Same number, different process: it never touched this slot."""
        import os

        by_id = self._arm(fake_slot, tmp_path, monkeypatch, [os.getpid()])
        # Rewrite the marker so the recorded start time predates this process.
        managed._atomic_write_private(
            managed._slot_taint_path("claude", tmp_path),
            json.dumps({"writers": [{"pid": os.getpid(), "started": 1.0}]}),
        )

        result = managed.reconcile_slot("claude", by_id=by_id, root=tmp_path)

        assert result.outcome == "matched" and result.record_id == "work-b"

    def test_the_original_process_still_holds_it(
        self, fake_slot, tmp_path, monkeypatch
    ):
        import os

        by_id = self._arm(fake_slot, tmp_path, monkeypatch, [os.getpid()])
        result = managed.reconcile_slot("claude", by_id=by_id, root=tmp_path)
        assert result.outcome == "slot-pinned" and str(os.getpid()) in result.detail

    def test_a_pid_only_marker_still_blocks_conservatively(
        self, fake_slot, tmp_path, monkeypatch
    ):
        """A marker written before start times existed cannot distinguish, so it
        refuses; the next switch replaces it with a full record."""
        import os

        by_id = self._arm(fake_slot, tmp_path, monkeypatch, [])
        managed._atomic_write_private(
            managed._slot_taint_path("claude", tmp_path),
            json.dumps({"pids": [os.getpid()]}),
        )
        assert managed.reconcile_slot(
            "claude", by_id=by_id, root=tmp_path
        ).outcome == "slot-pinned"


class TestSwitchNeverBindsAnIdentity:
    def test_a_switch_does_not_bind_an_unbound_record(self, fake_slot, tmp_path):
        """A switch materializes the record's STORED snapshot, whose provenance
        is the store, not the operator. An earlier out-of-band login plus
        capture-before-overwrite can leave one account's credential filed under
        another's id, so binding from it would manufacture confident attribution
        to the wrong account."""
        by_id = _register_two(fake_slot, tmp_path)

        managed.switch(by_id["work-a"], by_id=by_id, root=tmp_path)

        assert managed.record_principal("work-a", tmp_path) is None

    def test_a_switch_invalidates_cached_principal_evidence(
        self, fake_slot, tmp_path
    ):
        """The slot now holds a different credential, so evidence about the
        previous occupant must not survive it."""
        by_id = _register_two(fake_slot, tmp_path)
        managed.note_slot_principal("claude", tmp_path, "acct-b", "B0", now=1000.0)

        managed.switch(by_id["work-a"], by_id=by_id, root=tmp_path)

        assert managed.cached_slot_principal(
            "claude", tmp_path, "B0", now=1000.0 + 1
        ) is None

    def test_the_pin_scan_carries_each_start_time_into_the_taint(
        self, fake_slot, tmp_path, monkeypatch
    ):
        """Sampling the start time after the scan would read the REPLACEMENT of
        a pid that exited in between - fingerprinting the process the start time
        exists to exclude."""
        by_id = _register_two(fake_slot, tmp_path)
        monkeypatch.setattr(
            managed, "pinning_sessions",
            lambda config_dir=None: [managed.PinningSession(77, "claude", 4242.0)],
        )
        monkeypatch.setattr(
            managed, "_process_started_at",
            lambda pid: pytest.fail("re-sampled a start time after the scan"),
        )

        managed.switch(by_id["work-a"], by_id=by_id, root=tmp_path)

        assert managed.tainting_writers("claude", tmp_path) == [(77, 4242.0)]


class TestOrganizationScopedIdentity:
    """Claude Code usage is organization-scoped, and one human can belong to two
    organizations. Comparing the account uuid alone would let an org-B bearer
    pass as the org-A record and file its usage there."""

    def test_identity_needs_both_halves(self):
        assert managed.identity_key(
            {"account_uuid": "a", "organization_uuid": "o"}
        ) == "a/o"
        assert managed.identity_key({"account_uuid": "a"}) is None
        assert managed.identity_key({"organization_uuid": "o"}) is None
        assert managed.identity_key(None) is None

    def test_same_account_different_org_is_a_mismatch(self, tmp_path, monkeypatch):
        managed.write_record_principal(
            "org-a", {"account_uuid": "human-1", "organization_uuid": "org-a"}, tmp_path
        )
        monkeypatch.setattr(
            managed, "principal_of_bearer",
            lambda bearer: (
                {"account_uuid": "human-1", "organization_uuid": "org-b"}, None
            ),
        )
        assert managed.bearer_principal_verdict(
            "claude", "org-a", tmp_path, "tok"
        ) == "mismatch"

    def test_an_incomplete_binding_cannot_vouch_for_anything(self, tmp_path):
        managed.write_record_principal("half", {"account_uuid": "human-1"}, tmp_path)
        assert managed.bearer_principal_verdict(
            "claude", "half", tmp_path, "tok"
        ) == "unprovable"

    def test_two_orgs_in_one_slot_is_ambiguous_not_a_match(
        self, fake_slot, tmp_path, monkeypatch
    ):
        by_id = _register_two(fake_slot, tmp_path)
        monkeypatch.setattr(
            managed, "canonical_slot_blobs", lambda cli: [_blob("A"), _blob("B")]
        )
        orgs = {_blob("A"): "org-a", _blob("B"): "org-b"}
        monkeypatch.setattr(
            managed, "slot_principal",
            lambda blob: (
                {"account_uuid": "human-1", "organization_uuid": orgs[blob]}, None
            ),
        )
        assert managed.principal_of_blobs(
            managed.canonical_slot_blobs("claude")
        ) == (None, None, "ambiguous-slot")
        assert managed.reconcile_slot(
            "claude", by_id=by_id, root=tmp_path
        ).outcome == "ambiguous-slot"


class TestReconcileProvesItsOwnCapture:
    def test_only_the_captured_bytes_are_ever_proven(
        self, fake_slot, tmp_path, monkeypatch
    ):
        """Re-reading the slot to prove it would let an A -> B -> A flip prove B,
        survive the later comparison against the captured A, and cache A's
        bearer under B's identity. The proof must see only the capture."""
        by_id = _register_two(fake_slot, tmp_path)
        _bind("work-b", "acct-b", tmp_path)
        managed._set_slot_taint("claude", tmp_path, True, [])
        reads = {"n": 0}

        def _blobs(cli):
            reads["n"] += 1
            # Anything after the capture sees a different credential.
            return [_blob("B0")] if reads["n"] == 1 else [_blob("IMPOSTOR")]

        proven: list[str] = []

        def _principal(blob):
            proven.append(blob)
            return (
                managed.principal_fingerprint(
                    _profile("acct-b" if blob == _blob("B0") else "acct-impostor")
                ),
                None,
            )

        monkeypatch.setattr(managed, "canonical_slot_blobs", _blobs)
        monkeypatch.setattr(managed, "slot_principal", _principal)

        result = managed.reconcile_slot("claude", by_id=by_id, root=tmp_path)

        # The impostor was never proven, and the genuine change was caught.
        assert proven == [_blob("B0")]
        assert result.outcome == "slot-changed"
        assert managed.slot_tainted("claude", tmp_path)

    def test_a_stable_slot_proves_and_commits(
        self, fake_slot, tmp_path, monkeypatch
    ):
        by_id = _register_two(fake_slot, tmp_path)
        _bind("work-b", "acct-b", tmp_path)
        managed._set_slot_taint("claude", tmp_path, True, [])
        proven: list[str] = []
        monkeypatch.setattr(
            managed, "canonical_slot_blobs", lambda cli: [_blob("B0")]
        )

        def _principal(blob):
            proven.append(blob)
            return managed.principal_fingerprint(_profile("acct-b")), None

        monkeypatch.setattr(managed, "slot_principal", _principal)

        result = managed.reconcile_slot("claude", by_id=by_id, root=tmp_path)

        assert result.outcome == "matched" and result.record_id == "work-b"
        assert proven == [_blob("B0")]  # proved once, over the capture


class TestUnreadableSlotIsARefusal:
    def test_a_keychain_timeout_is_typed_not_raised(self, tmp_path, monkeypatch):
        """`security` can time out or be denied; an operator command must report
        that, not surface a traceback."""
        def _boom(cli):
            raise managed.KeychainError("`security find-generic-password` timed out")

        monkeypatch.setattr(managed, "canonical_slot_blobs", _boom)

        result = managed.reconcile_slot("claude", by_id={}, root=tmp_path)

        assert result.outcome == "slot-unreadable" and "timed out" in result.detail

    def test_an_unreadable_slot_is_typed_for_the_binding_too(
        self, tmp_path, monkeypatch
    ):
        """The drift read moved to `binding`; a denied Keychain still refuses
        rather than raising, and reads as unknown rather than as healthy."""
        from fno.adapters.providers import binding

        managed.stamp_active_slot("claude", "work-a", tmp_path)
        managed.write_record_principal(
            "work-a", {"account_uuid": "a", "organization_uuid": "o"}, tmp_path
        )

        def _boom(cli):
            raise managed.KeychainError("denied")

        monkeypatch.setattr(managed, "canonical_slot_blobs", _boom)
        got = binding.resolve_account_binding(None, harness="claude", root=tmp_path)

        assert got.status == binding.UNKNOWN
        assert got.reason == "credential-unreadable"


class TestProvisionalTaintSpansTheCommit:
    """A stamp exists but is unverified for the length of the commit. The taint
    has to cover that whole window, or a crash - or a Keychain read that fails
    at the end - leaves an unverified stamp fully trusted."""

    def test_an_untainted_drift_repair_is_tainted_while_it_commits(
        self, fake_slot, tmp_path, monkeypatch
    ):
        """The drift path starts with no marker at all, so without this there
        would be no window cover during its writes."""
        by_id = _register_two(fake_slot, tmp_path)
        _bind("work-a", "acct-a", tmp_path)
        assert not managed.slot_tainted("claude", tmp_path)
        monkeypatch.setattr(
            managed, "canonical_slot_blobs", lambda cli: [_blob("A_FRESH")]
        )
        monkeypatch.setattr(
            managed, "slot_principal",
            lambda blob: (managed.principal_fingerprint(_profile("acct-a")), None),
        )
        tainted_when_stamped: list[bool] = []
        real_stamp = managed.stamp_active_slot

        def _stamp(cli, record_id, root=None):
            tainted_when_stamped.append(managed.slot_tainted(cli, root))
            real_stamp(cli, record_id, root)

        monkeypatch.setattr(managed, "stamp_active_slot", _stamp)

        result = managed.reconcile_slot("claude", by_id=by_id, root=tmp_path)

        assert result.outcome == "matched" and result.record_id == "work-a"
        assert tainted_when_stamped == [True]
        assert not managed.slot_tainted("claude", tmp_path)

    def test_a_failed_final_read_during_reconcile_keeps_the_taint(
        self, fake_slot, tmp_path, monkeypatch
    ):
        by_id = _register_two(fake_slot, tmp_path)
        _bind("work-a", "acct-a", tmp_path)
        reads = {"n": 0}

        def _blobs(cli):
            reads["n"] += 1
            if reads["n"] >= 3:
                raise managed.KeychainError("denied")
            return [_blob("A_FRESH")]

        monkeypatch.setattr(managed, "canonical_slot_blobs", _blobs)
        monkeypatch.setattr(
            managed, "slot_principal",
            lambda blob: (managed.principal_fingerprint(_profile("acct-a")), None),
        )

        result = managed.reconcile_slot("claude", by_id=by_id, root=tmp_path)

        assert result.outcome == "slot-changed"
        assert managed.slot_tainted("claude", tmp_path)


class TestCredentialFileIsACandidate:
    """The usage probe reads `~/.claude/.credentials.json` FIRST, even on darwin
    where claude reads the Keychain. A stale file bearer could otherwise prove
    out and have its quota reported while the Keychain account occupies the
    slot."""

    def test_the_file_joins_the_candidate_set_on_darwin(self, tmp_path, monkeypatch):
        monkeypatch.setenv("HOME", str(tmp_path))
        monkeypatch.setattr(managed.sys, "platform", "darwin")
        slot = tmp_path / ".claude"
        slot.mkdir()
        (slot / ".credentials.json").write_text(_blob("FILE"))
        monkeypatch.setattr(
            managed, "_read_claude_keychain_item",
            lambda service: _blob("KEYCHAIN")
            if service == managed._CLAUDE_KEYCHAIN_SERVICE
            else None,
        )

        assert managed.canonical_slot_blobs("claude") == [
            _blob("KEYCHAIN"), _blob("FILE")
        ]

    def test_a_file_matching_the_keychain_adds_no_ambiguity(
        self, tmp_path, monkeypatch
    ):
        monkeypatch.setenv("HOME", str(tmp_path))
        monkeypatch.setattr(managed.sys, "platform", "darwin")
        slot = tmp_path / ".claude"
        slot.mkdir()
        (slot / ".credentials.json").write_text(_blob("SAME"))
        monkeypatch.setattr(
            managed, "_read_claude_keychain_item", lambda service: _blob("SAME")
        )

        assert managed.canonical_slot_blobs("claude") == [_blob("SAME")]

    def test_a_logged_out_file_residue_is_not_a_candidate(
        self, tmp_path, monkeypatch
    ):
        monkeypatch.setenv("HOME", str(tmp_path))
        monkeypatch.setattr(managed.sys, "platform", "darwin")
        slot = tmp_path / ".claude"
        slot.mkdir()
        (slot / ".credentials.json").write_text(
            json.dumps({"claudeAiOauth": {"accessToken": "", "refreshToken": ""}})
        )
        monkeypatch.setattr(
            managed, "_read_claude_keychain_item",
            lambda service: _blob("LIVE")
            if service == managed._CLAUDE_KEYCHAIN_SERVICE
            else None,
        )

        assert managed.canonical_slot_blobs("claude") == [_blob("LIVE")]


class TestUnreadableKeychainItemStopsAttribution:
    """Collapsing "denied" into "absent" silently shrinks the candidate set: the
    scoped item could hold account B while the read fails, leaving the slot
    looking unambiguous and A stamped - with claude still able to read B."""

    @staticmethod
    def _security(monkeypatch, returncode, stdout="", stderr=""):
        class _Out:
            def __init__(self):
                self.returncode = returncode
                self.stdout = stdout
                self.stderr = stderr

        monkeypatch.setattr(managed, "_run_security", lambda args: _Out())

    def test_item_not_found_is_absence(self, monkeypatch):
        self._security(monkeypatch, 44, stderr="could not be found in the keychain")
        assert managed._read_claude_keychain_item("svc") is None

    def test_any_other_failure_raises(self, monkeypatch):
        self._security(monkeypatch, 51, stderr="User interaction is not allowed.")
        with pytest.raises(managed.KeychainError) as exc:
            managed._read_claude_keychain_item("svc")
        assert "51" in str(exc.value)

    def test_a_denied_read_refuses_reconciliation(self, tmp_path, monkeypatch):
        """It must surface as a typed refusal, not a narrower candidate set."""
        monkeypatch.setenv("HOME", str(tmp_path))
        monkeypatch.setattr(managed.sys, "platform", "darwin")

        def _item(service):
            if service == managed._CLAUDE_KEYCHAIN_SERVICE:
                return _blob("READABLE")
            raise managed.KeychainError("User interaction is not allowed.")

        monkeypatch.setattr(managed, "_read_claude_keychain_item", _item)

        result = managed.reconcile_slot("claude", by_id={}, root=tmp_path)

        assert result.outcome == "slot-unreadable"
