"""US3: recover a live-injected message's sender from the transcript envelope."""
from __future__ import annotations

from fno.mail.reply_resolve import sender_from_transcript_text


def test_extracts_from_of_matching_id_json_escaped():
    # The envelope lives inside a JSONL record, so its quotes arrive escaped.
    line = (
        '{"type":"user","message":{"role":"user","content":'
        '"<fno_mail from=\\"9a063cd3\\" harness=\\"claude-code\\" model=\\"opus\\" '
        'id=\\"msg-live1\\">\\nping\\n</fno_mail>"}}'
    )
    assert sender_from_transcript_text(line, "msg-live1") == "9a063cd3"


def test_compact_tag_resolves_beside_the_legacy_tag():
    # AC1-LEGACY (x-d7cf): the compact tag drops harness/model but keeps id= and
    # from_session=, so the order-agnostic resolver answers both shapes in one
    # transcript. The legacy line is an input fixture, not a renderer output.
    legacy = (
        '<fno_mail from="deadbeef" harness="codex" model="gpt" '
        'id="msg-old" from_session="11112222-3333-4444-5555-666677778888"> hi'
    )
    compact = (
        '<fno_mail from="deadbeef" id="msg-new" '
        'from_session="11112222-3333-4444-5555-666677778888"> hi'
    )
    assert sender_from_transcript_text(legacy, "msg-old") == (
        "11112222-3333-4444-5555-666677778888"
    )
    assert sender_from_transcript_text(compact, "msg-new") == (
        "11112222-3333-4444-5555-666677778888"
    )


def test_picks_the_envelope_carrying_the_id_not_a_neighbor():
    # Two envelopes in the stream; only the one with the target id is answered.
    text = (
        '<fno_mail from="aaaa1111" id="msg-a"> first\n'
        '<fno_mail from="bbbb2222" id="msg-b"> second'
    )
    assert sender_from_transcript_text(text, "msg-b") == "bbbb2222"


SID = "bbbb2222-1111-4222-8333-444444444444"  # canonical handle: bbbb2222


def test_to_requirement_rejects_a_forwarded_envelope():
    # A forward quotes the original envelope verbatim, so the id IS present in a
    # session the message was never sent to. Matching on the id alone reads that
    # quote as a receipt and answers in the wrong session.
    text = '<fno_mail from="aaaa1111" to="cccc3333" id="msg-fwd"> quoted'
    assert sender_from_transcript_text(text, "msg-fwd", session_id=SID) is None


def test_resolve_live_sender_picks_the_store_holding_the_receipt(tmp_path, monkeypatch):
    # The defect this fixes: a claude session launched under codex carries BOTH
    # families' markers. resolve_harness_identity picks codex by precedence, so
    # the resolver read a stranger's rollout as this session's transcript and
    # every received message refused with "not in the bus log or this session's
    # transcript" -- while the envelope sat in the claude transcript all along.
    #
    # The codex candidate is searched FIRST here, matching the real marker order
    # that produced the bug, and its store carries an envelope with the SAME id.
    # Only `to=` separates them: without it this returns the foreign sender.
    import fno.harness_identity as hi
    import fno.mail.reply_resolve as rr

    codex_id = "01a02125-4eb4-7bf1-b74e-d238887eb092"
    claude_id = "b30d467b-ef4c-4780-8500-7084c9087f07"

    foreign = tmp_path / "codex.jsonl"
    foreign.write_text('<fno_mail from="ffff9999" to="cafebabe" id="msg-live"> x')
    own = tmp_path / "claude.jsonl"
    own.write_text('<fno_mail from="0ae85ed8" to="b30d467b" id="msg-live"> x')

    monkeypatch.setattr(
        hi,
        "present_harness_markers",
        lambda *a, **k: (
            ("CODEX_THREAD_ID", "codex", codex_id),
            ("CLAUDE_CODE_SESSION_ID", "claude", claude_id),
            ("CODEX_SESSION_ID", "codex", codex_id),
        ),
    )
    monkeypatch.setattr(
        rr,
        "_transcript_path",
        lambda harness, session_id: foreign if harness == "codex" else own,
    )

    assert rr.resolve_live_sender("msg-live") == "0ae85ed8"


def test_resolve_live_sender_skips_an_unreadable_store(tmp_path, monkeypatch):
    # An unreadable candidate must not end the search: the store that matters
    # can be the one after it. Aborting on the first OSError reproduces the
    # original symptom for a different reason.
    import fno.harness_identity as hi
    import fno.mail.reply_resolve as rr

    own = tmp_path / "claude.jsonl"
    own.write_text('<fno_mail from="0ae85ed8" to="b30d467b" id="msg-live"> x')

    monkeypatch.setattr(
        hi,
        "present_harness_markers",
        lambda *a, **k: (
            ("CODEX_SESSION_ID", "codex", "01a02125-4eb4-7bf1-b74e-d238887eb092"),
            ("CLAUDE_CODE_SESSION_ID", "claude", "b30d467b-ef4c-4780-8500-7084c9087f07"),
        ),
    )
    monkeypatch.setattr(
        rr,
        "_transcript_path",
        lambda harness, session_id: (tmp_path / "gone.jsonl") if harness == "codex" else own,
    )

    assert rr.resolve_live_sender("msg-live") == "0ae85ed8"
