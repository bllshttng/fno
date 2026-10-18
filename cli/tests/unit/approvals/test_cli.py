"""AC-A10-UI: the operator surface is explicit and inert."""

from __future__ import annotations

import datetime as _dt
from pathlib import Path

import pytest
from typer.testing import CliRunner

from fno.approvals import (
    AdapterCapability,
    ApprovalRequest,
    DecisionKind,
    EffectState,
    EffectStore,
    action_digest,
)
from fno.approvals.cli import approvals_app
from fno.approvals.policy import ConfigAuthority

# The `db` fixture files its request through the Rust effect gate
# (`authorized-merge` op effect-submit), so the module needs this checkout's
# fno-agents dev build; smoke legs skip via the dev_build marker.
pytestmark = pytest.mark.dev_build

runner = CliRunner()

FOUNDER = "principal:founder"
INTERN = "principal:intern"
NOW = _dt.datetime(2026, 8, 2, 12, 0, tzinfo=_dt.timezone.utc)


def _request(**overrides: object) -> ApprovalRequest:
    fields: dict[str, object] = {
        "request_id": "req-1",
        "principal_id": FOUNDER,
        "work_order_id": "x-1111",
        "attempt_id": "attempt-1",
        "effect_id": "effect-1",
        "effect_class": "communication.external",
        "destination": "email:customer@example.com",
        "action_digest": action_digest({"body": "original"}),
        "created_at": NOW,
        "expires_at": NOW + _dt.timedelta(days=3650),
    }
    fields.update(overrides)
    return ApprovalRequest(**fields)  # type: ignore[arg-type]


@pytest.fixture
def policy(monkeypatch: pytest.MonkeyPatch) -> ConfigAuthority:
    authority = ConfigAuthority({"*": [FOUNDER]})
    monkeypatch.setattr("fno.approvals.cli.load_authority", lambda: authority)
    return authority


@pytest.fixture
def db(tmp_path: Path, policy: ConfigAuthority) -> Path:
    path = tmp_path / "approvals.db"
    with EffectStore(path, authority=policy, events_path=tmp_path / "events.jsonl") as store:
        store.submit(_request())
    return path


def _invoke(*args: str):
    return runner.invoke(approvals_app, list(args))


def test_ac10_ui_show_names_every_bound_field(db: Path) -> None:
    digest = _request().request_digest
    result = _invoke("show", digest, "--db", str(db))

    assert result.exit_code == 0, result.output
    for expected in (
        FOUNDER,
        "x-1111",
        "attempt-1",
        "effect-1",
        "communication.external",
        "email:customer@example.com",
        _request().action_digest,
        "pending",
    ):
        assert expected in result.output


def test_ac10_ui_decision_records_without_claiming_acknowledgment(db: Path) -> None:
    digest = _request().request_digest
    result = _invoke("decide", digest, "--as", FOUNDER, "--approve", "--db", str(db))

    assert result.exit_code == 0, result.output
    assert "approved" in result.output
    assert "not executed" in result.output
    assert "not acknowledged" in result.output

    with EffectStore(db, authority=ConfigAuthority()) as store:
        decision = store.get_decision(digest)
        assert decision is not None and decision.decision is DecisionKind.APPROVED
        assert store.attempts_for_request(digest) == []


def test_ac10_ui_unauthorized_principal_gets_a_stable_refusal(db: Path) -> None:
    digest = _request().request_digest
    result = _invoke("decide", digest, "--as", INTERN, "--approve", "--db", str(db))

    assert result.exit_code == 2
    assert "REFUSED [unauthorized_principal]" in result.output
    assert "config.approvals.authorized_principals" in result.output

    with EffectStore(db, authority=ConfigAuthority()) as store:
        assert store.get_decision(digest) is None


def test_ac10_ui_replayed_decision_is_refused(db: Path) -> None:
    digest = _request().request_digest
    assert _invoke("decide", digest, "--as", FOUNDER, "--decline", "--db", str(db)).exit_code == 0

    result = _invoke("decide", digest, "--as", FOUNDER, "--approve", "--db", str(db))
    assert result.exit_code == 2
    assert "REFUSED [replay]" in result.output


def test_ac10_ui_unconfigured_policy_refuses_every_decision(
    db: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr("fno.approvals.cli.load_authority", lambda: ConfigAuthority())
    result = _invoke("decide", _request().request_digest, "--as", FOUNDER, "--approve", "--db", str(db))

    assert result.exit_code == 2
    assert "no approval policy is configured" in result.output


def test_ac10_ui_show_surfaces_an_ambiguous_outcome_and_its_recovery(
    db: Path, policy: ConfigAuthority
) -> None:
    digest = _request().request_digest
    with EffectStore(db, authority=policy) as store:
        store.decide(
            request_digest=digest,
            deciding_principal_id=FOUNDER,
            decision=DecisionKind.APPROVED,
        )
        tok = store.prepare(
            request_digest=digest,
            idempotency_key="key-1",
            adapter=AdapterCapability(adapter_id="smtp", adapter_version="1"),
        ).dispatch_token
        store.settle(
            dispatch_token=tok, idempotency_key="key-1", state=EffectState.UNKNOWN
        )

    result = _invoke("show", digest, "--db", str(db))
    assert result.exit_code == 0, result.output
    assert "key-1: unknown" in result.output
    assert "ambiguous" in result.output
    assert "reconciliation" in result.output.lower()


def test_config_authority_scopes_a_principal_to_its_effect_class() -> None:
    authority = ConfigAuthority({"communication.external": [INTERN]})
    assert authority.may_approve(
        principal_id=INTERN, effect_class="communication.external", destination="d"
    )
    assert not authority.may_approve(
        principal_id=INTERN, effect_class="publication.public", destination="d"
    )
