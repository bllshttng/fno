# verify-footnotes limitations

## Known Limitations and Deferred Work

- The premise pass runs as a true read-only reader only where the harness offers one (Claude Code's `Explore` agent type). Elsewhere it is an inline pass under a written no-edit rule, which is discipline, not a sandbox.
- The shipped eval case grades with keyword matches on the answer's first line. That is a floor. A rubric judge grades the reasoning, and none exists yet.
- Claims outside the target, blueprint and review drivers answer to this skill. No gate checks plain chat or mail, and nothing mechanical reads them.
