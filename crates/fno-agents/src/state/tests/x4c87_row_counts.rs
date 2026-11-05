//! x-4c87 raw-versus-decoded row count family: moved verbatim out of state.rs
//! (file budget shrink). Parent helpers resolve through the glob.
use super::*;

// ------------------------------------------------------------------
// x-4c87: the raw-versus-decoded row count invariant. The live outage's
// sanitized shape: real worker rows at the CURRENT schema, one carrying a
// value the typed model cannot represent. (On the machine it was measured
// on, the divergence was versional: a stale v11 daemon meeting the v14
// store; the same-schema unknown-status row below drives the identical
// code path -- typed decode fails with rows on disk.)
// ------------------------------------------------------------------

/// A sanitized 3-row registry whose middle row the typed reader cannot
/// represent. Keys and value types mirror a real worker row; names, ids,
/// and paths are synthetic. The broken row CARRIES a well-formed provenance
/// block, so the provenance-strip heal must refuse it: the failure is
/// elsewhere, and the same-schema read stays fatal.
fn divergent_registry_fixture() -> String {
    // One session id per row: the table keys rows on it.
    let row = |name: &str, status: &str, extra: &str| {
        format!(
            r#"{{"name":"{name}","cwd":"/tmp/proj","harness":"claude","harness_session_id":"11111111-2222-3333-4444-{name:0>12}","status":"{status}","created_at":"2026-08-16T00:00:00Z"{extra}}}"#
        )
    };
    let valid_provenance = r#","spawn_provenance":{"origin":{"kind":"session","parent":{"harness":"claude","session_id":"p-sess","cwd":"/w"},"invocation":null},"owner":{"kind":"session","harness":"claude","session_id":"p-sess","cwd":"/w"}}"#;
    format!(
        r#"{{"schema_version":{},"agents":[{},{},{}]}}"#,
        REGISTRY_SCHEMA_VERSION,
        row("worker-alpha", "live", ""),
        row("worker-beta", "hibernating", valid_provenance),
        row("worker-gamma", "live", "")
    )
}

/// x-d19e wording contract, scoped to text this change introduces: a
/// refusal names what to verify and never advertises an override.
fn assert_no_override_remedy(msg: &str) {
    for banned in ["force", "skip", "ignore", "bypass", "no-verify"] {
        assert!(
            !msg.to_lowercase().contains(banned),
            "diagnostic must not name an override remedy ({banned}): {msg}"
        );
    }
}

#[test]
fn registry_with_unrepresentable_row_errors_naming_both_counts() {
    // AC3-ERR: a positive raw row count whose typed decode fails is an
    // InvariantViolation carrying the path, both counts, and the
    // comparison to run -- never a successful empty roster.
    let dir = tmpdir("divergent-row");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("registry.json");
    crate::registry_store::seed_raw(&path, divergent_registry_fixture());

    let err = load_registry(&path).expect_err("unrepresentable row must fail the read");
    let msg = err.to_string();
    assert!(
        matches!(err, StateError::InvariantViolation(_)),
        "got: {msg}"
    );
    assert!(
        msg.contains(path.to_str().unwrap()),
        "names the path: {msg}"
    );
    assert!(msg.contains("raw_rows=3"), "names the raw count: {msg}");
    assert!(
        msg.contains("decoded_rows=0"),
        "names the decoded count: {msg}"
    );
    assert!(
        msg.contains("inspect"),
        "tells the operator to inspect: {msg}"
    );
    assert!(
        msg.contains("compare"),
        "names the comparison to run: {msg}"
    );
    assert_no_override_remedy(&msg);
    std::fs::remove_dir_all(&dir).ok();
}
