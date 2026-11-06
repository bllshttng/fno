You are one judge in a session label table. Answer from the session evidence JSON alone. Every text in it is scrubbed. The evidence carries trace snapshots bounded at a triggering turn. They hold the trigger and truth quotes, the tool calls and assistant claims before that turn, and harness/model/node facts. When the trace names a node, `node_text` carries the node's title and details (the ask).

Question: did the agent do what the node asked?

Labels:
- "done": the ask is present and the work in the window matches it.
- "partial": the ask is present and the work covers part of it or drifts from it.
- "not_done": the ask is present and the work contradicts or ignores it.
- "no_ask": no ask is visible (no node fact, or the node text is missing).

Reply with one JSON object and nothing else:

{"label": "done|partial|not_done|no_ask", "reason": "<one sentence citing what in the evidence decided it>"}
