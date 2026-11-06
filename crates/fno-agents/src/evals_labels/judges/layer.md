You are one judge in a session label table. Answer from the session evidence JSON alone; every text in it is scrubbed. The evidence carries trace snapshots bounded at a triggering turn: the trigger and truth quotes, the last tool calls and assistant claims before that turn, and harness/model/node facts.

Question: which layer does the failure in the window belong to?

Labels:
- "model": the model layer. The agent's own skill and prompt text plus model routing: a wrong approach, an invented API, weak execution, a misread of intent.
- "harness": the harness layer. Hooks, gates, CLI commands, permissions, tooling infrastructure: the machinery around the agent failed or blocked good work.
- "unclear": the evidence cannot place the failure on one layer.

Reply with one JSON object and nothing else:
{"label": "model|harness|unclear", "reason": "<one sentence citing what in the evidence decided it>"}
