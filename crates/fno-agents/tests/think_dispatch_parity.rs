//! Characterization for the `fno do think dispatch` port (protocol steps 2-4,
//! docs/architecture/dual-implementation-inventory.md). The Rust leg is
//! `fno_agents::think_dispatch`; the Python leg it freezes against is
//! `fno.provenance.spawn_think` (the conversational dispatch tail) and the
//! `fno.provenance.cli` command glue. Every deterministic part (the seed
//! prompt, the output-path reuse ladder, the worker name, the receipt parse,
//! the dedup token, the decision matrix over patched seams, and the command's
//! flag, pointer and node-resolution glue) replays one frozen case from
//! `tests/golden/think_dispatch/pure.json`, captured by
//! `tests/fixtures/think_dispatch/capture_driver.py`.

//! parity-stage: characterization
//! parity-oracle: fno.provenance.spawn_think

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};

use fno_agents::think_dispatch::{
    self as td, ClaimTry, CliEnv, DispatchResult, Preflight, Provenance, SeedTranscript,
    SpawnError, SpawnRequest, TailSeams,
};
use serde_json::{json, Map, Value};

fn cases() -> Vec<Value> {
    let path = format!(
        "{}/tests/golden/think_dispatch/pure.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = std::fs::read_to_string(&path).expect("read think-dispatch goldens");
    let doc: Value = serde_json::from_str(&text).expect("parse think-dispatch goldens");
    doc["cases"].as_array().expect("cases").clone()
}

fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or("")
}

fn opt<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

fn transcript(v: &Value) -> SeedTranscript {
    SeedTranscript {
        path: opt(v, "path").map(str::to_string),
        resolved: v["resolved"].as_bool().unwrap_or(false),
        reason: opt(v, "reason").map(str::to_string),
        ambiguous: v["ambiguous"].as_bool().unwrap_or(false),
    }
}

fn leak(v: Option<&str>) -> Option<&'static str> {
    v.map(|s| -> &'static str { Box::leak(s.to_string().into_boxed_str()) })
}

fn result_of(v: &Value) -> DispatchResult {
    let text = |key: &str| opt(v, key).map(str::to_string);
    DispatchResult {
        decision: leak(opt(v, "decision")).unwrap_or(""),
        event: leak(opt(v, "event")),
        reason: text("reason"),
        node_id: text("node_id"),
        presence: text("presence"),
        resolved: v.get("resolved").and_then(Value::as_bool),
        think_session: text("think_session"),
        detail: text("detail"),
    }
}

/// A fresh per-case scratch dir under the system temp root.
fn scratch(i: usize) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "fno-think-dispatch-parity-{}-{i}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("plans")).expect("scratch plans dir");
    dir
}

fn rel(path: &str, pdir: &Path) -> String {
    let prefix = format!("{}/", pdir.display());
    match path.strip_prefix(prefix.as_str()) {
        Some(rest) => format!("<pdir>/{rest}"),
        None => path.to_string(),
    }
}

fn output_case(i: usize, input: &Value) -> Value {
    let dir = scratch(i);
    let pdir = dir.join("plans");
    for f in input["files"].as_array().cloned().unwrap_or_default() {
        std::fs::write(pdir.join(s(&f, "name")), s(&f, "text")).expect("write plans file");
    }
    let node_id = s(input, "node_id");
    let found = td::find_node_doc(&pdir, node_id).map(|p| rel(&p.to_string_lossy(), &pdir));
    let out = td::output_path_in(&pdir, node_id, s(input, "slug"), "{date}");
    let _ = std::fs::remove_dir_all(&dir);
    json!({"find_node_doc": found, "think_output_path": rel(&out, &pdir)})
}

fn tail_case(input: &Value) -> Value {
    let t = transcript(&input["transcript"]);
    let out_path = input
        .get("output_path")
        .and_then(Value::as_str)
        .unwrap_or("/plans/out.md")
        .to_string();
    let acquire_mode = opt(input, "acquire").unwrap_or("ok").to_string();
    let spawn_mode = s(input, "spawn").to_string();
    let probed = RefCell::new(Vec::<String>::new());
    let acquired = RefCell::new(Vec::<String>::new());
    let released = RefCell::new(Vec::<String>::new());
    let spawned = RefCell::new(Value::Null);
    let bumps = Cell::new(0_i64);
    let stamps = RefCell::new(Vec::<Value>::new());
    let seen_transcript = RefCell::new(Value::Null);
    let seen_output = RefCell::new(Value::Null);
    let events = RefCell::new(Vec::<Value>::new());
    let notes = RefCell::new(Vec::<String>::new());

    let presence = || opt(input, "presence").unwrap_or("away").to_string();
    let transcript_seam = |h: &str, sid: &str, cwd: &str| {
        *seen_transcript.borrow_mut() = json!([h, sid, cwd]);
        t.clone()
    };
    let output_seam = |id: &str, slug: &str| {
        *seen_output.borrow_mut() = json!([id, slug]);
        out_path.clone()
    };
    let day_count = || input["day_count"].as_i64().unwrap_or(0);
    let claim_live = |key: &str| {
        probed.borrow_mut().push(key.to_string());
        input["claim_live"].as_bool().unwrap_or(false)
    };
    let acquire = |key: &str, _holder: &str, _reason: &str| {
        acquired.borrow_mut().push(key.to_string());
        match acquire_mode.as_str() {
            "held" => ClaimTry::Held,
            m => match m.strip_prefix("error:") {
                Some(e) => ClaimTry::Error(e.to_string()),
                None => ClaimTry::Acquired,
            },
        }
    };
    let release = |key: &str, _holder: &str| released.borrow_mut().push(key.to_string());
    let spawn = |req: &SpawnRequest| {
        *spawned.borrow_mut() = json!({
            "node_id": req.node_id,
            "prompt": req.prompt,
            "node_cwd": req.node_cwd,
            "node_slug": req.node_slug,
            "reason": td::REASON,
            "suffix": req.suffix,
            "model": req.model,
            "provider": req.provider,
        });
        if spawn_mode == "exists" {
            return Err(SpawnError::AlreadyRunning);
        }
        if let Some(e) = spawn_mode.strip_prefix("error:") {
            return Err(SpawnError::Failed(e.to_string()));
        }
        Ok(spawn_mode.strip_prefix("ok:").unwrap_or("").to_string())
    };
    let bump = || bumps.set(bumps.get() + 1);
    let stamp = |id: &str, session: &str, output: &str| {
        let output = Some(output).filter(|o| !o.is_empty());
        stamps.borrow_mut().push(json!([id, session, output]));
    };
    let emit = |kind: &str, data: Map<String, Value>| events.borrow_mut().push(json!([kind, data]));
    let note = |line: &str| notes.borrow_mut().push(line.to_string());
    let seams = TailSeams {
        armed: input["autonomy"].as_bool().unwrap_or(true),
        rank: "env",
        max_per_run: input["cap"].as_u64().unwrap_or(5) as usize,
        day_cap: input["day_cap"].as_i64().unwrap_or(20),
        presence: &presence,
        transcript: &transcript_seam,
        output_path: &output_seam,
        day_count: &day_count,
        claim_live: &claim_live,
        acquire: &acquire,
        release: &release,
        spawn: &spawn,
        bump: &bump,
        stamp: &stamp,
        emit: &emit,
        note: &note,
    };
    let result = td::conversational_tail(
        &input["node"],
        s(input, "session_id"),
        s(input, "cwd"),
        s(input, "harness"),
        &seams,
    );
    json!({
        "result": result.to_json(),
        "events": events.into_inner(),
        "calls": {
            "probed": probed.into_inner(),
            "acquired": acquired.into_inner(),
            "released": released.into_inner(),
            "spawn": spawned.into_inner(),
            "bumps": bumps.get(),
            "stamps": stamps.into_inner(),
            "transcript": seen_transcript.into_inner(),
            "output_path": seen_output.into_inner(),
        },
        "notes": notes.into_inner(),
    })
}

fn cli_case(input: &Value) -> Value {
    let argv: Vec<String> = input["argv"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let prov = &input["provenance"];
    let stdout = RefCell::new(String::new());
    let stderr = RefCell::new(String::new());
    let reached = RefCell::new(Value::Null);
    let entries = || -> Result<Vec<Value>, String> {
        Ok(input["entries"].as_array().cloned().unwrap_or_default())
    };
    let project_root = |_: &str| -> Option<String> { None };
    let out = |line: &str| stdout.borrow_mut().push_str(&format!("{line}\n"));
    let err = |line: &str| stderr.borrow_mut().push_str(&format!("{line}\n"));
    let env = CliEnv {
        provenance: Provenance {
            session_id: opt(prov, "source_session_id").map(str::to_string),
            harness: opt(prov, "source_harness").map(str::to_string),
            cwd: opt(prov, "source_cwd").map(str::to_string),
        },
        process_cwd: s(input, "process_cwd").to_string(),
        external_backend: false,
        entries: &entries,
        project_root: &project_root,
        out: &out,
        err: &err,
    };
    let dispatch = |node: &Value, pre: &Preflight| {
        *reached.borrow_mut() = json!({
            "node_id": node.get("id"),
            "model": node.get("model"),
            "provider": node.get("provider"),
            "session_id": pre.session_id,
            "cwd": pre.cwd,
            "harness": pre.harness,
        });
        result_of(&input["result"])
    };
    let (exit, _) = td::run_with(&argv, &env, &dispatch);
    json!({
        "exit": exit,
        "stdout": stdout.into_inner(),
        "stderr": stderr.into_inner(),
        "reached": reached.into_inner(),
    })
}

/// The Rust leg's answer for one case, in the golden's projection.
fn answer(i: usize, f: &str, input: &Value) -> Value {
    match f {
        "assemble_seed" => {
            let seed = td::assemble_seed(
                &input["node"],
                &transcript(&input["transcript"]),
                s(input, "output_path"),
            );
            json!({
                "prompt": seed.prompt,
                "offer_line": seed.offer_line,
                "resolved": seed.resolved,
                "output_path": seed.output_path,
            })
        }
        "output_path" => output_case(i, input),
        "worker_agent_name" => match td::worker_agent_name(
            s(input, "node_id"),
            opt(input, "slug"),
            opt(input, "suffix"),
            opt(input, "model"),
        ) {
            Ok(name) => json!(name),
            Err(_) => json!({"error": true}),
        },
        "parse_short_id" => json!(td::parse_short_id(s(input, "stdout"))),
        "dedup_key" => json!(td::dedup_key(
            s(input, "node_id"),
            td::invocation_suffix(s(input, "session_id")).as_deref()
        )),
        "tail" => tail_case(input),
        "cli" => cli_case(input),
        other => panic!("no Rust answer for golden fn {other}"),
    }
}

#[test]
fn every_frozen_think_dispatch_case_replays_on_the_rust_leg() {
    let cases = cases();
    assert!(cases.len() >= 90, "golden set shrank: {}", cases.len());
    let mut failures = Vec::new();
    for (i, case) in cases.iter().enumerate() {
        let f = s(case, "fn");
        let got = answer(i, f, &case["input"]);
        if got != case["output"] {
            failures.push(format!(
                "case {i} {f}\n  input:    {}\n  expected: {}\n  got:      {}",
                case["input"], case["output"], got
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} think-dispatch cases diverge:\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
}
