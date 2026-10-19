"""``config.lead`` term keys: defaults present, degradation fail-safe (AC19-20).

The texts arm a self-injected /loop and /goal, so a bad value must degrade to
the default and be nameable, never raise: a config typo has no business ending
a term at load time.
"""
from __future__ import annotations


from fno.config import LEAD_CHECKIN_TEXT, LeadBlock


def test_ac20_bad_interval_degrades_to_default() -> None:
    block = LeadBlock(checkin_interval="not a duration")
    assert block.checkin_interval == "55m"
    # Valid shapes pass through verbatim.
    assert LeadBlock(checkin_interval="15m").checkin_interval == "15m"
    assert LeadBlock(checkin_interval="2h").checkin_interval == "2h"
    assert LeadBlock(checkin_interval=45).checkin_interval == "55m"  # type: ignore[arg-type]


def test_write_roots_default_and_coercion() -> None:
    assert LeadBlock().write_roots == []
    assert LeadBlock(write_roots="docs").write_roots == ["docs"]
    # Blanks and non-strings drop; a relative entry passes through as written,
    # the guard resolves it against the repo root.
    assert LeadBlock(write_roots=["docs", " ", 3, ".claude/rules"]).write_roots == ["docs", ".claude/rules"]  # type: ignore[arg-type]
    assert LeadBlock(write_roots=7).write_roots == []  # type: ignore[arg-type]


def test_shipped_defaults_pass_the_style_gate_that_sends_them() -> None:
    """The mail bus lints the body it sends, so a default that fails the gate
    refuses its own injection.

    Both texts ship as the body of a `fno agents mail send --raw`, and a fresh
    install running /fno:lead had to pass --style-exception to arm at all: the
    old checkin text carried two semicolons and the old goal text ran one
    39-word sentence. The assertion covers the INJECTED body, not the bare
    default, because the `/loop <interval> ` and `/goal ` prefixes are part of
    what the linter reads.
    """
    from fno import rust_binary
    from fno.config import load_settings

    settings = load_settings()
    cap = settings.style.word_cap.mail
    interval = settings.lead.checkin_interval
    err, receipt = rust_binary.style_receipt(
        f"/loop {interval} {LEAD_CHECKIN_TEXT}", "mail", cap
    )
    assert not err and not receipt.get("violations")
