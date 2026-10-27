//! Parity for the deleted Python leg `cli/src/fno/status_fanout.py` ported
//! to `fno_agents::status_fanout`. The goldens freeze the Python decision
//! records: the tick harness froze `{result, errors, cursors, park_keys,
//! label}` (sort-keys compact JSON), the adapter cases froze `{"calls": [...]}`
//! for the recorded webhook posts, the template cases froze `{"rendered"}`
//! records, and the two CLI cases froze the front-door usage bytes.
//!
//! parity-stage: characterization
//! parity-oracle: cli/src/fno/status_fanout.py
//!
//! Capture deviation: the mission freeze barred local cargo runs, so the
//! goldens were captured by driving the Python leg directly (a sandboxed
//! driver pinning HOME/FNO_STATE_DIR/FNO_TEST_HERMETIC/FNO_NO_CANONICAL_CONFIG,
//! scripting dispatch and the stream read exactly as this test scripts its
//! seams) instead of the FNO_CAPTURE_GOLDEN=1 path. The oracle file is gone,
//! so capture mode refuses. These bytes are final.

mod common;

use common::{assert_golden, env_mutex, Golden};
use fno_agents::status_fanout::{
    run_tick, DispatchFn, FanoutTuning, HttpPost, HttpResult, SinkConfig, StreamFn, TickResult,
};
use fno_agents::wake_read::python_json_default;
use serde_json::{json, Value};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The four fixture events the capture driver froze, byte-identical.
fn jline(ts: &str, etype: &str, extra: Value) -> Value {
    let mut row = json!({"ts": ts, "v": 1, "type": etype, "source": "target", "data": {}});
    let obj = row.as_object_mut().unwrap();
    if let Some(ex) = extra.as_object() {
        for (k, v) in ex {
            obj.insert(k.clone(), v.clone());
        }
    }
    row
}

/// Build a SinkConfig the way the driver built `StatusSinkConfig(**base)`.
#[allow(clippy::too_many_arguments)]
fn sink_cfg(
    name: &str,
    etypes: &[&str],
    stype: &str,
    url: Option<&str>,
    url_env: Option<&str>,
    extra: Value,
) -> SinkConfig {
    let mut s = SinkConfig {
        name: name.to_string(),
        kind: stype.to_string(),
        events: etypes.iter().map(|e| e.to_string()).collect(),
        match_keys: BTreeMap::new(),
        url: url.map(str::to_string),
        url_env: url_env.map(str::to_string),
        template: None,
        field: "content".to_string(),
        raw_body: false,
        cloudevents: false,
        enabled: true,
    };
    if let Some(ex) = extra.as_object() {
        for (k, v) in ex {
            match k.as_str() {
                "match" => {
                    for (mk, mv) in v.as_object().unwrap() {
                        s.match_keys.insert(mk.clone(), mv.clone());
                    }
                }
                "template" => s.template = v.as_str().map(str::to_string),
                "field" => s.field = v.as_str().unwrap().to_string(),
                "raw_body" => s.raw_body = v.as_bool().unwrap(),
                "cloudevents" => s.cloudevents = v.as_bool().unwrap(),
                "enabled" => s.enabled = v.as_bool().unwrap(),
                _ => {}
            }
        }
    }
    s
}

/// The recording HTTP seam: returns the scripted results, records
/// `{"url", "body", "content_type"}` per post, JSON bodies re-parsed so the
/// record renders them sort-keys like the Python fake did.
struct RecordingPost {
    responses: Vec<HttpResult>,
    calls: std::cell::RefCell<Vec<(String, Value, String)>>,
}

impl RecordingPost {
    fn always_ok() -> Self {
        RecordingPost {
            responses: vec![],
            calls: RefCell::new(Vec::new()),
        }
    }
}

impl HttpPost for RecordingPost {
    fn post(
        &mut self,
        url: &str,
        content_type: &str,
        body: &str,
        _timeout_secs: u64,
    ) -> HttpResult {
        self.calls.borrow_mut().push((
            url.to_string(),
            if content_type == "application/json" {
                serde_json::from_str(body).unwrap_or(Value::String(body.to_string()))
            } else {
                Value::String(body.to_string())
            },
            content_type.to_string(),
        ));
        self.responses.pop().unwrap_or(HttpResult {
            ok: true,
            status: Some(200),
            retry_after: None,
        })
    }
}

/// The sandbox: HOME and FNO_STATE_DIR pinned inside a temp dir, the hermetic
/// posture on, no spaces override. Mirrors the capture driver's environment.
struct Sandbox {
    root: PathBuf,
    state: PathBuf,
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl Sandbox {
    fn new(tag: &str) -> Self {
        let _guard = env_mutex();
        let base = std::env::temp_dir().join(format!("fanout-parity-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let root = base.join(tag).join("proj");
        std::fs::create_dir_all(&root).expect("sandbox proj dir");
        std::env::set_var("HOME", &home);
        std::env::set_var("USERPROFILE", &home);
        std::env::set_var("FNO_STATE_DIR", &base.join("state"));
        std::env::set_var("FNO_TEST_HERMETIC", "1");
        std::env::set_var("FNO_NO_CANONICAL_CONFIG", "1");
        let _ = std::env::remove_var("FNO_SPACES_DIR");
        let _ = std::env::remove_var("FNO_CONFIG");
        let state = fno_agents::paths::space_dir(&root).join("status-sinks");
        Sandbox {
            root,
            state,
            _guard,
        }
    }
}

/// Read the state files back the way the capture driver's `projection` did:
/// cursors keyed by filename, errors rows parsed per file, park keys sorted
/// (or null when the file is absent).
fn projection(result: &TickResult, state: &Path, label: &str) -> Value {
    let mut cursors = BTreeMap::new();
    if let Ok(entries) = std::fs::read_dir(state) {
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if name.ends_with(".cursor") {
                let raw = std::fs::read_to_string(&path).unwrap_or_default();
                if let Ok(v) = serde_json::from_str::<Value>(&raw) {
                    cursors.insert(name.to_string(), v);
                }
            }
        }
    }
    let mut errors: BTreeMap<String, Value> = BTreeMap::new();
    if let Ok(entries) = std::fs::read_dir(state) {
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if name.ends_with(".errors.jsonl") {
                let raw = std::fs::read_to_string(&path).unwrap_or_default();
                let rows: Vec<Value> = raw
                    .lines()
                    .filter_map(|l| serde_json::from_str(l).ok())
                    .collect();
                errors.insert(name.to_string(), Value::Array(rows));
            }
        }
    }
    let parks_path = state.join("short-circuit-backoff.json");
    let park_keys: Value = if parks_path.exists() {
        let raw = std::fs::read_to_string(&parks_path).unwrap_or_default();
        let obj: BTreeMap<String, f64> = serde_json::from_str(&raw).unwrap_or_default();
        json!(obj.keys().collect::<Vec<_>>())
    } else {
        Value::Null
    };
    json!({
        "result": tick_result_value(result),
        "errors": errors,
        "cursors": cursors,
        "park_keys": park_keys,
        "label": label,
    })
}

/// Per-case options, defaulted to a plain delivered-everything tick.
struct CaseOpts {
    cursors: Vec<(String, String, i64)>,
    parks: Option<Value>,
    pre_lock: bool,
    dry_run: bool,
    script: BTreeMap<String, (String, String)>,
    stream: Option<(Vec<Value>, u64, Option<i64>)>,
    raw_lines: Vec<String>,
}

impl Default for CaseOpts {
    fn default() -> Self {
        CaseOpts {
            cursors: Vec::new(),
            parks: None,
            pre_lock: false,
            dry_run: false,
            script: BTreeMap::new(),
            stream: None,
            raw_lines: Vec::new(),
        }
    }
}

/// One scripted tick case: seed state, write the journal, run the tick with
/// the scripted dispatch and stream seams, then assert the frozen record.
#[allow(clippy::too_many_arguments)]
fn run_tick_case(sb: &Sandbox, label: &str, rows: &[Value], sinks: &[SinkConfig], opts: CaseOpts) {
    let _ = std::fs::remove_dir_all(&sb.state);
    std::fs::create_dir_all(&sb.state).unwrap();
    for (name, ts, n) in &opts.cursors {
        let cur = json!({"ts": ts, "n": n}).to_string();
        std::fs::write(sb.state.join(format!("{name}.cursor")), cur).unwrap();
    }
    if let Some(parks) = &opts.parks {
        std::fs::write(
            sb.state.join("short-circuit-backoff.json"),
            parks.to_string(),
        )
        .unwrap();
    }
    if opts.pre_lock {
        let lock = sb.state.join(".tick.lock.d");
        std::fs::create_dir_all(&lock).unwrap();
        let past = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            - 1;
        let times = [libc::timeval {
            tv_sec: past as libc::time_t,
            tv_usec: 0,
        }; 2];
        let c = std::ffi::CString::new(lock.as_os_str().as_encoded_bytes()).unwrap();
        unsafe { libc::utimes(c.as_ptr(), times.as_ptr()) };
    }
    let journal = fno_agents::paths::events_path(&sb.root);
    std::fs::create_dir_all(journal.parent().unwrap()).unwrap();
    let mut text = String::new();
    for row in rows {
        text.push_str(&row.to_string());
        text.push('\n');
    }
    for raw in &opts.raw_lines {
        text.push_str(raw);
        text.push('\n');
    }
    std::fs::write(&journal, text).unwrap();

    let mut post = RecordingPost::always_ok();
    let script = opts.script;
    let stream_rows = opts.stream;
    let mut dispatch_seam: Box<DispatchFn> = Box::new(move |sink, _event| {
        script
            .get(&sink.name)
            .cloned()
            .unwrap_or_else(|| ("delivered".to_string(), String::new()))
    });
    let mut stream_seam: Box<StreamFn> = match stream_rows {
        Some((rows, skipped, high)) => Box::new(move |_, _, _| (rows.clone(), skipped, high)),
        None => Box::new(|a, s, q| fno_agents::status_fanout::stream_since(a, s, q)),
    };
    let result = run_tick(
        &sb.root,
        sinks,
        &FanoutTuning::default(),
        opts.dry_run,
        &mut post,
        Some(&mut *dispatch_seam),
        Some(&mut *stream_seam),
    );
    let record = projection(&result, &sb.state, label);
    let dumped = python_json_default(&record);
    assert_fanout_golden(
        label,
        &Golden {
            exit: Some(0),
            streams: vec![dumped],
        },
    );
}

/// The Python capture driver's `asdict(result)`: the tick outcome as the
/// record's `result` field (field names are the deleted leg's dataclass).
fn tick_result_value(result: &TickResult) -> Value {
    json!({
        "sinks": result
            .sinks
            .iter()
            .map(|s| {
                json!({
                    "name": s.name,
                    "matched": s.matched,
                    "dispatched": s.dispatched,
                    "dropped": s.dropped,
                    "short_circuited": s.short_circuited,
                    "new_cursor": s.new_cursor,
                })
            })
            .collect::<Vec<_>>(),
        "skipped_lines": result.skipped_lines,
        "rows_read": result.rows_read,
        "locked_out": result.locked_out,
        "lease_lost": result.lease_lost,
        "undeliverable_sinks": result.undeliverable_sinks,
        "parked_sinks": result.parked_sinks,
    })
}

/// One adapter case, the capture driver's shape: fresh state, a pre-seeded
/// cursor at the fixture event's instant with occurrence 0, the single event
/// as the journal, one tick, then the frozen `{"calls": [...]}` record. The
/// driver serialized with `json.dumps(..., sort_keys=True)` default
/// separators, so keys go in sorted order through `python_json_default`.
fn adapter_case(sb: &Sandbox, label: &str, sink: SinkConfig, post: &mut RecordingPost, ev: &Value) {
    let _ = std::fs::remove_dir_all(&sb.state);
    std::fs::create_dir_all(&sb.state).unwrap();
    let cursor = json!({"ts": "2026-01-01T00:00:02Z", "n": 0}).to_string();
    std::fs::write(sb.state.join(format!("{}.cursor", sink.name)), cursor).unwrap();
    let journal = fno_agents::paths::events_path(&sb.root);
    std::fs::create_dir_all(journal.parent().unwrap()).unwrap();
    // The capture driver froze the tick over its two-event fixture (the ok
    // event the caller built, plus the fail event at 00:00:05Z), so every
    // adapter golden carries both calls byte-identically.
    let ev2 = jline(
        "2026-01-01T00:00:05Z",
        "task_done",
        json!({"run": "r1", "node": "ab-12", "outcome": "fail"}),
    );
    std::fs::write(&journal, format!("{ev}\n{ev2}\n")).unwrap();
    // No dispatch seam: the goldens hold the real adapters' webhook bodies,
    // so the real dispatch_event path must run against the RecordingPost.
    // A seam would answer "delivered" without ever touching the post.
    let _ = run_tick(
        &sb.root,
        std::slice::from_ref(&sink),
        &FanoutTuning::default(),
        false,
        post,
        None,
        None,
    );
    let calls: Vec<Value> = post
        .calls
        .borrow()
        .iter()
        .map(|(url, body, content_type)| {
            let mut obj = serde_json::Map::new();
            obj.insert("body".into(), body.clone());
            obj.insert("content_type".into(), Value::String(content_type.clone()));
            obj.insert("url".into(), Value::String(url.clone()));
            Value::Object(obj)
        })
        .collect();
    post.calls.borrow_mut().clear();
    let record = json!({"calls": calls});
    assert_fanout_golden(
        label,
        &Golden {
            exit: Some(0),
            streams: vec![python_json_default(&record)],
        },
    );
}

/// Every status_fanout golden lives under the capture driver's `fanout `
/// label prefix; the shared helper takes the label verbatim.
fn assert_fanout_golden(label: &str, golden: &Golden) {
    assert_golden("status_fanout", &format!("fanout {label}"), golden, None);
}

/// A dispatch script entry for one sink.
fn script_of(name: &str, status: &str, detail: &str) -> BTreeMap<String, (String, String)> {
    BTreeMap::from([(name.to_string(), (status.to_string(), detail.to_string()))])
}

#[test]
fn fanout_cases_match_the_captured_goldens() {
    let sb = Sandbox::new("ticks");

    // The four fixture events the driver froze, byte-identical.
    let e = [
        jline("2026-01-01T00:00:01Z", "task_started", json!({"run": "r1"})),
        jline(
            "2026-01-01T00:00:02Z",
            "task_done",
            json!({"run": "r1", "node": "ab-12", "outcome": "ok"}),
        ),
        jline(
            "2026-01-01T00:00:02Z",
            "blocked",
            json!({"run": "r1", "node": "ab-12", "reason": "ci red"}),
        ),
        jline(
            "2026-01-01T00:00:05Z",
            "task_done",
            json!({"run": "r1", "node": "ab-12", "outcome": "fail"}),
        ),
    ];

    let url = "https://example.invalid/hook";
    let hook = |name: &str, etypes: &[&str]| {
        sink_cfg(name, etypes, "json-webhook", Some(url), None, json!({}))
    };

    run_tick_case(
        &sb,
        "fresh sink starts at eof",
        &e,
        &[hook("s1", &["task_done"])],
        CaseOpts::default(),
    );
    run_tick_case(
        &sb,
        "resumed cursor delivers newer",
        &e,
        &[hook("s1", &["task_done"])],
        CaseOpts {
            cursors: vec![("s1".into(), "2026-01-01T00:00:02Z".into(), 1)],
            ..Default::default()
        },
    );
    run_tick_case(
        &sb,
        "same ts occurrence advanced past",
        &e,
        &[sink_cfg(
            "s1",
            &["task_done", "blocked"],
            "json-webhook",
            Some(url),
            None,
            json!({}),
        )],
        CaseOpts {
            cursors: vec![("s1".into(), "2026-01-01T00:00:02Z".into(), 2)],
            ..Default::default()
        },
    );
    run_tick_case(
        &sb,
        "dry run writes nothing",
        &e,
        &[hook("s1", &["task_done"])],
        CaseOpts {
            dry_run: true,
            ..Default::default()
        },
    );
    run_tick_case(
        &sb,
        "short circuit holds cursor",
        &e,
        &[hook("s1", &["task_done"])],
        CaseOpts {
            cursors: vec![("s1".into(), "2026-01-01T00:00:02Z".into(), 1)],
            script: script_of("s1", "short_circuit", ""),
            ..Default::default()
        },
    );
    run_tick_case(
        &sb,
        "drop advances with errors row",
        &e,
        &[hook("s1", &["task_done"])],
        CaseOpts {
            cursors: vec![("s1".into(), "2026-01-01T00:00:02Z".into(), 1)],
            script: script_of("s1", "dropped", "http 422"),
            ..Default::default()
        },
    );
    run_tick_case(
        &sb,
        "unknown status drops named",
        &e,
        &[hook("s1", &["task_done"])],
        CaseOpts {
            cursors: vec![("s1".into(), "2026-01-01T00:00:02Z".into(), 1)],
            script: script_of("s1", "wat", ""),
            ..Default::default()
        },
    );
    run_tick_case(
        &sb,
        "undeliverable sink skips read",
        &e,
        &[sink_cfg(
            "s1",
            &["task_done"],
            "json-webhook",
            None,
            Some("NOPE_ENV"),
            json!({}),
        )],
        CaseOpts::default(),
    );
    run_tick_case(
        &sb,
        "parked sinks skip read",
        &e,
        &[hook("s1", &["task_done"])],
        CaseOpts {
            parks: Some(json!({"s1": 9999999999.0})),
            ..Default::default()
        },
    );
    run_tick_case(
        &sb,
        "locked out tick skips",
        &e,
        &[hook("s1", &["task_done"])],
        CaseOpts {
            pre_lock: true,
            ..Default::default()
        },
    );
    run_tick_case(
        &sb,
        "disabled sink is a noop",
        &e,
        &[sink_cfg(
            "s1",
            &["task_done"],
            "json-webhook",
            Some(url),
            None,
            json!({"enabled": false}),
        )],
        CaseOpts::default(),
    );
    run_tick_case(&sb, "no sinks at all", &e, &[], CaseOpts::default());
    run_tick_case(
        &sb,
        "malformed lines skip and count",
        &e,
        &[hook("s1", &["task_done"])],
        CaseOpts {
            raw_lines: vec!["not json".into(), String::new()],
            ..Default::default()
        },
    );
    run_tick_case(
        &sb,
        "store window advances scan cursor",
        &e,
        &[hook("s1", &["task_done"])],
        CaseOpts {
            stream: Some((e.iter().take(3).cloned().collect(), 1, Some(41))),
            ..Default::default()
        },
    );
    run_tick_case(
        &sb,
        "match key filters events",
        &e,
        &[sink_cfg(
            "s1",
            &["task_done"],
            "json-webhook",
            Some(url),
            None,
            json!({"match": {"outcome": "ok"}}),
        )],
        CaseOpts::default(),
    );
    run_tick_case(
        &sb,
        "two sinks isolate",
        &e,
        &[hook("s1", &["task_done"]), hook("s2", &["task_done"])],
        CaseOpts {
            cursors: vec![
                ("s1".into(), "2026-01-01T00:00:02Z".into(), 1),
                ("s2".into(), "2026-01-01T00:00:02Z".into(), 1),
            ],
            script: script_of("s2", "short_circuit", ""),
            ..Default::default()
        },
    );
}

#[test]
fn fanout_adapter_and_template_cases_match_the_captured_goldens() {
    let sb = Sandbox::new("adapters");
    let ev = jline(
        "2026-01-01T00:00:02Z",
        "task_done",
        json!({"run": "r1", "node": "ab-12", "outcome": "ok"}),
    );
    let url = "https://example.invalid/hook";
    let mut post = RecordingPost::always_ok();
    adapter_case(
        &sb,
        "cloudevents wrap id and envelope",
        sink_cfg(
            "ce",
            &["task_done"],
            "json-webhook",
            Some(url),
            None,
            json!({"cloudevents": true}),
        ),
        &mut post,
        &ev,
    );
    adapter_case(
        &sb,
        "text webhook discord shape",
        sink_cfg(
            "tw",
            &["task_done"],
            "text-webhook",
            Some(url),
            None,
            json!({"template": "job {data.outcome} for {run} missing=[{nope}]", "field": "content"}),
        ),
        &mut post,
        &ev,
    );
    adapter_case(
        &sb,
        "text webhook slack defang",
        sink_cfg(
            "tw2",
            &["task_done"],
            "text-webhook",
            Some(url),
            None,
            json!({"field": "text", "template": "<!channel {data.outcome}"}),
        ),
        &mut post,
        &ev,
    );
    adapter_case(
        &sb,
        "text webhook raw body",
        sink_cfg(
            "tw3",
            &["task_done"],
            "text-webhook",
            Some(url),
            None,
            json!({"template": "raw {data.outcome}", "raw_body": true}),
        ),
        &mut post,
        &ev,
    );

    for (label, template) in [
        ("simple field", "{type}"),
        ("dotted path", "{data.outcome}"),
        ("missing renders empty", "[{nope}]"),
        ("deep missing chain", "[{data.nope.deeper}]"),
        ("positional renders empty", "[{0}]"),
        ("bad brace degrades raw", "{unbalanced"),
    ] {
        let rendered = fno_agents::status_fanout::render_template(Some(template), &ev);
        let record = json!({"rendered": rendered});
        assert_fanout_golden(
            &format!("template {label}"),
            &Golden {
                exit: Some(0),
                streams: vec![python_json_default(&record)],
            },
        );
    }

    // The eof cursor on the fixture journal and on an absent file. The
    // golden froze {"eof": [...], "eof_empty": ["", 0]}.
    let journal = fno_agents::paths::events_path(&sb.root);
    std::fs::create_dir_all(journal.parent().unwrap()).unwrap();
    let four = [
        jline("2026-01-01T00:00:01Z", "task_started", json!({"run": "r1"})),
        jline(
            "2026-01-01T00:00:02Z",
            "task_done",
            json!({"run": "r1", "node": "ab-12", "outcome": "ok"}),
        ),
        jline(
            "2026-01-01T00:00:02Z",
            "blocked",
            json!({"run": "r1", "node": "ab-12", "reason": "ci red"}),
        ),
        jline(
            "2026-01-01T00:00:05Z",
            "task_done",
            json!({"run": "r1", "node": "ab-12", "outcome": "fail"}),
        ),
    ];
    let mut text = String::new();
    for row in &four {
        text.push_str(&row.to_string());
        text.push('\n');
    }
    std::fs::write(&journal, text).unwrap();
    let eof = fno_agents::status_fanout::eof_cursor(&journal);
    let eof_empty = fno_agents::status_fanout::eof_cursor(&sb.root.join("none.jsonl"));
    let record = json!({"eof": eof, "eof_empty": eof_empty});
    assert_fanout_golden(
        "eof cursor values",
        &Golden {
            exit: Some(0),
            streams: vec![python_json_default(&record)],
        },
    );

    // The dry-run CLI case: spawn the real binary the way the doctor door
    // does (cwd inside the sandbox project, no sinks configured). The owner
    // stamp is the triad guard's contract: a spawned bin must know the test
    // process that owns it.
    let bin = env!("CARGO_BIN_EXE_fno-agents");
    let out = std::process::Command::new(bin)
        .args(["status", "--fanout", "tick", "--dry-run"])
        .current_dir(&sb.root)
        .env("FNO_CONFIG", sb.root.join("no-config.toml"))
        .envs(fno_agents::test_run::self_owner_env())
        .output()
        .unwrap();
    assert_fanout_golden(
        "cli doctor event fanout tick dry run",
        &Golden {
            exit: out.status.code(),
            streams: vec![
                String::from_utf8_lossy(&out.stdout).to_string(),
                String::from_utf8_lossy(&out.stderr).to_string(),
            ],
        },
    );
}
