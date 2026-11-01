# Limitations of /fno:intel

## Known Limitations and Deferred Work

- User is witnessed, not inferred. When a person presses Enter in a pane or portal, the mux writes an `operator_submit` row. The fold binds turns to those rows by session and time. A turn outside the witness reads `unknown`, never `operator`. Uncovered paths: a bare terminal, the desktop apps, and claude.ai jobs. That typing never passes the mux. A hand-started harness in a shell pane writes `resolution: unresolved`, and nothing joins it. A submit queued past the 30s bind window also reads unknown. `witness.unwitnessed_sessions` counts sessions typed outside the witness.
- The relay delivered-check is a substring read over the session's transcript. A bus body that reaches the transcript through some channel other than this session's turn flow still reads as delivered.
- Opencode is folded now. Its `operator` class is the same witness join as claude's. Sessions with a `parent_id` (subagent children) are excluded. When no store is readable, `skipped.opencode` carries the sqlite reason.
- Facet files under `~/.fno/intel/facets/` are keyed by session id + mtime + size. Each facet also carries the `question_key` it answered. When the key and the question match, the fold accepts the cached facet. A facet judged under one question never judges another question's sample. A session whose transcript is rewritten in place at the same size defeats the key and is not re-judged.
- Category shares rest on the sample, not the window. The Executive summary and Categories percentages speak for the `judged` population only.
- A session still active at report time is idle-gated out of the sample and is judged on a later run.
- Opencode rows carry no activity counters: their tokens, lines, tool errors, and languages read null, and `activity.unmeasured` names the harness.
- Hours, response time, and the parallel measure are empty for history without witness coverage. Witnessed user turns are the only input, and sessions folded before the witness existed have none.
- Line counts come from edit arguments, so a rejected edit still counts its lines, the same choice Claude's `/insights` makes.
- The HTML scrub is pattern-based. A secret with no known shape under 40 characters, and paraphrased transcript text with no quotes, pass through. A quoted span outside `Operator corrections` is dropped, so a needed quote outside that section must move into it.
- The HTML copy is a snapshot. When the markdown or the fold JSON changes, the copy does not update. Re-run `--render` to refresh it. The retention rule prunes stamped copies past the 12 newest. A hand-renamed HTML outside the stamp pattern is never pruned.
- Two JSONs land per run. The raw sidecar (`.fold.json`) holds every session row. The judgment summary (`.json`) holds aggregates plus categories, no per-session rows. A report whose step 4 failed names the raw sidecar in `fold:` instead. The summary is small. The sidecar is not.
- Model attribution reads the majority of fresh assistant rows. A zai-routed row carries a fabricated `msg_` + 8-digit id. The row never votes a claude label the route contradicts. A session with only such rows lands in `scorecard.coverage.sessions_unknown`. A session that switched models reads as whichever model authored more rows, and the switch itself is invisible. A session whose source names no model lands in `scorecard.coverage.sessions_unknown`.
- Lead rows are a floor under the rollup replay. Witness-bound turns on the replay carry timestamps only, so `restated_asks` misses their texts. `coverage.texts_best_effort` says so.
- The per-model refusal read is the fold's error classes, not the hand evals' seven buckets. The bucket crosswalk is the judgment layer's read over those classes.
- A model row compares fairly only inside one `by_kind` bucket. Compare a code-heavy model against a doc-heavy one across the whole window and the rates mislead.
