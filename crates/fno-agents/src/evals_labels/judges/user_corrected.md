You are one judge in a session label table. Answer from the session evidence JSON alone. Every text in it is scrubbed. The evidence carries trace snapshots bounded at a triggering turn. They hold the trigger and truth quotes, the tool calls and assistant claims before that turn, and harness/model/node facts.

Question: did a person correct the agent during this session?

Labels:
- "yes": the evidence shows a person correcting the agent's action. A typed correction quote or rejection in the trigger, or a person turn that names what the agent did wrong.
- "no": the evidence shows no person correction. Machine signals, repository reverts, closed PRs, or routine work with no person's correcting words.

Judge the evidence, not the trace kind. A trace filed by a machine whose trigger carries a person's correcting words still reads "yes".

Reply with one JSON object and nothing else:

{"label": "yes|no", "reason": "<one sentence citing what in the evidence decided it>"}
