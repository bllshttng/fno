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
def test_the_short_address_refusal_covers_the_lanes_that_return_early(tmp_path, monkeypatch, extra):
    """An address rule that only covers the lanes reached last is not a rule.

    The refusal sat below the `--raw`, `--to-project`, `--kind` and job-address
    returns, so `send <codex-head-8> '/verb' --raw` still fired a verb at
    whichever colliding session discovery happened to list. `--raw` is the worst
    of them: it runs a verb rather than leaving a message to be read.
    """
    from types import SimpleNamespace

    from typer.testing import CliRunner

    import fno.agents.registry as registry
    from fno.mail.cli import mail_app
    from fno.paths_testing import use_tmpdir

    use_tmpdir(monkeypatch, tmp_path)
    entry = SimpleNamespace(
        name="codex-worker",
        harness="codex",
        harness_session_id=V7_A,
        session_id=V7_A,
        mux={"session": "main", "pane_id": 3},
        cwd="/w",
        status="live",
    )
    monkeypatch.setattr(registry, "load_registry", lambda *_a, **_k: [entry])

    result = CliRunner().invoke(mail_app, ["send", V7_A[:8], "/fno:review", *extra])

    assert result.exit_code == 2, result.output
    assert "addresses codex" in (result.stderr or result.output)


def test_to_self_is_not_refused_as_an_ambiguous_codex_handle(tmp_path, monkeypatch):
    """Self-addressing has nothing to be ambiguous between.

    `--to-self` DERIVES the address from the running session, then the shape
    refusal rejected that head-8 on a codex worker. So
    `mail send --to-self --raw '/<verb>'` was dead on codex: the documented
    self-invocation path, and the one the --force refusal recommends. The caller
    could not even work around it, because `--to-self` rejects a positional
    address. The rule guards a slice that cannot pick between two sessions, and
    there is only ever one session running this process.
    """
    from typer.testing import CliRunner

    from fno.mail.cli import mail_app
    from fno.paths_testing import use_tmpdir

    import fno.agents.registry as registry

    use_tmpdir(monkeypatch, tmp_path)
    monkeypatch.setattr(
        "fno.agents.self_stamp.require_self_identity",
        lambda *_a, **_k: SimpleNamespace(session_id=V7_A, harness="codex"),
    )
    # The registry row is what proves the head-8 is codex's, so without it the
    # refusal never fires and this test would pass against the broken code.
    monkeypatch.setattr(
        registry,
        "load_registry",
        lambda *_a, **_k: [
            SimpleNamespace(
                name="me",
                harness="codex",
                harness_session_id=V7_A,
                session_id=V7_A,
                mux={"session": "main", "pane_id": 1},
                cwd="/w",
                status="live",
            )
        ],
    )
    fired: dict = {}
    monkeypatch.setattr(
        "fno.mail.cli._forward_mail_send_raw",
        lambda argv: fired.update(argv=argv),
    )

    result = CliRunner().invoke(mail_app, ["send", "/fno:review", "--to-self", "--raw"])

    assert result.exit_code == 0, (result.output, result.stderr)
    assert fired["argv"][1] == canonical_handle(V7_A), fired
    assert "--raw" in fired["argv"], fired
    assert "--self-ok" in fired["argv"], "the raw lane needs its own self permission"
