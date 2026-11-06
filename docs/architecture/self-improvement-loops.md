# Self-improvement loops

The loops that turn what the fleet did into what the fleet does better. Each one is: capture writers feed a log, a trigger reads the log, the output lands somewhere a later session reads. This page is the map an operator asked for on 2026-09-16: where one loop starts and ends, and which config turns each on. The corrections loop is the eval loop: trace files, labels, and the go-N fixer (`crates/fno-agents/src/evals_traces.rs`, `evals_labels.rs`).

## The loops

| Loop | Trigger | Config key | Default | Output lands in | Read command |
|------|---------|-----------|---------|-----------------|-----------|
| Trace pass | Terminal traces at finalize (`evals_traces.rs`: `pass`, `false_claim`, `user_correction`) + the daemon's daily arm (`maybe_tick`) | (none) | always available | Trace files under `~/.fno/evals/traces/`; `seed-s2.log` froze the 139 historic S2 rows on first import | `fno-agents evals` |
| Golden drafts | Daily daemon arm, `crates/fno-agents/src/evals_drafts.rs` | (none; watermark in `~/.fno/evals/golden-drafts/.watermark`) | always available | One draft per new typed correction in `~/.fno/evals/golden-drafts/`, reviewed by a person into `evals/bank` | `fno-agents evals drafts` |
| evals | The daemon's one-minute evals arm checks for due gate and bank work | `evals.schedule_days` (days between regression runs; `0` disables) | `7` | Run receipts + trend rows (`fno-agents evals-arm`, `evals-trend`) | `fno doctor evals` |
| question judge | The daemon's `question_judge` arm, hourly | `questions.judge.enabled` | `false` | Shadow verdicts in `~/.fno/state/judge/verdicts.jsonl`; a user answer that differs feeds the next prompt | `fno inbox outstanding judge --report` |
| upstream issue | An intel suggestion on fno's own code, or the user's own words | (none) | always available | A redacted draft on stdout plus a prefilled `issues/new` link. Nothing is filed. | `fno do feedback --node <id>` or `--text "<words>"` |
| pr-watch heal | The pr-watch tick over every red open PR | `auto_heal.enabled` | `false` | Heal attempts + reports per PR (`fno do pr heal` applies the mechanical fix) | `fno do pr heal <n>` |

## What feeds what

- Failures reach the trace pass at finalize: a `Budget`, `NoProgress`, `Aborted` or false-claim terminal writes a trace row. S2 corrections reach it off-loop, through the daemon's correction fold (`correction_fold.rs` `agent_corrected` feeds the `user_correction` trace). The 139 rows the old corrections.log held were frozen into `seed-s2.log` once, at first import.
- Labels in `evals_labels.rs` name the go-N fixer model per label: the eval loop drafts the fix from the trace bank, on demand, not on a schedule.
- Evals and heal are daemon-tick loops: both read what the other loops wrote. A healed PR re-runs evals banks. A graded failure in a scheduled regression run files one bug node per task. An open node holds the next filing, and a plumbing failure files no node.

- A node is the join for the upstream-issue loop. `fno do feedback` masks seven classes of private text. They are secrets, paths, emails, session ids, vault paths, project names, and login and host names. It then rescans and refuses a draft that still carries one. The user's Submit click on github.com is the only publish step.

## Tuning

Start nothing by hand: `evals.n: 0` plus `auto_heal.enabled: false` are the stock quiet posture. The trace pass writes only at terminals and on the daemon's daily arm; it has no installer to run or remove.
