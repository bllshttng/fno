---
name: setup
description: Set up a machine for Footnote by launching the interactive setup wizard and reading its report.
---

# Setup

Setup is `fno do wizard`, an interactive terminal program. It detects the machine, measures a concurrency band, probes each harness and model, and writes the config in one grouped apply. This skill launches the wizard and reads the report. It asks no setup question itself.

## Launch inside the mux

When `FNO_PANE` is set, open a split beside this pane and run the wizard there:

```bash
fno mux pane run --at current --split right --json -- fno do wizard
```

Keep the pane id from the receipt. Tell the person the wizard is in the split and that they answer there. Wait for the run to end:

```bash
fno mux pane wait <pane> --pattern 'wizard-report: ' --timeout 1800
```

`fno mux pane read <pane>` shows the wizard screens when the person asks what it shows. `fno mux pane send <pane> --text <key> --raw` types one key. Send a key only when the person asks for that exact answer, never to pick for them.

## Launch outside the mux

A session with no pane cannot host a terminal program. Print the command for the person to run in their own terminal:

```
fno do wizard
```

Do steps "Read the report" and "Say the result" below when they say the run is done.

## No person at all

An unattended machine cannot answer the wizard. Run the batch probe and report it:

```bash
fno config setup run --dry-run --json
```

Apply nothing of kind `approve`. An approve needs a person.

## Read the report

The run writes `<state_dir>/sidecar/wizard-report.json` and prints `wizard-report: <path>` as its last line. Check two things before you speak for the run: `run_id` is newer than your launch, and `status` is not `running` (a run writes a running stub first and the final report over it). Then say three things: the red rows with their fix, the files written, and `next_action`.

## Modes

| Invocation | Effect |
|------------|--------|
| `/setup` | launch `fno do wizard` |
| `/setup join <file>` | launch with `--join <file>`: apply a profile exported on another machine |
| `/setup check` | run `fno config setup run --check --json` and show the drift with each fix |

## The keys the wizard does not ask

For the advanced config tier and the post-merge parking lot, `fno config setup plan --advanced` and `fno config setup post-merge` remain the verbs.
