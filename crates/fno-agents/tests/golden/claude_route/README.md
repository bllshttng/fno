# claude-route goldens

One JSON case file per decision the route builder makes. Each case pins
`HOME` and `FNO_CONFIG` (or the project layer) to a fresh temp dir at run
time, so an answer depends only on the case's own inputs; the `{keys}`
placeholder in a config template becomes the temp keys.env path, and
`{tmp}` inside an expected value becomes that temp dir.

Re-capture recipe (run from a checkout whose Python leg still carries
`_route_for_target`, i.e. any head before the deletion):

    PYTHONPATH=cli/src uv run --project cli python <capture-script>

The capture script reads input-only skeletons (files with `"expected": null`),
calls `_route_for_target` / `effective_providers` / `key_source` through
`load_settings` with the case's pinned env, and rewrites each file with the
expected answer. Delete the `expected` and `expected_providers` blocks to
re-capture a case.

Driven by `tests/claude_route_parity.rs` (`parity-stage: characterization`,
`parity-oracle: fno.agents.model_routing._route_for_target`): the test locks
the process env, rebuilds each case's temp world, runs
`fno_agents::provider_route::claude_route` / `providers_json`, and compares
against `expected` (and `expected_providers` when the file carries it).
