# Beat by harness

The lead has one push contract on every harness. When a covered PR settles green, the daemon's `lead_settle` arm mails the lead. When a covered node merges and closes, it mails again. The beat column is data: each harness row's `beat` in `crates/fno-agents/src/harness_capabilities.toml`, read through `fno-agents harness-beat` while `agents.<harness>.beat` is `auto`. The daemon lead-wake mails an overdue lead on every harness. An unverified cell is an explicit gap, not a claim.

| Harness | Beat | Heartbeat |
|---|---|---|
| claude | `loop` | Mail wake lands as a conversation turn, and `/loop 55m` is the cheap heartbeat the role arms itself. `internal/claude/docs/code/scheduled-tasks.md:33`. The row flips to `daemon` once the wake has a delivery bound past the user-typing hold and SessionStart reports a stopped daemon. |
| codex | `goal` | Mail wake lands through the fno Codex RPC lane. No in-thread timer is documented. App automations start a new run. `Codex best-practices.md:176` |
| opencode | `daemon` | Mail wake lands through the fno opencode serve lane. No native timer is verified. `internal/opencode/docs/ecosystem.md:44` |
| grok | `daemon` | unverified |
| agy | `schedule` | Mail wake lands through the ask surface. The harness documents a recurring `schedule` tool with `CronExpression` and `Prompt`. `internal/agy/docs/Hookslink.md:135` |

## Launch by harness

Seat a claude worker with its own line: `claude --bg --name <name> '/fno:target <node>'`. Seat every other harness through `fno agents spawn`, because none of them has a paneless native start. The harness choice table, the glm route and one page per harness live in the agent skill: [Launch a worker](../../agent/SKILL.md#launch-a-worker).

deepseek has no capability row, so no launch line is verified. Run `fno agents admit capabilities deepseek` before you seat one, and `fno doctor harness deepseek --live` before you call the lane supported.

## Triage lane

If the user authorizes model triage, run `claude --bare -p --model haiku --output-format json`. Bare mode skips PreToolUse guards. Keep it read-only or act only through fno verbs. Wake the lead with one line naming the reason. A bare session never meets the spawn gate on a native line, so this lane launches agents only through `fno agents spawn`. No lead step calls this lane today. `--fallback-model sonnet,haiku` / `fallbackModel` is the user's Opus-outage setting, not a new lead arm.
