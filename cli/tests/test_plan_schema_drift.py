"""Anti-drift guards for the plan-frontmatter schema.

Mirrors ``test_config_schema_drift.py``: ``fno.plan.schema.PlanFrontmatter`` is
the single source of truth for plan frontmatter, and these tests fail CI the
moment the model drifts from its three upstream sources - ``status.py`` (the
status vocabulary the enum is derived from), ``_stamp.py`` (the ship-time
writer whose keys the model must cover), and ``skills/blueprint/references/
single-doc-spec.md`` (the design-doc template blueprint mutates every plan
from).

This is the one git-CI piece of the plan-schema feature (Locked Decision 4):
plans themselves live in the untracked vault, so the model - which lives in the
repo - is what CI can protect.
"""
from __future__ import annotations

import re
from pathlib import Path

from fno.plan._status import STATUS_PROGRESSION, TERMINAL_STATUSES
from fno.plan.schema import PlanFrontmatter, PlanStatus

_STAMP_RS = (
    Path(__file__).resolve().parents[2]
    / "crates" / "fno-agents" / "src" / "plan_doc" / "stamp.rs"
)


def test_plan_status_axis_matches_status_module() -> None:
    """PlanStatus members == STATUS_PROGRESSION plus exactly {done, superseded} (AC2-HP).

    Fails the build the moment someone edits the axis in ``status.py`` (or the
    enum here) without the other - the exact drift-killer the config schema
    already uses.
    """
    members = {m.value for m in PlanStatus}
    axis = set(STATUS_PROGRESSION)
    terminals = set(TERMINAL_STATUSES)

    assert terminals == {"done", "superseded"}, (
        f"off-axis terminals drifted: {sorted(terminals)} != ['done', 'superseded']"
    )
    assert members == axis | terminals, (
        "PlanStatus drifted from status.py; the enum must be derived from "
        "STATUS_PROGRESSION + TERMINAL_STATUSES (never hand-listed)"
    )
    # done/superseded are off the monotonic axis, never inserted into it.
    assert members - axis == terminals


def test_stamp_written_fields_are_modeled() -> None:
    """Every frontmatter key the plan writer stamps has a PlanFrontmatter field.

    Catches the drift class where the ship-time writer starts emitting a key
    the schema doesn't know about, so ``fno do plan validate`` would silently pass
    a plan carrying an unmodeled ship field. The writer is the Rust
    plan_doc/stamp.rs port; the regex reads its `fields.insert` calls.
    """
    src = _STAMP_RS.read_text(encoding="utf-8")
    written = set(re.findall(r'fields\.insert\("(\w+)"', src))

    # Guard against the regex going inert (a refactor renaming `fields`): the
    # load-bearing ship keys must always be found.
    documented = {"status", "shipped_at", "urls", "session_ids", "expected_url_count"}
    assert documented <= written, (
        f"stamp-writer regex missed documented keys: {sorted(documented - written)}"
    )

    modeled = set(PlanFrontmatter.model_fields)
    missing = written - modeled
    assert not missing, (
        f"the plan writer stamps frontmatter keys with no PlanFrontmatter field: {sorted(missing)}"
    )


# -- single-doc-spec.md, the third upstream source (x-b9d7 US4) --

DESIGN_SPEC = (
    Path(__file__).resolve().parents[2]
    / "skills/blueprint/references/single-doc-spec.md"
)

def _design_template() -> str:
    """The ```markdown design-doc block from single-doc-spec.md.

    The spec carries the design-doc shape blueprint mutates; the template block
    is the one holding the frontmatter, identified by its `status: design`.
    """
    blocks = re.findall(r"```markdown\n(.*?)```", DESIGN_SPEC.read_text(encoding="utf-8"), re.S)
    contract = [b for b in blocks if "status: design" in b]
    assert len(contract) == 1, (
        f"expected exactly one design-doc block in {DESIGN_SPEC.name}, "
        f"found {len(contract)} - the extractor has gone inert or the doc grew a twin"
    )
    return contract[0]


def test_design_template_actually_validates() -> None:
    """AC8: the template's own keys and VALUES must satisfy the model.

    A name-subset check cannot see a type error - a template writing
    `claims` as a list, `status` as a retired spelling, or `created` as
    something that is not a date shows the right keys and passes it. That is
    exactly the drift that shipped (`claims: [x-9999, x-8888]` -> `Input should
    be a valid string`), so this instantiates the model from the template with
    its `<...>` placeholders filled in. A missing required field fails here too.
    """
    import yaml

    frontmatter = _design_template().split("---")[1]
    # Fill the angle-bracket placeholders the way the doc tells an author to.
    filled = (
        frontmatter.replace("<title>", "Probe")
        .replace("<node id; a scalar, never a list>", "x-9999")
        .replace("<node id>", "x-9999")
        .replace("<YYYY-MM-DD>", "2026-07-28")
        .replace("<low | medium | high>", "medium")
        .replace("light|standard|deep", "standard")
        .replace("[<artifacts actually read>]", "[probe.py]")
        .replace("<absorb | append | proceed_alone>", "proceed_alone")
    )
    assert "<" not in filled, (
        "the frontmatter template grew a placeholder this test does not fill, "
        f"so it is no longer validating the real shape: {filled!r}"
    )
    PlanFrontmatter(**yaml.safe_load(filled))  # raises ValidationError on drift
