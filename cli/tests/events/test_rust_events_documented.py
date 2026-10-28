"""Rust events are documented in events-schema.yaml.

Every kind in the Rust ``KNOWN_EVENT_KINDS`` const has a schema.yaml entry
listing its producer source. Supervisor events name ``daemon`` or ``subagent``;
``version_skew`` is emitted by the standalone Rust spawn gate and names ``rust``.
The pre-existing sources and Python event types stay additive-only.

The kind list is parsed out of lib.rs, not restated here, so adding a kind on
the Rust side cannot leave this file quietly asserting nothing about it.
"""
from __future__ import annotations

import re
from pathlib import Path

import pytest
import yaml

REPO_ROOT = Path(__file__).resolve().parents[3]
SCHEMA_PATH = REPO_ROOT / "cli/src/fno/events/schema.yaml"
LIB_RS = REPO_ROOT / "crates/fno-agents/src/lib.rs"

# Floor for the parsed const size, guarding against a VACUOUS pass: a regex that
# matched the wrong block or nothing at all would otherwise leave every assertion
# below comparing against an empty list.
#
# Deliberately well below the real count (49 at the time of writing) rather than
# just under it. Tightening it to catch a parse that loses a handful of kinds also
# turns a legitimate removal of a handful into a false red, and the const-to-
# schema.yaml direction is independently checked by step 6 of
# scripts/check-event-schema-parity.sh, so this does not have to carry that too.
# Its one job is telling a broken parse from a working one.
MIN_EVENT_KINDS = 40

# Source values that must be in the envelope.source enum
REQUIRED_SOURCES = [
    "target", "megawalk", "megatron", "fno-loop",
    "hook", "subagent", "migration", "test", "backlog",
    "daemon",  # added in W7
]


RUST_CLIENT_EVENT_SOURCES = {
    "version_skew": "rust",
    # The native triage engine emits the apply telemetry from the backlog
    # client command, the same producer source its Python twin declares.
    "triage_applied": "backlog",
    # The native archive sweep emits from the backlog archive command.
    "graph_archive_swept": "backlog",
    # A standalone research round records its DoneAdvisory terminal under the
    # hook source, as its Python twin did.
    "termination": "hook",
    # The native observer (fno doctor observer) emits its findings and run
    # close under the observer source its Python twin declared.
    "skill_eval_finding": "observer",
    "skill_eval_run_complete": "observer",
}


@pytest.fixture(scope="module")
def schema() -> dict:
    return yaml.safe_load(SCHEMA_PATH.read_text(encoding="utf-8"))


def parse_known_event_kinds(text: str) -> list[str]:
    """Pull the KNOWN_EVENT_KINDS entries out of lib.rs source text.

    The floor assert keeps a parse that matched nothing from passing vacuously.
    """
    start = text.find("pub const KNOWN_EVENT_KINDS")
    assert start != -1, "KNOWN_EVENT_KINDS const not found"
    end = text.find("];", start)
    assert end != -1, "KNOWN_EVENT_KINDS block is unterminated"
    kinds = re.findall(r'"([A-Za-z0-9_]+)"', text[start:end])
    assert len(kinds) >= MIN_EVENT_KINDS, (
        f"parsed only {len(kinds)} kinds out of KNOWN_EVENT_KINDS, expected at "
        f"least {MIN_EVENT_KINDS}. The const was probably reformatted and this "
        f"parser needs updating. If instead kinds were legitimately removed and "
        f"the count is really this low, lower the floor deliberately - but check "
        f"the parse first, because that is the failure this floor is here for."
    )
    return kinds


@pytest.fixture(scope="module")
def rust_event_kinds() -> list[str]:
    """Rust event kinds, read out of KNOWN_EVENT_KINDS in lib.rs.

    Derived from source text rather than mirrored in a Python list. The mirror
    this replaced had drifted 16 entries behind the const while every test here
    kept passing: the checks are additive-only, so a kind missing from the
    mirror is a kind nothing asserts about.
    """
    # Assert, do not skip. The const lives in this repo, so absence means it
    # moved or the crate was renamed - and a skip is a green run, which would
    # quietly stop asserting the very thing this fixture exists to assert.
    assert LIB_RS.exists(), f"{LIB_RS} not found; did the crate move?"
    return parse_known_event_kinds(LIB_RS.read_text(encoding="utf-8"))


def test_existing_sources_preserved(schema: dict) -> None:
    """All pre-existing sources must still be in the enum (additive-only)."""
    enum = set(schema["envelope"]["properties"]["source"]["enum"])
    for src in REQUIRED_SOURCES:
        assert src in enum, f"source {src!r} removed from enum (additive-only rule)"


def test_rust_events_have_their_producer_source(schema: dict, rust_event_kinds: list[str]) -> None:
    """Rust event entries name the daemon/subagent or standalone-client producer."""
    documented = {e["name"]: e for e in schema.get("event_types", [])}
    for kind in rust_event_kinds:
        entry = documented.get(kind)
        if entry is None:
            continue  # caught by scripts/check-event-schema-parity.sh
        sources = entry.get("sources", [])
        client_source = RUST_CLIENT_EVENT_SOURCES.get(kind)
        allowed = (
            ("daemon", "subagent", client_source)
            if client_source is not None
            else ("daemon", "subagent")
        )
        assert any(
            s in allowed for s in sources
        ), f"Rust event {kind!r} sources {sources!r} must include one of {allowed!r}"
