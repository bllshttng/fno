# Toolbag

The fno skills ship together as one plugin. A target run reaches for the skill the moment calls for. A reach is a call by name. It is never a copy of the skill's text into target.

When a moment in the table below arrives, load this file. The CLI-versus-skill split is in `skills/using-fno/SKILL.md`.

## How to reach a skill mid-run

| Harness | User types | Mid-run reach by the agent |
|---|---|---|
| claude | `/fno:<name>` | The `Skill` tool with `skill: "fno:<name>"`. A skill that also ships as an agent (design-critic) can run through the `Agent` tool with `subagent_type: "fno:<name>"`. |
| opencode | `/fno:<name>` | The native `skill` tool with `skill({ name: "<name>" })`. The install writes the skills under their bare names, for example `fix`. |
| codex | `$fno:<name>` | Open the skill's `SKILL.md` from the skills list and follow it. Single-quote `$fno:` on a shell line. A mid-run reach on codex is unmeasured. |
| pi, agy | `/skill:<name>`, `/<name>` | Unmeasured. |

Never reach a skill by a relative path such as `../fix/SKILL.md`. On a codex development install the skill folders are renamed `plugin--fno--<name>`, so the path names a folder that does not exist.

A pack install drops five skills: agent, mail, dnd, fno-me and lead. Their rows say so.

## The skills

| Skill | Reach for it when | It returns |
|---|---|---|
| `fno:fix` | A test or build is red, before or after the PR. Use mode `investigate` when the cause is unknown. | A green suite and a named root cause, or a failing repro and hypotheses. |
| `fno:tdd` | Any behavior change, before the production edit. | A test seen failing, then passing. |
| `fno:test-audit` | Before you add or change a test, and when the test delta nears its cap. | Four answers per test, or a prune list. |
| `fno:think` | A question that one measurement cannot settle, or a refuted premise that needs research. | One cited findings document. |
| `fno:verify-footnotes` | Before the first edit (premises), before a figure reaches a reader (claims), and before a done claim. | A verdict with file and line for each premise or claim. |
| `fno:design-critic` | A diff touches `merge.visual_paint_paths`, after the shots and `fno-agents visual critique-prep`, before the look. | Findings in `<node>-critique.json`. |
| `fno:blueprint` | There is no plan, or a ruling changed the plan (amend). | A plan path and a node. |
| `fno:execute` | The plan declares waves. | Executed tasks and `SUMMARY.md`. |
| `fno:review` | Before the PR, on the final head. | Findings and the head-pinned attestation. |
| `fno:ship` | PR create, check, and merged. | The PR URL and the review state. |
| `fno:ship-docs` | User-facing behavior changed. | The doc pages. |
| `fno:speculate` | Two or more honest routes that only a side-by-side can choose between. | N variant worktrees. |
| `fno:triage` | Several ready nodes block on order. | A proposed order for approval. |
| `fno:audit` | Before you call a feature complete across a surface. | A gap report and follow-up nodes. |
| `fno:intel` | A stall or a postmortem asks where sessions needed correction. | The provenance report. |
| `fno:law` | The user states a durable rule mid-run. | The recorded law id. |
| `fno:harness-commands` | Before you use a harness-native slash command. | The roster with risk classes. |
| `fno:setup` | `fno` is missing, or `fno config doctor` fails. | The setup report. |
| `fno:using-fno` | You are unsure whether a CLI primitive or a skill fits. | The surface table. |
| `fno:agent` | A second worker is needed (spawn, handoff, peek). Full install only. | A spawn receipt. |
| `fno:mail` | The lead or a peer must hear something. Full install only. | A message id. |
| `fno:growth-launch`, `fno:groom`, `fno:lead`, `fno:dnd`, `fno:fno-me`, `fno:cache-keepalive`, `fno:harness-verbs` | Never from a target run. Each owns its own session or loop. `fno:harness-verbs` only redirects to `fno:harness-commands`. | Nothing. | <!-- verb-wording: allow -->

When a skill folder has no row here, `scripts/lint/check-skill-reach.sh` fails.
