"""Minted docs must be born passing the difficulty gate (x-e3d1).

skills/blueprint/scripts/mutate_doc.py stamps ``created: <today>`` into every
doc it first-fills, and the difficulty gate
(:data:`fno.plan.schema.DIFFICULTY_REQUIRED_AFTER`) requires a ``difficulty``
band on plans created strictly after 2026-08-26. The producer never wrote one,
so the wall clock crossing the boundary at 00:00Z on 2026-08-27 made every
minted doc born failing its own validator - with no code change at all.

The proof: the mint must CONTAIN a band and PASS the gate.
"""

from __future__ import annotations

import datetime
import importlib.util
import sys
import types
from pathlib import Path

import pytest
import yaml

from fno.plan.schema import (
    WAVES_REQUIRED_AFTER,
    difficulty_gate_error,
    waves_gate_error,
)

_REPO_ROOT = Path(__file__).resolve().parents[4]
_SCRIPT_PATH = _REPO_ROOT / "skills" / "blueprint" / "scripts" / "mutate_doc.py"


def _load_mutate_module():
    spec = importlib.util.spec_from_file_location("mutate_doc_difficulty", _SCRIPT_PATH)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules["mutate_doc_difficulty"] = module
    spec.loader.exec_module(module)
    return module


_mutate_doc = _load_mutate_module()

# The /think output shape: findings with no frontmatter at all. This is the doc
# that triggers _first_fill_block(), the mint that stamps created=<today>.
_THINK_DOC = """# Example

## Problem

Body text.

## User Stories

**US1:** As an operator, the thing works.

## Failure Modes

**Errors**
- It can fail.

## Open Questions

- Question one?
"""

# An authored doc (frontmatter present) created strictly after the gate date.
_AUTHORED_POST_GATE_NO_BAND = """---
title: example spec
status: design
created: 2026-08-27
---

# Example

## User Stories

**US1:** As an operator, the thing works.

## Open Questions

- Question one?
"""

_AUTHORED_POST_GATE_WITH_BAND = _AUTHORED_POST_GATE_NO_BAND.replace(
    "created: 2026-08-27", "created: 2026-08-27\ndifficulty: high"
)

_AUTHORED_POST_GATE_TYPO_BAND = _AUTHORED_POST_GATE_NO_BAND.replace(
    "created: 2026-08-27", "created: 2026-08-27\ndifficulty: hihg"
)


_AUTHORED_POST_GATE_WITH_EXEC_STRATEGY = (
    _AUTHORED_POST_GATE_NO_BAND
    + """
## Acceptance Criteria

### AC1-HP: The thing works

**Given** a configured operator
**When** the thing runs
**Then** it works.

## Execution Strategy

```yaml
execution_mode: mixed
waves:
  - wave: 1
    mode: sequential
    name: Implementation
    difficulty: medium
    tasks:
      - '1.1'
tasks:
  - id: '1.1'
    title: Do the thing
    surface:
      - cli/src/fno/plan/_status.py
    verify: pytest cli/tests/unit/plan -q
    acceptance:
      - AC1-HP
```
"""
)


def _frontmatter_of(doc_text: str) -> dict:
    assert doc_text.startswith("---\n"), "minted doc lost its frontmatter block"
    rest = doc_text[4:]
    end = rest.find("\n---")
    assert end != -1
    return yaml.safe_load(rest[:end])


@pytest.fixture
def post_gate_clock(monkeypatch: pytest.MonkeyPatch) -> None:
    """Freeze mutate_doc's mint date to a strictly-post-gate day.

    The mint stamps ``created: <local today>`` and the gate is date-keyed, so
    an unfrozen test's verdict flips with the runner's timezone: on 2026-08-27
    a PDT machine is still locally pre-gate. Freezing makes the mint-vs-gate
    pair deterministic on any machine, any day.
    """
    frozen = types.SimpleNamespace(
        date=type(
            "FrozenDate",
            (),
            {"today": staticmethod(lambda: datetime.date(2026, 8, 27))},
        )
    )
    monkeypatch.setattr(_mutate_doc, "datetime", frozen)


def test_minted_doc_carries_a_band_and_passes_the_gate(
    tmp_path: Path, post_gate_clock: None
) -> None:
    """The real mint path emits a difficulty key, and the emitted frontmatter
    passes difficulty_gate_error."""
    doc = tmp_path / "think-findings.md"
    doc.write_text(_THINK_DOC, encoding="utf-8")

    rc, out = _mutate_doc.mutate(doc, mode="greenfield", rewrite=False)
    assert rc == 0, f"mutate failed: {out}"

    # Positive marker on the FILE the real path wrote, not the return text:
    # asserting the mint does not raise proves nothing, because the mint never
    # validated its own output.
    fm = _frontmatter_of(doc.read_text(encoding="utf-8"))
    assert fm.get("difficulty") in {"low", "medium", "high"}, (
        f"minted frontmatter carries no difficulty band: {fm.get('difficulty')!r}"
    )
    assert difficulty_gate_error(fm) is None, "minted doc is born failing its own validator"


def test_author_set_band_is_preserved(tmp_path: Path) -> None:
    """An author's judged band must survive mutation verbatim."""
    doc = tmp_path / "authored.md"
    doc.write_text(_AUTHORED_POST_GATE_WITH_BAND, encoding="utf-8")

    rc, out = _mutate_doc.mutate(doc, mode="greenfield", rewrite=False)
    assert rc == 0, f"mutate failed: {out}"

    fm = _frontmatter_of(doc.read_text(encoding="utf-8"))
    assert fm.get("difficulty") == "high", f"author band clobbered: got {fm.get('difficulty')!r}"


def test_typoed_band_is_refused_not_coerced(tmp_path: Path) -> None:
    """A present but out-of-vocabulary band is the author's error to see, not
    a value to silently replace with the floor (review finding on the first
    cut of the probe: it treated 'hihg' as absent and stamped 'low' over it,
    pre-empting the schema refusal that names the bad value)."""
    doc = tmp_path / "authored.md"
    doc.write_text(_AUTHORED_POST_GATE_TYPO_BAND, encoding="utf-8")

    rc, out = _mutate_doc.mutate(doc, mode="greenfield", rewrite=False)
    assert rc == 3, f"expected the schema refusal, got rc={rc}: {out}"
    assert "difficulty must be one of" in out, out

    # Nothing was written; the author's typo is still on disk, not coerced.
    fm = _frontmatter_of(doc.read_text(encoding="utf-8"))
    assert fm.get("difficulty") == "hihg"


def test_finalize_writes_a_banded_doc(tmp_path: Path) -> None:
    """finalize() is the second frontmatter write path; a design doc promoted
    to ready must not be born failing the gate either."""
    doc = tmp_path / "authored.md"
    doc.write_text(_AUTHORED_POST_GATE_WITH_EXEC_STRATEGY, encoding="utf-8")

    rc, out = _mutate_doc.finalize(doc, no_emit=True)
    assert rc == 0, f"finalize failed: {out}"

    fm = _frontmatter_of(out)
    assert fm.get("difficulty") in {"low", "medium", "high"}, (
        f"finalized doc carries no band: {fm.get('difficulty')!r}"
    )
    assert difficulty_gate_error(fm) is None


def test_plan_mode_skeleton_is_born_passing_the_gate(tmp_path: Path) -> None:
    """The plan-mode backfill skeleton is a third mint: it stamps a created
    date, so it must stamp the canonical key and the band, or its doc is born
    failing the gate (and undatable besides - the legacy created_at synonym
    is read by no canonical reader)."""
    import subprocess

    native = tmp_path / "native.md"
    native.write_text("# Approved native plan\n\nDo the thing.\n", encoding="utf-8")
    out_doc = tmp_path / "backfilled.md"

    proc = subprocess.run(
        [
            "bash",
            str(_REPO_ROOT / "skills" / "target" / "scripts" / "backfill-plan.sh"),
            "skeleton",
            str(native),
            str(out_doc),
        ],
        capture_output=True,
        text=True,
    )
    assert proc.returncode == 0, proc.stderr

    fm = _frontmatter_of(out_doc.read_text(encoding="utf-8"))
    assert "created" in fm, f"skeleton mints no canonical created key: {sorted(fm)!r}"
    assert fm.get("difficulty") in {"low", "medium", "high"}, (
        f"skeleton carries no difficulty band: {fm.get('difficulty')!r}"
    )
    assert difficulty_gate_error(fm) is None


# ---------------------------------------------------------------------------
# The Execution Strategy gate (x-a804), the same date-keyed shape one rung up.
#
# `quick` was told to skip `## Execution Strategy` on the assertion that a
# quick plan is single-task, and nothing checked the assertion: 200 of the 232
# flat quick-plans in a fortnight carried more than one numbered change. Each
# measured width 0, so `fno backlog join` could not tell a genuinely
# single-task plan from one that never declared its parallelism.
#
# A post-gate plan with no strategy must be REFUSED, and the identical plan
# dated on or before the gate must still PASS (so the fix cannot silently
# become a retroactive gate that refuses 1385 plans nobody is going to rewrite).
# ---------------------------------------------------------------------------

_ONE_DAY = datetime.timedelta(days=1)


def _quick_fm(created: datetime.date, *, status: str = "ready") -> dict:
    return {
        "status": status,
        "kind": "quick-plan",
        "created": created.isoformat(),
        "difficulty": "medium",
    }


def test_waves_gate_refuses_a_post_gate_plan_with_no_strategy() -> None:
    """AC5-ERR: strictly after the boundary, the section is required and the
    refusal names the gate date so an author can date their own plan."""
    fm = _quick_fm(WAVES_REQUIRED_AFTER + _ONE_DAY)
    err = waves_gate_error(fm, has_execution_strategy=False)
    assert err is not None
    assert WAVES_REQUIRED_AFTER.isoformat() in err, err
    assert "Execution Strategy" in err, err


def test_waves_gate_boundary_is_strictly_after_not_on_or_after() -> None:
    """A plan created ON the gate date predates the gate reaching its author,
    the same boundary the difficulty gate uses. Pinned because an off-by-one
    here silently refuses a day's worth of plans."""
    assert waves_gate_error(_quick_fm(WAVES_REQUIRED_AFTER), has_execution_strategy=False) is None
    assert (
        waves_gate_error(_quick_fm(WAVES_REQUIRED_AFTER + _ONE_DAY), has_execution_strategy=False)
        is not None
    )


@pytest.mark.parametrize("status", ["idea", "design"])
def test_waves_gate_exempts_the_pre_execution_rungs(status: str) -> None:
    """THE MIDNIGHT TRAP, closed by construction.

    `scaffold_separate_plan` mints an epic child with `created` stamped at
    RUNTIME and no Execution Strategy, and a /think doc sits at `design` until
    /blueprint appends one. A gate that fired on either would go red at
    00:00Z on a commit nobody touched - exactly what the difficulty gate did
    at 2026-08-27T00:00Z, which took smoke-rest red on every open PR.
    """
    fm = _quick_fm(WAVES_REQUIRED_AFTER + _ONE_DAY, status=status)
    assert waves_gate_error(fm, has_execution_strategy=False) is None


def test_waves_gate_undatable_refuses_on_the_minting_lanes() -> None:
    """Same split as the difficulty gate: a lane with no fallback dater
    refuses rather than minting a strategy-less plan it cannot date."""
    fm = {"status": "ready", "kind": "quick-plan", "difficulty": "medium"}
    err = waves_gate_error(fm, has_execution_strategy=False)
    assert err is not None
    assert "cannot read created" in err, err
