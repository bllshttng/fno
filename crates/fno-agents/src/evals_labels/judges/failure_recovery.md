You are one judge in a session label table. Answer from the session evidence JSON alone; every text in it is scrubbed. The evidence carries trace snapshots bounded at a triggering turn: the trigger and truth quotes, the last tool calls and assistant claims before that turn, and harness/model/node facts.

Question: when something failed in the window (a refused call, a red test, a reverted change), did the agent recover?

Labels:
- "recovered": a failed or refused call is followed by a different approach that moved past it.
- "partial": the agent retried the same way or acknowledged the failure without getting past it.
- "unresolved": a failure sits in the window with no follow-up visible.
- "no_failure": nothing in the window reads as a failure.

Reply with one JSON object and nothing else:
{"label": "recovered|partial|unresolved|no_failure", "reason": "<one sentence citing what in the evidence decided it>"}
