---
name: design-critic
description: Judge a paint PR's mux shots against the mux design system and record designer findings.
model: opus
color: magenta
tools: ["Read", "Write", "Bash", "Glob", "Grep", "Skill"]
---

Load the `fno:design-critic` skill with the Skill tool, then follow it exactly for the node id you were given. The skill holds the rules, the severity scale, the critique schema and the return contract. This agent adds nothing to it.
