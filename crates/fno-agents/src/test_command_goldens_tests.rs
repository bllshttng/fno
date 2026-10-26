//! Golden parity: the Rust test-command family against the goldens captured
//! from the Python leg before its deletion. Each test rebuilds the
//! capture's specimen (universe + probes), runs the Rust implementation,
//! normalizes with the capture's placeholder map ({SB}/{TMPR}/{TMP}/{FX}/
//! {HOME}), and asserts the exact golden tables. Dead oracles:
//! fno.hermetic.neutralise, fno.hermetic.poison,
//! fno.hermetic.AMBIENT_IDENTITY_ENV, fno.hermetic.AMBIENT_LEAK_CANARY,
//! fno.test_cmd (check-parity-test-provenance.sh).
use crate::test_env::{classify, neutralise, passwd_home, poison};
use crate::test_smoke_registry as reg;
use std::collections::BTreeMap;
use std::path::PathBuf;

fn golden(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden/test_command")
        .join(name);
    std::fs::read_to_string(path).unwrap_or_default()
}

/// The capture's specimen: identity markers + ambient + environment +
/// runner-passthrough, dedup'd in order, plus the future-name probes and
/// the leak canary. Values are `v-<NAME>` so a kept row is detectable.
fn specimen() -> BTreeMap<String, String> {
    let mut universe: Vec<String> = Vec::new();
    for list in crate::test_env::vocab_lists() {
        for name in list {
            if !universe.contains(&name) {
                universe.push(name);
            }
        }
    }
    universe.push("FNO_FUTURE_VAR".to_string());
    universe.push("TARGET_FUTURE_VAR".to_string());
    universe.push("TOTALLY_UNKNOWN_VAR".to_string());
    universe.push(crate::test_env::ambient_leak_canary().to_string());
    universe
        .into_iter()
        .map(|n| (n.clone(), format!("v-{n}")))
        .collect()
}

/// The capture's placeholder map, applied in the capture's order (a longer
/// path that contains a shorter one must be replaced first).
fn norm(
    v: &str,
    sb: &str,
    sb_real: &str,
    tmp: &str,
    tmp_real: &str,
    fx: &str,
    home: &str,
) -> String {
    v.replace(sb_real, "{SB}")
        .replace(sb, "{SB}")
        .replace(tmp_real, "{TMPR}")
        .replace(tmp, "{TMP}")
        .replace(fx, "{FX}")
        .replace(home, "{HOME}")
}

fn tmp_root() -> String {
    std::env::var("TMPDIR")
        .unwrap_or_else(|_| "/tmp".to_string())
        .trim_end_matches('/')
        .to_string()
}

fn fx_dir() -> String {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo = manifest
        .parent()
        .and_then(|p| p.parent())
        .unwrap_or(manifest.as_path());
    repo.join("cli/tests/fixtures/ambient-poison")
        .to_string_lossy()
        .to_string()
}

fn cap() -> (String, String, String, String, String, String) {
    // (sb, sb_real, tmp, tmp_real, fx, home)
    let tmp = tempfile::tempdir().unwrap();
    let sb = tmp.path().join("sb");
    std::fs::create_dir_all(&sb).unwrap();
    let sb_str = sb.to_string_lossy().to_string();
    let sb_real = std::fs::canonicalize(&sb)
        .unwrap()
        .to_string_lossy()
        .to_string();
    let tmp_root = tmp_root();
    let tmp_real = std::fs::canonicalize(&tmp_root)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| tmp_root.clone());
    let fx = fx_dir();
    let home = passwd_home()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    // Hold the TempDir open for the run: dropping it removes the sandbox.
    std::mem::forget(tmp);
    (sb_str, sb_real, tmp_root, tmp_real, fx, home)
}

#[test]
fn neutralise_matches_the_python_golden() {
    let (sb, sb_real, tmp, tmp_real, fx, home) = cap();
    let parent = specimen();
    let out = neutralise(&parent, std::path::Path::new(&sb)).unwrap();
    let dropped: Vec<String> = parent
        .keys()
        .filter(|k| !out.contains_key(*k))
        .cloned()
        .collect();
    assert_eq!(dropped.join("\n") + "\n", golden("neutralise_dropped.txt"));
    let kept: Vec<String> = parent
        .keys()
        .filter(|k| out.get(*k) == parent.get(*k))
        .cloned()
        .collect();
    assert_eq!(kept.join("\n") + "\n", golden("neutralise_kept.txt"));
    let mut repinned: Vec<String> = Vec::new();
    for (k, v) in &out {
        if parent.get(k).map(|p| p != v).unwrap_or(false) {
            let line = format!(
                "{}\t{}",
                k,
                norm(v, &sb, &sb_real, &tmp, &tmp_real, &fx, &home)
            );
            repinned.push(line);
        }
    }
    repinned.sort();
    let want = golden("neutralise_repinned.tsv");
    assert_eq!(repinned.join("\n") + "\n", want, "repinned rows diverge");
    let mut added: Vec<String> = Vec::new();
    for (k, v) in &out {
        if !parent.contains_key(k) {
            let line = format!(
                "{}\t{}",
                k,
                norm(v, &sb, &sb_real, &tmp, &tmp_real, &fx, &home)
            );
            added.push(line);
        }
    }
    added.sort();
    let want = golden("neutralise_added.tsv");
    assert_eq!(added.join("\n") + "\n", want, "added rows diverge");
}

#[test]
fn classify_matches_the_python_golden() {
    let mut names: Vec<String> = specimen().into_keys().collect();
    names.sort();
    let mut rows: Vec<String> = Vec::new();
    for n in &names {
        rows.push(format!("{}\t{}", n, classify(n)));
    }
    let want = golden("classify.tsv");
    assert_eq!(rows.join("\n") + "\n", want, "classify rows diverge");
}

#[test]
fn poison_matches_the_python_golden() {
    let (sb, sb_real, tmp, tmp_real, fx, home) = cap();
    let parent = specimen();
    let out = poison(&parent, std::path::Path::new(&fx)).unwrap();
    let mut changed: Vec<String> = Vec::new();
    for (k, v) in &out {
        let in_parent = parent.get(k);
        if in_parent.map(|p| p != v).unwrap_or(true) {
            changed.push(format!(
                "{}\t{}",
                k,
                norm(v, &sb, &sb_real, &tmp, &tmp_real, &fx, &home)
            ));
        }
    }
    changed.sort();
    let want = golden("poison_changed.tsv");
    assert_eq!(changed.join("\n") + "\n", want, "poison rows diverge");
}

#[test]
fn structural_steps_match_the_captured_registry() {
    let want = golden("smoke_structural.tsv");
    let rows: Vec<String> = reg::STRUCTURAL_STEPS
        .iter()
        .map(|(n, c, m)| format!("{n}\t{c}\t{}", m.replace('\n', "\u{1f}")))
        .collect();
    assert_eq!(
        rows.join("\n") + "\n",
        want,
        "structural registry diverges from the captured tsv"
    );
}

#[test]
fn deferred_list_matches_the_captured_quarantine() {
    let want = golden("smoke_deferred.txt");
    let got: Vec<&str> = reg::DISCOVERY_DEFERRED.iter().copied().collect();
    assert_eq!(got.join("\n") + "\n", want, "DISCOVERY_DEFERRED diverges");
}

#[test]
fn constants_match_the_captured_json() {
    let v: serde_json::Value = serde_json::from_str(&golden("smoke_constants.json")).unwrap();
    assert_eq!(reg::TAIL_LINES, v["TAIL_LINES"].as_u64().unwrap() as usize);
    assert_eq!(
        reg::AMBIENT_MODES,
        v["AMBIENT_MODES"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        reg::STATE_MODES,
        v["STATE_MODES"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        reg::STATE_CONTROL_TEST,
        v["STATE_CONTROL_TEST"].as_str().unwrap()
    );
    assert_eq!(
        reg::SMOKE_FAILURE_RECORD_DEFAULT,
        v["SMOKE_FAILURE_RECORD_DEFAULT"].as_str().unwrap()
    );
    assert_eq!(
        reg::CHANGED_RC_UNSELECTED as i64,
        v["CHANGED_RC_UNSELECTED"].as_i64().unwrap()
    );
    assert_eq!(
        reg::CHANGED_RC_UNEVALUATED as i64,
        v["CHANGED_RC_UNEVALUATED"].as_i64().unwrap()
    );
    assert_eq!(
        reg::CHANGED_RC_PREREQ as i64,
        v["CHANGED_RC_PREREQ"].as_i64().unwrap()
    );
    assert_eq!(
        reg::CHANGED_RECEIPT_DEFAULT,
        v["CHANGED_RECEIPT_DEFAULT"].as_str().unwrap()
    );
    for (key, got) in [
        ("INFRA_EXACT", reg::INFRA_EXACT),
        ("INFRA_PREFIX", reg::INFRA_PREFIX),
        ("INFRA_BASENAME", reg::INFRA_BASENAME),
        ("INFRA_SELECTS", reg::INFRA_SELECTS),
        ("BUNDLE_TEST_TARGETS", reg::BUNDLE_TEST_TARGETS),
        ("RUST_BIN_MARKERS", reg::RUST_BIN_MARKERS),
        ("TARGET_BIN_RELS", reg::TARGET_BIN_RELS),
        ("HARNESS_GLOBS", reg::HARNESS_GLOBS),
    ] {
        let want_list: Vec<String> = v[key]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap().to_string())
            .collect();
        assert_eq!(got.to_vec(), want_list, "const {key} diverges");
    }
    for (key, got) in [
        ("ALWAYS_LINTS", reg::ALWAYS_LINTS),
        ("ORCHESTRATED_SUBTREES", reg::ORCHESTRATED_SUBTREES),
    ] {
        let want_rows: Vec<Vec<String>> = v[key]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| {
                x.as_array()
                    .unwrap()
                    .iter()
                    .map(|y| y.as_str().unwrap().to_string())
                    .collect()
            })
            .collect();
        assert_eq!(got.to_vec(), want_rows, "const {key} diverges");
    }
    assert_eq!(
        reg::RUST_BUILD_STEP,
        v["RUST_BUILD_STEP"].as_str().unwrap(),
        "RUST_BUILD_STEP diverges"
    );
    assert_eq!(
        reg::CLAIM_DOOR_NAME,
        v["CLAIM_DOOR_NAME"].as_str().unwrap(),
        "CLAIM_DOOR_NAME diverges"
    );
    assert_eq!(
        reg::CENSUS_TRANCHE_BOUND_S as i64,
        v["CENSUS_TRANCHE_BOUND_S"].as_i64().unwrap()
    );
    assert_eq!(
        reg::CENSUS_KILL_BOUND_S_DEFAULT as i64,
        v["CENSUS_KILL_BOUND_S_DEFAULT"].as_i64().unwrap()
    );
}

#[test]
fn gitconfig_pin_matches_the_captured_file() {
    let (sb, _sb_real, _tmp, _tmp_real, _fx, _home) = cap();
    let parent = specimen();
    let _ = neutralise(&parent, std::path::Path::new(&sb)).unwrap();
    let body = std::fs::read_to_string(std::path::Path::new(&sb).join("gitconfig")).unwrap();
    assert_eq!(body, golden("gitconfig.out"));
}
