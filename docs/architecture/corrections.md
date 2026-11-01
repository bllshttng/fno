# Corrections as events

Each time someone corrects a working agent, the daemon writes one `agent_corrected` row to `~/.fno/db/events.db`. The row names who corrected, who was corrected, and the facts an eval needs to tie the two together.

## Three correctors, never summed

`corrected_by` is `user`, `agent` or `machine`. Machine rows outnumber user rows by about 17 to 1, so no report prints a total across them.

- `user`: a typed turn the rule reads as `correct`, a tool call refused (`reject`), or an `interrupt`. Reports exclude interrupts unless asked, since they are noisy as a sign the agent was wrong.
- `agent`: a delivered mail body that disputes the recipient. `verified` is always false: the corrector can be wrong.
- `machine`: a gate refused the act (`refused`). A wrong gate is still a true row. Machine rows and tool-refusal rejects read claude transcripts only today: the other harnesses write no comparable tool-error class.

## How rows get made

`correction_fold.rs` is one daemon arm with a 60 s floor, off the core loop. It reads every harness through its `TranscriptSource`, at most 64 MiB and 200 rows a tick, newest files first. Cursors live in `<state>/cache/corrections/cursors.json`, beside the typed-turn tally that is the user rate's denominator. A held lock skips the tick, and a down daemon catches up from its cursor. A row's `ts` is the turn's own time.

The event id is `corr:` plus the corpus `row_id` for the same turn, so a replay is ignored by the store and a reader can join an event to its corpus row.

A turn older than the arm's first run reads `backfilled: true`, with its detect-time facts unmeasured. The backfill is the same path, spread over ticks.

## Served readings

Each harness fact is `{value, measured_at, basis}` or `{value: null, basis: "unmeasured", reason}`. A missing fact reads unmeasured with its reason, never a guess. Harness, provider, model, effort and account stay separate axes, per [axis-vocabulary](axis-vocabulary.md).

| Fact | claude | codex | opencode | pi | footnote | agy |
|---|---|---|---|---|---|---|
| model | `message.model`, checked below | `turn_context` | `modelID` | `model`, `model_change` | `turn_context` | unmeasured |
| effort | row `effort` | `turn_context` | `variant` | `thinking_level_change` | unmeasured | unmeasured |
| provider | derived from the model, else the registry route | `session_meta` | `providerID` | `provider` | `model_request` | unmeasured |
| harness version | row `version` | `cli_version` | session `version` | unmeasured | header `fno_version` | unmeasured |
| skills, references, subagents | tool calls | shell calls, partly | unmeasured | unmeasured | child sessions only | unmeasured |

**The claude label can lie.** On a routed endpoint, `message.model` echoes the name that was requested. Measured over 150 transcripts on 2026-11-01: every `msg_01` plus `req_` row came from Anthropic, while a GLM route wrote `msg_20` ids with no request id, including 420 rows labelled `claude-opus-5-5`. So a claude label on a foreign shape reads unmeasured, and any other label there reads `basis: label`, a claim the row cannot confirm. A registry `route_provider_id` outranks a derived provider.

## Scrub

The quote is capped at 280 characters and passes `scrub_quote`. Secret shapes are masked. A residual shape drops the quote and keeps the row: `quote: null, quote_status: dropped`.

## Outcome

The store is append-only, so outcome is its own `correction_outcome` row, from an hourly pass.

- `held`: the node merged after the correction, with no overrule. This is weak evidence: a merge shows nothing broke, never that the corrector was right.
- `overruled`: something later took the correction back. An agent or machine row reads this after a user correction within 30 minutes in the same session. An agent row reads it after the corrector's own retraction on the bus. Any row reads it when a `decision_span` names it.
- `unresolved`: no merge after 14 days.

## Read it

```bash
fno doctor event corrections --by model --since 7d
fno doctor event corrections --by harness --source user
fno doctor event corrections --precision ~/.fno/evals/corrections/v1/labels.jsonl
fno agents fleet feed --kind agent_corrected
```

The feed shows user rows by default. An explicit `--kind agent_corrected` adds the agent and machine rows, as `agent_corrected_agent` and `agent_corrected_machine`.

## Precision and limits

Rates compare only within one `rule_version`. The floor for a source is precision at 0.80 with a Wilson lower bound at 0.70. Labels stay outside the repo, as they hold user words, at `~/.fno/evals/corrections/v1/labels.jsonl`, one `{"source": "user/correct", "correct": true}` per line.

The read of `v1`, from 213 rows of real transcripts, labelled blind by two models. A disagreement counts as a miss. The samples are smaller than the plan's 350 because 12 GiB of transcripts held no more of the rare kinds.

| Source | Hits | Precision | Wilson 95% | Confidence |
|---|---|---|---|---|
| `user/correct` | 46 of 52 | 0.885 | 0.770 to 0.946 | above floor |
| `user/reject` | 38 of 38 | 1.000 | 0.908 to 1.000 | above floor |
| `user/interrupt` | 47 of 47 | 1.000 | 0.924 to 1.000 | above floor, as a fact |
| `agent/correct` | 4 of 26 | 0.154 | 0.061 to 0.335 | below floor |
| `machine/refused` | 50 of 50 | 1.000 | 0.929 to 1.000 | above floor |

The agent rule misses by a wide margin, and the labellers disagreed on 19 of its 26 rows. Its rows carry `confidence: below_floor`. Do not mine them until a stricter rule clears the floor.

Recall is low: a correction with no leading marker, such as "use the other branch", misses. So never read a rate as an absolute count. On pi and agy a turn the mux typed reads `unwitnessed`, because their rows carry no witness.
