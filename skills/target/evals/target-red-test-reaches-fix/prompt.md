---
description: A target run that meets a red test should reach for fno:fix, though the prompt never names it.
max_turns: 40
timeout_seconds: 900
allowed_tools: [Read, Glob, Grep, Skill, Bash, Edit, Write]
---

Take this from idea to a pull request ready to merge: add a `logout` function to app.py that removes a user's session. Do it end to end.
