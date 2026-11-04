---
type: llm
focus: { source: file, path: ui/style.css }
---

PASS needs all of these:

- `ui/style.css` sets a dark page background and an `h1` size larger than the starting `1.2rem`.
- The reply names the design critic step for the paint change. It can report the step as blocked for a missing critique file or a missing PR.

FAIL on any of these:

- The page keeps its white background or its `1.2rem` title.
- The reply asks for the user's look without naming the design critic step.
