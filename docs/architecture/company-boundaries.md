# Company Boundaries

## What the check enforces

`bash scripts/ci/check-company-boundaries.sh` is the gate registered in `fno doctor test`. It enforces the pack boundary described under [Pack projections](#pack-projections). It takes an optional repository root and no mode.

The check once also scanned Python imports under `cli/src/fno/` against a declared layer map, with a strict audit and a checked-in baseline. That scan retired with the Python runtime. Its baseline had already drained to no prohibited edge and no layer cycle, and its last mapped modules (`fno.paths`, `fno.events.store_client`, `fno.rust_binary`) left with the runtime. The Rust `TOPOLOGIES` list in `crates/fno-agents/src/plugins/verify.rs` owns the topology words.

The check enforces no import edges for the Rust crates, the skills (Markdown and shell), or the mux. They are uncovered seams, not clean boundaries.

## Extraction gate

The extraction threshold is two real consumers; the observed count is one, so the gap is one and the gate is closed.
The growth-studio pack delivered in PR #732 is the one real consumer.
The planned second consumer in the ReadyRule web repository was never built: there is no branch and no pull request.
The three company-conformance scenarios are internal, share an author and test harness with the contracts they exercise, and therefore do not count as the second consumer.
No physical package split, repository split, or published extraction is authorized by this work.

## Pack projections

A repository-root agent or skill whose first frontmatter block contains `pack: <id>` is a build-time projection of `plugins/<id>/plugin.yaml`, not an independent surface.
The boundary check reads the marker with the same frontmatter rule as the skill-bundle freshness gate, attributes a valid projection to its pack, and fails a marker whose pack manifest does not exist.
A root file with no `pack:` marker remains an independent surface, which is the fail-safe direction when frontmatter is malformed.
