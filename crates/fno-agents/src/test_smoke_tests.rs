//! The smoke runner's contracts, ported beside it when the Python leg died:
//! the --only/--skip selector pair, the --state verdict, the canary
//! bracket, the company-boundary registry reachability, and the CI
//! workflow's shard plumbing. Dead oracles: fno.test_cmd
//! (_parse_smoke_args, _name_matches, _state_both_exit, _is_state_control,
//! _run_state_canary, _state_canary_snapshot, _shard_selected_indices,
//! smoke_steps, smoke_step_weights) and the five cli/tests/unit
//! smoke-test files they replaced.
use crate::test_smoke::{is_state_control, parse_smoke_args, run_state_canary, state_both_exit};
use crate::test_smoke_discovery as disc;
use crate::test_smoke_registry as reg;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .unwrap_or(Path::new(env!("CARGO_MANIFEST_DIR")))
        .to_path_buf()
}

#[test]
fn skip_refuses_an_empty_or_forgotten_value() {
    for argv in [
        vec!["--skip=".to_string()],
        vec!["--skip".to_string()],
        vec!["--skip".to_string(), "--retry-failed".to_string()],
    ] {
        let err = parse_smoke_args(&argv).unwrap_err();
        assert!(err.contains("needs a value"), "refusal: {err}");
    }
}

#[test]
fn skip_is_its_own_subset_mode() {
    for argv in [
        vec!["--only".into(), "a".into(), "--skip".into(), "b".into()],
        vec!["--skip".into(), "b".into(), "--changed".into()],
        vec!["--skip".into(), "b".into(), "--retry-failed".into()],
    ] {
        let err = parse_smoke_args(&argv).unwrap_err();
        assert!(err.contains("separate subset modes"), "refusal: {err}");
    }
}

#[test]
fn a_step_name_containing_a_comma_still_selects_exactly() {
    let commad = "Command-surface ratchet (real count, both binaries)";
    assert!(crate::test_smoke_discovery::name_matches(commad, commad));
    assert!(!crate::test_smoke_discovery::name_matches(
        "Sync + build",
        commad
    ));
    assert!(crate::test_smoke_discovery::name_matches(
        "Sync + build",
        "Sync + build,Pytest*"
    ));
}

#[test]
fn state_mode_parser_rejects_unknown_shapes() {
    let err = parse_smoke_args(&["--state".into(), "invalid".into()]).unwrap_err();
    assert!(
        err.contains("--state") && err.contains("clean/populated/both"),
        "refusal: {err}"
    );
}

#[test]
fn state_both_exit_green_only_when_control_fires_and_diff_is_canary_only() {
    let canary = "tests.unit.test_state_canary::test_state_canary_detects_populated_state";
    let other = "tests.unit.test_state::reads_live_graph";
    assert_eq!(
        state_both_exit(
            0,
            &[canary.to_string()],
            Some("passed"),
            Some("failed"),
            false,
            false
        ),
        0
    );
    assert_eq!(
        state_both_exit(
            0,
            &[canary.to_string(), other.to_string()],
            Some("passed"),
            Some("failed"),
            false,
            false
        ),
        1
    );
    assert_eq!(
        state_both_exit(
            0,
            &[canary.to_string()],
            Some("passed"),
            Some("passed"),
            false,
            false
        ),
        1
    );
    assert_eq!(
        state_both_exit(0, &[canary.to_string()], None, Some("failed"), false, false),
        1
    );
    assert_eq!(
        state_both_exit(0, &[], Some("passed"), Some("failed"), false, false),
        1
    );
    assert_eq!(
        state_both_exit(
            3,
            &[canary.to_string()],
            Some("passed"),
            Some("failed"),
            false,
            false
        ),
        3
    );
    assert_eq!(
        state_both_exit(
            0,
            &[canary.to_string()],
            Some("passed"),
            Some("failed"),
            true,
            false
        ),
        1
    );
}

#[test]
fn state_both_exit_is_red_when_a_lane_never_took_a_baseline() {
    let canary = "tests.unit.test_state_canary::test_state_canary_detects_populated_state";
    assert_eq!(
        state_both_exit(
            0,
            &[canary.to_string()],
            Some("passed"),
            Some("failed"),
            false,
            true
        ),
        1
    );
    assert_eq!(
        state_both_exit(
            0,
            &[canary.to_string()],
            Some("passed"),
            Some("failed"),
            true,
            false
        ),
        1
    );
    assert_eq!(
        state_both_exit(
            0,
            &[canary.to_string()],
            Some("passed"),
            Some("failed"),
            false,
            false
        ),
        0
    );
}

#[test]
fn the_control_matcher_does_not_accept_a_wiring_testcase() {
    let control = "tests.unit.test_state_canary::test_state_canary_detects_populated_state";
    let wiring = "tests.unit.test_state_canary_wiring::test_the_verb_reaches_the_script_verbatim";
    assert!(is_state_control(control));
    assert!(!is_state_control(wiring));
    assert!(is_state_control(&format!("{control}[param]")));
}

fn write_canary_script(root: &Path, body: &str) {
    let dir = root.join("scripts/ci");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("check-state-canary.sh");
    std::fs::write(&path, format!("#!/usr/bin/env bash\n{body}\n")).unwrap();
    #[allow(deprecated)]
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
}

#[test]
fn a_missing_script_refuses_on_verify_and_passes_on_plant() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    assert_eq!(run_state_canary(root, "verify"), 1);
    assert_eq!(run_state_canary(root, "plant"), 0);
}

#[test]
fn a_refusing_verify_is_relayed_as_a_nonzero_return() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_canary_script(root, "exit 1");
    assert_eq!(run_state_canary(root, "verify"), 1);
}

#[test]
fn the_canary_runs_on_the_parent_home_not_the_sandbox() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_canary_script(root, "echo \"$HOME\" > \"$FNO_STATE_CANARY_SNAPSHOT.home\"");
    let parent = root.join("parent-home");
    std::fs::create_dir_all(&parent).unwrap();
    std::env::set_var("HOME", &parent);
    let snapshot = crate::test_smoke::state_canary_snapshot();
    assert_eq!(run_state_canary(root, "plant"), 0);
    let seen = std::fs::read_to_string(format!("{snapshot}.home")).unwrap();
    assert_eq!(seen.trim(), parent.to_string_lossy());
    let _ = std::fs::remove_file(format!("{snapshot}.home"));
}

#[test]
fn company_boundary_gate_family_has_registered_callers() {
    let blob: String = reg::STRUCTURAL_STEPS
        .iter()
        .map(|(_, _, cmd)| cmd.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    for required in [
        "scripts/ci/check-company-boundaries.sh",
        "tests/ci/test_check_company_boundaries.sh",
        "scripts/ci/check-company-function-agnostic.sh",
        "scripts/ci/check-plugins-function-agnostic.sh",
    ] {
        assert!(
            blob.contains(required),
            "unreachable company boundary gate: {required}"
        );
    }
}

/// The workflow yaml, as the six shard-plumbing tests read it.
fn workflow() -> serde_yaml_ng::Value {
    let text = std::fs::read_to_string(repo().join(".github/workflows/cli-ci.yml")).unwrap();
    serde_yaml_ng::from_str(&text).unwrap()
}

/// (job, index, total, mode, globs) for every full-gate matrix leg, read out
/// of the workflow (never restated: hardcoding them would make this a test
/// of two constants someone could widen to --skip '*' and gut the gate).
fn shard_selectors() -> Vec<(String, usize, usize, String, String)> {
    let doc = workflow();
    let jobs = doc.get("jobs").unwrap();
    let needs = match jobs.get("smoke").unwrap().get("needs") {
        Some(v) if !v.is_null() => v
            .as_sequence()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap().to_string())
            .collect::<Vec<_>>(),
        _ => vec![],
    };
    let shard_re = regex::Regex::new(r"--shard\s+\$\{\{\s*matrix\.shard\s*\}\}/(\d+)").unwrap();
    let sel_re = regex::Regex::new(r"--(only|skip)[= ]+'([^']*)'|--(only|skip)[= ]+(\S+)").unwrap();
    let mut found = Vec::new();
    for name in &needs {
        let job = jobs.get(name).unwrap();
        let run = job
            .get("steps")
            .map(|steps| {
                steps
                    .as_sequence()
                    .unwrap()
                    .iter()
                    .filter_map(|s| s.get("run").and_then(|r| r.as_str()))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        let shard = shard_re.captures(&run);
        let matrix: Vec<usize> = job
            .get("strategy")
            .and_then(|s| s.get("matrix"))
            .and_then(|m| m.get("shard"))
            .map(|m| {
                m.as_sequence()
                    .unwrap()
                    .iter()
                    .map(|x| x.as_i64().unwrap() as usize)
                    .collect()
            })
            .unwrap_or_default();
        let legs: Vec<(usize, usize)> = match &shard {
            Some(c) => matrix.iter().map(|i| (*i, c[1].parse().unwrap())).collect(),
            None => vec![(0, 0)],
        };
        for c in sel_re.captures_iter(&run) {
            let flag = c.get(1).or(c.get(3)).unwrap().as_str().to_string();
            let globs = c.get(2).or(c.get(4)).unwrap().as_str().to_string();
            for (index, total) in &legs {
                found.push((name.clone(), *index, *total, flag.clone(), globs.clone()));
                let _ = &flag;
                let _ = &globs;
            }
        }
    }
    found
}

fn selected_names(
    names: &[String],
    flag: &str,
    globs: &str,
    shard: usize,
    total: usize,
    weights: &BTreeMap<String, f64>,
) -> Vec<String> {
    let steps: Vec<disc::Step> = names
        .iter()
        .map(|n| (n.clone(), ".".to_string(), String::new()))
        .collect();
    let mut indices: Vec<usize> = names
        .iter()
        .enumerate()
        .filter(|(_, n)| disc::name_matches(n, globs) == (flag == "only"))
        .map(|(i, _)| i)
        .collect();
    if total > 0 {
        indices =
            crate::test_smoke::shard_selected_indices(&steps, &indices, shard, total, weights);
    }
    indices.into_iter().map(|i| names[i].clone()).collect()
}

#[test]
fn the_workflow_actually_shards_the_gate() {
    let selectors = shard_selectors();
    assert!(
        !selectors.is_empty(),
        "the smoke gate needs no shard jobs carrying --only/--skip"
    );
    let mut by_job: BTreeMap<&str, usize> = BTreeMap::new();
    for (job, _, _, _, _) in &selectors {
        *by_job.entry(job.as_str()).or_insert(0) += 1;
    }
    assert_eq!(
        by_job.keys().cloned().collect::<Vec<_>>(),
        vec!["smoke-pytest", "smoke-rest"],
        "expected pytest and rest legs, found {by_job:?}"
    );
    assert!(by_job.values().all(|v| *v >= 1));
}

#[test]
fn every_pr_affected_job_overrides_the_implicit_success_gate() {
    let doc = workflow();
    let jobs = doc.get("jobs").unwrap();
    for (name, job) in jobs.as_mapping().unwrap() {
        let job_if = job.get("if").and_then(|v| v.as_str()).unwrap_or("");
        if !job_if.contains("needs.pr-affected") {
            continue;
        }
        assert!(
            job_if.contains("!cancelled()"),
            "{} is gated on pr-affected without !cancelled(); a push run skips it and main runs no tests",
            name.as_str().unwrap()
        );
    }
    for name in [
        "smoke-pytest",
        "smoke-rest",
        "hook-latency",
        "test-agents",
        "test-agents-integration",
        "test-mux",
    ] {
        let job = jobs.get(name).unwrap();
        assert!(
            job.get("if")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .contains("needs.pr-affected"),
            "{name} stopped being gated on pr-affected; update this guard"
        );
    }
    let smoke = jobs.get("smoke").unwrap();
    let needs: Vec<String> = match smoke.get("needs") {
        Some(v) if !v.is_null() => v
            .as_sequence()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap().to_string())
            .collect(),
        _ => vec![],
    };
    assert!(needs.contains(&"pr-affected".to_string()));
    let pr = jobs.get("pr-affected").unwrap();
    assert_eq!(
        pr.get("if").and_then(|v| v.as_str()).unwrap(),
        "${{ !cancelled() }}"
    );
    let changed_smoke = jobs.get("changed-smoke").unwrap();
    assert_eq!(
        changed_smoke.get("if").and_then(|v| v.as_str()).unwrap(),
        "github.event_name == 'pull_request'"
    );
}

#[test]
fn the_shards_cover_every_step() {
    let steps = disc::smoke_steps(&repo());
    let names: Vec<String> = steps.iter().map(|(n, _, _)| n.clone()).collect();
    assert!(
        !names.is_empty(),
        "the smoke registry is empty; this test would pass vacuously"
    );
    let weights = disc::smoke_step_weights(&repo());
    let mut covered: std::collections::BTreeSet<String> = Default::default();
    for (job, shard, total, flag, globs) in shard_selectors() {
        let _ = job;
        let leg = selected_names(&names, &flag, &globs, shard, total, &weights);
        covered.extend(leg);
    }
    for name in &names {
        assert!(covered.contains(name), "step {name:?} runs in no CI shard");
    }
}

#[test]
fn the_rust_binary_is_built_in_the_shard_that_needs_it() {
    let names: Vec<String> = disc::smoke_steps(&repo())
        .iter()
        .map(|(n, _, _)| n.clone())
        .collect();
    let build = reg::RUST_BUILD_STEP;
    let pytest_step = "Pytest (unit + integration)";
    assert!(
        names.iter().any(|n| n == build),
        "the build step was renamed; re-check the shard seam"
    );
    for (job, shard, total, flag, globs) in shard_selectors() {
        let selected = selected_names(&names, &flag, &globs, shard, total, &Default::default());
        if selected.contains(&pytest_step.to_string()) {
            assert!(
                !selected.iter().any(|n| n == build),
                "{job} runs pytest and the rust build together; pytest deletes the binary that build produces"
            );
        }
    }
}

#[test]
fn the_rest_legs_split_by_measured_weight() {
    let names: Vec<String> = disc::smoke_steps(&repo())
        .iter()
        .map(|(n, _, _)| n.clone())
        .collect();
    let weights = disc::smoke_step_weights(&repo());
    assert!(weights.len() >= 10, "the weight table is missing or empty");
    let rest: Vec<_> = shard_selectors()
        .into_iter()
        .filter(|(job, _, _, _, _)| job == "smoke-rest")
        .collect();
    assert!(
        !rest.is_empty(),
        "no smoke-rest legs read from the workflow"
    );
    for (_job, shard, total, flag, globs) in rest {
        let leg = selected_names(&names, &flag, &globs, shard, total, &weights);
        let seconds: f64 = leg
            .iter()
            .map(|n| weights.get(n).copied().unwrap_or(2.0))
            .sum();
        assert!(
            seconds <= 600.0,
            "rest leg {shard}/{total} estimates {seconds:.0}s"
        );
    }
}

#[test]
fn smoke_setup_cleans_before_building_cached_artifacts() {
    let text =
        std::fs::read_to_string(repo().join(".github/actions/smoke-setup/action.yml")).unwrap();
    let action: serde_yaml_ng::Value = serde_yaml_ng::from_str(&text).unwrap();
    let run = action
        .get("runs")
        .unwrap()
        .get("steps")
        .unwrap()
        .as_sequence()
        .unwrap()
        .iter()
        .filter_map(|s| s.get("run").and_then(|r| r.as_str()))
        .collect::<Vec<_>>()
        .join("\n");
    let lines: Vec<&str> = run
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    for package in ["fno-agents", "fno"] {
        let clean = lines
            .iter()
            .position(|l| {
                *l == format!(
                    "cargo clean -p {package} --manifest-path crates/{package}/Cargo.toml"
                )
            })
            .unwrap_or_else(|| panic!("no clean line for {package}"));
        let build = lines
            .iter()
            .position(|l| {
                l.starts_with(&format!(
                    "CARGO_BUILD_BUILD_DIR=\"$PWD/crates/{package}/target\" "
                )) && l.contains(&format!("--manifest-path crates/{package}/Cargo.toml"))
            })
            .unwrap_or_else(|| panic!("no build line for {package}"));
        assert!(
            clean < build,
            "smoke setup can execute a stale cached {package} binary"
        );
    }
}

#[test]
fn journey_cargo_builds_pin_the_classic_layout() {
    // The smoke sandbox pins CARGO_BUILD_BUILD_DIR into its /tmp home, so a
    // bare `cargo build` in a smoke leg is a cold rebuild into the sandbox,
    // and the CI runner's /tmp quota cannot fit one (EDQUOT os error 122,
    // smoke-rest (4), 2026-10-25). Every cargo build in the
    // workspace-standalone journey must pin the classic layout the script
    // itself reads and smoke-setup pre-builds warm. The invocation pattern
    // consumes shell assignment prefixes, so an indented build or one
    // carrying another env prefix (RUSTFLAGS=... cargo build) is caught too.
    let lines = std::fs::read_to_string(repo().join("tests/mux-workspace-standalone.sh")).unwrap();
    let invocation = regex::Regex::new(
        r#"(?:^|[;&|]\s*|\b(?:then|do|else)\s+)(?:[A-Za-z_][A-Za-z0-9_]*=(?:"[^"]*"|'[^']*'|\S+)\s+)*cargo build\b"#,
    )
    .unwrap();
    let bare: Vec<String> = lines
        .lines()
        .enumerate()
        .filter(|(_, line)| {
            invocation.is_match(line.trim()) && !line.contains("CARGO_BUILD_BUILD_DIR=")
        })
        .map(|(i, line)| format!("line {}: {}", i + 1, line))
        .collect();
    assert!(
        bare.is_empty(),
        "unpinned cargo build in tests/mux-workspace-standalone.sh: {bare:?}"
    );
}
