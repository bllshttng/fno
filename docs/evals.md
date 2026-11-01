# Eval task bank (`fno doctor evals`)

A live-execution eval harness: give the pipeline a known task, run it, grade the result mechanically, and score how reliably it passes. Distinct from the observer (offline corpus replay) and skill-diff eval-after-merge (prompt-diff scoring) - this is live task execution with a pass/fail verdict.

## The two disciplines

**Capability vs regression tiers.** Every bank task declares `tier: capability | regression`.

- `capability` - a hill to climb: a hard task the pipeline currently fails. Failures are informational, not alarms.
- `regression` - must stay ~100%: a task that used to pass and must keep passing (e.g. the CLI starts, a known-flaky suite is green). Any regression-tier task below 100% fires the **regression alarm**.

A capability task that passes its last N consecutive full runs (default 3) becomes graduation-eligible. `fno doctor evals graduate <id>` retags its YAML to `regression`. Graduation is a reviewed edit: the command rewrites the file, and a human ships the PR. It is never a silent runtime flip.

**pass^k reliability.** `--repeat K` runs a task K times. The report shows `pass@1` (single-run success rate) and `pass^k` (every run passed) per task, plus a flake list (tasks that passed sometimes but not always). This turns "we re-run CI and it clears" folklore into a graded flake rate - the CI-flake regression tasks (e.g. the `loop_check` suite) are the first targets.

## Bank format

One file per task under `evals/bank/*.yaml`:

```yaml
id: capability-add-hello-command
tier: capability            # capability | regression
prompt: |                   # optional: omit for a grade-only task (no worker step)
  <the task the worker is asked to perform>
repo_fixture: HEAD          # git ref or fixture dir (default HEAD)
grade:                      # >=1 mechanical check; a gradeless task is rejected at load
  - kind: exit              # a shell command's exit code must equal `expect` (default 0)
    command: "cd cli && uv run fno-py hello"
  - kind: grep              # a workdir-relative file must contain a substring
    path: "cli/src/fno/cli.py"
    pattern: "hello"
  - kind: file-exists       # a workdir-relative path must exist
    path: "out/report.md"
  - kind: tool-budget       # the worker's claude transcript must hold at most N tool calls
    max_tool_calls: 120     # a missing transcript (codex, opencode, or a lane account with its own CLAUDE_CONFIG_DIR) fails as unmeasured
timeout_minutes: 20
tags: [cli, greenfield]
```

Success criteria must be **mechanical** (develop-tests discipline): a task with no runnable `grade` is invalid at load time, and an all-trivial grade (`command: true`) warns. A **grade-only** task (no `prompt`) skips the worker step and grades the fixture directly - the honest model for a CI-flake regression task where there is no agent work, only a flake to measure.

## Commands

| Command | What it does |
|---|---|
| `fno doctor evals run [--task ID[,ID]] [--tier T] [--tag TAG] [--repeat K] [--workers N] [--provider P] [--variant NAME] [--ref REF] [--lane NAME] [--cohort ID] [--overlay-fixtures REF]` | Run bank tasks in disposable worktrees, grade mechanically, append one history line per task-run. Confirms above 20 total runs (`--yes` skips). `--variant v1 --ref REF` scores a change (see Variants). `--lane NAME --cohort ID` qualifies a model lane (see Lanes). `--workers N` runs N tasks at once. |
| `fno doctor evals report [--since N] [--graduate] [--json]` | Fold history: per-tier pass rates, pass@1, pass^k, flake list, regression alarm (exit 4 on alarm). `--graduate` lists eligible capability tasks. |
| `fno doctor evals report --compare A --to B [--repeats N] [--json]` | Compare two runs, task by task. See Variants. |
| `fno doctor evals report --by-failure-type [--repeats N] [--json]` | Rank the named failure types by cost, then by failing goldens. See Failure types. |
| `fno doctor evals report --check-metrics [--json]` | Run each golden's grader on its labelled cases. Exit 1 when a type is not proved. No model spend. See Failure types. |
| `fno-agents evals candidates [--limit N] [--source KIND] [--json]` | Rank real failures to label as goldens. See Goldens. Read-only. |
| `fno-agents evals drafts` | Run the correction-to-draft pass now and print new, open and expired counts. The daemon runs it daily. See Goldens. |
| `fno-agents intel --plans-ladder [--teacher-url URL] [--hosted NAME=COMMAND]` | Score the fixed labelled set rung by rung and stop at the first pass. See Plan premise labels. |
| `fno-agents intel --plans [--limit N] [--dry-run]` | Label plan premises with the winning rung, or refuse and print each score. See Plan premise labels. |
| `fno doctor evals report --climb --baseline C --trial C [--cohorts-yaml P] [--stage V --trial-lane L] [--prices JSON] [--json]` | The climb verdict over two cohorts against the declared split: `underpowered`, `regression`, `overfit`, `unmeasured_cost`, `propose`, `hold`. On `propose` with `--stage`/`--trial-lane` it prints a one-step accept line (`fno config set` for the lanes list plus `on_exhausted degrade`); without them the verdict alone. See the Climb round recipe in [lane-qualification](architecture/lane-qualification.md#climb-round). |
| `fno doctor evals trend` | Score the recent window against the prior one, per task (exit 4 when a regression-tier task dropped). See Trend. |
| `fno doctor evals macro [--since 30d] [--topic TYPE:LABEL] [--window 20] [--all] [--json] [--events PATH]` | Fold existing event journals into a recurring failure-pattern leaderboard, or drill into one pattern. |
| `fno-agents evals gate-retry <sha>` | Queue a head again for the change gate. See The change gate. |
| `fno doctor evals graduate <id>` | Retag a capability task's YAML to regression. |
| `fno doctor evals grade --brief B --golden G` | Grade a research brief against a golden doc (three mechanical assertions); exit 0 green. |

Each run executes the task in a disposable worktree via the one-shot spawn lane (`fno agents spawn --once`, never bare `claude -p`). The spawn cap and provider rotation stay in play. The run removes the worktree after grading. A bank task never runs in your working copy. History appends to `~/.fno/history/evals-history.jsonl` (override via `config.paths.evals_history`).

## Macro eval

Macro eval reads the existing event journals offline, including retained rotations, so it uses no model and has zero per-run API cost. Use `fno doctor evals macro --since 90d` to find patterns across the evidence fno already produced, or pass `--events PATH` to inspect a fixture or selected journal.

A pattern is a labelled event: `TYPE:LABEL`, where the label is the first present value from `reason`, `outcome`, `verdict`, or `termination_reason`. Healthy labels and routine noise are omitted by default. The command groups existing labels instead of clustering free text because the events already carry structured topics. `--all` includes the omitted rows.

The suspect column shows patterns that commonly appeared in the preceding event window before a failure. Lift compares that co-occurrence with the suspect's overall event frequency, so a high value identifies an upstream lead for drilldown, not proof of root cause. Use `--topic TYPE:LABEL` to print recent sessions and their preceding event chains.

## Variants

The bank scores whether the pipeline passes. The variant axis scores whether a CHANGE improved it, which is the question anyone tuning a skill prompt or a config default actually has.

A variant is a **git ref of this repository**. Every prompt, skill, and project config fno controls is a tracked file. So "the pipeline with change Y" is "this repo at ref Y". `fno doctor evals run --variant v1 --ref my-branch` checks the disposable worktree out at `my-branch` instead of the fixture ref. The diff lives in git. The history row stores the resolved sha in `bank_rev`. Run `git diff <baseline_rev> <variant_rev>` to see the change a score move belongs to. Global config under `~/.fno` is not part of a variant. A config experiment commits to the ref or it is not a variant.

- **Names.** A variant is named exactly `baseline` or `v<N>` (`v1`, `v2`, ...). Anything else is refused before the run starts, on both the runner and the CLI. `--ref` without a non-baseline `--variant`, or the reverse, is also refused. Each history row carries a `variant` column naming its round.
- **Every variant runs every case.** Comparability needs the shared denominator. A variant that skipped a task reads `error` in the compare view. It cannot silently shrink the scored set.
- **The default report is baseline-only.** A failing `v1` never drags the baseline pass rate down. It never fires the regression alarm in doctor and triage health. Rows written before the variant axis carry no `variant` key and read as baseline.
- **Compare.** `fno doctor evals report --compare baseline --to v1` compares two sides. A side names a cohort, else a variant. A name that no row carries refuses with exit 2. Each task gets one status per side from its newest `--repeats` grades (default 3). The statuses are `always_pass`, `always_fail` and `flaky`. With fewer grades than repeats, a task reads `error`. Only a scored grade counts, so a plumbing row is never a pass. The report prints Fixed, Regressions (always pass to always fail), Now flaky (always pass to flaky), Still failing, and Errors. Counts and the pass rate come last, because a rate hides which behaviour moved. It exits 1 on a regression, or on an error for a task that had grades on side A. A move to flaky exits 0 and is listed. `--json` emits the same fold. Each side's line names the revs it read, so `git diff` of those revs shows the change.
- **Flake sort.** Each attempt records `tree_hash`, the git tree of what the worker left. The runner reads it before grading, on a temporary index. Compare marks a flaky status with its noise. `grader_noise` means one tree got both grades, so read the grader. `agent_noise` means the trees differ, so the agent did different work. `unknown` means a row has no hash and no tree shows both grades. Two different trees that are both right still read `agent_noise`. That blocks nothing. Ignored files are not in the hash.

## Backdated cohorts

A past session never answered a golden prompt, so it cannot grade one. Two honest reads cover the past instead.

`fno doctor event corrections --by day --since 28d` prints correction rates per day from real transcripts. It costs no model call. A day with typed turns and no correction reads a rate of 0.

`fno doctor evals run --tag golden --variant v1 --ref <sha> --overlay-fixtures <newer ref> --lane <lane> --cohort gate-backdate-<date>-<sha8> --repeat 1` runs today's goldens on an older tree. An older tree has no golden fixtures, so the flag commits `evals/fixtures/golden` from the newer ref on top of the checkout. Pass `--bank` to read today's tasks. The bank itself stays out of the tree. Each golden states its expected answer, and a worker told to read the repo can read it. Each row records `overlay_rev`. A row without `overlay_rev` ran with no overlay: an older `fno-agents` ignores the flag, and a fixture golden then fails on its missing fixture.

The cohort names a tree, never a past result. The model is today's, so the cohort says what today's model does with that day's repo. A claude golden worker loads no plugin (see [Goldens](#goldens)). The installed `fno` stays today's. The `gate-` prefix counts the run against `evals.gate.daily_usd`.

## Goldens

A golden is a bank task with a `golden:` block that states the real case it came from and the rule it holds. A failure then names what broke, not only a task id.

```yaml
golden:
  source: {kind: correction, ref: "e49583f3:repeated_correction"}
  quote: "MUCH less comments please"
  expected: "The function, with at most one comment line and no node id."
  rule: "AGENTS.md principle 6: match the file's comment density."
  failure_type: comment-noise
  cost: rework
  outcome: failed
```

A block with no `source`, `expected`, `rule`, `failure_type` or `outcome` refuses at load and names the task and file. `outcome` says what the source case did (`failed` or `passed`). It is history, not a current result. The current result comes from a run.

`fno-agents evals candidates` ranks the pool to label from. It reads the rule-labelled reactions in `~/.fno/evals/decision/v1`, typed `agent_corrected` events, S2 rows in `corrections.log`, and `Revert` commits. It also reads plans by superseded status and caused commits, and plan premises that a later record contradicts. It prints a score, kind, reference, and one scrubbed quote. It writes nothing and calls no model. Every row is a candidate for a person to read. A rejected reaction is often a refused permission prompt, so the S2 rows are the stronger source.

A daily daemon pass turns each new typed correction into one draft in `~/.fno/evals/golden-drafts/`. It reads `agent_corrected` events after a watermark and skips interrupts, which carry no words. Two corrections with the same words are one case. A draft has empty `prompt`, `grade`, `expected`, `rule` and `failure_type` fields. A person fills them and moves the file to `evals/bank`, because an auto-committed bad answer holds merges with nobody having read it. A correction with no fixture shape stays a draft. A draft expires after 30 days, the pass counts it, and its case never returns. `fno-agents evals drafts` prints the counts.

A golden's rule must be findable in the repo, because a worker reads only the repo and the prompt. It must have a mechanical grade: an answer the grader reads from a decision file, or the work product itself.

A golden asks for recall, never a choice among listed options. On a tree with no AGENTS.md, the old bank passed 25 of 25. In 17 of those runs the worker read no answer key, and each one still passed. So the prompt asks for the exact command or action, and the grader rejects the lure from the source case.

A claude golden worker reads its rules from the tree under test, and from nowhere else. Other bank tasks keep the plugin and a linked worktree, because some invoke its skills.

The tree is a new git repo with one commit, built from `git archive` of the ref under `$TMPDIR/fno-evals`. It has no link to the real repo. It has no parent commit, because a parent holds the rules and the answer keys one `git show` away. It sits outside `$HOME`, so no `~/.claude/CLAUDE.md` loads from an ancestor path, and a `TMPDIR` inside `$HOME` refuses as a fixture failure before any worker starts. It leaves out `evals/bank` and `evals/fixtures/metrics`, which hold the answers. Workers did read these: 20 of 101 kept transcripts touched one.

The runner passes `--setting-sources project --strict-mcp-config` and a `--settings` read fence, and sets `CLAUDE_CODE_DISABLE_AUTO_MEMORY=1`, `GIT_CONFIG_GLOBAL=/dev/null` and `FNO_EVALS_SEALED=1`. The last one drops the spawn's state grants. The fence blocks file-tool reads outside the working directory, denies the file tools all of `$HOME`, and denies `WebFetch` and `WebSearch`. It runs Bash in the OS sandbox with `$HOME` and the sibling trees unreadable and no network. A canary confirmed each door: `git`, `cat` and writes work in the tree, and the real checkout, `~/.fno`, `~/.claude`, a sibling tree and `github.com` all fail. A setting in the tree's own `.claude/settings.json` did not lift the block. No plugin loads, not even the tree's, so a worker must open a skill's file to read it. The spawn still adds its relay-compression block to the prompt.

Each row records `obs.worker_sealed`, true only when the sealed tree and the fence both applied. `obs.worker_sealed` is the divider between cohorts: a row without it ran in a tree that could read the real checkout, so it is not comparable to a sealed row. On 2026-11-01, before the seal, a fixtures-only tree beat head in 2 of 3 pairs at repeat 1. Other harnesses have no such flags. They get the sealed tree, no fence and today's plugin, so their rows read false.

A failed trial keeps its claude transcript for 14 days, and `obs.kept_transcript` names the directory. A passed trial's transcript is deleted.

A failure that needs long session state or fleet state cannot reduce to this shape. It stays out of the bank, and the scorecard holds that ground.

## The change gate

The daemon checks every open PR that changes behaviour, and posts one commit status, `evals/gate`. CI does not run this: CI has no worker lanes, and a bank run needs a spawn-capable machine. Each `evals-arm` tick runs one pass. The tick waits on no run, and a later tick reads the history rows, so a daemon restart loses nothing.

- **The main run.** Once a day, the tick runs every `golden` task at `origin/main`. It uses 3 repeats and 2 workers, as cohort `gate-main-<date>-<hash>`. The hash covers `routing.models` and `agents.profiles`, so a route change starts a new run at once. Each finished cohort with a grade becomes the baseline, and it is compared to the one before. A regression files one node, keyed like the scheduled bank's. The compare lands in `<history dir>/evals-gate/compare-<cohort>.json`. Read it again with `fno doctor evals report --compare <older> --to <newer>`. No person starts these runs. Two nights after install, the first compare exists. A run that writes no grade does not spend the day. The tick tries again an hour later, up to 3 tries a cohort. `state.json` keeps the cause under `main_failure`. Each leg's output lands in `<history dir>/evals-gate/main.log` or `head.log`, replaced at each launch.
- **The head run.** This covers an open PR from this repository. Its diff must touch `skills/`, `agents/`, `hooks/`, `commands/` or `evals/bank/`. It gets a pending status. Then the daemon runs its `gate` goldens at that head, as cohort `gate-head-<sha>`. It uses 3 repeats and 4 workers, one PR at a time. A newer head kills the older run. A PR from a fork never runs, because its hooks and skills run on this machine.
- **The task set.** A head runs the tasks tagged `gate`, with two exceptions. The gate reads the task set at the ref being tested, through a detached worktree. The daemon checkout can sit behind that ref. So a golden the ref adds runs at once, before the checkout moves. A task with no file at that head stays out, because the head branched before it landed. A task whose baseline median is over 180 s stays out too.
- **The fail rule.** Red means one of two things. A task went from always pass to always fail over 3 runs. Or a task with baseline grades has no result. A move to flaky is green and lists the task. A task with no baseline is green and counted. A golden whose failure type is unproved cannot turn the gate red.
- **The limit.** A head run has 20 minutes. Past that, the daemon kills the run, and the status is red. It names the cause and the clearing command, `fno-agents evals gate-retry <sha>`. That command also clears any other no-result status.
- **The budget.** `evals.gate.daily_usd` (default 15) caps the day's estimated spend, at 0.04 USD an attempt. Over the cap, a head is red and names the key. It runs again on the next UTC day. Set the key to 0 to turn the gate off. `evals.gate.lane` (default `zai-flash`) names the lane. Both keys live in `config.toml`.
- **The merge door.** A red status turns `fno do pr status` red, so `fno do pr merge` holds. Pending holds it too. The ruleset does not require the context, because that holds every PR on one machine. So a daemon that never saw a head posts nothing, and that head can merge. `<history dir>/evals-gate/state.json` holds the last pass's note.
- **First install.** Until the first main run gives a baseline, the head run posts nothing. A fresh install holds no PR on a check it cannot do yet.
## Failure types

`evals/bank/failure-types.yaml` names each type and its cost tag, and `cost_order` ranks the tags. A golden names its type in `failure_type`. The label `none` marks a golden with a passing source case. It is not a type, and it can always fail a gate.

`report --by-failure-type` prints each type in cost order, then by failing goldens. If a golden's newest `--repeats` baseline grades read `always_fail` or `flaky`, the golden counts as failing. With too few grades it counts as no result.

A metric proves a grader before anyone trusts it. Each golden has `evals/fixtures/metrics/<id>.yaml`, with `good` and `bad` cases written as file contents. `report --check-metrics` writes each case to a temporary directory and runs the golden's own `grade:` list there. A proved type has at least 3 good and 3 bad cases per golden. Every golden passes its good cases and fails its bad cases. The line for an unproved type names the case that broke it.

A golden whose type is not proved lists under Unproved in a compare, and it cannot make the compare exit 1. With no readable catalog, nothing reads unproved, so a regression still fails.

No type needs a judge grader today, because each golden grades a decision file or a work product. `macro` patterns stay out of this list. They are session and fleet states such as `termination:NoProgress`, which a fixture cannot reach.

## Trend

The variant axis answers "did this change help?". The trend axis answers "are we regressing week over week?" - the question `--compare` cannot, because it scores git refs, not time.

Where the history lives: `~/.fno/history/evals-history.jsonl` (`config.paths.evals_history`), one append-only line per task-run, one writer. It is not a git artifact: a bank run needs a spawn-capable machine, and rows are per machine. It is not a second journal either. The `evals_scheduled_run` event is the receipt that a run happened. The history rows are what it graded.

A window is `evals.stale_days` days (default 7) - the same length every health surface already calls "recent". `fno doctor evals trend` scores the recent window `(now - W, now]` against the prior window `(now - 2W, now - W]`, per task, with `improved`, `regressed` or `unchanged` verdicts. Tasks present in only one window land in the missing lists with no verdict. `regressed` names regression-tier tasks whose verdict is `regressed`, and drives exit 4.

The default report's regression alarm reads the recent window too. A 50-day-old flake fixed three minutes later no longer holds exit 4 open. `tiers`, `tasks` and `flakes` stay the all-rows long view. `fno doctor` prints `evals REGRESSING - <ids> dropped against the prior <W>d window`, and `fno backlog triage health` appends ` REGRESSED <n>` to its evals line.

## Attempts, grades, and declared cohorts

An **attempt** is one planned execution of a (task, repeat) slot. A **grade** is a valid task verdict: the grader ran and returned a result. They are separate things, and the report keeps them separate.

Every attempt persists as one history row. The row carries a unique `attempt_id` and an `obs` block of structured observations: fixture prepared, worker started, grader ran, grader result. The `fno-agents evals-attempt` door classifies each row from those observations alone. The verdicts:

- `graded` - a valid task grade, pass or fail. A stall is a worker that started and ran out its budget. It grades as a fail with no grader run, and it does not retry. Spawn exit 124 is a stall. If exit 12 says the run timed out, it is a stall too. A registry failure before launch also uses 12.
- `infrastructure` - the fixture never prepared (retryable)
- `unavailable` - the worker was lost: spawn refused, cancellation, or the gate (retryable)
- `ungraded` - the worker ran but produced no grader result (retryable)
- `legacy` - no structured evidence

Legacy rows are never reinterpreted. A task whose rows are all legacy keeps the pre-attempt boolean fold, so old history keeps its alarm semantics.

Only infrastructure and unavailable verdicts are retryable (`run` retries via its `max_retries`). A graded failure is a final answer, not a plumbing problem. It never retries and never consumes a retry. Rows carry the raw `reason` strings. Nothing classifies by text matching.

The report never lets a plumbing failure dilute the headline. A modern task's `pass@1` divides passes by **valid grades**. Infrastructure, unavailable, and ungraded attempts stay visible in `attempts` and in `runs`. The tier line prints the same fraction it rates, then names the rest: `capability: 4/20 pass (20%); 75 of 95 attempts not graded: 75 plumbing, 0 legacy`. A task with no valid grade prints `[NO GRADE]`. It never fires the regression alarm or a regressed verdict. No task's rows are dropped: with `--planned '{"task": N}'` the report also shows `expected_attempts`, `missing_attempts`, and `completion`. Unrun work can never improve the picture.

**Declared cohorts** live in `evals/bank/cohorts.yaml`: `bank_rev` (the pinned revision) plus `train`, `validation`, and `qualification` id lists. The split is validated natively before any worker call: existence against the bank, uniqueness, disjointness. Export and qualification validate again, fail-closed. An unreachable door refuses. When the bank files change after the pin, a split pinned to that rev is void. Redeclare it. Nothing infers a percentage split from a tiny bank: no declaration, no held-out claim.

- `fno doctor evals export --out FILE` writes ONLY train-cohort rows (tagged `role: train`) for prompt tuning. It refuses without a declared, valid, bank-current split. Held-out prompts and trajectories never enter a tuning view.
- `fno doctor evals qualify` prints the allowed aggregate projection for the qualification cohort: coverage, passes over valid grades, attempt breakdown, and `wrong_rev`/`legacy` exclusions. A missing split, an empty qualification list, or a stale pin reports `{"qualified": false, "reason": ...}` honestly. It never prints a held-out prompt, trace, or per-attempt row.

## Lanes

A **lane** is a NAME joined against the existing `config.routing.models` inventory (harness, model, effort, route, account). `agents.profiles.*.lanes` already references these same rows. This is never a second model/effort enum. An unknown lane name refuses and lists the declared lanes. A lane that config never declared cannot be requested.

`fno doctor evals run --lane astra-high --cohort astra-trial` resolves `astra-high` from config and runs every selected task by the `config.routing.models` inventory row (harness, model, effort, route, account). Every history row from the run records:

- `requested_lane` / `requested_harness` / `requested_model` / `requested_effort`: what was asked for.
- `observed_harness`, `observed_model`, `observed_model_basis`, `observed_effort` and `observed_session_id` are read once from the transcript store. The read runs after the worker finishes, and nothing re-derives them. The native observe door is `evals-attempt` with stdin `op: observe`. It finds the transcript by its unique workdir. Opencode reads from its sqlite session store. Claude and footnote read as session records. A footnote record is `~/.fno/sessions/<project-slug>/<fno_id>.jsonl`. A claude transcript in `~/.claude/projects` reads through the adapter in `crates/fno-agents/src/claude_records.rs`. For both, the first cwd names the workdir. The model is the reported model of the `model_response` records. Claude repeats one API message over several rows. The adapter counts its usage once. A footnote transcript with an unknown non-ignorable record type reads `unverified`. The door returns the observed fields plus `usage` and `usage_source`. `usage` holds input, output, cache read and cache write, summed over the attempt. `observed_effort` and `observed_model_basis` stay null today. No store carries them for a headless attempt.
- `usage` / `usage_source`: token usage summed over the attempt from the same transcript. Null (never zero) when the store has no readable transcript (AC10-EDGE).
- `lane_status`: `ok`, `substituted`, `unavailable`, `not-applicable`, or `unverified`. When the transcript shows a different harness or model than requested, the run is `substituted`. It is never counted as a sample of the requested lane. A refused spawn is `unavailable`, and no model is graded as the requested one. A grade-only task never attempts a worker, so a `--lane` on it reads `not-applicable`, never a false `unavailable`. When the store holds no transcript for the attempt workdir, or the harness has no reader yet (pi, zcode), the run reads `unverified`: the run is real, only its identity stays unconfirmed.
- `experiment_id`: the `--cohort` id, the join key the cohort report folds by.

Treat `unverified` as "probably ran as requested but not independently checked", not as a failure. Only `ok` and `substituted` carry a checked identity.

## The cohort comparison report

`fno doctor evals report --by-cohort [--prices <manifest.json's table>]` folds the history by `experiment_id`. One row per cohort lists attempts, graded, scored (the rows the rate reads), accepted (scored and passed), the pass rate, its 95% Wilson interval, and the `lane_status` counts. Exclusions count by reason: substituted, unavailable, infrastructure, ungraded, contaminated via the row's `excluded_reason`. The row also carries median wall time, tokens, and dollars per accepted change. Cost reads `unmeasured` unless every scored row carries usage and a complete price line (input and output rates, plus a cache rate for any cache tokens the row carries) for its model. A price line missing a rate the usage needs reads as no price line. It never prints a partial sum. `--json` prints the same fold. Rows without lane evidence count under `(no cohort)` and can never join a scored cohort.

## Comparing two lanes

`fno doctor evals report --compare <glm> --to <opus> --prices <table>` runs the compare on the rows both lanes hold for the same goldens. Each side names its excluded rows by task, and `substituted` and `account-dropped` rows stay unscored. Each side prints dollars per accepted result, or `unmeasured` under the cohort report's cost rule. `evals/harness-fit/manifest.json` holds the price table.

## Plan premise labels

A checkable plan sentence is a Context, Why or audit line that names a path or symbol in backticks. `fno-agents intel --plans` pairs each one with the later records on its node: rulings, notes and the closing note. A ruling or note counts only from the plan's day on, and a wave row is never a record. It keeps the newest 10 records per plan. A classifier labels each pair as supports, contradicts or unrelated. Rows land in `~/.fno/evals/plan-labels/rows.jsonl`, and a contradicted premise becomes a candidate, never a golden by itself. A row's `overruled_by` reads only a mechanical fact: a later ruling on the node, a revert, or a supersede.

No rung labels plans until it passes a fixed set of 60 labelled pairs in `~/.fno/evals/plan-labels/ladder-set.jsonl`, 20 per label. Two models label blind, and the set keeps only pairs they agree on. Each row reads `person_read: false` until the user checks it, so a model-labelled set never opens a hosted rung. Agreed labels can share an error, so the user also reads a sample of agreed pairs. Contradicts is rare: 28 agreed pairs of 1,340 labelled, most from pools picked for correction words, so a random sample cannot fill that label. `--dry-run` prints candidate pairs for that set, and calls no model. A rung passes at 0.80 balanced with contradicts recall of 0.70 or more. `--plans-ladder` scores fnod first, then tuned kev-9b at `--teacher-url`, and stops at the first pass. The set never trains a model.

Data leaving the machine: fnod and kev-9b stay on loopback, and any other address refuses. A hosted rung sends plan text to a vendor, and a treg route adds treg as a second party. A `--hosted NAME=COMMAND` rung needs two local failed scores, a clean scrub on each pair and `person_read: true` on each pair. Without all three, the run refuses before any call. The command takes a pair's JSON file as its last argument and prints `{"label": ..., "dollars": ...}`. A regex scrub misses third-party and customer names, which is why a person reads each pair. A hosted winner still refuses `--plans`, because the full pass sends far more text. That pass needs its own user ruling, with the per-pair price and the text count stated.

State: these files sit in the `evals/` folder the corpus already uses, and they add no top-level folder. Drafts expire after 30 days. `.seen` and the label files stay.

## Legacy attribution note

A row missing `experiment_id` or `requested_lane` is legacy/unattributed. It folds under `(no cohort)` in the cohort report, never into a scored cohort.

## Run cadence and demand

Without a forcing function an eval harness dies. The bank sat fully built while its history went quiet. A 40-day-old 100% rendered exactly like a fresh one. The demand is built in now. When the bank ages past the schedule, the tick launches the regression tier detached. The health surfaces go red past the window.

- **The tick and the arm.** The daemon's evals phase resolves paths and schedule, then invokes the native `fno-agents evals-arm`. The arm prints one JSON receipt line. It never runs the bank inside the tick, whose wall budget is 480s shared by nine phases. When the newest regression run is older than `evals.schedule_days` (default 7, `0` disables the leg), the arm fires. Only a fleet incident or RAM under `agents.min_free_gb` holds it. A full fleet does not: the bank has a reserved slot. Then it launches `fno doctor evals run --tier regression -y` detached. The child leads a new process group with stdio on /dev/null. The arm holds the `evals:scheduled-run` claim across ticks. No second run starts while one is in flight. The run is bounded by its own 3600s budget, above the tier's 50-minute task-timeout sum. The group is killed on timeout. Graded fails are data: the run still journals as scheduled. Each failing task gets one backlog node, keyed `evals-regression:<task>`. An open node holds the next filing. Plumbing failures are could-not-fire: a refused gate, a timeout, a non-zero exit, an exit-0 run with no rows.
- **The bank's checkout.** The run reads `evals/bank` from its working directory. The launchd tick runs from `$HOME`, so the arm picks the checkout. It takes the first that has an `evals/bank` directory. The order is the plugin root, the cwd and its ancestors, then each registered project (`work.workspaces`). The plugin root counts only as a git checkout. Every task cuts a worktree from the root, so a staged copy grades every task as failed. The child runs there. With no bank anywhere, the arm skips with `no_bank` and launches nothing.
- **The journal and the notice.** Every outcome lands in the global events journal. A pass or graded fail journals `evals_scheduled_run` with task count, passes, and duration. Rows are attributed by their own timestamps, never by a count delta. A plumbing failure journals `evals_stale` with the reason. Past twice `evals.stale_days`, it also carries one OS notice per schedule window. The journal row IS the dedup, so no state file of its own exists.
- **The red row.** `fno backlog triage health` renders the evals line with the newest run's age (`age 9d STALE`). When the trend names regressions, it appends ` REGRESSED <n>`. When the trend drops a task, `fno doctor` adds the REGRESSING line. A stale bank prints STALE with the age and the run command. The lead check-in names why a stale or never-ran bank did not run: `STALE (scheduled run: no_bank: ...)`. No history or no regression row prints UNKNOWN. Unknown never asserts staleness. A fresh, trending-clean bank prints nothing. A red evals row is an escalation, never a gate. Nothing refuses while the bank is stale. The row is what demands the run.
- **By hand.** Run `fno doctor evals run --tier regression` before a risky change. Run `--repeat 5` on a suspect flaky task.

The report has two wired consumers from day one, so the harness is not a write-only artifact:

1. With history, `fno backlog triage health` shows an `evals` line: regression pass rate, flake count, age, STALE.
2. CI-flake suites live as regression tasks, making the flake ledger a graded artifact instead of memory notes.

## Scratch-shape sweep

Agents author throwaway scripts under `~/.claude/jobs/<job>/tmp/`. The scratch-shape sweep (`fno doctor scratch sweep`) notices a recurring question and files one p1 node for it. The census behind the rule table, and the remedy rank per shape, live in the crate's scratch module documentation. That rule table is the shipped classifier.

- **What it reads.** `<jobs>/*/tmp/**/*.py|sh` files with mtime inside the window. The window defaults to 28 days (`config.evals.scratch_window_days`). A file whose git blob hash is in repo history is a copy of source. Flat copies report as the `copy_for_diff` shape. A copy under a directory holding `__init__.py`, `.git` or `pyproject.toml` is checkout residue and is skipped. Only what survives is authored scratch.
- **The recurrence unit is (job, shape).** Files per job measure one agent's habit. Distinct jobs measure the fleet's need. One lead writing 158 mail wrappers in a night is one recurrence. Each new pair emits one `scratch_shape_observed` row to the global journal. A pair already recorded inside the window never re-emits. The journal read covers `events.jsonl` and its rotated sibling. That is the ceiling: one 8 MB generation plus the one before it.
- **Threshold: three jobs.** Set `config.evals.scratch_threshold`, default 3. The threshold counts every walked pair in the window, remembered or not. Two can be one agent twice, because a resume mints a new job dir. Shapes rank by jobs, then files.
- **Only a missing remedy files.** A shape at three jobs does not always have an fno remedy. The census measured `file_patch` at 26 jobs and `git_wrap` at 17 with none. Each shape's remedy row says whether it can file. A report-only shape still emits observed rows and shows in the report. Before filing, the sweep runs the row's help probe. A match means the remedy shipped, so it prints `served:<shape>` and files nothing. A probe call that fails files with the table hint, so a broken probe never hides a missing remedy.
- **One node per run.** `MAX_FILES_PER_SWEEP` is 1. A long list of new nodes is the failure the operator named. Five shapes crossing in one day file one node today. The next files tomorrow. The filing index is the graph. Every sweep node carries `origin_evidence` `scratch-shape:<shape>` and is minted with `--separate`, so text similarity never picks the node. When a scratch file is newer than its last write, a live keyed node folds (`--wave-of`, no new id). A same-day re-run adds nothing. A keyed node done inside the window suppresses. A keyed node done before the window refiles with `--caused-by`.
- **State words on stdout.** One per line, never silent: `ok`, `insufficient: <reason>`, `filed:<id>`, `folded:<id>`, `suppressed:<id>`, `served:<shape>`, and `would-file:<shape>` under `--dry-run`. The sweep classifies shipped fixture specimens before touching the real jobs dir. A classifier that finds nothing there prints `insufficient: control failed` and emits nothing. A broken classifier can never read as a quiet machine.
- **Command hint.** Details name the census rule, the file and job counts, and the three newest specimens. The hint names the missing command or flag. A probe reads the `--help` of the group the missing leaf joins, because a retired root no longer answers.
- **Reading it.** `fno doctor scratch report` renders the ranked table from the journal: shape, files, jobs, monitors, first and last seen, filed node. `--json` serves machines. The sweep runs as stage five of the daily eval-sweep ignition. It is bounded and logged to `.fno/logs/eval-sweep.log` like every other stage.

## Context-to-outcome trace

`fno whoami scoreboard --plan-fidelity` derives a context-to-outcome trace from the ledger, backlog graph, and canonical event journal.
It does not persist a second graph or treat a terminal label as downstream evidence.
A context-pruning or graph-widening comparison is labeled an improvement only when its event-carried contract declares the cohort members, observation window, exclusions, budgets, and declaration time before the window starts.
Every selected trace must also carry a falsifiable CI, review, merge, revert, recovery, latency, or spend observation.

The table below is generated by `fno.scoreboard.fold.render_context_trace_field_docs`; update the parser-owned field contract and regenerate this block instead of duplicating field prose.

<!-- context-outcome-fields:start -->
| Field | Derived from | Meaning |
|---|---|---|
| `objective` | graph, ledger | Objective or node title attributed to the delivery. |
| `plan_node` | ledger, graph | Canonical backlog node identifier. |
| `worker_session` | ledger, events | Session that produced the delivered commit. |
| `claim` | claim events | Historical claim observations; never current ownership truth. |
| `commit` | ledger, loop_check | Exact delivered commit when observed. |
| `evidence_receipt` | ledger | Existing receipt reference; the trace never invents one. |
| `pr` | ledger | Pull request number and URL. |
| `context` | context_snapshot event | Harness, entry state, delivered bytes, tokens, source hashes, and measurement completeness. |
| `outcomes.ci` | loop_check events | Latest observed CI state. |
| `outcomes.review` | review events, loop_check | Review verdict and finding observations. |
| `outcomes.merge` | events, graph | Observed merge outcome. |
| `outcomes.revert` | events, graph | Observed revert outcome. |
| `outcomes.recovery` | recovery and handoff events | Recovery observations without changing resume behavior. |
| `outcomes.latency_minutes` | ledger | Recorded delivery latency. |
| `outcomes.spend_usd` | ledger | Recorded delivery spend. |
| `falsifiable` | derived | True only when at least one downstream observation can disprove a claim. |
| `provenance` | derived | Canonical inputs that contributed to the trace. |
<!-- context-outcome-fields:end -->

## Delivery classification

Every scoreboard view reads one delivery classifier (the Rust keeper's
`scoreboard_classify` op). It distinguishes a merged PR, a delivered code
node, a delivered document and a session stop. A confirmed merge delivers
with no ledger row; a session terminal on a known unmerged node records
evidence and never a delivery; a terminal on a node the graph lost counts
as `inferred`, never as a confirmed merge.

Quality cohorts judge only deliveries that completed the 14-day observation
window; younger deliveries are reported pending, not in the denominator.
The human_touch emission-failure counter is server-instance telemetry, read
from `fno mux stats --json` with its own measurement window; an unreachable
server renders as Unknown, not as a measured zero.
