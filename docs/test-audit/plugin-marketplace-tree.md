# Marketplace tree test audit

Audited owners: `scripts/ci/check-plugin-tree.sh`, `plugin_install::tests::stage_serves_fno_entry_locally_without_drift`, `cli/tests/unit/test_join_partition_hook.py`.

The CI acceptance script owns the package and release contracts. It builds a real source archive, exercises the 5000-file boundary and forbidden entries, installs through Codex, and validates through Claude. Removing the filter or restoring repository publication fails these assertions. A bare Git remote and an existing clone prove update ancestry, including migration from the old full repository. The stale-publisher case proves refusal without changing the tip. These tests use the production builder and publisher without a test-only production seam.

The existing Rust stage test gains the Codex marketplace case. When the public manifest changes to an external source, this test protects local developer installation. The existing freshness assertion verifies that rewritten bytes remain fresh. Test count stays at 24 in `plugin_install.rs`.

The join-policy suite retains ownership of policy decisions. The new CI case covers the separate package invariant: the actual hook must deny a forbidden write from a read-only generated tree without invoking uv. Source-version warning probes cover matching and mismatching binary identities. No additional Rust or Python test function is added.

The postinstall smoke drops its exact unquoted-command substring check. The context census and manifest tests cover the live registration. Source smoke validates the marketplace manifest. Strict plugin validation runs against the generated artifact in CI, because the source repository's root context file does not ship.

Local validation covers the existing join-policy suite, Rust formatting, standalone builder compilation and shell syntax. The generic `fno doctor test` runner treats a Rust source path as a pytest argument and exits 4. Rust execution and all package/publication acceptance run in CI.
