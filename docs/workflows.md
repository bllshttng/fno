# Workflows: which command for which task

## Is this page for you?

You use fno every day on one project. This page maps each task to the command you run and the guide that covers it. Each command is a skill (`/fno:<command>` in Claude Code, `$fno:<command>` in Codex) or a CLI command (`fno <command>` in any terminal).

Not for: a first run. That is the [quickstart](quickstart.md). For several agents at once, go to [fleet](fleet.md).

## How to choose

| Task | Run | Guide |
|---|---|---|
| Ship a feature from an idea | `/fno:target "the idea"` | [target](guides/target.md) |
| Ship a plan you already have | `/fno:target path/to/plan.md` | [target](guides/target.md) |
| Ship a backlog node | `/fno:target <node-id>` | [backlog](backlog-usage.md) |
| Explore a design before building | `/fno:think "X"` | [think and plan](guides/think-and-plan.md) |
| Turn a design into a plan | `/fno:blueprint "X"` | [think and plan](guides/think-and-plan.md) |
| Run a plan with no review and no PR | `/fno:execute path/to/plan.md` | [think and plan](guides/think-and-plan.md) |
| Review a diff before you push | `/fno:review` | [review](guides/review.md) |
| Open, check and close out a PR by hand | `/fno:ship pr create`, `check`, `merged` | [PR lifecycle](guides/pr-lifecycle.md) |
| Fix a red build or failing tests | `/fno:fix` | [fix](guides/fix.md) |
| Find the cause of a bug | `/fno:fix investigate "the bug"` | [fix](guides/fix.md) |
| Work a whole board | `/fno:target bg --all-ready` | [fleet](fleet.md) |

## Design and build

| Command | What it's for |
|---------|---------------|
| `/fno:think "X"` | Explore a design before building. Routes: default (design + acceptance criteria), `what-if` (stress-test failure modes), `panel` (multi-persona debate). |
| `/fno:blueprint "X"` | Turn an approved design into an executable plan with waves and tasks. |
| `/fno:target "X"` | The flagship loop: think to plan to code to review to a merge-ready PR. Point it at a feature, a plan path, or a backlog node id. |
| `/fno:execute` | Execute a plan: `flat` (default) or `waves`. No review, no PR. Target uses it for its build phase. |

Size profiles set how much ceremony target runs. `S` is build and PR only. `M` (the default) adds docs and external review. `L` adds adversarial review, browser checks and a clean pass. Example: `/fno:target L "rebuild billing"`.

## Review and ship the PR

The PR has a lifecycle, and there is a command for each step. Full walkthrough: [the PR lifecycle guide](guides/pr-lifecycle.md).

| Command | What it's for |
|---------|---------------|
| `/fno:review` | Review a diff before you push. Default is the owned lane: one inline reviewer that emits a head-pinned attestation; `peer` gets a cross-model second opinion (e.g. have Codex review Claude's code). |
| `/fno:ship pr create` | Open a PR from your commits inline in the invoking session. |
| `/fno:ship pr check` | Poll for the external review bot, then implement its feedback. |
| `/fno:ship pr merged` | The post-merge ritual: reconcile the backlog, run the retro, and file any follow-up work. Run it after a PR merges. |

`target` runs review, `pr create`, and (by default) `pr check` for you. To drive by hand or pick up a PR mid-flight, reach for the individual commands.

## Fix and investigate

`/fno:fix` repairs a red build one fix at a time. Each fix is committed before verification, so a fix that makes things worse reverts. `/fno:fix investigate "the bug"` runs a hypothesis loop first: reproduce, form a falsifiable hypothesis, test it, log the result. The [fix guide](guides/fix.md) compares the two.

## Keep going past one feature

The backlog is the queue. Capture work, see what is ready, ship it:

```bash
fno backlog idea "add webhook retries"   # capture work as a backlog node
fno backlog next                          # what's ready to ship now
/fno:target <node-id>                     # ship a ready node end to end
```

To work a whole board instead of one node: `/fno:target bg --all-ready` dispatches every ready, non-deferred node as background workers. `fno backlog advance` (opt-in, merge-triggered) dispatches a node's dependents after its PR merges. The backlog is optional. `/fno:target "feature"` runs end to end with no backlog required. The [backlog guide](backlog-usage.md) covers the rest.

## Utilities

`fno doctor codemap` writes an AST and PageRank map of the most important symbols in the repo. Blueprint and target read it for structural context. `--db-schema` appends the database schema, read-only.

`fno agents workspace worktree status` lists worktrees with their live session. `fno agents workspace worktree cleanup --merged` removes merged ones. [Worktrees](guides/worktrees.md) has the full lifecycle.

## Change behavior with config

Every key and its default: [configuration guide](configuration-guide.md). Paths: [path config](path-config.md). Environment variables: [env vars](env-vars.md).
