//! The native scoreboard door: `fno-agents scoreboard [flags]`, the
//! binary-direct transport `fno whoami scoreboard` execs through the front
//! door. Never registered in ALL_CLIENT_ACTIONS (the action list is
//! shrink-only), never routed by `fno agents`: the `==` dispatch sits in
//! client.rs beside `stalls` and `evals-macro`.

use super::args::{parse, ArgError};
use serde_json::{json, Value};

/// Exit 2 message rendering, the Python leg's typer refusal exit.
fn refuse(message: &str) -> i32 {
    eprintln!("{message}");
    2
}

pub fn run_scoreboard(args: &[String]) -> i32 {
    let parsed = match parse(args) {
        Ok(p) => p,
        Err(ArgError::SinceBelowOne) => return refuse("--since must be at least 1 (days)."),
        Err(ArgError::MutuallyExclusive(views)) => {
            return refuse(
                &format!(
                    "{} are mutually exclusive views; pick one.",
                    views.join(" and ")
                )
                .to_string(),
            );
        }
        Err(ArgError::Unknown(arg)) => {
            return refuse(&format!("fno-agents scoreboard: unknown argument {arg}"));
        }
        Err(ArgError::MissingValue(flag)) => {
            return refuse(&format!("fno-agents scoreboard: {flag} needs a value"));
        }
    };
    if parsed.help {
        println!("{}", super::args::USAGE);
        return 0;
    }
    let project = parsed.project.clone();
    let mut inputs = match super::views::load_run(project.as_deref()) {
        Ok(inputs) => inputs,
        Err(message) => {
            if let Some(rest) = message.strip_prefix("BROKEN_LEDGER\u{1}") {
                eprintln!("{rest}");
                return 1;
            }
            eprintln!("fno-agents: {message}");
            return 1;
        }
    };

    // --project scope: load_run already classified WITH the project, so the
    // scoped rows and node ids ride the classification it computed.
    let mut scope: Option<Value> = None;
    let mut pnodes: std::collections::HashSet<String> = Default::default();
    let project_mode = project.is_some();
    if project_mode {
        let scoped = match inputs.classified.get("scoped") {
            Some(s) => s,
            None => {
                eprintln!("fno-agents: classifier returned no scope");
                return 1;
            }
        };
        if let Some(ids) = scoped.get("node_ids").and_then(Value::as_array) {
            pnodes.extend(ids.iter().filter_map(|v| v.as_str().map(String::from)));
        }
        let scoped_rows = scoped
            .get("rows")
            .and_then(Value::as_array)
            .filter(|r| !r.is_empty());
        if let Some(rows) = scoped_rows {
            inputs.rows = rows.clone();
        }
        // The folds saw the scoped entries too (out-of-project nodes never
        // joined an outcome on the Python leg).
        if let Some(entries) = scoped.get("entries").and_then(Value::as_array) {
            inputs.graph_nodes = entries.clone();
        }
        scope = inputs
            .classified
            .pointer("/coverage/project_scope")
            .cloned();
    }

    // The one journal read per kind-set, project-filtered exactly like the
    // Python leg's `_events` (coverage stays whole; only events filter).
    let in_scope = |e: &Value| {
        !project_mode || {
            let raw = e.get("data");
            let data = raw.filter(|d| d.is_object());
            let nid = e
                .get("graph_node_id")
                .or_else(|| data.and_then(|d| d.get("graph_node_id")))
                .and_then(Value::as_str);
            nid.map(|n| pnodes.contains(n)).unwrap_or(false)
        }
    };
    let read = |kinds: &[&str]| {
        let (events, coverage) =
            super::readers::read_events_with_coverage(&inputs.events_paths, kinds);
        let events: Vec<Value> = events.into_iter().filter(|e| in_scope(e)).collect();
        (events, coverage)
    };

    // The per-view finisher: scope rides every view, then JSON or render.
    enum R {
        Main,
        Cal,
        Skill,
        Eff,
        Lanes,
        Pf,
    }
    let finish = |view: &mut Value, which: R| -> i32 {
        if let Some(scope) = &scope {
            view["project_scope"] = scope.clone();
        }
        if parsed.json_out {
            println!("{}", serde_json::to_string_pretty(view).unwrap_or_default());
        } else {
            match which {
                R::Main => super::render::render(view),
                R::Cal => super::render::render_calibration(view),
                R::Skill => super::render::render_by_skill(view),
                R::Eff => super::render::render_efficiency(view),
                R::Lanes => super::render::render_lanes(view),
                R::Pf => super::render::render_plan_fidelity(view),
            }
        }
        0
    };

    // The view dispatch, the Python leg's order: each flag answers exactly
    // one fold; the bare scoreboard is the fallthrough.
    if parsed.calibration {
        let (events, coverage) = read(&["verifier_verdict"]);
        let mut cal = super::views::build_calibration(
            &events,
            &inputs,
            parsed.since_days,
            &super::views::node_maps(&inputs.graph_nodes),
        );
        cal["event_coverage"] = coverage;
        return finish(&mut cal, R::Cal);
    }
    if parsed.by_skill {
        let (events, coverage) = read(&["human_touch"]);
        let mut sb = super::views::build_skill_scoreboard(
            &inputs,
            &events,
            parsed.since_days,
            chrono::Local::now(),
        );
        sb["event_coverage"] = coverage;
        return finish(&mut sb, R::Skill);
    }
    if parsed.efficiency {
        let (events, coverage) = read(&["loop_check"]);
        let mut eff = super::views::build_efficiency(
            &inputs,
            &events,
            parsed.since_days,
            chrono::Local::now(),
        );
        eff["event_coverage"] = coverage;
        return finish(&mut eff, R::Eff);
    }
    if parsed.by_provider {
        // In-process here: the keeper op and the door share one fold, so the
        // Python leg's "keeper predates the op" downgrade path cannot fire.
        let params = json!({
            "entries": inputs.graph_nodes,
            "rows": inputs.rows,
            "since_days": parsed.since_days,
            "now": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
        });
        match crate::scoreboard_provider::view(&params) {
            Ok(reply) => {
                if parsed.json_out {
                    let mut view = reply.get("view").cloned().unwrap_or_default();
                    if let Some(scope) = &scope {
                        view["project_scope"] = scope.clone();
                    }
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&view).unwrap_or_default()
                    );
                } else {
                    let text = reply
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    print!("{text}");
                }
                return 0;
            }
            Err(e) => {
                eprintln!("fno-agents: by-provider: {e}");
                return 1;
            }
        }
    }
    if parsed.lanes {
        // Registry rows ride as {status, provider, model, effort}; provider
        // passes through so unattributed registry rows stay unattributed.
        let home = crate::paths::AgentsHome::from_env();
        let registry_rows: Vec<Value> = crate::state::load_registry(&home.registry_json())
            .map(|reg| {
                reg.entries
                    .iter()
                    .map(|e| {
                        json!({
                            "status": e.status,
                            "provider": e.provider,
                            "model": e.model,
                            "effort": e.effort,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let probe = crate::spawn_gate_command::probe_answer(&json!({
            "mode": "probe",
            "only": ["lanes"],
        }));
        let mut caps: std::collections::HashMap<String, Option<i64>> = Default::default();
        if let Some(lanes_obj) = probe.get("lanes").and_then(Value::as_object) {
            for (name, lane) in lanes_obj {
                let cap = lane.get("cap").and_then(Value::as_i64);
                caps.insert(name.clone(), cap);
            }
        }
        let (rate_events, coverage) = read(&["provider_rate_limited"]);
        let mut view = super::views::build_lanes(
            &inputs,
            &registry_rows,
            &rate_events,
            &caps,
            parsed.since_days,
            chrono::Local::now(),
        );
        view["event_coverage"] = coverage;
        return finish(&mut view, R::Lanes);
    }
    if parsed.plan_fidelity {
        // The gate shares this fold through the scoreboard_plan_fidelity
        // keeper op; the view reads its own trace-journal set here: the run
        // journals, the project log, each shipped row's root_path journal,
        // and every canonical salvage journal.
        let mut trace_paths = inputs.events_paths.clone();
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let root = crate::paths::worktree_repo_root(&cwd);
        // Python `_paths.project_log("events.jsonl")`: the per-repo SPACE
        // journal (`<spaces>/<slug-of-canonical-root>/events.jsonl`), not
        // the checkout's `.fno` file.
        trace_paths.push(crate::paths::space_dir(&root).join("events.jsonl"));
        let mut canonical_roots: Vec<std::path::PathBuf> = Vec::new();
        if let Some(c) = crate::paths::canonical_repo_root(&root) {
            canonical_roots.push(c);
        } else {
            canonical_roots.push(root.clone());
        }
        for row in &inputs.rows {
            if let Some(rp) = row.get("root_path").and_then(Value::as_str) {
                let p = std::path::PathBuf::from(rp);
                if p.exists() {
                    trace_paths.push(crate::outstanding::project_log(&p, "events.jsonl"));
                }
            }
            if let Some(cr) = row.get("canonical_root_path").and_then(Value::as_str) {
                canonical_roots.push(std::path::PathBuf::from(cr));
            }
        }
        canonical_roots.sort();
        canonical_roots.dedup();
        for r in &canonical_roots {
            let salvage = r.join(".fno").join("salvage");
            if salvage.is_dir() {
                let mut entries: Vec<std::path::PathBuf> = std::fs::read_dir(&salvage)
                    .map(|rd| {
                        rd.filter_map(|e| e.ok())
                            .map(|e| e.path())
                            .filter(|p| p.is_dir())
                            .filter_map(|d| {
                                let f = d.join("events.jsonl");
                                f.exists().then_some(f)
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                entries.sort();
                trace_paths.extend(entries);
            }
        }
        let (trace_events, coverage) = super::readers::read_events_with_coverage(
            &trace_paths,
            &super::context_trace::CONTEXT_TRACE_EVENT_KINDS,
        );
        let trace_events: Vec<Value> = trace_events.into_iter().filter(|e| in_scope(e)).collect();
        let loop_events: Vec<Value> = trace_events
            .iter()
            .filter(|e| e.get("type").and_then(Value::as_str) == Some("loop_check"))
            .cloned()
            .collect();
        let mut pf = match super::plan_join::build_plan_fidelity(
            &inputs,
            parsed.since_days,
            chrono::Local::now(),
            &loop_events,
            &trace_events,
            Some(&coverage),
            "unjoined",
        ) {
            Ok(view) => view,
            Err(e) => {
                eprintln!("{e}");
                return 1;
            }
        };
        pf["event_coverage"] = coverage;
        return finish(&mut pf, R::Pf);
    }

    // The main scoreboard: human_touch journals + the emission-failure
    // counter, then the fold. `now` is local: the ledger's `completed` is
    // written naive-local, so one timeline, no local/UTC boundary skew.
    let (touch, coverage) = read(&["human_touch"]);
    let mut sb =
        super::views::build_scoreboard(&inputs, &touch, parsed.since_days, chrono::Local::now());
    sb["event_coverage"] = coverage;
    sb["emission_failures"] = emission_failures_snapshot();
    return finish(&mut sb, R::Main);
}

/// The mux server's human_touch emission-failure counter, handed over by the
/// front door as `FNO_MUX_STATS_JSON` (one control roundtrip over the
/// server socket there, never a nested `fno` spawn here: the seam budget
/// bars a second crossing site). Unreachable means Unknown, not zero.
fn emission_failures_snapshot() -> Value {
    let raw = std::env::var("FNO_MUX_STATS_JSON").unwrap_or_default();
    if raw.is_empty() {
        return json!({"available": false, "reason": "mux stats unavailable"});
    }
    let v: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    json!({
        "available": true,
        "count": v.get("touch_emit_failures"),
        "measured_since": v.get("started_at"),
        "measured_at": v.get("measured_at"),
    })
}
