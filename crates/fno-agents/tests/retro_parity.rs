//! Characterization for the retro family port (protocol steps 2-4,
//! docs/architecture/dual-implementation-inventory.md). The Rust leg is
//! `fno_agents::retro`; the Python leg it freezes against was the
//! `cli/src/fno/retro/` package, deleted in the same change. Every pure
//! decision the family makes (dedup keys, classification, the harvest
//! parsers, the postmortem frontmatter, the sweep plan and report, the
//! gate-escape summary, the reconcile targets) replays one frozen case from
//! `tests/golden/retro/pure.json`, captured from the Python leg.

//! parity-stage: characterization
//! parity-oracle: fno.retro

use std::collections::HashSet;

use fno_agents::retro::{self, classify, dedup, gate_escape, harvest, keep_going, postmortem};
use fno_agents::retro::{reconcile_findings, sweep, Candidate, RawItem};
use serde_json::{json, Value};

fn cases() -> Vec<Value> {
    let path = format!(
        "{}/tests/golden/retro/pure.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = std::fs::read_to_string(&path).expect("read retro goldens");
    let doc: Value = serde_json::from_str(&text).expect("parse retro goldens");
    doc["cases"].as_array().expect("cases").clone()
}

fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or("")
}

fn opt(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_string)
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn set(v: &Value) -> HashSet<String> {
    strings(v).into_iter().collect()
}

fn sorted(it: impl IntoIterator<Item = String>) -> Value {
    let mut v: Vec<String> = it.into_iter().collect();
    v.sort();
    json!(v)
}

fn raw(v: &Value) -> RawItem {
    serde_json::from_value(v.clone()).expect("raw item")
}

fn threads(v: &Value) -> Vec<harvest::Thread> {
    v.as_array()
        .map(|a| a.iter().map(harvest::Thread::from_value).collect())
        .unwrap_or_default()
}

fn nodes(v: &Value) -> Vec<Value> {
    v.as_array().cloned().unwrap_or_default()
}

fn render_with(input: &Value, applied: bool, mutate: bool) -> Vec<String> {
    let mut items = sweep::plan_sweep(&nodes(&input["carveouts"]), &nodes(&input["nodes"]));
    if mutate {
        for item in items.iter_mut() {
            if item.disposition == sweep::DISPOSITION_FILE {
                item.node_id = opt(input, "file_node_id");
            }
        }
        items[0].error = opt(input, "first_error");
    }
    sweep::render_sweep(&sweep::SweepReport {
        items,
        applied,
        consumed: input["consumed"].as_u64().unwrap_or(0) as usize,
        ..Default::default()
    })
}

/// The Rust leg's answer for one case, in the golden's projection.
fn answer(f: &str, input: &Value) -> Value {
    match f {
        "normalize_finding" => json!(dedup::normalize_finding(s(input, "text"))),
        "content_hash" => json!(dedup::content_hash(s(input, "text"))),
        "trailer" => json!(dedup::trailer(
            input["source_pr"].as_i64(),
            s(input, "hash")
        )),
        "cv_cite_needle" => json!(dedup::cv_cite_needle(s(input, "cv"))),
        "existing_keys_from_nodes" => {
            sorted(dedup::existing_keys_from_nodes(&nodes(&input["nodes"])))
        }
        "cv_ids_cited_in_nodes" => json!(dedup::cv_ids_cited_in_nodes(
            &nodes(&input["nodes"]),
            &strings(&input["cv_ids"])
        )),
        "extract_severity" => json!(harvest::extract_severity(s(input, "body"))),
        "normalize_severity" => json!(harvest::normalize_severity(s(input, "body"))),
        "derive_title" => json!(classify::derive_title(&raw(input))),
        "build_body" => json!(classify::build_body(&raw(input), classify::BODY_CAP)),
        "classify_item" => json!(classify::classify_item(&raw(input), classify::BODY_CAP)),
        "severity_to_priority" => json!(classify::severity_to_priority(input["severity"].as_str())),
        "severity_to_difficulty" => {
            json!(classify::severity_to_difficulty(input["severity"].as_str()))
        }
        "severity_to_tier" => json!(classify::severity_to_tier(input["severity"].as_str())),
        "classify_postmortem" => {
            let (disposition, candidate) =
                classify::classify_postmortem(&raw(input), classify::BODY_CAP);
            json!({"disposition": disposition, "candidate": candidate})
        }
        "addressed_ids_from_comments" => sorted(harvest::addressed_ids_from_comments(
            &nodes(&input["comments"]),
            &strings(&input["commit_dates"]),
        )),
        "addressed_ids_from_threads" => sorted(harvest::addressed_ids_from_threads(&threads(
            &input["threads"],
        ))),
        "resolved_ids_from_threads" => sorted(harvest::resolved_ids_from_threads(&threads(
            &input["threads"],
        ))),
        "parse_skipped_table" => json!(harvest::parse_skipped_table(s(input, "text"))),
        "skipped_ids_from_rows" => {
            let rows: Vec<harvest::SkippedRow> =
                serde_json::from_value(input["rows"].clone()).expect("rows");
            sorted(harvest::skipped_ids_from_rows(
                &rows,
                &threads(&input["threads"]),
            ))
        }
        "harvest_reviews" => {
            let mut warnings = Vec::new();
            let dates = input["commit_dates"]
                .is_array()
                .then(|| strings(&input["commit_dates"]));
            let items = harvest::harvest_reviews(
                &nodes(&input["comments"]),
                &set(&input["resolved_ids"]),
                &set(&input["skipped_ids"]),
                dates.as_deref(),
                input["author_login"].as_str(),
                input["source_pr"].as_i64(),
                &mut warnings,
            );
            json!({"items": items, "warnings": warnings})
        }
        "harvest_deferred_findings" => json!(harvest::harvest_deferred_findings_text(
            s(input, "text"),
            input["source_pr"].as_i64()
        )),
        "harvest_carveouts" => {
            let mut warnings = Vec::new();
            let sessions = input["session_ids"]
                .is_array()
                .then(|| strings(&input["session_ids"]));
            let items = harvest::harvest_carveouts_text(
                s(input, "ledger"),
                sessions.as_deref(),
                input["source_pr"].as_i64(),
                &mut warnings,
            );
            json!({"items": items, "warnings": warnings})
        }
        "split_frontmatter" => match postmortem::split_frontmatter(s(input, "text")) {
            Err(_) => json!({"ok": false}),
            Ok((fm, body)) => {
                let map = fm.clone().unwrap_or_default();
                let blocked = match map.get("blocked_reason") {
                    Some(Value::Object(b)) => b
                        .get("kind")
                        .filter(|k| !k.is_null() && *k != "")
                        .map(|k| k.as_str().map_or_else(|| k.to_string(), str::to_string))
                        .unwrap_or_default(),
                    _ => String::new(),
                };
                let consumed = map
                    .get("consumed_at")
                    .is_some_and(|v| !v.is_null() && *v != "" && *v != false);
                let invocation = map
                    .get("target_invocation")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                json!({
                    "ok": true,
                    "has_fm": fm.is_some(),
                    "consumed": consumed,
                    "blocked_kind": blocked,
                    "target_invocation": invocation,
                    "body": body,
                })
            }
        },
        "harvest_postmortems" => {
            let dir = tempfile::tempdir().expect("tempdir");
            for (name, text) in input["files"].as_object().expect("files") {
                std::fs::write(dir.path().join(name), text.as_str().unwrap_or("")).unwrap();
            }
            std::fs::write(dir.path().join("skip.txt"), "ignored").unwrap();
            let mut warnings = Vec::new();
            let items = postmortem::harvest_postmortems(dir.path(), &mut warnings);
            let files: Vec<String> = warnings
                .iter()
                .map(|w| w.split(':').next().unwrap_or("").to_string())
                .collect();
            json!({"items": items, "warning_files": files})
        }
        "stamp_postmortem_consumed" => {
            let text = s(input, "text");
            let stamped = postmortem::stamped_text(text, s(input, "ts")).expect("stamp");
            json!(stamped.unwrap_or_else(|| text.to_string()))
        }
        "find_tracking_node" => {
            let (node_id, reason, exact, link_to) =
                sweep::find_tracking_node(&input["rec"], &nodes(&input["nodes"]));
            json!({"node_id": node_id, "reason": reason, "exact": exact, "link_to": link_to})
        }
        "plan_sweep" => json!(sweep::plan_sweep(
            &nodes(&input["carveouts"]),
            &nodes(&input["nodes"])
        )
        .iter()
        .map(|item| {
            let mut v = sweep::item_json(item);
            v["candidate"] = json!(item.candidate);
            v
        })
        .collect::<Vec<_>>()),
        "render_sweep" => json!(render_with(input, false, false)),
        "render_sweep_applied" => json!(render_with(input, true, true)),
        "render_sweep_read_failed" => json!(sweep::render_sweep(&sweep::SweepReport {
            read_failed: true,
            ..Default::default()
        })),
        "sequence_ratio" => json!(retro::difflib::ratio(s(input, "a"), s(input, "b"))),
        "classify_followup" => {
            let candidate = Candidate {
                title: "t".into(),
                body: "b".into(),
                tier: retro::TIER_NODE.into(),
                priority: "p2".into(),
                source_id: "s".into(),
                extra: input["extra"].as_object().cloned().unwrap_or_default(),
                ..Default::default()
            };
            json!(keep_going::classify_followup(&candidate))
        }
        "gate_escape_summary" => {
            let text = s(input, "events");
            let lines: Vec<&str> = text.lines().collect();
            let summary = gate_escape::summarize_lines(&lines, s(input, "failures"));
            json!({"summary": summary, "render": gate_escape::render(&summary)})
        }
        "retro_targets" => json!(reconcile_findings::retro_targets(
            &nodes(&input["nodes"]),
            input["include_planned"].as_bool().unwrap_or(false)
        )),
        other => panic!("no Rust answer for golden fn {other}"),
    }
}

#[test]
fn every_frozen_retro_case_replays_on_the_rust_leg() {
    let cases = cases();
    assert!(cases.len() >= 170, "golden set shrank: {}", cases.len());
    let mut failures = Vec::new();
    for (i, case) in cases.iter().enumerate() {
        let f = s(case, "fn");
        let got = answer(f, &case["input"]);
        if got != case["output"] {
            failures.push(format!(
                "case {i} {f}\n  input:    {}\n  expected: {}\n  got:      {}",
                case["input"], case["output"], got
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} retro cases diverge:\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
}
