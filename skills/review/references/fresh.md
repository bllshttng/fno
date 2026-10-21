<!-- style-exception: the outcome list and the emit command are verbatim contract shapes shared with single-lane.md and emit-attestation.sh; rewording them would fork the vocabulary the attestation reader parses. -->

# The fresh runner (`--fresh`)

The default lane, run ONCE in a context that is not the builder's. One reviewer subagent executes [single-lane.md](single-lane.md) with only the diff and the plan's acceptance criteria, never the builder's reasoning. The dispatching session keeps ownership: it emits the head-pinned attestation, fixes the findings, and runs any `--verify-fixes`. Width is 1, always. The six-way panel stays retired.

Why a fresh context: the building session grading its own diff shares every blind spot with the build. docs/architecture/review-lanes.md records the forked native review catching a silent data-loss bug that two in-session passes missed. `--fresh` is that lesson as an explicit flag: the user names the level of independence they want.

Inputs from the router: a level, optional `--comment` and `--fix`, and an optional target. `--fresh` composes with both flags. Both stay BUILDER-side. The reviewer never fixes, never posts: `--fix` applies findings after the fresh round lands, and `--comment` posts through the existing hold-and-publish path.

## Phase F1 - the width gate

Before anything dispatches:

```bash
fno config assert-subagent-budget --width 1
```

Exit 0 (permit) continues. Exit 1 (refusal) ends the fresh attempt: print the refusal reason and `fresh review not run (width gate refused)`, dispatch nothing, emit nothing. Never widen to pass the gate, never run a panel instead. The user can re-run bare for the inline lane, which records the level as self.

A harness with no subagent dispatch at all takes the same exit. The fresh attempt is not run and the report says `not reviewed`. The inline lane is the fallback. That round records the level as self. A fresh promise never silently degrades into a self review.

## Phase F2 - assemble the brief

The brief is what the reviewer sees. It carries exactly four things and nothing else:

1. The lane: load `skills/review/references/single-lane.md` and execute it in full, in your own context, at the resolved level, with `--fix` off. Run it at FIRST-ROUND SCOPE. Skip the Phase 2 carry-forward journal read, never narrow to incremental, and re-validate no carried finding. The builder owns carried findings and their dispositions.
2. The diff inputs: `BASE` and `REVIEW_TARGET` as Step 1 and the empty-diff guard resolved them. Phase 0 in the lane re-derives the diff from these.
3. The acceptance criteria: the plan's acceptance-criteria block, pasted verbatim. With no plan bound, paste the node's details. Criteria say DONE, not HOW.
4. The output contract below, and the review-only constraint: change nothing, commit nothing, post nothing. The reviewer's only artifacts are the report and the findings array.

The brief never carries: the builder's transcript, session state, SUMMARY.md, intermediate notes, prior review rounds' reasoning, or anything describing how the work was done. The reviewer reads the diff and the criteria. Context beyond that is the contamination `--fresh` exists to prevent.

Dispatch exactly ONE fresh subagent through the Agent/Task tool (a general-purpose or reviewer type that starts with an empty context). Never a fork or any dispatch that inherits this conversation: a fork carries the builder's transcript, which is the exact contamination this mode bars. One, never two, never a roster.

## Phase F3 - the three outcomes

**A report with findings.** The reviewer returns the report and a findings JSON array (the shape single-lane.md's Report section defines). Write the array to a temp file and emit from THIS session, with the fresh context stamps in the positional slots:

```bash
bash "${SKILL_DIR}/scripts/emit-attestation.sh" code-review pass fresh fork --findings-file "$FINDINGS"
```

`reviewer_context=fresh`, `execution_context=fork`. The typed `pass` is ignored on this path: the classifier measures the verdict from the findings record. Then fix the findings HERE, in this session, and run `--verify-fixes` here. A scoped fix-verification is not a round.

**A clean report.** An explicit empty findings array is a real pass: a reviewer ran, read the diff, and named zero findings. Same emit line.

**Silence.** No return, an error, an empty message, or prose with no findings array is NOT a review, never a pass. Emit no attestation. Journal the terminal attempt with the emitter's refusal mode, as a plain argv call:

```bash
bash "${SKILL_DIR}/scripts/emit-attestation.sh" --refuse fresh_reviewer_silent
```

The writer resolves the invocation id, head, and branch itself, journals one refused `review_invocation` row, and exits 1. The exit is the refusal. The row is the record, so the attempt reads as attempted-and-refused, never as never-attempted. Print `fresh review did not return; not reviewed` and stop the fresh attempt. Re-run bare only as a user decision. That round records the level as self.

## Ownership

The subagent emits nothing. One producer surface ([emit-attestation.sh](../scripts/emit-attestation.sh)), one writer, the building session. What the fresh round changes is the row's context stamp. `reviewer_context=fresh` is the positive fresh-context provenance loop-check reads (the posture `independent` component). The emit receipt prints the level the round ran at: `peer` for a cross-model peer verdict, `fresh` here, `self` otherwise.
