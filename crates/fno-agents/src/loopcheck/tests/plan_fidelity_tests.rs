use super::*;
use crate::loopcheck::plan_fidelity::{
    body_guard_verdict, bounded_spawn, classify_plan_fidelity, compute_plan_fidelity_from,
    evaluate_plan_fidelity, FidelityGate, GateInputs,
};

// Hermetic characterization of the native plan-fidelity gate, replayed
// against goldens captured from the Python leg (`fno do plan fidelity`,
// crates/fno-agents/tests/golden/plan_fidelity/) before that leg deleted.
// The two readers (stop gate + merge gate) share the decision; one suite
// serves both.

const GOLDEN_DIR: &str = "tests/golden/plan_fidelity";

/// The capture clock, pinned so the 180-day window never drifts the fixture
/// rows out of it.
fn pinned_now() -> chrono::DateTime<chrono::Local> {
    use chrono::TimeZone;
    chrono::Local
        .with_ymd_and_hms(2026, 11, 15, 12, 0, 0)
        .single()
        .unwrap()
}

struct Fixture {
    dir: tempfile::TempDir,
    // The joined paths the GateInputs borrows, so the borrows outlive
    // `inputs()` instead of pointing at temporaries it created.
    repo: std::path::PathBuf,
    ledger: std::path::PathBuf,
    graph: std::path::PathBuf,
}

const LEDGER_ROWS: &str = r#"{"entries": [
  {"type": "plan", "plan_path": "/x/plans/only-shipped.md", "project": "widget", "session_id": "sess-ship", "completed": "2026-10-30T10:00:00+00:00", "termination_reason": "DonePRGreen"},
  {"type": "plan", "plan_path": "/x/plans/unjoined.md", "project": "widget", "session_id": "sess-u1", "completed": "2026-10-30T10:00:00+00:00"},
  {"type": "plan", "plan_path": "/x/plans/unjoined.md", "project": "widget", "session_id": "sess-u2", "completed": "2026-10-30T11:00:00+00:00"},
  {"type": "plan", "plan_path": "/x/plans/joined.md", "project": "widget", "session_id": "sess-j1", "completed": "2026-10-30T10:00:00+00:00", "termination_reason": "DonePRGreen"},
  {"type": "plan", "plan_path": "/x/plans/forbid.md", "project": "widget", "session_id": "sess-f1", "completed": "2026-10-30T10:00:00+00:00"}
]}"#;

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let run = |args: &[&str]| {
            std::process::Command::new("git")
                .current_dir(&repo)
                .args(args)
                .output()
                .unwrap()
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "t@example.com"]);
        run(&["config", "user.name", "t"]);
        run(&[
            "remote",
            "add",
            "origin",
            "https://github.com/acme/widget.git",
        ]);
        std::fs::create_dir_all(dir.path().join("state")).unwrap();
        std::fs::write(dir.path().join("state/ledger.json"), LEDGER_ROWS).unwrap();
        std::fs::write(dir.path().join("state/graph.json"), "[]").unwrap();
        let plans = dir.path().join("plans");
        std::fs::create_dir_all(&plans).unwrap();
        std::fs::write(plans.join("only-shipped.md"), "# plan only-shipped\n").unwrap();
        std::fs::write(plans.join("unjoined.md"), "# plan unjoined\n").unwrap();
        std::fs::write(plans.join("joined.md"), "# plan joined\n").unwrap();
        std::fs::write(
            plans.join("forbid.md"),
            "---\ncarveouts: forbidden\nnode: x-forb\n---\n# plan forbid\n",
        )
        .unwrap();
        Fixture {
            repo: dir.path().join("repo"),
            ledger: dir.path().join("state/ledger.json"),
            graph: dir.path().join("state/graph.json"),
            dir,
        }
    }

    fn root(&self) -> std::path::PathBuf {
        self.dir.path().to_path_buf()
    }

    fn inputs<'a>(
        &'a self,
        plan: &'a str,
        read_body: &'a dyn Fn(u64) -> Option<String>,
        script: Option<&'a std::path::Path>,
    ) -> GateInputs<'a> {
        GateInputs {
            plan_path: plan,
            cwd: &self.repo,
            ledger_path: &self.ledger,
            graph_path: &self.graph,
            since_days: 180,
            now: pinned_now(),
            read_body,
            oos_script: script,
        }
    }

    fn no_body(_: u64) -> Option<String> {
        None
    }
}

fn golden(name: &str, root: &std::path::Path) -> serde_json::Value {
    let raw = std::fs::read_to_string(format!("{GOLDEN_DIR}/{name}.json")).unwrap();
    let substituted = raw.replace("__CASE_DIR__", &root.to_string_lossy());
    serde_json::from_str(&substituted).unwrap()
}

fn assert_golden(name: &str, fix: &Fixture, decision: serde_json::Value) {
    let want = golden(name, &fix.root());
    assert_eq!(
        decision, want,
        "{name}: native gate diverged from the Python golden"
    );
}

#[test]
fn golden_replay_unreadable_plan_refuses() {
    let fix = Fixture::new();
    let plan = fix.root().join("plans/nope.md");
    let decision =
        compute_plan_fidelity_from(&fix.inputs(&plan.to_string_lossy(), &Fixture::no_body, None));
    assert_golden("missing", &fix, decision);
}

#[test]
fn golden_replay_uncovered_shortfall_refuses() {
    let fix = Fixture::new();
    let plan = fix.root().join("plans/unjoined.md");
    let decision =
        compute_plan_fidelity_from(&fix.inputs(&plan.to_string_lossy(), &Fixture::no_body, None));
    assert_golden("unjoined", &fix, decision);
}

#[test]
fn golden_replay_joined_plan_passes() {
    let fix = Fixture::new();
    let plan = fix.root().join("plans/joined.md");
    let decision =
        compute_plan_fidelity_from(&fix.inputs(&plan.to_string_lossy(), &Fixture::no_body, None));
    assert_golden("joined", &fix, decision);
}

#[test]
fn classify_reads_the_refused_bool_only() {
    match classify_plan_fidelity(br#"{"refused": true, "reason": "1 unjoined, 0 carveouts"}"#) {
        FidelityGate::Refused { reason } => assert!(reason.contains("unjoined")),
        other => panic!("expected Refused, got {:?}", other),
    }
    assert!(matches!(
        classify_plan_fidelity(b"not json at all"),
        FidelityGate::Absent
    ));
    assert!(matches!(
        classify_plan_fidelity(br#"{"refused": false, "planned": 3}"#),
        FidelityGate::Pass
    ));
}

#[test]
fn stop_gate_fails_open_without_a_plan() {
    let fix = Fixture::new();
    assert!(matches!(
        evaluate_plan_fidelity(None, &fix.root().join("repo")),
        FidelityGate::Absent
    ));
    assert!(matches!(
        evaluate_plan_fidelity(Some(""), &fix.root().join("repo")),
        FidelityGate::Absent
    ));
}

fn stub_script(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    path
}

fn echo_body_refuser(dir: &std::path::Path) -> std::path::PathBuf {
    stub_script(
        dir,
        "refuse.sh",
        "#!/bin/bash\necho \"stub refusal: $PR_BODY | carve=$PLAN_CARVEOUTS\" >&2\nexit 1\n",
    )
}

#[test]
fn body_guard_exit_one_carries_the_guard_line_and_env() {
    let fix = Fixture::new();
    let script = echo_body_refuser(&fix.root());
    let reason = crate::loopcheck::plan_fidelity::run_body_guard(
        "the body text".to_string(),
        Some(&script),
        &fix.root().join("repo"),
    )
    .unwrap();
    assert!(
        reason.contains("stub refusal: the body text | carve=forbidden"),
        "{reason}"
    );
}

#[test]
fn body_guard_timeout_is_named_not_silent() {
    let fix = Fixture::new();
    let slow = stub_script(&fix.root(), "slow.sh", "#!/bin/bash\nsleep 5\n");
    let mut cmd = std::process::Command::new("bash");
    cmd.arg(&slow).current_dir(fix.root().join("repo"));
    let out = bounded_spawn(cmd, std::time::Duration::from_millis(200)).unwrap();
    let reason = body_guard_verdict(&out).unwrap();
    assert!(
        reason.contains("carve-out check unreadable (timeout)"),
        "{reason}"
    );
}

#[test]
fn the_real_body_parser_stays_the_one_parser() {
    // The two captured body-script runs replay through the real repo script:
    // a clean body passes, an exclusion section refuses with its own line.
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let script = manifest.join("../../scripts/ci/check-oos-tracked.sh");
    let fix = Fixture::new();
    let clean = crate::loopcheck::plan_fidelity::run_body_guard(
        "clean body, no exclusions\n".to_string(),
        Some(&script),
        &fix.root().join("repo"),
    );
    assert_eq!(clean, None);
    let oos = crate::loopcheck::plan_fidelity::run_body_guard(
        "## Out of scope\n- the flaky bit\n".to_string(),
        Some(&script),
        &fix.root().join("repo"),
    )
    .unwrap();
    let want = std::fs::read_to_string(format!("{GOLDEN_DIR}/body-oos.err"))
        .unwrap()
        .lines()
        .next()
        .unwrap()
        .to_string();
    assert_eq!(oos, want);
}
