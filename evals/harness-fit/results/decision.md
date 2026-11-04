# Decision: harness fit for glm-5.3-flash

Rule 1 fires. opencode ties Claude Code on accepted changes and costs 37% less per accepted change. The recommendation is a GLM routing change toward opencode. It is a recommendation for the user. This study edits no production lane.

The rules apply in order (README, "Decision rules"). Rule 1 is checked first and fires, so rules 2 and 3 are not reached. Every number below is from the iMac rows in `run-0.md` and `run-1.md`, in the record Amendment 13 defines. Two earlier decisions used a scorer that misread ordinary timeouts as rate limits. Both also fired Rule 1, and they sit in their own section below. The arm64 pilot fires no rule.

## Rule 1: opencode or zcode against Claude Code

The rule: "If opencode or zcode beats Claude Code on accepted changes, recommend a GLM routing change. A tie counts only with 30% lower cost per accepted change."

zcode is `unavailable` in both runs, so only opencode is tested.

### Accepted changes: a tie

- Run 0 pairs the 88 tasks both arms graded. opencode minus Claude Code is +5.7 points, 95% interval -3.4 to +15.9. opencode alone passed 12 of them, Claude Code alone 7.
- Run 1, paired over the 10 tasks: opencode minus Claude Code is +3.3 points, 95% interval -10.0 to +20.0. opencode accepted 2 of 30, Claude Code 1 of 30.
- Both intervals hold zero, and both differences sit inside the 15-point noise band. Neither arm beats the other.

### Cost per accepted change: opencode is 37% lower

- Run 0, all spend in the record over passes: opencode $2.58 for 56 passes is $0.0461 per pass. Claude Code $3.75 for 51 passes is $0.0736 per pass. opencode is 37% lower.
- Both arms have more than 20 graded trials (opencode 88, Claude Code 89). Each has 2 trials with no usage.
- Run 1 points the same way: $0.66 against $10.17 per accepted change. Stalls carry no usage in Run 1, so those figures are floors. They cannot carry the rule.

So the tie counts, and Rule 1 fires on Run 0's numbers.

## How firm the cost margin is

The 30% line holds on all four ways to count spend.

| Basis | opencode per pass | Claude Code per pass | opencode lower by |
|---|---|---|---|
| All spend in the record (the preregistered metric) | $0.0461 | $0.0736 | 37% |
| All spend, with the attempts outside the record | $0.0569 | $0.1010 | 44% |
| The 88 tasks both arms graded | $0.0453 | $0.0713 | 36% |
| Graded trials only | $0.0453 | $0.0736 | 38% |

The gap is not rate limits. Rate limits touched 17 original attempts across every arm, 12 of them opencode's, and 2 of Claude Code's. Claude Code costs more per pass because it spends more on each trial, including the 27 that ran to their timeout.

## Caveats

- Claude Code timed out on 27 of 89 Run 0 tasks. Those count as failures. The other arms timed out on 10 to 33.
- Claude Code's original arm ran last, from 05:36Z to 12:00Z on 2026-10-03. So its hours of the day differ from the other arms.
- The original arms ran 5 GLM sessions at once on one shared z.ai account. That was 4 Run 0 trials and 1 Run 1 attempt. The retries ran 4 at once, with Run 1 finished.
- Run 1's bank is hard for both harnesses: 3 accepted changes in 60 scored attempts. Run 1 agrees with Run 0 but adds little power.
- zcode was never measured, and pi had no Run 1 lane. Rule 1 says nothing about either.
- Prices are list prices for glm-5.3-flash (manifest). One machine, one model, one pass of Terminal-Bench 2 plus one retry of each rate-limited trial.

## Earlier decisions (substring test)

Both earlier decisions fired Rule 1 on the same tie. Their scorer read any `1302` in an agent log as a rate limit (Amendment 13).

- First results (commit 71151e0a50): on 59 shared tasks, opencode minus Claude Code was -1.7 points, 95% interval -10.2 to +6.8. The cost margin was 37% on all spend, 39% on shared tasks and 18% on graded trials only.
- Second results, after Amendment 12 (commit 2a139fc2b8): on 63 shared tasks, -1.6 points, 95% interval -9.5 to +6.3. The margin was 39% on all spend, 39% on shared tasks and 20% on graded trials only.
- Both blamed Claude Code's cost on rate limits. The corrected record shows ordinary timeouts instead. The graded-only margin now clears the 30% line too.

## Recommendation

Route GLM coding work through opencode lanes in `config.routing.models`, at the same model and effort. Claude Code on GLM solves about as many tasks, at higher cost per solved task. The user decides. No production config was changed.

A cheaper follow-up can test the margin without a new study. Find why Claude Code runs 27 tasks to their timeout on GLM at effort high. The 2026-10-02 smoke streamed about 40,000 thinking tokens in one turn. Then re-run its Run 0 arm with the cause fixed. If its cost per pass falls within 30% of opencode's, the routing case weakens.

## The footnote arm: the Amendment 14 gate

Verdict: footnote minus opencode is 0.0 points on the covered tasks, PASS at the -15.0 bar. The paired 95% bootstrap interval is 0.0 to 0.0 (4,000 resamples, seed 272). The interval is degenerate because both arms scored zero on every covered task: the comparison reads two floors, not a margin. Footnote's alone rate sits at 0.0% over 3 scored attempts, Wilson interval 0.0% to 56.2%. Opencode's recorded rate on the same tasks is 0.0% over 9, interval 0.0% to 29.9%.

The lane ran on imac2d2 from 02:22Z on 2026-11-04 under approval q-e3937891 option 1, capped at 25 attempts. It covered 3 of 10 replay tasks with 7 attempts. The driving worker's session died mid-chain, and the lane stopped at 05:42Z after its last driver invocation finished. No worker remained to seat the next task. The smoke passed every Amendment 14 term before any paid attempt. The rows, exclusions and spend live in `results/run-1.md`, footnote arm section.

The exclusions, counted: 5 pre-lane rows score as smoke noise. The thread named 3: 2 spawn refusals and 1 bypassless graded attempt. Transcript headers show 5, and the extra 2 also read `permission_mode: default`. In the lane, 1 attempt died on a registry lock before starting. Three attempts on replay-x-5978 hit the harness budget at about 45 minutes and never graded. Reading those 3 as stalls per Amendment 4 gives footnote 0 of 6 scored instead of 0 of 3. The paired difference stays 0.0 points.

This arm is underpowered by the record's own rule: under 20 graded attempts fires nothing alone. The verdict certifies parity at the floor on the covered tasks and nothing more. The remaining 7 tasks need a fresh approval and a seated worker.

Spend: $2.97 in the paid lane, $3.17 in smoke-phase noise, $6.15 across the footnote lane. The node's spend probe read all Run 0 jobs at $10.17, so committed spend is about $16.32 against the $200 ceiling.
