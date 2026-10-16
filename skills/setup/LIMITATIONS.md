# Setup limitations

## Known Limitations and Deferred Work

- The wizard needs a terminal on stdin: a non-TTY run is refused, and an unattended machine uses `fno config setup run --dry-run --json` instead.
- An approve-kind step always waits for a person to press the key; the wizard never self-approves a step.
- The report read trusts two checks (fresh `run_id`, final `status`); a report left mid-run by a killed wizard keeps the running stub and is flagged, not parsed.
