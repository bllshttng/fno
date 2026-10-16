"""Unit tests for ``fno backlog join`` (x-8d1d, epic x-956c joiner 2).

``join_node`` resolves the holder's worktree from the LIVE node claim,
computes the bound plan's ready-graph width, and spawns
``min(workers, width - 1)`` ``/fno:execute waves <plan>`` joiners there as
visitors. The graph and claim seams are monkeypatched; the plan fixtures are
real files (the width walk runs the actual parser); the spawn subprocess is
recorded, never run.
"""

from __future__ import annotations

import json

import pytest


PARALLEL_PLAN = """---
title: t
status: ready
---

## Execution Strategy

```yaml
execution_mode: parallel
waves:
  - wave: 1
    mode: parallel
    tasks: ['1.1', '1.2', '1.3']
tasks:
  - id: '1.1'
    title: a
    surface: ['src/a.py']
  - id: '1.2'
    title: b
    surface: ['src/b.py']
  - id: '1.3'
    title: c
    surface: ['src/c.py']
```
"""

SEQUENTIAL_PLAN = """---
title: t
status: ready
---

## Execution Strategy

```yaml
execution_mode: sequential
waves:
  - wave: 1
    tasks: ['1.1']
  - wave: 2
    tasks: ['2.1']
  - wave: 3
    tasks: ['3.1']
  - wave: 4
    tasks: ['4.1']
  - wave: 5
    tasks: ['5.1']
tasks:
  - id: '1.1'
    title: a
  - id: '2.1'
    title: b
  - id: '3.1'
    title: c
  - id: '4.1'
    title: d
  - id: '5.1'
    title: e
```
"""


# One distinct band, six independent tasks. The width is what should size the
# lane count; `len(bands)` is 1 and used to cap it at a single joiner.
SINGLE_BAND_WIDE_PLAN = """---
title: single band wide
status: ready
---

## Execution Strategy

```yaml
execution_mode: parallel
waves:
  - wave: 1
    mode: parallel
    difficulty: medium
    tasks: ['1.1', '1.2', '1.3', '1.4', '1.5', '1.6']
tasks:
  - id: '1.1'
    title: a
    surface: ['a.py']
  - id: '1.2'
    title: b
    surface: ['b.py']
  - id: '1.3'
    title: c
    surface: ['c.py']
  - id: '1.4'
    title: d
    surface: ['d.py']
  - id: '1.5'
    title: e
    surface: ['e.py']
  - id: '1.6'
    title: f
    surface: ['f.py']
```
"""


# Four single-task waves banded high / medium / medium / low; the later
# waves declare explicit empty blockers so the whole graph is ready at once
# (width 4). Distinct bands: three, and the lane count is now the width.
BANDED_PLAN = """---
title: banded
status: ready
---

## Execution Strategy

```yaml
execution_mode: parallel
waves:
  - wave: 1
    mode: sequential
    difficulty: high
    tasks: ['1.1']
  - wave: 2
    mode: sequential
    difficulty: medium
    tasks: ['1.2']
  - wave: 3
    mode: sequential
    difficulty: medium
    tasks: ['1.3']
  - wave: 4
    mode: sequential
    difficulty: low
    tasks: ['1.4']
tasks:
  - id: '1.1'
    title: a
  - id: '1.2'
    title: b
    blocked_by: []
  - id: '1.3'
    title: c
    blocked_by: []
  - id: '1.4'
    title: d
    blocked_by: []
```
"""


def _naming_real(cmd, **kw):
    """The name verbs run in the binary; a blanket stub answers the mint with
    a spawn receipt. Capture the real run before any test patches it."""
    parts = [str(part) for part in cmd]
    if {"name-mint", "name-codes", "name-parse", "registry-commit"} & set(parts):
        return _REAL_RUN(cmd, **kw)
    if {"doctor", "event"} <= set(parts):
        # Event emission rides the same subprocess seam; it is not a spawn.
        return _REAL_RUN(cmd, **kw)
    return None

_REAL_RUN = __import__("subprocess").run






def call_env(call, key):
    return (call["env"] or {}).get(key, "")
































# ---------------------------------------------------------------------------
# Per-band lanes (x-dadc): one worker per band present, the grid resolves
# each band's lane, the band rides both env and the brief's table.
# ---------------------------------------------------------------------------
















# ---------------------------------------------------------------------------
# The roster-name rung in resolve_task_holder (joiner 1's holder contract,
# completed for daemon-forked workers where the env export cannot reach)
# ---------------------------------------------------------------------------


def _identity(monkeypatch, tmp_path, session_id):
    import json
    from types import SimpleNamespace

    reg = tmp_path / "registry.json"
    reg.write_text(json.dumps({"schema_version": 2, "agents": [
        {"name": "j-x-9734-2", "harness_session_id": session_id, "status": "live"},
    ]}))
    monkeypatch.setattr("fno.paths.agents_registry_path", lambda: reg)
    monkeypatch.setattr(
        "fno.claims.self_identity.resolve_self_identity",
        lambda env, **_k: SimpleNamespace(
            session_id=session_id, harness="claude", disposition="proven"
        ),
    )


def test_task_holder_prefers_env_name(tmp_path, monkeypatch):
    from fno.claims.self_identity import resolve_task_holder

    holder, why = resolve_task_holder({"FNO_WORKER_NAME": "j-x-9734-1"})
    assert (holder, why) == ("j-x-9734-1", "")


def test_task_holder_reads_roster_binding_when_env_missing(tmp_path, monkeypatch):
    """The daemon fork drops the env export; the spawn-time registry row
    proves the same name for the worker it bound."""
    from fno.claims.self_identity import resolve_task_holder

    sid = "721b0775-8b99-4f8f-a067-b9642e8ced8a"
    _identity(monkeypatch, tmp_path, sid)
    holder, why = resolve_task_holder({})
    assert (holder, why) == ("j-x-9734-2", "")


def test_task_holder_ambiguous_registry_falls_back(tmp_path, monkeypatch):
    import json
    from types import SimpleNamespace

    from fno.claims.self_identity import resolve_task_holder

    sid = "aaaa0000-0000-0000-0000-000000000000"
    reg = tmp_path / "registry.json"
    reg.write_text(json.dumps({"schema_version": 2, "agents": [
        {"name": "row-a", "harness_session_id": sid},
        {"name": "row-b", "harness_session_id": sid},
    ]}))
    monkeypatch.setattr(
        "fno.paths.agents_registry_path", lambda: reg
    )
    monkeypatch.setattr(
        "fno.claims.self_identity.resolve_self_identity",
        lambda env, **_k: SimpleNamespace(
            session_id=sid, harness="claude", disposition="proven"
        ),
    )
    holder, _why = resolve_task_holder({})
    assert holder == sid


def test_task_holder_missing_registry_keeps_session_id(tmp_path, monkeypatch):
    from types import SimpleNamespace

    from fno.claims.self_identity import resolve_task_holder

    sid = "bbbb0000-0000-0000-0000-000000000000"
    reg = tmp_path / "absent.json"
    monkeypatch.setattr(
        "fno.paths.agents_registry_path", lambda: reg
    )
    monkeypatch.setattr(
        "fno.claims.self_identity.resolve_self_identity",
        lambda env, **_k: SimpleNamespace(
            session_id=sid, harness="claude", disposition="proven"
        ),
    )
    holder, why = resolve_task_holder({})
    assert (holder, why) == (sid, "")


# ---------------------------------------------------------------------------
# The sandbox write-policy layer (join.sandbox, default off)
# ---------------------------------------------------------------------------

# Two bands whose surfaces are disjoint: both can be narrowed. Wave 1 carries
# three declared-empty blockers so width is 3 and both bands get a joiner.
SANDBOX_PLAN = """---
title: sandboxed
status: ready
---

## Execution Strategy

```yaml
execution_mode: mixed
waves:
  - wave: 1
    mode: parallel
    difficulty: high
    tasks: ['1.1', '1.2', '1.3']
  - wave: 2
    mode: parallel
    difficulty: medium
    tasks: ['2.1']
tasks:
  - id: '1.1'
    title: a
    surface: ['src/high_a.py']
    blocked_by: []
  - id: '1.2'
    title: b
    surface: ['src/high_b.py']
    blocked_by: []
  - id: '1.3'
    title: c
    surface: ['src/high_c.py']
    blocked_by: []
  - id: '2.1'
    title: d
    surface: ['cli/src/fno/med_a.py']
    blocked_by: []
```
"""

SHARED_SURFACE_PLAN = SANDBOX_PLAN.replace("['cli/src/fno/med_a.py']", "['src/high_a.py']")












# ---------------------------------------------------------------------------
# The bare-join derivation: the sizing table, and a default that stopped
# being 1. A bare join on a wide plan used to ask for exactly one joiner,
# which is why nothing in production ever joined.
# ---------------------------------------------------------------------------


def _two_wave_plan(mode: str) -> str:
    return f"""---
title: t
status: ready
---

## Execution Strategy

```yaml
execution_mode: {mode}
waves:
  - wave: 1
    mode: {mode}
    tasks: ['1.1', '1.2']
  - wave: 2
    mode: {mode}
    tasks: ['2.1']
tasks:
  - id: '1.1'
    title: a
    surface: ['src/a.py']
  - id: '1.2'
    title: b
    surface: ['src/b.py']
  - id: '2.1'
    title: c
    surface: ['src/c.py']
```
"""


WIDE_PLAN = """---
title: t
status: ready
---

## Execution Strategy

```yaml
execution_mode: sequential
waves:
  - wave: 1
    mode: sequential
    tasks: ['1.1', '1.2', '1.3', '1.4', '1.5', '1.6', '1.7']
tasks:
  - id: '1.1'
    title: a
  - id: '1.2'
    title: b
  - id: '1.3'
    title: c
  - id: '1.4'
    title: d
  - id: '1.5'
    title: e
  - id: '1.6'
    title: f
  - id: '1.7'
    title: g
```
"""

NARROW_PLAN = """---
title: t
status: ready
---

## Execution Strategy

```yaml
execution_mode: sequential
waves:
  - wave: 1
    mode: sequential
    tasks: ['1.1']
tasks:
  - id: '1.1'
    title: a
```
"""


def _entry_priority(monkeypatch, tmp_path, priority):
    """Re-stub the graph read with a priority-carrying node entry."""
    entry = {
        "id": "x-8d1d",
        "slug": "joiner-2",
        "plan_path": str(tmp_path / "plan.md"),
        "priority": priority,
    }
    monkeypatch.setattr("fno.graph.api.wire_rows", lambda *_a, **_k: [entry])
















# ---------------------------------------------------------------------------
# The collision partition runs for EVERY wave (x-a804 task 5.1).
#
# It used to be gated on `mode == "parallel"`, so two tasks in one sequential
# wave editing the same file read as simultaneously ready. 396 of 430 waves in
# a fortnight are sequential, so the partition ran on roughly 8% of waves and
# the label named `sequential` scheduled MORE in parallel than the one named
# `parallel`. join could hand a joiner a task colliding with the holder's.
#
# Corpus re-measure over 532 plans carrying an Execution Strategy: 193 change,
# every one downward, zero raised. The partition can only narrow.
# ---------------------------------------------------------------------------

SEQUENTIAL_SAME_FILE_PLAN = """---
title: sequential same file
status: ready
---

## Execution Strategy

```yaml
execution_mode: sequential
waves:
  - wave: 1
    mode: sequential
    tasks: ['1.1', '1.2', '1.3']
tasks:
  - id: '1.1'
    title: a
    surface: ['src/one.py']
  - id: '1.2'
    title: b
    surface: ['src/one.py']
  - id: '1.3'
    title: c
    surface: ['src/one.py']
```
"""

SEQUENTIAL_DISJOINT_PLAN = SEQUENTIAL_SAME_FILE_PLAN.replace(
    """  - id: '1.2'
    title: b
    surface: ['src/one.py']
  - id: '1.3'
    title: c
    surface: ['src/one.py']""",
    """  - id: '1.2'
    title: b
    surface: ['src/two.py']
  - id: '1.3'
    title: c
    surface: ['src/three.py']""",
)
