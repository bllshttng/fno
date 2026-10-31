# SUMMARY

Node: x-4099. The MacBook mux shows questions asked on the iMac but cannot answer them: the answer must reach the asking machine's question store.

## What landed

Four tasks of the plan (vault plan: `internal/fno/plans/20261030-macbook-mux-shows-questions-asked-imac.md`), one commit each:

1. e5e7123c02 `feat(needs)`: the mux answer door records an answer for a peer copy (refusal removed; delegate still refuses copies by name).
2. 989f16dbd1 `feat(question-bus)`: the `fleet_question_answer` carrier. Publish, apply-once via `applied_at`, retention aging.
3. b5f1921817 `fix(attention)`: the ladder marks a copy's answer terminal (`carried` delivery row), so no local clear burns retries.
4. cbb0e182c9 `docs(architecture)`: the Questions section names the answer carrier and the one-writer split.

## Deviations from the plan

- The plan named the carrier column `origin_host`. I named it `answer_host`: the column holds the ANSWERING machine's host (the asker is `origin_machine`), and AC2's `sink: peer:<answerer host>` and the door receipt's `carries_to` derive from it. `origin_host` would read as the asker.
- Local tests did not run (the repo cargo wrapper refuses local test runs from agent sessions; CI arbitrates). Local proof: `cargo check --lib --tests` green, `cargo fmt --check` green, junk-token sweep of both diffs clean.
- Diff budget: +421 added lines vs the +300 budget (140%). Per the one-node-one-PR law the PR ships whole and states the overage in the body.

## Done probes (CI-measured)

- `cargo test question_bus::tests::a_copy_answer_reaches_the_asking_machine_and_its_close_follows`
- `cargo test attention_reply::tests::a_copy_answer_row_never_enters_the_local_ladder`
