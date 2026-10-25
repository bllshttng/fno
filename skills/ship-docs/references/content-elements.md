# Content Elements

Reusable patterns for how-to guide content.

## Screenshots/UI Descriptions

```
**You'll see:**
┌─────────────────────────────┐
│  Welcome, Sarah!            │
│  [Sign In]  [Sign Out]      │
└─────────────────────────────┘
```

## Common Issues Box

```
**Common issue**: "I don't see my child's name"
**Solution**: Ask the facility to verify your phone number matches their records.
```

## Pro Tips

```
**Pro Tip**: Complete this step while in the parking lot to save time at drop-off.
```

## Reader Testing

Use fresh reader subagents with no author context. Give each reader the page text or a neutral copy outside the repository.

Do not provide repository paths, source files, or this conversation. Readers must not fill gaps from product knowledge.

Run two independent probes:

1. Ask the reader to perform the task step by step. Require exact actions, expected results, and every missing value or prerequisite.
2. Ask five to eight practical questions. Require a supporting sentence for each answer, labeled CONFIDENT, INFERRED, or NOT IN PAGE.

```
Read this guide as a [audience member] who has never used the app.
Use only the supplied pages. Do not inspect product source.
Describe each action, expected result, and any guess you must make.
End with DONE, STUCK at a named step, or DONE WITH GUESSES.
```

Common issues reader testing catches:
- Missing navigation instructions ("How do I get to Settings?")
- Assumed knowledge ("What is a compliance score?")
- Confusing button states ("Is gray active or inactive?")
- Missing "who does what" clarity ("Do I enter this or does staff?")

Fix missing answers, incorrect confident answers, and guesses needed to finish the task. Then rerun the probes.

After individual pages pass, walk the complete path in order against one machine, workspace, and set of files.

Check that earlier steps leave the state later steps require. Put policy checks before actions they govern, and cleanup after dependent walkthroughs.

Run mutating examples in disposable state. Record the real command, version, exit code, and relevant output before publishing an example.

Distinguish a launch receipt from a completed task. A queued message is not a delivered response.

If a check is blocked, record it as unverified. A successful preview response does not prove visual inspection passed.

## Writing Principles

- **Use visible detail**: Name the exact button, field, or screen the reader needs
- **One step at a time**: Break complex flows into atomic steps
- **Anticipate errors**: State each visible error and its recovery action
- **Use their words**: "Sign in your child" not "Initiate attendance record"
- **Visual markers**: Describe the result of each step
