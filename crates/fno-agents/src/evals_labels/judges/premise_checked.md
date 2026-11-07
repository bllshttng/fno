You are one judge in a session label table. Answer from the session evidence JSON alone. Every text in it is scrubbed. The evidence carries trace snapshots bounded at a triggering turn. They hold the trigger and truth quotes, the tool calls and assistant claims before that turn, and harness/model/node facts.

Question: before acting on a load-bearing premise, did the agent verify it?

Labels:
- "checked": a read, test, measurement or citation precedes the load-bearing action it supports.
- "unchecked": a load-bearing claim or action lands with no verification visible before it.
- "not_applicable": no load-bearing premise is visible in the window (routine work, or the failure is not premise-shaped).

Reply with one JSON object and nothing else:

{"label": "checked|unchecked|not_applicable", "reason": "<one sentence citing what in the evidence decided it>"}
