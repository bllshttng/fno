# SUMMARY - the doctor family port

Branch `feature/x-975c-port-doctor-family-bundle`, seven commits on top of
origin/main.

## What shipped

The `fno doctor` family answers natively. `crates/fno/src/doctor/` holds the
front-door claim (`classify`/`run`), the report tree (collectors, advisors,
ops, transports, a codex/ directory module, the plugin-hooks probe), the
codex bind canary, and the human renderer. The front door claims bare
`doctor`, `doctor bundle`, `doctor plugin-file`, and four flag arms
(`--find-transcript`, `--codex-app-server`, `--codex-hooks`, `--codex-bind`);
`intel`/`stalls`/`harness`/`harness-matrix` route to the fno-agents binary;
unknown doctor leaves forward to Python byte-verbatim until their families
port. The Python legs are deleted (`doctor.py`, `doctor_cli.py`, `bundle/`,
the lazy cli.py entries and their collapse keeps); the update signpost moved
to `rust_binary.py`; the two importer call sites (the mail drain receipt's
socket probe and the mux recipient-transcript lookup) read the native flag
arms.

7 golden cases (18 files) frozen from the Python legs under a pinned empty
machine gate `crates/fno/tests/doctor_parity.rs`. The suite lives in the fno
crate's own test target because `CARGO_BIN_EXE_fno` resolves only there, and
it copies the fresh binary to an isolated dir per case so the
sibling-of-binary lookup finds no `fno-agents`, matching the capture machine.

## Known divergences (each deliberate)

- **Parity suite path.** crates/fno/tests, not the fno-agents tree the plan
  named; `CARGO_BIN_EXE_fno` resolves only inside the fno package's own test
  target.
- **Review-invocation scan.** The native review-gap scan reads the review
  journal raw jsonl only; the db-only rows Python also folded are
  undercounted, with no settle sweep.
- **Bind canary argv.** The canary launches codex with a canary-local,
  env-pinned argv rather than the production pane builder; the production
  build stays spawn-lane property.
- **Pre-push bound.** The pre-push hook check bounds the script at 5s where
  Python gave 15s; the script is a check that answers in well under a second
  when healthy.
- **codex_marketplace_duplicates.** The native report never produces this
  finding; it keyed on the legacy dev-marketplace row shape no current
  install writes.
- **Native binary resolver.** The bundled-wheel leg (`<package>/_bin`) is
  absent from the native resolver; PATH and the launcher-sibling lookups
  cover the installed shapes.
- **yaml-only settings.** The native config readers read the toml and json
  layers; a yaml-only settings file reads as absent.
- **Forwarding window.** Doctor-prefixed foreign leaves (`codemap`, `evals`,
  `test`, the `lint` subcommands beyond style and menu-caps, and the rest)
  forward to the installed wheel until their families port; a wheel released
  after this PR answers `fno doctor <anything>` on Python-only installs with
  the signpost teaching, per the plan's signpost acceptance. The families
  must land before that release.
- **The Python forward (plan bug fixed inline).** Change 2 (foreign leaves
  forward to Python) and change 5 (a hidden doctor entry signposts any
  `fno doctor` spelling) contradicted each other: the signpost swallowed
  `fno doctor test` and the 490+ documented doctor-prefixed spellings, and
  five CI lanes proved it (command matrix, real-codex, packet-size, publish
  dry-run, two-machine). `cli/src/fno/doctor_forward.py` is the fix: it
  mounts the foreign leaves exactly as the deleted group did, and signposts
  the family's own leaves. Its rows die in the PR whose family ports the
  leaf.

## Expected CI findings, pre-declared

- The seam-crossings lint should not move: the new spawn sites pass argv
  variables, not `Command::new("fno")` literals, and no resolver env keys or
  `join("fno")` lines were added; if the shape match disagrees, the new
  sites need classification rows.
- About 4,700 lines of new Rust compile for the first time in CI (local
  builds are frozen by operator order); expect a fix loop on first red.

## Verification status

- rustfmt --check is clean on every new file at this head; the local
  cargo build/test/clippy freeze holds, so CI is the first compile.
- Parity provenance: the header declares `parity-stage: characterization`
  with oracle `fno.doctor`, a module the deletion removed, per the
  provenance rule; the gate itself runs in CI.

## Late folds and rounds

- **The evals health door (fold 2026-10-26).** Main's evals port (#3905)
  moved the deleted leg's evals section onto the native `evals-trend op
  health` door; the fold brings the same convergence into the port: the
  collector calls the health op with `evals.stale_days` from the config
  ladder and passes the door's payload through, no doctor-side re-fold. The
  empty-machine golden pins null either way.
- **Round 10 (CI batch).** The three ported reports (plugin_cache_report,
  auto_merge_review_gap, merge_gating_optout_report) land; the bind canary's
  write-only condition sink drops and the never-assigned last_alive write
  lands (expired binds now report the last observed pane aliveness, a port
  bug the deny-warnings gate surfaced); the pytest skip-site ratchet drops
  the row the test_hooks_json_validity path fix un-skipped.
