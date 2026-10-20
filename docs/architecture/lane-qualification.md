# Lane qualification: tuning an operator's model lanes with reproducible evals

The operator runs across several models, harnesses and effort levels. This page is the recipe for qualifying one of them with the existing eval bank, not a new benchmark service. Full mechanics of the lane machinery live in [docs/evals.md](../evals.md#lanes). This page is the operator workflow that sits on top of it.

## The paired fixtures

Three bank tasks isolate one axis each, so a lane's weakness shows up on the right axis instead of one blended score:

- `evals/bank/capability-lane-blueprint.yaml` - an ambiguous scope decision. It grades whether the worker names the correct in-scope file and rules the wrong one out.
- `evals/bank/capability-lane-implementation.yaml` - an unambiguous implementation. It grades observed functional behavior only, with no scope judgment involved.
- `evals/bank/capability-lane-review.yaml` - a seeded review defect. It grades a finding tied to the specific symbol and the specific wrong return value, never a generic "found a bug" match.

Run all three against the same fixture revision and the same repeat count. A lane that wins only on implementation, and loses on scope decisions, is a different qualification result than one that wins across all three. The split is the point.

## Running a paired trial

Run the baseline lane first, at the existing effort level. Vary one factor at a time. Swap only the model, or only the effort, or only the harness. `--task` takes one id per invocation. Run all three:

```bash
for t in capability-lane-blueprint capability-lane-implementation capability-lane-review; do
  fno doctor evals run --task "$t" --lane current-baseline-name --cohort current-baseline --repeat 5
done
```

Then run the trial lane against the identical fixture revision and repeat count:

```bash
for t in capability-lane-blueprint capability-lane-implementation capability-lane-review; do
  fno doctor evals run --task "$t" --lane astra-high --cohort astra-trial --repeat 5
done
```

Each run appends one history row per task-run to `~/.fno/history/evals-history.jsonl`, carrying `experiment_id` (the `--cohort` id), `requested_lane`, and the `lane_status`/observed fields ([docs/evals.md](../evals.md#lanes)).

## Reading the result

`fno doctor evals report --by-cohort` folds the history by cohort id. One row per cohort lists attempts, graded, accepted, the pass rate with its 95% Wilson interval, and lane-status counts. It also counts exclusions by reason. Median wall time, tokens, and dollars per accepted change close the row. Pricing needs `--prices`, and cost reads `unmeasured` unless every scored row carries usage and a price line. A `--json` flag prints the same fold. Read the fold side by side across the two cohort ids for the comparison.

A `lane_status` of `substituted` on any row means capacity served a different harness than requested. Exclude that row before comparing lanes. It is not a real sample of the lane you meant to qualify.

A `lane_status` of `unavailable` means the account cannot reach the lane at all. Read this as `unavailable`. Never read it as a score of zero on the requested model.

A `lane_status` of `unverified` means the run succeeded but nothing confirmed which harness or model actually served it. This is the normal case for the default headless spawn today ([docs/evals.md](../evals.md#lanes)). A bad model or harness name fails the spawn loudly, so count an `unverified` row as a pass-rate sample of the requested lane. Do not cite it as proof of identity the way an `ok` row is.

## One whole-delivery comparison before splitting roles

Run one comparison of a lane doing the full delivery (plan, implement, review) against the current baseline before recommending a planner/implementer split for that lane. A lane can be weak at blueprint's scope judgment (task 1) and still be a fine implementer (task 2) under a stronger planner. Splitting on a single blended score hides that difference. The three paired tasks exist precisely so this comparison does not require guessing which axis moved.

## Budget and access

A live trial runs only with an explicit configured budget and an account that can actually reach the lane. No access, no budget, or a lane the account cannot run reports `unavailable`. It never reports a synthetic pass standing in for a trial that never ran.

## What this does not do

- It never edits `config.routing.models`, `agents.profiles`, or any production lane order. Applying a qualification result is a separate, reviewed config change.
- It never claims one model is universally superior. A result is scoped to the fixtures and the cohort it was measured against.
- It adds no new eval scheduler, benchmark service, or model/effort enum. The lane vocabulary is `config.routing.models`, the same rows `agents.profiles.*.lanes` already reference.

## Climb round

A climb round turns one comparison into a verdict the user can accept in one step. The fold reads the declared split and applies the harness-fit preregistration's decision rules (evals/harness-fit/README.md). The verdicts: underpowered (under 20 graded held-out rows per cohort), regression (trial held-out 15 points down), overfit (train up 15 points, held-out flat), propose, hold.

1. Pick one stage and one candidate lane from `fno config route inventory`. For the blueprint stage the candidates are only the blueprint-floor models: opus, fable, gpt-5.6-sol, gpt-6-astra. A cheaper model never drafts a plan.
2. Run that stage's train and held-out tasks for baseline and trial under one cohort id each, same `--repeat`. Command shape: `fno doctor evals run --lane <baseline lane> --cohort <baseline id>`. The trial run repeats the shape with its own lane and id.
3. Fold it: `fno doctor evals report --climb --baseline <baseline id> --trial <trial id> [--stage <verb> --trial-lane <trial lane>] --json`.
4. On `propose`, write the JSON to `evals/climbs/<date>-<stage>.json` and ask the user with the printed accept line. On `overfit` or `regression`, read the reason and stop.
5. Choosing the next candidate reads only train transcripts. Held-out rows are never inspected to steer the climb. The held-out set stays a test, not a tuning surface.
