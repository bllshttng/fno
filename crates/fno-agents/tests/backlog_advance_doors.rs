//! Runtime contracts migrated from the retired Python dispatch owners.
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

struct World {
    dir: tempfile::TempDir,
    root: PathBuf,
    config: PathBuf,
}
impl World {
    fn new(rows: Value) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("state");
        std::fs::create_dir_all(&root).unwrap();
        let config = root.join("config.toml");
        std::fs::write(&config,format!("state_dir = {:?}\n[agents]\nmax_live = 4\n[agents.defaults]\nprovider = \"claude\"\nmodel = \"glm-5.3-flash[1m]\"\n",root.to_string_lossy())).unwrap();
        fno_agents::graph_store::seed_rows(&root.join("graph.json"), rows.as_array().unwrap())
            .unwrap();
        std::fs::create_dir_all(dir.path().join("agents")).unwrap();
        Self { dir, root, config }
    }
    fn command(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_fno-agents"));
        c.current_dir(&self.root)
            .env("FNO_CONFIG", &self.config)
            .env("FNO_STATE_DIR", &self.root)
            .env("FNO_TRACKER_BACKEND", "graph")
            .env("FNO_CONFIG_SEARCH_ROOT", self.dir.path())
            .envs(fno_agents::test_run::self_owner_env())
            .env("FNO_EVENTS_PATH", self.root.join("events.jsonl"))
            .env("FNO_AGENTS_HOME", self.dir.path().join("agents"))
            .env("FNO_CLAIMS_ROOT", self.dir.path().join("claims"))
            .env("FNO_GLOBAL_SETTINGS_PATH", "/dev/null")
            .env("FNO_NO_CANONICAL_CONFIG", "1")
            .env("FNO_SPAWN_GATE", "0")
            .env("FNO_AUTO_CONTINUE", "0")
            .env("CLAUDE_CONFIG_DIR", self.dir.path().join("claude"));
        for key in [
            "FNO_AGENT_SELF",
            "FNO_HARNESS_NAME",
            "FNO_HARNESS_SESSION_ID",
            "FNO_SESSION_ID",
            "CODEX_THREAD_ID",
            "CLAUDECODE_SESSION_ID",
            "CLAUDE_SESSION_ID",
            "FNO_BG",
            "TARGET_BRIEF",
        ] {
            c.env_remove(key);
        }
        c
    }
    fn run(&self, args: &[&str], request: Option<&Value>) -> Output {
        let mut c = self.command();
        c.args(args);
        self.output(c, request)
    }
    fn output(&self, mut c: Command, request: Option<&Value>) -> Output {
        c.stdout(Stdio::piped()).stderr(Stdio::piped());
        if let Some(request) = request {
            let mut child = c.stdin(Stdio::piped()).spawn().unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(request.to_string().as_bytes())
                .unwrap();
            child.wait_with_output().unwrap()
        } else {
            c.output().unwrap()
        }
    }
    fn claim(&self, id: &str, worktree: &Path) {
        let key = format!("node:{id}");
        let metadata = json!({"worktree":worktree}).to_string();
        let out = self.run(
            &[
                "claim",
                "acquire",
                &key,
                "--holder",
                "fixture-holder",
                "--pid",
                &std::process::id().to_string(),
                "--ttl-ms",
                "180000",
                "--metadata",
                &metadata,
            ],
            None,
        );
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    fn plan(&self, text: &str) -> PathBuf {
        let p = self.dir.path().join("plan.md");
        std::fs::write(&p, text).unwrap();
        p
    }
    #[cfg(unix)]
    fn endpoint(&self, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = self.dir.path().join("fno-endpoint");
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }
    fn events(&self) -> Vec<Value> {
        let roots = [self.root.clone(), self.dir.path().join("agents")];
        let mut out = Vec::new();
        for root in roots {
            let journal = root.join("events.jsonl");
            if fno_agents::event_store::store_path(&journal).is_file() {
                for row in
                    fno_agents::event_store::query_events(&journal, &Default::default()).unwrap()
                {
                    out.push(serde_json::from_str::<Value>(&row.line).unwrap());
                }
            }
        }
        out
    }
}
fn plan(tasks: &str, waves: &str) -> String {
    format!("---\ndifficulty: high\nstatus: ready\n---\n## Execution Strategy\n```yaml\ntasks:\n{tasks}\nwaves:\n{waves}\n```\n")
}
fn row(id: &str, extra: Value) -> Value {
    let mut r = json!({"id":id,"title":id,"slug":id,"type":"task","project":"fixture","priority":"p2","status":"ready","dispatch_command":"/target","difficulty":"high","created_at":chrono::Utc::now().to_rfc3339()});
    r.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    r
}
fn decode(out: &Output) -> Value {
    assert!(
        out.status.success(),
        "status={} stdout={} stderr={}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stdout)))
}

#[test]
fn public_advance_preserves_disabled_receipt_and_validates_flag_combinations() {
    let w = World::new(json!([]));
    let out = w.run(&["backlog", "advance"], None);
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "skipped reason=disabled\n"
    );
    let events = w.events();
    assert_eq!(
        events
            .iter()
            .filter(|r| r["type"] == "advance_skipped" || r["kind"] == "advance_skipped")
            .count(),
        1
    );
    for args in [
        vec!["--epic", "x-aaaa", "--closed", "x-bbbb"],
        vec!["--stop"],
        vec!["--continuation"],
        vec!["--loose"],
        vec!["--source", "bogus"],
        vec!["--model", ""],
    ] {
        let mut argv = vec!["backlog", "advance"];
        argv.extend(args);
        assert_eq!(w.run(&argv, None).status.code(), Some(2));
    }
    let out = w.run(&["backlog", "advance", "--explain"], None);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("SELECTION  0 candidates -> 0 eligible"));
    assert!(text.contains("GATES\n"));
    assert!(text.contains("(no chain: nothing to route)"));
    assert!(text.contains("would dispatch: nothing (no eligible node)"));
    assert_eq!(
        w.events().len(),
        events.len(),
        "explain must not emit decisions or take claims"
    );
}
#[test]
fn join_refusal_codes_have_one_outcome_event_and_do_not_launch() {
    let w = World::new(json!([row("x-aaaa", json!({}))]));
    let out = w.run(&["backlog", "join", "x-aaaa"], None);
    assert_eq!(out.status.code(), Some(4));
    let path = w.plan(&plan(
        "- id: a\n  surface: [a.rs]",
        "- wave: 1\n  tasks: [a]",
    ));
    let w = World::new(json!([row("x-aaaa", json!({"plan_path":path}))]));
    assert_eq!(
        w.run(&["backlog", "join", "x-aaaa"], None).status.code(),
        Some(2)
    );
    let pid = std::process::id().to_string();
    let acquired = w.run(
        &[
            "claim",
            "acquire",
            "node:x-aaaa",
            "--holder",
            "fixture-holder",
            "--pid",
            &pid,
            "--ttl-ms",
            "180000",
        ],
        None,
    );
    assert!(acquired.status.success());
    let missing = w.run(&["backlog", "join", "x-aaaa"], None);
    assert_eq!(missing.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&missing.stderr).contains("names no holder worktree"));
    let released = w.run(
        &[
            "claim",
            "release",
            "node:x-aaaa",
            "--holder",
            "fixture-holder",
        ],
        None,
    );
    assert!(released.status.success());
    w.claim("x-aaaa", &w.root);
    let out = w.run(&["backlog", "join", "x-aaaa"], None);
    assert_eq!(
        out.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let events = w.events();
    let refusals = events
        .iter()
        .filter(|r| r["type"] == "join_refused" || r["kind"] == "join_refused")
        .collect::<Vec<_>>();
    assert_eq!(refusals.len(), 3);
    assert!(events
        .iter()
        .all(|r| r["type"] != "join_dispatched" && r["kind"] != "join_dispatched"));
}
#[cfg(unix)]
#[test]
fn join_persists_brief_and_enforced_policies_before_each_launch() {
    let seed = World::new(json!([]));
    let p=seed.plan(&plan("- id: a\n  surface: [a.rs]\n- id: b\n  surface: [b.rs]\n- id: c\n  surface: [c.rs]\n  blocked_by: []","- wave: 1\n  difficulty: high\n  tasks: [a, b]\n- wave: 2\n  difficulty: low\n  tasks: [c]"));
    for sandbox in [false, true] {
        let w = World::new(json!([row("x-aaaa", json!({"plan_path":p}))]));
        let prior = std::fs::read_to_string(&w.config).unwrap();
        std::fs::write(&w.config, format!("{prior}\n[join]\nsandbox = {sandbox}\n")).unwrap();
        w.claim("x-aaaa", &w.root);
        let endpoint=w.endpoint("printf '%s\\n' \"$*\" >> \"$LAUNCH_LOG\"\npolicy=\nfor arg in \"$@\"; do\n  if [ \"$policy\" = next ]; then [ -s \"$arg\" ] || exit 9; policy=seen; fi\n  if [ \"$arg\" = --sandbox-write-policy ]; then policy=next; fi\ndone\n[ -s \"$JOIN_BRIEF\" ] || exit 10\nprintf '%s\\n' '{\"short_id\":\"abcd1234\"}'");
        let log = w.dir.path().join("launches");
        let mut c = w.command();
        c.args([
            "backlog",
            "join",
            "x-aaaa",
            "--workers",
            "4",
            "--model",
            "glm-5.3-flash[1m]",
        ])
        .env("FNO_BIN", endpoint)
        .env("LAUNCH_LOG", &log)
        .env("JOIN_BRIEF", w.root.join(".fno/join-briefs/x-aaaa.md"));
        let receipt = decode(&w.output(c, None));
        assert_eq!(receipt["width"], 3);
        assert_eq!(receipt["spawned"].as_array().unwrap().len(), 2);
        let launches = std::fs::read_to_string(log).unwrap();
        assert_eq!(launches.lines().count(), 2);
        assert_eq!(
            launches.matches("--sandbox-write-policy").count(),
            if sandbox { 2 } else { 0 }
        );
        // The joiners seat threads on the lane template: the retired model and
        // substrate axes never ride the spawn.
        assert!(
            !launches.contains("--model") && !launches.contains("--substrate"),
            "{launches}"
        );
        let brief = std::fs::read_to_string(w.root.join(".fno/join-briefs/x-aaaa.md")).unwrap();
        assert!(brief.contains(receipt["lead"].as_str().unwrap()));
        assert!(brief.contains("claim ONE ready task"));
        assert_eq!(
            w.events()
                .iter()
                .filter(|r| r["type"] == "join_dispatched" || r["kind"] == "join_dispatched")
                .count(),
            1
        );
    }
}
#[cfg(unix)]
#[test]
fn isolated_lanes_use_the_nodes_repo_and_return_failed_slots() {
    let seed = World::new(json!([]));
    let p = seed.plan("---\nstatus: ready\ndifficulty: high\n---\n");
    let w = World::new(json!([row(
        "x-aaaa",
        json!({"plan_path":p,"cwd":seed.root,"model":"glm-5.3-flash[1m]"})
    )]));
    let script_dir = seed.root.join("scripts/setup");
    std::fs::create_dir_all(&script_dir).unwrap();
    std::fs::write(script_dir.join("setup-worktree.sh"), "#!/bin/sh\nexit 0\n").unwrap();
    let lane = w.dir.path().join("lane");
    std::fs::create_dir_all(&lane).unwrap();
    let endpoint=w.endpoint("case \"$*\" in\n  'agents workspace worktree ensure '*) printf '%s\\n' \"$LANE_ROOT\"; printf '%s\\n' \"$*\" >> \"$LAUNCH_LOG\" ;;\n  'agents spawn '*) printf '%s\\n' \"$*\" >> \"$LAUNCH_LOG\"; [ -s \"$LANE_ROOT/.fno/config.local.toml\" ] || exit 9; printf '%s\\n' 'launch transport failed' >&2; exit 1 ;;\n  *) printf '%s\\n' '{}' ;;\nesac");
    let log = w.dir.path().join("launches");
    let mut c = w.command();
    c.args([
        "backlog",
        "dispatch-lanes",
        "--max",
        "1",
        "--project",
        "fixture",
        "--harness",
        "claude",
        "--model",
        "glm-5.3-flash[1m]",
    ])
    .env("FNO_BIN", &endpoint)
    .env("LANE_ROOT", &lane)
    .env("LAUNCH_LOG", &log);
    let out = w.output(c, None);
    let value: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "{e}: {} / {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    });
    assert_eq!(value["fill"]["selected"], 1, "{value}");
    assert_eq!(value["fill"]["skipped"], 1);
    assert_eq!(value["fill"]["dispatched"], 0);
    assert_eq!(out.status.code(), Some(1));
    let calls = std::fs::read_to_string(log).unwrap();
    assert!(calls.contains(&format!("--repo {}", seed.root.display())));
    assert!(calls.contains(&format!("--cwd {}", lane.display())));
    let state = decode(&w.run(&["claim", "status", "dispatch:x-aaaa"], None));
    assert_eq!(state["state"], "free");
    let count = decode(&w.run(&["claim", "lane-count", "--json"], None));
    assert_eq!(count["active_lanes"], 0);
}

#[test]
fn closed_advance_is_single_flight_and_epic_scopes_are_independent() {
    let w = World::new(json!([]));
    let pid = std::process::id().to_string();
    let out = w.run(
        &[
            "claim",
            "flight-acquire",
            "flight:backlog-advance",
            "--holder",
            "fixture",
            "--scope",
            "advance",
            "--pid",
            &pid,
            "--ttl-ms",
            "180000",
        ],
        None,
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value = decode(&w.run(
        &["backlog", "advance", "--closed", "x-aaaa", "--json"],
        None,
    ));
    assert_eq!(value["decision"], "held");
    assert!(value["held"] == true);
    assert!(w.events().iter().all(|e| e["type"] != "advance_dispatched"));
    let out = w.run(&["backlog", "advance", "--epic", "x-aaaa", "--json"], None);
    assert_eq!(out.status.code(), Some(1));
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["error"], "no-such-node");
}

#[cfg(unix)]
#[test]
fn advance_reads_real_selection_and_preserves_launch_pins() {
    let seed = World::new(json!([]));
    let p = seed.plan("---\nstatus: ready\ndifficulty: high\n---\n");
    let w = World::new(json!([row(
        "x-aaaa",
        json!({"plan_path":p,"cwd":seed.root,"model":"glm-5.3-flash[1m]","provider":"claude","effort":"high"})
    )]));
    let endpoint=w.endpoint("case \"$*\" in\n  'agents spawn '*) printf '%s\\n' \"$*\" >> \"$LAUNCH_LOG\"; printf '%s\\n' '{\"short_id\":\"abcd1234\"}' ;;\n  *) exec \"$NATIVE_DOOR\" \"$@\" ;;\nesac");
    let log = w.dir.path().join("launches");
    let invoke = || {
        let mut c = w.command();
        c.args([
            "backlog",
            "advance",
            "--project",
            "fixture",
            "--model",
            "glm-5.3-flash[1m]",
            "--provider",
            "claude",
            "--json",
        ])
        .env("FNO_AUTO_CONTINUE", "1")
        .env("FNO_BIN", &endpoint)
        .env("NATIVE_DOOR", env!("CARGO_BIN_EXE_fno-agents"))
        .env("LAUNCH_LOG", &log);
        w.output(c, None)
    };
    let receipt = decode(&invoke());
    assert_eq!(receipt["decision"], "dispatched", "{receipt}");
    assert_eq!(receipt["node_id"], "x-aaaa");
    let calls = std::fs::read_to_string(&log).unwrap();
    assert_eq!(calls.lines().count(), 1);
    // The node's model and effort pins resolve at the spawn door through
    // the lane template; the retired axes never ride the launch argv.
    assert!(
        !calls.contains("--effort") && !calls.contains("--model"),
        "{calls}"
    );
    assert!(calls.contains("--node x-aaaa"), "{calls}");
    w.claim("x-aaaa", &seed.root);
    let _ = decode(&invoke());
    assert_eq!(
        std::fs::read_to_string(log).unwrap().lines().count(),
        1,
        "a live node must not launch twice"
    );
}
