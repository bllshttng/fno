# verify-footnotes limitations

## Known Limitations and Deferred Work

- The premise pass runs as a true read-only reader only where the harness offers one (Claude Code's `Explore` agent type). Elsewhere it is an inline pass under a written no-edit rule, which is discipline, not a sandbox.
- The shipped eval case grades with keyword matches on the answer's first line. It is a floor; a rubric judge would grade the reasoning, and none exists yet.
- Claims outside the target, blueprint and review drivers (plain chat, mail) are governed by this skill but checked by no gate. Nothing mechanical reads them.
