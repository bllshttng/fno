# Stage-task notes runbook

Each stage task writes its artifact in the repository root, and the
grader reads it from there. Keep the runbook in the fixture: it is the
only file a task may change.

When you recieve a stage task, read the prompt twice before you commit.
The grader recieves nothing else: it reads the repo state and the commit
log only.
