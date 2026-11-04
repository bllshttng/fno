# Single-Doc Plan Detection and Dispatch

Reference for how `/execute waves` reads plans. Architectural overview: `docs/architecture/lean-blueprint.md`.

A plan is always a single `.md` file (G1 blocks authoring new folder plans; G3 removed folder-plan *reading* entirely). There is no shape detection step anymore - callers pass a file path directly.

## `fno-agents wave ready <plan>`

`/execute waves` asks `fno-agents wave ready <plan> [--state <STATE.md>] [--node <id>] [--band low|medium|high]` for the tasks it may dispatch. The loader (`load` in `crates/fno-agents/src/wave_ready.rs`):

1. Reads the plan through `plan_doc::doc::load_plan`, the same parser the `fno do plan parse` door uses. `fno do plan brief` uses the same parser.
2. Validates the frontmatter through the plan-schema door. It refuses on a missing strategy section, a strategy parse error, or an edge error.
3. Builds the waves, their tasks and each task's band, then adds the partition edges for tasks that share a file.

## Failure surfaces

Every failure prints one diagnostic to stderr, prints `No execution strategy found in <path>` to stdout, and exits 1. It never prints a partial ready list.

| Condition | Detail on stderr |
|-----------|--------|
| Plan file missing / unreadable | `BLOCKED blocked_reason=plan_unreadable: <reason>` |
| Plan doc malformed (frontmatter parse failure) | `Error: malformed plan doc (frontmatter): <reason>` |
| Frontmatter fails the plan-schema door | `BLOCKED blocked_reason=plan_frontmatter_invalid: <path>` and one line per field |
| `## Execution Strategy` section absent | `Warning: No execution strategy section found in <path>` |
| Execution Strategy YAML malformed | `Error: malformed Execution Strategy YAML in <path>: <reason>` |
| A task edge names an unknown task or makes a cycle | `BLOCKED blocked_reason=plan_task_edges_invalid: <path>` and the edge errors |
| No valid waves parsed | `Error: No valid waves found in Execution Strategy of <path>` |

## Task-scoped file targets

The partition step reads each task's `Files:` section from the `### Task <id>` heading in the one plan file, with no directory globbing. Tasks in a parallel wave that share a path serialize in wave order (`crates/fno-agents/src/backlog/collision.rs`).

## See also

- `crates/fno-agents/src/plan_doc/doc.rs` - `load_plan`, `PlanDocument`
- `crates/fno-agents/src/plan_doc/brief.rs` - `parse_execution_strategy` (the canonical Execution Strategy YAML parser)
- `crates/fno-agents/src/plan_doc/cli.rs` - the `fno do plan` door and its `parse` leaf
- `crates/fno-agents/src/wave_ready.rs` - `load`, the ready payload, the partition edges
- `skills/blueprint/references/single-doc-spec.md` - mutation contract and section ownership
