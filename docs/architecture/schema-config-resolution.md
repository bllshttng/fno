# Schema and config resolution from a foreign cwd

How `fno` finds its events schema and resolves project and config paths when it runs from a directory that is not the footnote checkout, and why those two concerns stay decoupled.

## The problem

`fno` ships inside the footnote plugin but runs from any repo. Two resolution paths once broke from a foreign cwd.

1. **Schema resolution.** The bash event validator loaded the schema from a file in the checkout. From a repo that did not vendor that file, it found the schema only through `FNO_REPO_ROOT` or `CLAUDE_PLUGIN_ROOT`. Neither is set in a plain terminal, so a gate write refused with `schema unavailable`.
2. **Config resolution.** The workaround an operator reaches for, `export FNO_REPO_ROOT=<footnote checkout>`, is an overloaded lever. Project and config resolution read `FNO_REPO_ROOT` first. So exporting it to fix the schema also repoints every `fno config get` at the footnote project instead of the cwd repo. That read is wrong and silent.

## The schema is compiled in

`crates/fno-agents/src/event_store/schema.yaml` is the one schema. `crates/fno-agents/build.rs` compiles it into the `fno-agents` binary, and the native judge validates every event against that copy. No file lookup happens at run time, so the cwd and the env vars cannot change which schema answers.

`scripts/lib/events-validate.sh` keeps one contract: an explicit `EVENTS_SCHEMA_PATH` that is not readable still refuses with `schema unavailable: <path>` (rc 2). A caller that names a schema never silently validates against a different one.

## FNO_REPO_ROOT scopes project and config only

`crates/fno-agents/src/paths.rs` reads `FNO_REPO_ROOT` first when it resolves the repo root, for tests and CI where git is unavailable. Since the schema no longer needs it, `FNO_REPO_ROOT` has one job: project and config scoping. Do not export it from inside another repo. It repoints `fno config get` at the pinned project.

## Files

| File | Role |
|------|------|
| `crates/fno-agents/src/event_store/schema.yaml` | the schema, compiled into `fno-agents` |
| `crates/fno-agents/build.rs` | projects the schema into the binary and the generated copies |
| `scripts/lib/events-validate.sh` | shell door to the native judge; honors `EVENTS_SCHEMA_PATH` |
| `crates/fno-agents/src/paths.rs` | `FNO_REPO_ROOT` repo-root override |
