//! parity-stage: characterization
//! parity-oracle: fno.recovery
//!
//! Replays the Python leg's captured answers (`tests/golden/recovery/`) against
//! the sweep, the classifiers and the redispatch order. Event bodies compare as
//! serialized text, so key order counts.

use fno_agents::claude_ask::SessionLocator;
use fno_agents::recovery_sweep::mission::mission_complete_with;
use fno_agents::recovery_sweep::redispatch::{
    redispatch, RedispatchOutcome, RedispatchSeams, RunResult,
};
use fno_agents::recovery_sweep::{
    classify, classify_session_error_at, classify_worker_refusal_at, node_id_from_worktree,
    prune_keep, run_recovery_sweep, Candidate, RecoveryCfg, SnapshotView, SweepSeams,
};
use fno_agents::state::RegistryEntry;
use serde_json::{json, Map, Value};
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn golden(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden/recovery")
        .join(name);
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn text(value: &Value) -> String {
    serde_json::to_string(value).unwrap()
}

/// A cwd of the shape a fixture names. `None` is a row with no cwd.
fn make_cwd(root: &Path, kind: &str, seq: &mut usize) -> Option<String> {
    if kind == "none" {
        return None;
    }
    *seq += 1;
    let dir = root.join(format!("cwd-{seq}"));
    std::fs::create_dir_all(&dir).unwrap();
    if kind != "missing-fno" {
        std::fs::create_dir_all(dir.join(".fno")).unwrap();
    }
    if let Some(body) = kind.strip_prefix("manifest:") {
        std::fs::write(dir.join(".fno/target-state.md"), body).unwrap();
    }
    Some(dir.to_string_lossy().into_owned())
}

#[test]
fn classifier_tables_match_the_python_leg() {
    let g = golden("classifiers.json");
    let now = g["now"].as_f64().unwrap();
    for case in g["session"].as_array().unwrap() {
        let got =
            classify_session_error_at(case["text"].as_str().unwrap(), case["tz"].as_str(), now)
                .map_or(Value::Null, |err| err.to_json());
        assert_eq!(text(&got), text(&case["expect"]), "{}", case["label"]);
    }
    for case in g["refusal"].as_array().unwrap() {
        let got = classify_worker_refusal_at(
            case["output_result"].as_str(),
            case["last_message"].as_str(),
            case["tz"].as_str(),
            now,
        )
        .map_or(
            Value::Null,
            |(err, source)| json!({"source": source, "err": err.to_json()}),
        );
        assert_eq!(text(&got), text(&case["expect"]), "{}", case["label"]);
    }
    for case in g["verdicts"].as_array().unwrap() {
        let got = classify(
            case["state"].as_str().unwrap(),
            900,
            case["truth_state"].as_str().unwrap(),
            case["truth_age_s"].as_f64(),
            case["mission_complete"].as_bool(),
        );
        assert_eq!(got, case["expect"].as_str().unwrap(), "{case}");
    }
    let live: BTreeSet<String> = ["aaaa1111".to_string()].into_iter().collect();
    for case in g["prune_keep"].as_array().unwrap() {
        let key = case["key"].as_str().unwrap();
        assert_eq!(
            prune_keep(key, &live),
            case["expect"].as_bool().unwrap(),
            "{key}"
        );
    }
    let root = tempfile::tempdir().unwrap();
    let mut seq = 0;
    for case in g["node_ids"].as_array().unwrap() {
        let kind = case["manifest"]
            .as_str()
            .map_or("node-less".to_string(), |m| format!("manifest:{m}"));
        let cwd = make_cwd(root.path(), &kind, &mut seq).unwrap();
        assert_eq!(
            node_id_from_worktree(&cwd).as_deref(),
            case["expect"].as_str(),
            "{case}"
        );
    }
}

struct Tick {
    entries: Vec<RegistryEntry>,
    located: Vec<String>,
    tick: Value,
    counts: RefCell<Map<String, Value>>,
    quota: RefCell<Vec<Value>>,
    now: f64,
}

impl Tick {
    fn of(&self, section: &str, short: &str) -> Value {
        self.tick
            .get(section)
            .and_then(|s| s.get(short))
            .cloned()
            .unwrap_or(Value::Null)
    }
}

impl SweepSeams for Tick {
    fn registry(&self) -> Result<Vec<RegistryEntry>, String> {
        Ok(self.entries.clone())
    }
    fn locate(&self, short_id: &str) -> Option<SessionLocator> {
        self.located
            .iter()
            .any(|s| s == short_id)
            .then(|| SessionLocator {
                pid: 1,
                short_id: short_id.to_string(),
                messaging_socket_path: format!("/sock/{short_id}"),
                jobs_dir: PathBuf::from(format!("/jobs/{short_id}")),
                session_id: None,
                cwd: None,
            })
    }
    fn read_state(&self, jobs_dir: &Path) -> SnapshotView {
        let short = jobs_dir.file_name().unwrap().to_str().unwrap();
        let state = self.of("state", short);
        SnapshotView {
            state: state["state"].as_str().unwrap_or("").to_string(),
            updated_at: None,
            output_result: state["output_result"].as_str().map(str::to_string),
        }
    }
    fn truth(&self, candidate: &Candidate) -> Value {
        let truth = self.of("truth", &candidate.short_id);
        if truth.is_null() {
            json!({})
        } else {
            truth
        }
    }
    fn liveness(&self, sock_path: &str) -> bool {
        let short = Path::new(sock_path).file_name().unwrap().to_str().unwrap();
        self.of("liveness", short).as_bool().unwrap_or(true)
    }
    fn load_counts(&self) -> Map<String, Value> {
        self.counts.borrow().clone()
    }
    fn save_counts(&self, counts: &Map<String, Value>) {
        *self.counts.borrow_mut() = counts.clone();
    }
    fn mission_complete(&self, candidate: &Candidate) -> Option<bool> {
        self.of("mission", &candidate.short_id).as_bool()
    }
    fn notify_close(&self, candidate: &Candidate) -> bool {
        self.of("notify", &candidate.short_id)
            .as_bool()
            .unwrap_or(true)
    }
    fn reset_timezone(&self, _cwd: Option<&str>) -> Option<String> {
        self.tick["tz"].as_str().map(str::to_string)
    }
    fn record_quota_lock(
        &self,
        account: Option<&str>,
        text: &str,
        resets_at: Option<f64>,
    ) -> Option<String> {
        self.quota
            .borrow_mut()
            .push(json!({"account": account, "text": text, "resets_at": resets_at}));
        account
            .filter(|a| !a.is_empty() && *a != "default" && !text.is_empty())
            .map(str::to_string)
    }
    fn now(&self) -> f64 {
        self.now
    }
}

#[test]
fn sweep_scenarios_match_the_python_leg() {
    let g = golden("sweep.json");
    let now = g["now"].as_f64().unwrap();
    let root = tempfile::tempdir().unwrap();
    let mut seq = 0;
    for scenario in g["scenarios"].as_array().unwrap() {
        let label = scenario["name"].as_str().unwrap();
        let entries: Vec<RegistryEntry> = scenario["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| RegistryEntry {
                name: e["name"].as_str().unwrap().to_string(),
                short_id: e["short_id"].as_str().unwrap().to_string(),
                harness: e["harness"].as_str().map(str::to_string),
                cwd: make_cwd(root.path(), e["cwd"].as_str().unwrap(), &mut seq)
                    .unwrap_or_default(),
                launch_account: e["launch_account"].as_str().map(str::to_string),
                ..Default::default()
            })
            .collect();
        let located: Vec<String> = scenario["located"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s.as_str().unwrap().to_string())
            .collect();
        let cfg = RecoveryCfg {
            idle_threshold_seconds: 900,
            max_nudges: scenario["max_nudges"].as_i64().unwrap_or(3),
        };
        let mut counts: Map<String, Value> = scenario
            .get("initial_counts")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        for (at, (tick, expect)) in scenario["ticks"]
            .as_array()
            .unwrap()
            .iter()
            .zip(scenario["expect"].as_array().unwrap())
            .enumerate()
        {
            let seams = Tick {
                entries: entries.clone(),
                located: located.clone(),
                tick: tick.clone(),
                counts: RefCell::new(counts.clone()),
                quota: RefCell::new(Vec::new()),
                now,
            };
            let mut events: Vec<Value> = Vec::new();
            let mut emit = |kind: &str, data: Value| {
                events.push(json!({"type": kind, "data": data}));
            };
            let candidates = run_recovery_sweep(&cfg, &seams, &mut emit).unwrap();
            counts = seams.counts.borrow().clone();
            let got = json!({
                "candidates": candidates,
                "events": events,
                "quota_calls": seams.quota.borrow().clone(),
                "counts": counts,
            });
            assert_eq!(text(&got), text(expect), "{label} tick {at}");
        }
    }
}

#[test]
fn mission_table_matches_the_python_leg() {
    let g = golden("mission.json");
    let root = tempfile::tempdir().unwrap();
    let mut seq = 0;
    for case in g["cases"].as_array().unwrap() {
        let candidate = Candidate {
            short_id: "aaaa1111".to_string(),
            sock_path: "/s".to_string(),
            jobs_dir: PathBuf::from("/j"),
            cwd: make_cwd(root.path(), case["cwd"].as_str().unwrap(), &mut seq),
            name: case["name"].as_str().map(str::to_string),
            session_id: None,
            launch_account: None,
        };
        let row_of = |node: &str| g["rows"].get(node).cloned();
        let hex_node = |hex: &str| g["hex"][hex].as_str().map(str::to_string);
        let got = mission_complete_with(&candidate, &row_of, &hex_node);
        assert_eq!(got, case["expect"].as_bool(), "{}", case["label"]);
    }
}

struct Stage<'a> {
    case: &'a Value,
    cwd: String,
    order: &'a RefCell<Vec<String>>,
    calls: RefCell<Vec<Value>>,
    aliases: RefCell<Vec<Value>>,
}

fn stage_of(argv: &[String]) -> &'static str {
    let words: Vec<&str> = argv.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["fno", "agents", "stop", ..] => "stop",
        ["fno", "mux", "pane", "kill", ..] => "kill",
        ["fno", "agents", "claim", "release", ..] => "release",
        ["fno", "agents", "spawn", ..] => "spawn",
        ["fno-agents", "claim", "lane-release", ..] => "lane",
        _ => panic!("unexpected command: {words:?}"),
    }
}

impl RedispatchSeams for Stage<'_> {
    fn run(
        &self,
        argv: &[String],
        cwd: &str,
        timeout_s: u64,
        env: &[(&str, &str)],
    ) -> Option<RunResult> {
        let stage = stage_of(argv);
        let shown: Vec<String> = argv.iter().map(|a| a.replace(&self.cwd, "<cwd>")).collect();
        let no_merge = env
            .iter()
            .find(|(key, _)| *key == "TARGET_NO_MERGE")
            .map(|(_, value)| *value);
        self.calls.borrow_mut().push(json!({
            "argv": shown,
            "cwd_is_worktree": cwd == self.cwd,
            "timeout": timeout_s,
            "target_no_merge": no_merge,
        }));
        self.order.borrow_mut().push(stage.to_string());
        let raises = self.case["raises"]
            .as_array()
            .is_some_and(|r| r.iter().any(|s| s.as_str() == Some(stage)));
        if raises {
            return None;
        }
        Some(RunResult {
            code: self.case["codes"][stage].as_i64().unwrap_or(0) as i32,
            stdout: String::new(),
            stderr: if stage == "stop" {
                self.case["stop_stderr"].as_str().unwrap_or("").to_string()
            } else {
                String::new()
            },
        })
    }
    fn node_is_done(&self, _node: &str) -> bool {
        self.case["node_done"].as_bool().unwrap_or(false)
    }
    fn alias_predecessor(&self, new_name: &str, old_name: Option<&str>) {
        self.aliases.borrow_mut().push(json!([new_name, old_name]));
    }
}

#[test]
fn redispatch_keeps_its_order_and_a_failed_stop_never_spawns() {
    let g = golden("redispatch.json");
    let root = tempfile::tempdir().unwrap();
    let mut seq = 0;
    for case in g["cases"].as_array().unwrap() {
        let label = case["label"].as_str().unwrap();
        let cwd = make_cwd(
            root.path(),
            case["cwd"]
                .as_str()
                .unwrap_or("manifest:graph_node_id: x-1\n"),
            &mut seq,
        );
        let candidate = Candidate {
            short_id: "aaaa1111".to_string(),
            sock_path: "/s".to_string(),
            jobs_dir: PathBuf::from("/j"),
            cwd: cwd.clone(),
            name: case["name"].as_str().map(str::to_string),
            session_id: None,
            launch_account: None,
        };
        let order: RefCell<Vec<String>> = RefCell::new(Vec::new());
        let seams = Stage {
            case,
            cwd: cwd.unwrap_or_default(),
            order: &order,
            calls: RefCell::new(Vec::new()),
            aliases: RefCell::new(Vec::new()),
        };
        let ready = case["pre_spawn"].as_bool();
        let pre_spawn = |answer: bool| {
            order.borrow_mut().push("pre_spawn".to_string());
            answer
        };
        let hook = ready.map(|answer| move || pre_spawn(answer));
        let hook_ref: Option<&dyn Fn() -> bool> = hook.as_ref().map(|f| f as &dyn Fn() -> bool);
        let flags: Option<Vec<String>> = case["flags"]
            .as_array()
            .map(|f| f.iter().map(|v| v.as_str().unwrap().to_string()).collect());
        let outcome = match redispatch(&candidate, &seams, hook_ref, flags.as_deref()) {
            RedispatchOutcome::Respawned => "respawned".to_string(),
            RedispatchOutcome::NotStarted => "not-started".to_string(),
            RedispatchOutcome::Failed(reason) => format!("failed:{reason}"),
        };
        let got = json!({
            "outcome": outcome,
            "order": order.borrow().clone(),
            "calls": seams.calls.borrow().clone(),
            "aliases": seams.aliases.borrow().clone(),
        });
        assert_eq!(text(&got), text(&case["expect"]), "{label}");
    }
}
