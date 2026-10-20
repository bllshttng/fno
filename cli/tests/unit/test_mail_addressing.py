"""Addressing: full sender provenance and collision-safe reply addresses.

The envelope carries the full session id as its reply address, so concurrent
sessions do not collide regardless of harness id format.

Kept in one module rather than split across the three legacy mail-test files,
because these behaviors form one contract.
"""
from __future__ import annotations

from types import SimpleNamespace

import pytest

from fno.harness_identity import (
    canonical_handle,
    is_unsafe_short_address,
)

# Two real-shaped UUIDv7 ids from one clock bucket: identical head-8, distinct
# tails. This is the collision, written down.
V7_A = "01a025f8-09af-74b2-a461-998a18e3bfca"


@pytest.mark.parametrize(
    "token,harness,unsafe",
    [
        ("01a025f8", "codex", True),
        ("01a025f8", "claude", False),  # v4 head-8 is 32 random bits
    ],
)
def test_unsafe_short_address_keys_on_shape_and_harness(token, harness, unsafe):
    assert is_unsafe_short_address(token, harness) is unsafe


def test_a_name_lane_row_replies_to_the_full_session(tmp_path, monkeypatch):
    """The regression this half exists for, exercised end to end.

    `_name_lane_send` stamps `to_kind="name"` on every record it writes, and it
    is the lane that carries `from_session`. A reply that consulted the full id
    only for `to_kind == "session"` therefore never read the value it had just
    been taught to store, and the ambiguous-handle refusal still fired.
    """
    from typer.testing import CliRunner

    from fno.bus.log import Envelope, append
    from fno.mail.cli import mail_app
    from fno.paths_testing import use_tmpdir

    use_tmpdir(monkeypatch, tmp_path)
    append(
        Envelope.new(
            id="msg-882e18",
            from_=canonical_handle(V7_A),
            to="me",
            kind="send",
            body="hi",
            to_kind="name",
            from_session=V7_A,
        )
    )

    seen: dict = {}
    monkeypatch.setattr(
        "fno.mail.cli._reply_to_name_handle",
        lambda *_a, **kw: seen.update(kw),
    )
    result = CliRunner().invoke(mail_app, ["reply", "ok", "--to", "msg-882e18"])

    assert result.exit_code == 0, result.output
    assert seen.get("target") == V7_A, seen


@pytest.mark.parametrize(
    "extra",
    [
        ["--raw"],
        ["--kind", "heads-up"],
    ],
    ids=["raw", "kind"],
)
def test_the_short_address_refusal_covers_the_lanes_that_return_early(
    tmp_path, monkeypatch, extra
):
    """An address rule that only covers the lanes reached last is not a rule.

    The refusal sat below the `--raw`, `--to-project`, `--kind` and job-address
    returns, so `send <codex-head-8> '/verb' --raw` still fired a verb at
    whichever colliding session discovery happened to list. `--raw` is the worst
    of them: it runs a verb rather than leaving a message to be read.
    """
    import json

    from typer.testing import CliRunner

    from fno.mail.cli import mail_app
    from fno.paths_testing import use_tmpdir

    use_tmpdir(monkeypatch, tmp_path)
    # The send body lives in the Rust leaf now, so the refusal reads the
    # registry FILE the subprocess sees; a python loader patch never reaches
    # it. The autouse home fixture points $FNO_AGENTS_HOME here.
    agents_home = tmp_path / ".fno" / "agents"
    agents_home.mkdir(parents=True)
    (agents_home / "registry.json").write_text(
        json.dumps(
            {
                "schema_version": 1,
                "agents": [
                    {
                        "name": "codex-worker", "harness": "codex",
                        "harness_session_id": V7_A, "session_id": V7_A,
                        "mux": {"session": "main", "pane_id": 3}, "cwd": "/w",
                        "log_path": "/w/main.log", "status": "live",
                    }
                ],
            }
        ),
        encoding="utf-8",
    )

    result = CliRunner().invoke(mail_app, ["send", V7_A[:8], "/fno:review", *extra])

    assert result.exit_code == 2, result.output
    assert "addresses codex" in (result.stderr or result.output)


def test_a_project_note_body_is_not_read_as_an_address(tmp_path, monkeypatch):
    """On the --to-project lane the positional is the BODY, not an address.

    The project is the address there. Hoisting the refusal without excluding
    this lane made an eight-hex body refuse as a codex handle whenever a live
    codex row happened to share those characters, which aims an addressing rule
    at content nobody was addressing.
    """
    from typer.testing import CliRunner

    import fno.agents.registry as registry
    from fno.mail.cli import mail_app
    from fno.paths_testing import use_tmpdir

    use_tmpdir(monkeypatch, tmp_path)
    entry = SimpleNamespace(
        name="codex-worker", harness="codex",
        harness_session_id=V7_A, session_id=V7_A,
        mux={"session": "main", "pane_id": 3}, cwd="/w", status="live",
    )
    monkeypatch.setattr(registry, "load_registry", lambda *_a, **_k: [entry])
    # A body that happens to BE the colliding head-8.
    result = CliRunner().invoke(
        mail_app, ["send", "--to-project", "footnote", V7_A[:8]]
    )

    # The assertion is the ABSENCE of an addressing refusal aimed at content.
    # What the project lane then does with the note is not this test's business,
    # so nothing downstream is stubbed and no exit code is pinned.
    combined = (result.stderr or "") + result.output
    assert "addresses codex" not in combined, combined
