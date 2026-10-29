---
created: 2026-05-01T00:00
status: shipped
---

# Graph Collision Detection

## Overview

Graph collision detection catches the failure mode where two specs written
hours apart silently target the same files and surfaces, and neither plan
author realizes they are stepping on each other until merge time. The
mechanism is a deterministic file-overlap comparison that four surfaces
consume:

1. **`/blueprint`** runs a check between writing the plan and auto-intake.
   High-severity collisions trigger an `AskUserQuestion` flow with four
   resolutions (proceed / modify / supersede / cancel).
2. **`fno backlog triage health`** compares the pending backlog and reports
   collisions beside its other health metrics.
3. **`fno backlog collisions check`** is the direct CLI surface for
   checking a plan against the graph from a script or one-off prompt.
4. **Dispatch** (the native lane-fill gate behind `lane-fill` /
   `dispatch-lanes` and the active-backlog daemon) compares each candidate
   against every node a live worker already holds before filling a lane.

## Architecture

The collision decision is native. `crates/fno-agents/src/backlog/collision.rs`
owns severity scoring and the comparison; `cutover.rs collision_thresholds`
resolves the thresholds from the config candidates with the same fail-open
contract the settings model's sanitizer holds; `advance_fill.rs` turns a
collision into a fill verdict at dispatch time. The dual-inventory row for
this decision reads RETIRED: the Python engine that once mirrored it died
when its last callers (the graph CLI's collisions sub-app and the Python
triage health) went native.

What stays in Python is the plan-file parser and the task-grain partition,
both under `cli/src/fno/graph/collision.py`, because the plugin skills
(which sit outside cli/src) import them:

| Symbol | Consumer | Purpose |
|--------|----------|---------|
| `parse_files_to_modify(plan_path)` | `has_file_surface` | Extract normalized paths from a plan's file table. Recognized headings: `Files to Modify`, `Files to Change`, `Files Touched`, `File Ownership Map`, `Files`. Handles single-file quick plans and folder plans (every `*.md` under the folder, so 00-INDEX.md plus each NN-* phase file). |
| `has_file_surface(plan_path)` | skills/blueprint `mutate_doc.py` | Whether the plan states a surface at all. False is UNEVALUATED, not clean: a caller that must tell "compared, no overlap" from "nothing to compare" asks here first. |
| `match_shared_root(path, roots)` | `partition`, the orchestrator | The generated-output root a normalized path falls under. |
| `partition(items, shared_roots)` | skills/execute `orchestrator.py` | Union-find grouping of `(task id, paths)` pairs; overlap serializes, an empty path set is its own unevaluated verdict. |
| `HIDDEN_SHARED_OUTPUT_ROOTS` | both skills | The shared-output root tuple, canonical home. |

## Severity and actions

Thresholds live under `config.collision.severity_thresholds` in
`~/.fno/config.toml` (or project-local `.fno/config.toml`); the defaults
ride the pydantic `CollisionThresholdsBlock`, whose sanitizer drops
non-numeric and negative values so a bad threshold degrades to the default
instead of breaking the load. The `/setup` wizard exposes them under
"advanced" and defaults to skip; most users never tune them. Severity
bands and the recommended actions (`coordinate`, `absorb`, `supersede`)
are the native engine's contract; their wording is pinned by the
reachable-paths port-twin baseline rows.

## Empty surfaces: unevaluated is not clean

The gate is only as good as its input, and the input was once empty.

`parse_files_to_modify` originally matched `Files to Modify` and its synonyms but
not `File Ownership Map` - the one section `/blueprint` actually writes.
Every blueprint-generated plan therefore parsed to an empty surface and silently
could not collide with anything.
The heading is now recognized.

A plan with no file table at all is still possible, and there the comparison
has nothing to compare - the same outcome as "compared, no overlap".
Callers that need to tell those apart ask `has_file_surface` first:

- `fno backlog collisions check --json` emits
  `{"status": "ok" | "unevaluated", "collisions": [...]}`.
  A bare list was the earlier shape.
- `/blueprint` warns on stderr when the plan it just wrote states no file
  surface. It deliberately does NOT auto-populate the table: the point is to
  force the surface to be stated at planning time, not to manufacture one that
  makes the warning go away.

## Dispatch-time gate

`/blueprint` catches a collision between two plans written hours apart.
It cannot catch a collision between two plans that are already `ready` and get
dispatched to concurrent workers in the same round.
That happened: three nodes describing one root cause, filed by three authors in
three phrasings, all landing in the same file, all fired in parallel.

The native fill gate consults the collision engine before it fills a lane.
For each candidate it compares the plan's file surface against the surfaces of
every node a live worker holds, plus this round's own picks.
Both claim shapes count, because both mean somebody is editing those files: a
`lane-slot:` holder (a peer lane) and a bare `node:<id>` claim (a hand-run
`/target`, which holds no slot).
A comparator whose own plan states no surface is reported rather than counted,
so the gate never reads clean without having compared against it at all.
A high-severity overlap skips the candidate and logs the node it collides with
and the shared files.

Two properties are load-bearing:

- **The node stays `ready`, never parked.** Skipping is the reversible option:
  the node is retried next round once the colliding lane finishes.
- **The gate fails OPEN end to end**, seeding included.
  An error reading a plan surface, the claims directory, or the graph lets the dispatch proceed with a warning.
  A dedup check that can wedge the dispatcher is worse than the duplicate work it
  prevents.

## Audit trail

Graph fields the mechanism reads and writes, all missing-field tolerant:

| Field | Owner | Purpose |
|-------|-------|---------|
| `collisions_acknowledged: list[str]` | the update verb | ab-IDs of collisions the user knowingly proceeded past. The sentinel `__skipped_check__` records "I ran with `--no-collision-check`." |
| `superseded_by: str \| None` | the supersede verb | The new node ID that replaced this one. |
| `supersedes: list[str]` | the supersede verb | The old node IDs this node replaces. |

Acknowledged collisions whose referenced nodes have since shipped surface
as a "your acknowledged collision is now resolved - verify the conflict
resolved cleanly" line in triage health, so a deliberately-accepted
conflict gets a closing-the-loop nudge.

## Status derivation

`statuses.py` adds a `superseded` derived state. Precedence ladder:

```
done > superseded > deferred > blocked > claimed > idea > ready
```

A superseded node never re-surfaces as ready or in `fno backlog next`.
Reactivation requires explicit `unsupersede` (not just `undefer`) because
the user must consciously revive a plan that another plan has already
supplanted. The supersede verb refuses to mutate already-shipped or
already-superseded nodes so the operation is never a destructive write
against ship history; the user opens a follow-up node instead.

## Out of scope (follow-ups)

- **Concept-overlap detection** without shared files (LLM-judged or via
  `topic`/`epic` tags) - separate design loop.
- **Auto-resolution** of low-severity collisions (auto-rebase the
  candidate against the existing plan) - opt-in via a future flag.
- **Cross-project collisions** - today's mechanism is project-scoped
  because the graph is project-scoped.
- **Kanban edge rendering** - showing "these two cards have a connecting edge" in `~/.fno/cache/pages/graph.md` requires extending the renderer.
- **Time-bounded collision tolerance** - "plans more than 60 days old
  don't count" risks papering over real conflicts; skipped.

## References

- Native engine: `crates/fno-agents/src/backlog/collision.rs`
- Python helpers: [cli/src/fno/graph/collision.py](../../cli/src/fno/graph/collision.py)
- Tests: [cli/tests/integration/test_collision.py](../../cli/tests/integration/test_collision.py)
- Dual inventory: [docs/architecture/dual-implementation-inventory.md](dual-implementation-inventory.md) (the file-collision row)
- Spec wiring: [skills/blueprint/SKILL.md](../../skills/blueprint/SKILL.md) (Quick step 3a, Full step 11a)
