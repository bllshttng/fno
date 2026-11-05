# Investigate phase

The blueprint names routes and unknowns. It does not pick the route. This phase settles every unknown and picks the route before the first source edit. It runs in this session, between init and implement.

## Triggers

- The bound plan has a `## Unknowns` section with a `- [ ]` line.
- The node, the plan or the brief asserts premises about the code or the product. Reach for `fno:verify-footnotes` and run its Premises section over them before step 1.
- The node is a bug, or the plan lists the cause as an unknown. Then reach for `fno:fix` in `investigate` mode for the cause before anything else.
- No plan is bound and the node details state a cause or a caller set that nobody measured. Measure it first, and say in the first commit body what you measured.

A plan whose `## Unknowns` reads `none: <why>` skips to implement.

## The steps

0. List the premises the node, the plan and the brief assert. Measure each with the Premises section of `fno:verify-footnotes`. A REFUTED premise takes the step 5 door. A premise no read can settle gets UNMEASURED and goes to the question door before any build.

1. For each `- [ ]` line, run its `measure:` command or read. One unknown at a time.
2. Write the finding in the plan: `- [x] <question> - settled: <finding, with file:line or the command output>`. A finding that contradicts the plan is the point of this phase. Record it. Do not bend it to fit the plan.
3. A measurement that opens a new question adds a new `- [ ]` line. Settle it the same way.
4. Under `## Routes`, append `chosen: <n> - <why>`. Name the settled unknown that ruled out each other route.
5. When no route survives, emit `<help class="stale-plan" reason="no route survives the measured facts" evidence="...">`. Never invent a fourth route in silence.

The plan is the record. A later reader sees what was measured and why the route won, without the transcript.

## The ship gate

`fno do pr create` reads the session's bound plan and refuses with exit 6 while one `- [ ]` line stays under `## Unknowns`. The refusal names each open line. Settle them. Never delete a line to pass. A deleted unknown is an unmeasured fact shipped as a measured one.

## Red CI mid-run

When CI goes red after the PR opens, reach for `fno:fix` (see [toolbag.md](toolbag.md) for how to load it). It makes one fix per iteration and reverts on regression. It names the root cause before any defensive check. The skill loads inside this session, so the claim, the manifest and the loop stay yours.
