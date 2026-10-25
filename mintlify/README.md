# Maintain the documentation site

This directory is an additive Mintlify site. The existing docs/ tree remains available in the repository.

Run the preview from this directory:

```bash
npx --yes mint@4.2.1125 dev --no-open
```

The preview prints its local URL. Stop its process after inspection.

Check links and anchors before pushing:

```bash
npx --yes mint@4.2.1125 broken-links --check-anchors
```

The Mintlify workflow runs that check on site changes. The public-reference guard also scans this entire directory.

## Add a page

Give each page one reader, one task and one end state. Register its extensionless path in docs.json.

Use current source and real CLI output for facts. Keep command, version, exit code and relevant output in the PR evidence.

Test a mutating example in disposable state. Do not change a live worker or the operator's configuration to illustrate a command.

## Test the reader path

Give a fresh reader only the pages and their linked pages. Ask it to perform the task and answer practical questions.

Fix missing steps, undefined terms and contradictions, then repeat. Walk the complete path against one machine and repository after individual pages pass.

Put merge-policy checks before launching a target. Keep the practice worker available through messaging, then stop it at the end.

Distinguish a launch receipt from a completed result. A queued message or a ready preview is not proof of delivery or visual inspection.

Record any verification you could not perform in the PR. Do not turn a refused browser check into a visual pass.
