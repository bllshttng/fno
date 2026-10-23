"""Tests for `fno agents send --kind` (bus epic G4 / US8, AC8-HP).

The faithful replacement for `fno inbox send --to <project> --kind <kind>`:
inbox kinds (heads-up / question / fyi) route through the durable inbox
write path so the recipient's drain still dispatches by kind. Covers the
data-layer core (`post_inbox_message`) and the CLI flag wiring.
"""
from __future__ import annotations

from pathlib import Path

import pytest
from typer.testing import CliRunner

from fno.paths_testing import use_tmpdir


@pytest.fixture
def runner() -> CliRunner:
    return CliRunner()


@pytest.fixture
def isolated(tmp_path: Path, monkeypatch):
    use_tmpdir(monkeypatch, tmp_path)
    monkeypatch.setenv("FNO_INBOX_ROOT", str(tmp_path / "inbox"))
    monkeypatch.setenv("FNO_INBOX_TEST_MODE", "1")  # suppress desktop notify
    return tmp_path


# ---------------------------------------------------------------------------
# CLI wiring: fno agents send --kind
# ---------------------------------------------------------------------------


def _invoke(runner: CliRunner, *args: str):
    from fno.mail.cli import mail_app

    return runner.invoke(mail_app, ["send", *args])


def test_ac3_kind_send_projectless_error_names_real_flags(isolated, runner) -> None:
    """AC3: a --kind send from a project-less repo (no ``project`` key) fails
    naming ``--from-name``/``--from-self``, the flags ``mail send`` actually
    has -- never the bare ``--from`` (which lives only on the inbox commands)."""
    scratch = isolated / "no-project"
    scratch.mkdir()
    # --to-project reaches the sender-identity resolution this AC tests; a bare
    # --kind fyi <handle> now hits the US10 kind-scoped guard first (see
    # test_ac10_kind_fyi_to_handle_refused).
    result = _invoke(
        runner,
        "--to-project", "acme-docs",
        "--kind", "fyi",
        "-b", "hi",
        "--cwd", str(scratch),
    )
    assert result.exit_code == 2, result.output
    assert "--from-name/--from-self" in result.output
    assert "pass --from\n" not in result.output


def test_cmd_send_kind_heads_up_writes_durable_thread(isolated, runner) -> None:
    from fno.inbox.store import read_unread_threads

    result = _invoke(
        runner,
        "--to-project", "acme-docs",
        "--kind", "heads-up",
        "--from-name", "acme-web",
        "locked schema change, impact is a migration",
    )

    assert result.exit_code == 0, result.output
    assert "queued (durable)" in result.output
    # The durable receipt names why it is durable.
    assert "param-forced: --kind heads-up" in result.output
    threads = read_unread_threads("acme-docs")
    assert len(threads) == 1
    assert threads[0].kind == "heads-up"
    assert threads[0].from_project == "acme-web"


def test_cmd_send_fyi_persist_memory(isolated, runner) -> None:
    from fno.inbox.store import read_unread_threads

    result = _invoke(
        runner,
        "--to-project", "acme-backend",
        "--kind", "fyi",
        "--persist", "memory",
        "--from-name", "acme-web",
        "lesson: always quote yaml scalars",
    )
    assert result.exit_code == 0, result.output
    threads = read_unread_threads("acme-backend")
    assert threads[0].persist_to_memory is True


def test_cmd_send_invalid_kind_rejected(isolated, runner) -> None:
    result = _invoke(
        runner, "--to-project", "x", "--kind", "bogus", "--from-name", "a", "body"
    )
    assert result.exit_code == 2
    assert "heads-up" in result.output  # names the valid kinds


def test_cmd_send_persist_nonfyi_rejected(isolated, runner) -> None:
    result = _invoke(
        runner,
        "--to-project", "x",
        "--kind", "heads-up",
        "--persist", "memory",
        "--from-name", "a",
        "body",
    )
    assert result.exit_code == 2


def test_cmd_send_reply_to_appends(isolated, runner) -> None:
    from fno.inbox.store import read_unread_threads, write_new_thread

    root = write_new_thread("acme-docs", "acme-web", "fyi", "root")
    result = _invoke(
        runner,
        "--to-project", "acme-docs",
        "--kind", "fyi",
        "--reply-to", root.thread_id,
        "--from-name", "acme-web",
        "appended reply",
    )
    assert result.exit_code == 0, result.output
    assert "appended (durable)" in result.output
    threads = read_unread_threads("acme-docs")
    assert len(threads[0].messages) == 2


def test_cmd_send_ref_flags_ride_in_thread(isolated, runner) -> None:
    """--ref-pr/--ref-node enrich the thread for recipient triage (AC8-UI)."""
    from fno.inbox.store import read_unread_threads

    result = _invoke(
        runner,
        "--to-project", "acme-web",
        "--kind", "heads-up",
        "--from-name", "acme-backend",
        "--ref-pr", "112",
        "--ref-node", "ab-1f3c9a2b",
        "--body", "region column needed",
    )
    assert result.exit_code == 0, result.output
    refs = read_unread_threads("acme-web")[0].refs
    assert refs.get("ref_pr") == "112"
    assert refs.get("ref_node") == "ab-1f3c9a2b"


def test_cmd_send_deprecated_kind_gives_migration_hint(isolated, runner) -> None:
    result = _invoke(
        runner, "--to-project", "x", "--kind", "lesson", "--from-name", "a", "body"
    )
    assert result.exit_code == 2
    out = result.stdout + (result.stderr or "")
    assert "fyi --persist memory" in out  # the retired-kind replacement hint


# ---------------------------------------------------------------------------
# --from-self (x-5ee2 US2): stamp the sender's own reply handle, not the project
# ---------------------------------------------------------------------------


def _only_marker(monkeypatch, marker: str, value: str) -> None:
    """Force ambient identity to resolve to exactly `marker`, clearing the
    higher-precedence markers so the test is deterministic on any host."""
    for m, _ in __import__("fno.harness_identity", fromlist=["x"]).HARNESS_SESSION_MARKERS:
        monkeypatch.delenv(m, raising=False)
    monkeypatch.setenv(marker, value)


def test_cmd_send_from_self_stamps_canonical_handle(isolated, runner, monkeypatch) -> None:
    """--from-self stamps the session's canonical handle as sender, so a reply to
    a --to-project note lands in the sender's session lane, not the project inbox."""
    from fno.inbox.store import read_unread_threads

    _only_marker(monkeypatch, "CLAUDE_CODE_SESSION_ID", "879d8d26-2505-4977-9b87-000000000000")
    result = _invoke(
        runner,
        "--to-project", "acme-docs",
        "--kind", "question",
        "--from-self",
        "--body", "which auth?",
    )
    assert result.exit_code == 0, result.output
    assert read_unread_threads("acme-docs")[0].from_project == "879d8d26"


def test_cmd_send_from_self_without_identity_fails_loud(isolated, runner, monkeypatch) -> None:
    """No ambient harness identity -> exit 2 with an explicit message, nothing sent
    (never the silent 'fno' floor stamp_from uses)."""
    for m, _ in __import__("fno.harness_identity", fromlist=["x"]).HARNESS_SESSION_MARKERS:
        monkeypatch.delenv(m, raising=False)
    result = _invoke(
        runner,
        "--to-project", "x", "--kind", "question", "--from-self", "--body", "q",
    )
    assert result.exit_code == 2
    assert "no ambient harness identity" in (result.stdout + (result.stderr or ""))


# ---------------------------------------------------------------------------
# US10: kind-scoped guard (AC10) + routing-reason disclosure (AC9)
# ---------------------------------------------------------------------------


def _bus_len() -> int:
    from fno.bus.log import iter_messages

    return len(list(iter_messages()))


def test_ac10_kind_fyi_to_handle_refused(isolated, runner):
    kind = "fyi"
    # A project-inbox kind aimed at a bare session handle has no drain; refuse,
    # name the two real intents, and write nothing. Wording is pinned so the
    # guidance cannot drift from the behavior.
    result = _invoke(runner, "cafe0001", "--kind", kind, "-b", "hi")
    assert result.exit_code == 2, result.output
    out = result.output
    assert f"--kind {kind} to a session handle" in out
    assert "Drop --kind to inject it live" in out
    assert f"add --to-project <project> to file it as a durable {kind} note" in out
    assert _bus_len() == 0  # nothing written for the refused combo
