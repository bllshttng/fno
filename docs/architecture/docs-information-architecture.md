# Docs information architecture: a journey map and a slot for every page

## Is this page for you?

If you decide where a doc goes, or you review the plan to reorganize `docs/`, read this page. It holds the reader journey, the target tree, and the move plan. The row-by-row verdicts live in [docs-map.tsv](../docs-map.tsv).

Not for: a user who wants to install or run fno. Start at the root [README.md](../../README.md) instead.

Status: approved as written. Wave 1 landed: the six entry pages exist, `getting-started.md` is split, and `deployment-guide.md`, `guides/execution-modes.md` and `guides/utilities.md` are merged. `docs/README.md` is still hand-kept, not generated. Waves 2 and 3 are open.

## The problem, measured

On 2026-10-19, `git ls-files 'docs/*.md'` counted 267 markdown files and 4 MB of text. Of those, 166 sit under `docs/architecture/`. An earlier measurement counted 161 files on 2026-08-19, so the tree grew by 106 pages in two months.

A reader has three indexes and none of them is complete. The root `README.md` lists 9 doc links under `## Docs`. `docs/README.md` links 42 pages. The `AGENTS.md` `## Deep-dive docs` list is for agents, and `scripts/ci/check-doc-entry-scope.sh` gates it. No page states what kind of page it is. 31 pages name their reader in a `## Is this page for you?` section. 236 do not.

## Method

The journey stages come from the `pm-market-research:customer-journey-map` skill. Its seven stages (Awareness to Advocacy) are folded into the six this work asked for. The persona is a developer who already uses Claude Code and wants agents to ship reviewed PRs. The skill's emotion column is dropped: no source here supports a claim about how a reader feels.

The doc-writing skill named for this work, `fno-doc-writing`, is not installed on this machine (`mdfind -name fno-doc-writing` returned nothing). The style comes from [style-rules.md](../style-rules.md) and `skills/ship-docs/` instead.

The shape follows the Claude Code docs. Their index, `https://code.claude.com/docs/llms.txt`, read on 2026-10-19, has these top-level groups: `Getting started`, `Build with Claude Code`, `Plugins`, `Administration`, `Configuration`, `Reference`, `Agent SDK`, `What's New`, `Resources`. `Getting started` splits into `Overview`, `Quickstart`, `Core concepts`, `Use Claude Code`, `Platforms and integrations`, and `Code review and pull requests`. Three habits carry over: one short entry page per stage, task guides apart from reference tables, and concept pages only where a model needs explaining.

Each page was classified from its first lines and its `##` headings, never from the whole 4 MB.

## Journey map

### Stage 1: Discover

The reader is a Claude Code user who heard of fno. The question: what does it do, and is it safe to point at my repo?

Touchpoints: the root `README.md` (4,976 bytes) and its `## How it works` section at line 65, and the footnote.sh website.

Docs that answer today: `README.md`, `docs/security-posture.md`, `docs/project-overview-pdr.md`, `docs/system-architecture.md`.

Pain points: the only overview inside `docs/` is `project-overview-pdr.md`. It is a product design review, last changed on 2026-06-18, and it says 26 skills. `system-architecture.md` says 24 skills at line 48 and 26 skills at line 587, and 12 agents at line 57 and 17 at line 595. `skills/` holds 27 skill folders and `agents/` holds 18 agent files.

Entry page: `docs/overview.md`. It draws from `README.md`, `docs/security-posture.md` and the product parts of `docs/project-overview-pdr.md`.

### Stage 2: Install

The reader decided to try it. The question: how do I install it on my harness, and how do I know it worked?

Touchpoints: `README.md:13` `## Install`, the `/fno:setup` wizard, `fno config setup run`, `fno doctor`.

Docs that answer today: `docs/getting-started.md` (its `## Install` at line 40), `docs/deployment-guide.md`, `docs/auth.md`, `docs/HARNESSES.md`, `docs/harnesses/codex.md`, `docs/guide/opencode.md`, `docs/SETUP-HERMES.md`, `docs/SETUP-OPENCLAW.md`.

Pain points: install steps live in three places (`README.md:13`, `getting-started.md:40`, and `deployment-guide.md`). The opencode guide is the only file in `docs/guide/`, while the codex guide sits in `docs/harnesses/`.

Entry page: `docs/install.md`. It merges the three install texts and links each harness page.

### Stage 3: First target

The reader has fno installed. The question: how do I ship my first PR with it, and how do I know it is done?

Touchpoints: `/fno:target`, the stop-hook block message, `fno do pr status`.

Docs that answer today: `docs/getting-started.md` (its `## Your first feature` at line 184), `docs/guides/target.md`, `docs/guides/how-done-is-decided.md`, `docs/troubleshooting.md`, `docs/best-practices.md`.

Pain points: the first run starts 184 lines into a 17 KB page that also covers install and daily commands. `docs/README.md` sends a reader to the same page for both install and first run.

Entry page: `docs/quickstart.md`. It takes the first-feature half of `getting-started.md`.

### Stage 4: Daily driver

The reader uses fno every day on one project. The question: which verb do I run for this task, and which key changes that behavior?

Touchpoints: `/fno:think`, `/fno:blueprint`, `/fno:review`, `/fno:ship`, `/fno:fix`, `fno backlog`, `fno config`.

Docs that answer today: `docs/backlog-usage.md`, `docs/guides/pr-lifecycle.md`, `docs/guides/review.md`, `docs/guides/fix.md`, `docs/guides/think-and-plan.md`, `docs/configuration-guide.md`, `docs/path-config.md`, `docs/env-vars.md`.

Pain points: `docs/guides/execution-modes.md` names "do" and "operator" modes, and `skills/` has neither. `docs/guides/utilities.md` names a debug skill, and `skills/` has `fix`, not `debug`. `docs/README.md` links `architecture/megawalk-pipeline.md` as "the multi-feature loop", while `docs/guides/megawalk-walker.md` says that walker is deleted. `docs/api-reference.md` says 24 skills and 17 agents. Two pages a daily user opens often are long: `configuration-guide.md` is 76 KB and `backlog-usage.md` is 47 KB.

Entry page: `docs/workflows.md`. It lists the task, the verb, and the guide, in the style of the Claude Code "Common workflows" page.

### Stage 5: Fleet

The reader runs several agents at once, maybe on several harnesses. The question: how do I spawn, watch, route, and recover workers?

Touchpoints: `fno agents spawn`, `fno mux`, `/fno:lead`, `fno agents fleet`, `fno config accounts`, refusal lines from the spawn gate.

Docs that answer today: `docs/guides/agents-quickstart.md`, five `docs/guides/fno-agents-*.md` guides, five `docs/harnesses/launch/*.md` pages, `docs/fleet-faq.md`, `docs/reaping-faq.md`, `docs/provider-rotation.md`, `docs/harness-command-matrix.md`, and 19 user-facing pages under `docs/architecture/`.

Pain points: `docs/README.md` has no fleet section. Those 19 pages sit in the same folder as 138 contributor notes, so a user cannot tell them apart. The fleet references are large: `provider-rotation.md` is 106 KB, `reaping-faq.md` 56 KB, `fleet-faq.md` 52 KB, `harness-command-matrix.md` 48 KB.

Entry page: `docs/fleet.md`. It starts from `docs/guides/agents-quickstart.md` and links the launch pages, the concepts, and the two FAQs.

### Stage 6: Contribute

The reader changes fno itself, as a person or as an agent. The question: how is this built, which rules and gates apply, and where does my new doc go?

Touchpoints: `CONTRIBUTING.md`, `AGENTS.md:183` `## Deep-dive docs`, `fno lint style`, the CI guards.

Docs that answer today: `docs/style-rules.md`, `docs/code-standards.md`, `docs/testing-guide.md`, `docs/state-root-inventory.md`, 166 pages under `docs/architecture/`, 15 under `docs/test-audit/`.

Pain points: `docs/README.md` links 6 of the 166 architecture pages. `CONTRIBUTING.md:91` links `docs/architecture/skill-encapsulation.md`, which does not exist. No rule says where a new page goes, so each one lands where its author guessed.

Entry page: `docs/contributing/README.md`. It links `CONTRIBUTING.md`, the house rules, and the architecture index, and it states the slot rule below.

## Information architecture

### Target tree

```
docs/
  README.md            docs home, generated from docs-map.tsv
  overview.md          Stage 1 entry
  install.md           Stage 2 entry
  quickstart.md        Stage 3 entry
  workflows.md         Stage 4 entry
  fleet.md             Stage 5 entry
  guides/              task guides; the map tags each one with a stage
  concepts/            pages that explain a model: done, axes, vocabulary, trust
  reference/           CLI, config keys, env vars, paths, harness matrices, schemas
  harnesses/           one install guide per harness, plus launch/
  protocol/            wire contracts for outside integrators
  contributing/        Stage 6 entry and the contributor rules
  architecture/        design notes, one per subsystem (stays in place)
  test-audit/          test-cut campaign ledgers (stays in place)
```

### Page kinds

- entry: one per stage. A short page that answers the stage question and links onward.
- guide: one task, in steps, with the verb to run.
- reference: a lookup table or schema. Often generated.
- concept: explains a model a user must hold to use a feature well.
- contributor: design notes, rules and gates for people who change fno.
- internal: a dated audit, trial, or working list. Kept for history, off every nav.

### One source of navigation

`docs/docs-map.tsv` is the single navigation source. A later change generates `docs/README.md` from it, and the website nav reads it too. The root `README.md` then keeps only its quickstart and one link to the docs home. The `AGENTS.md` deep-dive list stays agent-facing and off the reader path.

The map, not the folder, puts a page on a nav. 18 user-facing pages under `docs/architecture/` keep their path, because code cites some of them (`thread-lanes.md` has 12 citing files). Their `stage` and `kind` columns put them on the user nav. When a folder misleads a reader and the move is cheap, the page moves. Otherwise it stays.

### The slot rule for a new page

A new page gets a row in `docs/docs-map.tsv` in the same change. Its kind picks its folder. Its stage picks its nav group. A page that fits no stage gets stage `none`, kind `internal`, and an `unplaced:` reason, and it waits for review. It is never guessed into a slot.

## Migration

### Counts

Measured from the map with `awk -F'\t' '!/^#/{print $4}' docs/docs-map.tsv | sort | uniq -c`:

| action | rows |
|---|---|
| keep | 203 |
| move | 47 |
| merge | 10 |
| retire | 6 |
| split | 2 |

By stage, with `awk -F'\t' '!/^#/{print $2}' docs/docs-map.tsv | sort | uniq -c`: contribute 168, fleet 45, daily-driver 32, install 11, first-target 5, none 5, discover 2. Six entry pages are new and have no row yet.

### Waves, in order

1. Entry pages. Write the six entry pages, split `getting-started.md`, and merge `deployment-guide.md` and the install half of `getting-started.md` into `install.md`. Generate `docs/README.md` from the map. This wave changes the reader path and moves the fewest cited files.
2. Reference and concepts. Create `reference/` and `concepts/`, move the reference and concept rows, and split `provider-rotation.md`. Generated pages (`env-vars.md`, `loops.md`) move with their generator in the same diff.
3. Contributor relocation. Move the root contributor pages into `contributing/`, then merge and retire the contributor rows.

### Citing files the move must edit

38 non-keep rows are named by files outside `docs/`, 101 file references in all. The move edits each citing file in the same diff as the move. The largest:

| path | action | citing files |
|---|---|---|
| `docs/HARNESSES.md` | move | 10 |
| `docs/style-rules.md` | move | 7 |
| `docs/path-config.md` | move | 6 |
| `docs/graph-search.md` | move | 5 |
| `docs/code-index-providers.md` | move | 5 |
| `docs/preflight.md` | move | 4 |
| `docs/hooks-fail-open.md` | move | 4 |
| `docs/harness-command-matrix.md` | move | 4 |
| `docs/configuration-guide.md` | move | 4 |
| `docs/architecture/lane-qualification.md` | move | 4 |

The full list: `awk -F'\t' '!/^#/ && $4!="keep" && $6>0' docs/docs-map.tsv`.

Inside `docs/`, pages carry 357 links that end in `.md` and 103 links that start with `../`. The 2026-08-19 count was 201 and 61. Every moved page needs its own links rewritten, and so does every page that links to it.

### Dead links found

Code and skills cite 197 distinct `docs/*.md` paths. 56 do not exist. Most are test fixtures (`docs/a.md`, `docs/PLAN.md`), examples in templates (`docs/architecture/chat-ux.md`), or paths in other tools' doc trees. `docs/PRODUCT.md` is an optional file that `scripts/check-product-md.sh` probes for, not a link. Two are real dead links:

- `CONTRIBUTING.md:91` links `docs/architecture/skill-encapsulation.md`.
- `skills/target/references/pre-promise.md:279` names `docs/triage.md`. The page is `docs/backlog-triage.md`.

The move PR fixes both.

## Review

These choices belong to the user. Nothing moves until they are made.

1. The six stage names: Discover, Install, First target, Daily driver, Fleet, Contribute. They follow the original request. Rename any of them now, because the entry page paths follow from them.
2. The retire list. The move deletes these six pages:
   - `docs/changelog.md`: says last updated 2026-03-31.
   - `docs/codebase-summary.md`: a hand-kept inventory that says 26 skills.
   - `docs/guides/memory-auto-distillation.md`: marks itself historical.
   - `docs/architecture/megawalk-pipeline.md`: design of a deleted walker.
   - `docs/architecture/megaspec.md`: no megaspec skill exists.
   - The "Bounded-Iteration Pattern Incorporation" page under `docs/architecture/`: it describes two skills that `skills/` does not hold.
3. Two unplaced pages need an owner's call:
   - `docs/backlog-current-state.md`: mixes node-field rules for users with a migration runbook for maintainers.
   - `docs/comment-prose-reading-list.md`: a ranked function list with no named reader and no generator found.
4. Whether architecture notes appear on the public website at all. This design keeps them off the user nav. The exception is 18 pages there that the map tags with a user stage.
5. Whether `docs/state-root-inventory.md` stays at the docs root. It has 19 citing files, so the map keeps it in place.
