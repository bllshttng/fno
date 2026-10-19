"""`fno agents mail send` over the one bus writer + legacy md migration.

Post-cutover (ab-cee91152): `fno agents mail send` writes through the SAME bus writer
(one log line, no md-store divergence) and the existing drain finds it; pre-bus
markdown threads still migrate into the log (no unread mail stranded invisibly),
and the migration is idempotent.
"""
from __future__ import annotations


import pytest
from typer.testing import CliRunner

from fno.mail.cli import mail_app


@pytest.fixture
def inbox_and_bus(tmp_path, monkeypatch):
    """Co-isolate the md store (FNO_INBOX_ROOT) and the bus log under tmp."""
    monkeypatch.setenv("FNO_INBOX_ROOT", str(tmp_path))
    # `fno agents mail send` resolves the sender via settings.yaml; pin it.
    monkeypatch.chdir(tmp_path)
    (tmp_path / ".fno").mkdir(exist_ok=True)
    (tmp_path / ".fno" / "settings.yaml").write_text(
        "project: sender-proj\n", encoding="utf-8"
    )
    return tmp_path


@pytest.fixture
def runner() -> CliRunner:
    return CliRunner()


# ---------------------------------------------------------------------------
# G4: the sender path (`fno agents mail send`, the alias's replacement) routes
# through the one bus writer (no divergence) and the drain still finds it.
# ---------------------------------------------------------------------------

def test_g4_agents_send_writes_one_bus_line(inbox_and_bus, runner):
    from fno.bus.log import iter_messages
    from fno.inbox.store import read_all_threads

    res = runner.invoke(
        mail_app,
        ["send", "--to-project", "acme", "--kind", "fyi", "--body", "build is green"],
    )
    assert res.exit_code == 0, res.output

    # Exactly ONE bus line, addressed to the recipient (no md-store divergence).
    msgs = list(iter_messages())
    assert len(msgs) == 1
    assert msgs[0].to == "acme"
    assert msgs[0].kind == "fyi"
    assert msgs[0].body == "build is green"

    # The md render exists too and agrees (same single message id).
    threads = read_all_threads("acme")
    assert len(threads) == 1
    assert threads[0].messages[0].msg_id == msgs[0].id


# ---------------------------------------------------------------------------
# AC8-EDGE: legacy md threads migrate into the log; idempotent
# ---------------------------------------------------------------------------

def _write_legacy_thread(inbox_root, recipient, msg_id, body):
    """Write a thread file the way a pre-bus `fno` did (NOT via write_new_thread)."""
    inbox = inbox_root / recipient / "inbox"
    inbox.mkdir(parents=True, exist_ok=True)
    (inbox / f"2026-05-01-{msg_id}.md").write_text(
        "---\n"
        f"thread_id: {msg_id}\n"
        "from: legacy-sender\n"
        f"to: {recipient}\n"
        "kind: heads-up\n"
        "created: 2026-05-01T10:00:00Z\n"
        "---\n\n"
        f"## {msg_id} · 2026-05-01T10:00:00Z · from:legacy-sender\n\n"
        f"{body}\n",
        encoding="utf-8",
    )


def test_ac8_edge_legacy_thread_migrates_into_bus(inbox_and_bus):
    from fno.inbox.store import migrate_md_threads_to_bus
    from fno.bus.log import iter_messages

    _write_legacy_thread(inbox_and_bus, "acme", "msg-legacy1", "old heads-up body")

    # Pre-migration: the legacy message is NOT on the bus (md-only, stranded).
    assert all(m.id != "msg-legacy1" for m in iter_messages())

    res = migrate_md_threads_to_bus()
    assert res.migrated == 1
    assert "acme" in res.recipients

    # Post-migration: it's on the canonical log, addressed correctly.
    msgs = {m.id: m for m in iter_messages()}
    assert "msg-legacy1" in msgs
    assert msgs["msg-legacy1"].to == "acme"
    assert msgs["msg-legacy1"].from_ == "legacy-sender"
    assert msgs["msg-legacy1"].kind == "heads-up"
    assert msgs["msg-legacy1"].body == "old heads-up body"


def test_ac8_edge_migration_is_idempotent(inbox_and_bus):
    from fno.inbox.store import migrate_md_threads_to_bus
    from fno.bus.log import iter_messages

    _write_legacy_thread(inbox_and_bus, "acme", "msg-legacy1", "body one")
    _write_legacy_thread(inbox_and_bus, "acme", "msg-legacy2", "body two")

    first = migrate_md_threads_to_bus()
    assert first.migrated == 2
    second = migrate_md_threads_to_bus()
    assert second.migrated == 0  # nothing new on the re-run

    ids = [m.id for m in iter_messages()]
    assert ids.count("msg-legacy1") == 1
    assert ids.count("msg-legacy2") == 1
