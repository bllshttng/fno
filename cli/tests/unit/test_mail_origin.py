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
    from fno.inbox.store import read_thread, write_new_thread
    from fno.paths_testing import use_tmpdir

    use_tmpdir(monkeypatch, tmp_path)
    handle = write_new_thread(
        "recipient",
        "sender",
        "send",
        "hello",
        origin="recovery",
    )
    parsed = read_thread(handle.path)
    assert parsed is not None
    assert parsed.origin == "recovery"
    assert "origin: recovery" in handle.path.read_text()
    from fno.bus.log import iter_messages

    assert list(iter_messages())[0].origin == "recovery"


def test_bus_envelope_carries_origin_with_legacy_meta_fallback():
    from fno.bus.log import Envelope, from_json_line, to_json_line

    env = Envelope.new(from_="a", to="b", kind="send", body="x", origin="operator")
    assert env.origin == "operator"
    line = to_json_line(env)
    assert '"origin":"operator"' in line
    assert from_json_line(line).origin == "operator"
    # A row written before the field existed carried origin only inside meta;
    # the parser falls back so old lines keep their provenance.
    legacy = to_json_line(
        Envelope.new(from_="a", to="b", kind="send", body="x", meta={"origin": "recovery"})
    )
    import json as _json

    assert "origin" not in _json.loads(legacy)
    parsed = from_json_line(legacy)
    assert parsed.origin == "recovery"


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
