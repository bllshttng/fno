# Single-Doc Plan Detection and Dispatch

Reference for how `/execute waves` reads plans. Architectural overview: `docs/architecture/lean-blueprint.md`.

A plan is always a single `.md` file (G1 blocks authoring new folder plans; G3 removed folder-plan *reading* entirely). There is no shape detection step anymore - callers pass a file path directly.

## `load_plan_strategy(plan_input: str)` API

`skills/execute/orchestrator.py`'s `load_plan_strategy` is the dispatcher `/execute waves` calls. It:

1. Runs `fno-agents plan-cli parse <plan>`, the hidden `fno do plan parse` leaf. The receipt carries the frontmatter, the sections, the parsed strategy, and its task-edge errors. `fno do plan brief` uses the same parser.
2. Validates the frontmatter through the plan-schema door. It refuses on a missing strategy section, a strategy parse error, or an edge error.
3. Builds and returns an `ExecutionStrategy` dataclass (`waves: List[Wave]`, `scope`, `project_tasks`).

`orchestrator.py` also has a lower-level `parse_execution_strategy(index_path: str)` - a self-contained, stdlib-only regex parser used directly by `main()`'s CLI entry point (`orchestrator.py <path-to-plan.md>`). It reads the same `## Execution Strategy` fenced YAML block from any single file. It does not go through the parse door.

## Failure surfaces (`load_plan_strategy`)

| Condition | Result | Detail |
|-----------|--------|--------|
| `fno-agents` binary not found | Returns `None` | Warning to stderr |
| Plan file missing / unreadable (door exit 1) | Returns `None` | `BLOCKED blocked_reason=plan_unreadable: <reason>` to stderr |
| Plan doc malformed (frontmatter parse failure, door exit 3) | Returns `None` | `Error: malformed plan doc (frontmatter): <reason>` to stderr |
| `## Execution Strategy` section absent | Returns `None` | `Warning: No execution strategy section found in <path>` to stderr |
| Execution Strategy YAML malformed (`strategy_error`) | Returns `None` | `Error: malformed Execution Strategy YAML in <path>: <reason>` to stderr |
| No valid waves parsed | Returns `None` | `Error: No valid waves found in Execution Strategy of <path>` |

Every failure mode returns `None` and prints a diagnostic to stderr - never raises past this function, never returns a partially-built `ExecutionStrategy`.

## Task-scoped file targets

`get_task_file_targets(plan_path, task_id)` extracts a single task's `Files:` section for parallel-wave conflict detection (`detect_hidden_output_conflicts`, `resolve_wave_execution_mode`). Since a plan is one file, this scans that file for the `### Task <task_id>` heading via `_extract_task_section` - no directory globbing.

## See also

- `crates/fno-agents/src/plan_doc/doc.rs` - `load_plan`, `PlanDocument`
- `crates/fno-agents/src/plan_doc/brief.rs` - `parse_execution_strategy` (the canonical Execution Strategy YAML parser)
- `crates/fno-agents/src/plan_doc/cli.rs` - the `fno do plan` door and its `parse` leaf
- `skills/execute/orchestrator.py` - `load_plan_strategy`, `parse_execution_strategy`, `get_task_file_targets`
- `skills/blueprint/references/single-doc-spec.md` - mutation contract and section ownership
