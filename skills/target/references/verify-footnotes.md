
# verify-footnotes

Every premise a task inherits, and every claim a worker makes, carries its footnote: a file:line, a command output, or a source link. Three sections, keyed to the moment: premises before the first edit, claims before a figure reaches a reader, done before any completion report.

## 1. Premises (before the first edit)

Before the first source edit, list what the node, the plan and the brief assert about the code or the product, and measure each. Skipping the measurement builds on a premise that can be false in a way that looks correct all the way to the PR.

Restate each premise as one sentence a measurement can contradict. "The gateway is inert" measures nothing. "No non-test file calls `gateway.apply`" does. A premise that bundles several claims splits, and each part is measured alone, because claims are usually right about some parts and wrong about the part that matters.

Ask what invokes it, not whether it exists. Existence is not reachability. Find the definition, then follow callers outward until the chain reaches something that runs: a handler, a CLI entry, a CI workflow, a scheduler. A live file can carry a function nothing calls, and a caller can itself be uncalled. "The file has a caller" is not "the function has a caller".

Declare the denominator. State what you searched across and how many files it holds. Then prove the search could have matched: run the same pattern against the definition itself, or a known reference, and require a hit. A search that cannot match its own target reads exactly like a true negative.

Run both arms on a guard. Present the input the guard must refuse, and quote the refusal text. Present valid input, and require it to pass. A guard that refuses everything is broken in the other direction, and only the admit arm tells the two apart.

Verdict each premise: CONFIRMED, REFUTED, PARTIAL, or UNMEASURED. Every verdict carries file:line and the command or read that produced it. A verdict without its denominator is an opinion.

- CONFIRMED: the premise held under measurement.
- REFUTED: the premise is false; say what is actually true. The build stops: a `help` with class `stale-plan`, or a note on the node. Never a silent build past a refuted premise.
- PARTIAL: some parts held, some did not; enumerate both.
- UNMEASURED: no read can settle it. A product call, a taste ruling, or a conflict between two named sources is decided by the owner, not by a search. It goes to the question door before any build. It is never CONFIRMED by default.

Run the pass as a reader that cannot edit. On Claude Code, hand the premise list to a forked `Explore` subagent and measure there. On a harness with no read-only reader, run it inline under a written no-edit rule. The measurer never proposes the fix; the verdict is the deliverable.

Worked example. A node asked for a 30-second GIF of one real target run at the top of a README. The growth plan beside it budgeted a founder video of one real run. A worker measured nothing, built a terminal replay of an old run, and the PR merged unseen: the two sources never agreed on what the asset was, and a replay of a log is not one real target run. Section 1 over that node's premises reads the plan beside the node, finds the conflict, and stops at UNMEASURED before any build.

## 2. Claims (before a figure reaches a reader)

A number may not carry an adjective nobody measured. The data is usually right; the sentence wrapped around it is what fails. Run all five checks before any figure reaches a reader, in a PR body, mail, docs, or chat.

1. Name the field from its definition. Point at the line of code or documentation that defines it, and import the meaning from there. Never retype it from memory.
2. State the scope with the figure, or do not state the figure. Which rows, selected by what rule, over what period. If a re-run at a different reasonable scope gives a different number, the sentence must say which scope produced yours.
3. Mark filed against computed. A value the source itself asserts and a value you derived are different kinds of fact; label which is which. Name-matching is always inference, and it is said in the same sentence.
4. Look for the self-disproof. Try to break the claim with the data already on screen before reaching for an outside source. The strongest disproofs are internal.
5. Include the figure that argues against you. An adversary finds it in the first minute, and an omission next to named facts reads as deliberate.

A negative claim names the surface searched. "The published files carry no index" is checkable and survives; "it does not exist" is not checkable and invites the attack.

## 3. Done

No done claim without fresh command output, read in this turn. The command that proves the claim runs now, and its output with the exit code sits beside the claim. A previous run, a partial check, and another agent's success report prove nothing.

The stop hook, not this section, decides completion: `fno-agents loop-check` reads the world (the PR exists, CI is green, reviews are drained) when a promise is seen. Run the local gates, then let it read the world.

Section 3 adapts the verification-before-completion skill from obra/superpowers (MIT); no sentence is copied.

## Known Limitations and Deferred Work

- On harnesses with no read-only reader, the premise pass runs inline under a no-edit rule, which is discipline, not a sandbox. See [LIMITATIONS.md](LIMITATIONS.md).
