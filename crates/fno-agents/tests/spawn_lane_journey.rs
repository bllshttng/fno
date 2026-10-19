//! Lane journey tests: the backend dispatch table
//! resolves every supported (harness, substrate) combination through the
//! packaged capability contract and refuses unsupported ones by name.

use fno_agents::spawn_backends::{resolve_backend, ResolvedBackend};

#[test]
fn each_supported_substrate_resolves_to_its_lane() {
    for (harness, substrate, want) in [
        ("codex", "thread", ResolvedBackend::CodexThread),
        ("claude", "pane", ResolvedBackend::MuxPane),
        ("claude", "headless", ResolvedBackend::ClientOneShot),
    ] {
        assert_eq!(
            resolve_backend(harness, substrate),
            want,
            "{harness} {substrate}"
        );
    }
}

#[test]
fn unsupported_combinations_refuse_by_name() {
    // A harness the contract says hosts its own detached thread client gets
    // the honest refusal, not a silent fallback; an unknown substrate names
    // itself.
    for (harness, substrate, needle) in [
        ("opencode", "thread", "opencode"),
        ("agy", "thread", "agy"),
        ("claude", "teleport", "teleport"),
    ] {
        match resolve_backend(harness, substrate) {
            ResolvedBackend::Refused(reason) => {
                assert!(reason.contains(needle), "{harness} {substrate}: {reason}");
            }
            other => panic!("{harness} {substrate} must refuse, got {other:?}"),
        }
    }
}
