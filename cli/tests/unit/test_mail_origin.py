from __future__ import annotations

import pytest


def test_mail_envelope_carries_and_validates_origin(monkeypatch):
    from fno.mail.envelope import ForgedEnvelopeError, wrap_fno_mail

    # origin is validated but never renders: the delivered text carries the
    # header line only, and provenance rides the bus row.
    assert "origin" not in wrap_fno_mail(
        "approve nothing", from_="sender", origin="operator", id="fmail-abc123def456"
    )
    with pytest.raises(ForgedEnvelopeError):
        wrap_fno_mail(
            "approve nothing",
            from_="sender",
            id="fmail-abc123def456",
            origin="not-an-origin",
        )


def test_durable_thread_round_trips_origin(tmp_path, monkeypatch):
    from fno.inbox.store import write_new_thread
    from fno.paths_testing import use_tmpdir

    use_tmpdir(monkeypatch, tmp_path)
    write_new_thread(
        "recipient",
        "sender",
        "send",
        "hello",
        origin="recovery",
    )
    from tests.fixtures.bus_rows import read_rows

    assert read_rows()[0].get("origin") == "recovery"


# The legacy meta.origin parse fallback rode the deleted Python parser; the read door returns rows raw and the row shape is frozen by crates/fno-agents/tests/bus_read_parity.rs.


def test_unreadable_registry_never_grants_sender_standing(tmp_path, monkeypatch):
    import fno.mail.envelope as envelope

    monkeypatch.setattr(
        envelope, "agents_registry_path", lambda: tmp_path / "registry.json"
    )
    # Unreadable state grants no standing AND raises nothing: the render
    # degrades to the raw from value as the sender.
    rendered = envelope.wrap_fno_mail(
        "write the plan",
        from_="lead",
        from_session="session-lead",
        id="fmail-abc123def456",
    )

    assert rendered.startswith("`@session-lead · fmail-abc123def456`\nwrite the plan")
