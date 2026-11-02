//! Contract tests for the ported lint checks, on fixture trees. The Python
//! suites died with their legs; these hold the load-bearing contracts:
//! each scanner fires on its shape, the ratchets fail both directions, and
//! the dispatcher refuses an unknown check with the real names.

use super::*;

fn fixture(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("fno-repo-lint-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("fixture dir");
    dir
}

fn write(dir: &Path, rel: &str, body: &str) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, body).expect("write");
}

#[test]
fn dispatcher_refuses_an_unknown_check_with_the_real_names() {
    let err = parse(&["bogus-check".to_string()]).unwrap_err();
    assert!(err.contains("unknown check"), "{err}");
    assert!(err.contains("state-roots"), "{err}");
    assert!(err.contains("seam-crossings"), "{err}");
}

#[test]
fn state_roots_rule_a_fires_on_a_hand_built_path_and_both_directions_hold() {
    let dir = fixture("state-roots");
    write(
        &dir,
        "cli/src/fno/bad.py",
        "from pathlib import Path\n\ndef bad(root):\n    return root / '.fno' / 'ledger.json'\n",
    );
    let hits = state_roots::rule_a_violations(&dir);
    assert_eq!(hits.len(), 1, "{hits:?}");
    let (filename, rel, _msg) = &hits[0];
    assert_eq!(filename, "ledger.json");
    assert_eq!(rel, "cli/src/fno/bad.py");
    // The stale direction: a baselined site that no longer matches fails.
    let baseline = dir.join("scripts/ci/state-roots-baseline.txt");
    std::fs::create_dir_all(baseline.parent().unwrap()).unwrap();
    std::fs::write(&baseline, "A\tcli/src/fno/fixed.py\tledger.json\n").unwrap();
    let inv = Invocation {
        check: "state-roots".into(),
        root: dir.clone(),
        update: false,
        no_degrade: false,
        dispatch_path: None,
        providers_dir: None,
        json: false,
        live: false,
        base: None,
    };
    assert_eq!(state_roots::run(&inv), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn state_roots_rule_b_fires_on_a_zero_arg_cache_over_a_resolver() {
    let dir = fixture("state-roots-b");
    write(
        &dir,
        "cli/src/fno/cached.py",
        "from functools import lru_cache\n\n@lru_cache(maxsize=8)\ndef settings():\n    return resolve_repo_root()\n",
    );
    let hits = state_roots::rule_b_violations(&dir);
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0].1, "cli/src/fno/cached.py");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn spawn_paths_fires_on_a_hand_assembled_session_argv() {
    let dir = fixture("spawn-paths");
    write(
        &dir,
        "cli/src/fno/rogue.py",
        "cmd = ['claude', '-p', 'go']\n",
    );
    let inv = Invocation {
        check: "spawn-paths".into(),
        root: dir.clone(),
        update: false,
        no_degrade: false,
        dispatch_path: None,
        providers_dir: None,
        json: false,
        live: false,
        base: None,
    };
    assert_eq!(spawn_paths(&inv), 1);
    // The allowlisted files stay silent: a clean tree passes.
    std::fs::remove_file(dir.join("cli/src/fno/rogue.py")).unwrap();
    assert_eq!(spawn_paths(&inv), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn provider_stderr_merge_fires_without_a_justification_window() {
    let dir = fixture("stderr-merge");
    let harness_dir = dir.join("cli/src/fno/agents/harnesses");
    std::fs::create_dir_all(&harness_dir).unwrap();
    std::fs::write(
        harness_dir.join("rogue.py"),
        "import subprocess\nout = subprocess.run(cmd, stderr=subprocess.STDOUT)\n",
    )
    .unwrap();
    std::fs::write(
        harness_dir.join("locked.py"),
        "import subprocess\n# Locked Decision: this provider's stderr is log noise only.\nout = subprocess.run(cmd, stderr=subprocess.STDOUT)\n",
    )
    .unwrap();
    let inv = Invocation {
        check: "provider-stderr-merge".into(),
        root: dir.clone(),
        update: false,
        no_degrade: false,
        dispatch_path: None,
        providers_dir: None,
        json: false,
        live: false,
        base: None,
    };
    assert_eq!(provider_stderr_merge(&inv), 1);
    std::fs::remove_file(harness_dir.join("rogue.py")).unwrap();
    assert_eq!(provider_stderr_merge(&inv), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hook_tombstone_reference_reader_names_plugin_root_scripts() {
    let refs = referenced_scripts(
        "python3 ${CLAUDE_PLUGIN_ROOT}/hooks/gate.sh \"$hook\" ${CODEX_PLUGIN_ROOT}/scripts/run.py",
    );
    assert_eq!(refs, vec!["hooks/gate.sh", "scripts/run.py"]);
}

#[test]
fn plan_filenames_flags_a_filename_that_disagrees_with_its_claim() {
    let dir = fixture("plan-filenames");
    write(
        &dir,
        "internal/fno/plan-x-aaaa.md",
        "---\nnode: x-bbbb\n---\nbody\n",
    );
    let scan = plan_filenames::scan(&dir);
    match scan {
        plan_filenames::Scan::Mismatched(rows) => {
            assert_eq!(rows.len(), 1, "{rows:?}");
            assert!(rows[0].line.contains("x-aaaa"), "{rows:?}");
            assert!(rows[0].line.contains("x-bbbb"), "{rows:?}");
        }
        plan_filenames::Scan::Clean => panic!("expected a mismatch"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn command_ratchet_refuses_a_rust_leaf_the_baseline_lacks() {
    let dir = fixture("ratchet");
    write(
        &dir,
        "scripts/ci/native-command-tree.txt",
        "# gen\nmux\tgroup\t-\nversion\troot\t-\n",
    );
    write(
        &dir,
        "scripts/ci/command-collapse-map.tsv",
        "current-leaf\ttier\nwhoami x\tT1\n",
    );
    write(&dir, "scripts/ci/command-collapse-flags.txt", "# c\n");
    write(&dir, "scripts/ci/command-baseline.txt", "# baseline\nmux\n");
    let inv = Invocation {
        check: "command-ratchet".into(),
        root: dir.clone(),
        update: false,
        no_degrade: false,
        dispatch_path: None,
        providers_dir: None,
        json: false,
        live: false,
        base: None,
    };
    assert_eq!(command_ratchet::run(&inv), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn shellout_drift_allowlist_parser_refuses_a_malformed_line() {
    let dir = fixture("shellout");
    let allowlist = dir.join("scripts/lint/.clone-only-scripts.txt");
    std::fs::create_dir_all(allowlist.parent().unwrap()).unwrap();
    std::fs::write(&allowlist, "no separator here\n").unwrap();
    let err = shellout_drift::parse_allowlist(&allowlist);
    assert!(err.is_err(), "{err:?}");
    let _ = std::fs::remove_dir_all(&dir);
}
