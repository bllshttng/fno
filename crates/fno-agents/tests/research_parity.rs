//! Characterization for the research family port (protocol steps 2-4,
//! docs/architecture/dual-implementation-inventory.md). The Rust leg is
//! `fno_agents::research`; the Python leg it freezes against was the
//! `cli/src/fno/research/` package. Every pure decision the family makes
//! (the query floor, the slug, the ddgs parse, the SSRF verdict, the
//! HTML-to-text extraction, the content-type and charset reads, the source
//! row, the store reader, the brief, the output_dir read) replays one frozen
//! case from `tests/golden/research/pure.json`, captured from the Python leg
//! by `tests/fixtures/research/capture_driver.py`.

//! parity-stage: characterization
//! parity-oracle: fno.research

use std::net::IpAddr;

use fno_agents::research::deliverable::{self, DeliverError, OUTPUT_DIR_UNSET};
use fno_agents::research::engine::{self, FetchResult, Source};
use fno_agents::research::pyfmt;
use serde_json::{json, Value};

fn cases() -> Vec<Value> {
    let path = format!(
        "{}/tests/golden/research/pure.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = std::fs::read_to_string(&path).expect("read research goldens");
    let doc: Value = serde_json::from_str(&text).expect("parse research goldens");
    doc["cases"].as_array().expect("cases").clone()
}

fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or("")
}

fn hex_bytes(text: &str) -> Vec<u8> {
    (0..text.len() / 2)
        .map(|i| u8::from_str_radix(&text[2 * i..2 * i + 2], 16).expect("hex byte"))
        .collect()
}

fn verdict(r: Result<(), String>) -> Value {
    match r {
        Ok(()) => json!("ok"),
        Err(reason) => json!(reason),
    }
}

fn source(row: &Value) -> Source {
    let field = |k: &str| row.get(k).cloned().unwrap_or(json!(""));
    Source {
        url: field("url"),
        fetched_at: field("fetched_at"),
        hash: field("hash"),
        extract: field("extract"),
        verified: row["verified"].as_bool().unwrap_or(false),
        reason: field("reason"),
    }
}

/// The Rust leg's answer for one case, in the golden's projection.
fn answer(f: &str, input: &Value) -> Value {
    match f {
        "normalize_query" => match engine::normalize_query(s(input, "topic")) {
            Ok(q) => json!({"ok": q}),
            Err(e) => json!({"err": e}),
        },
        "slugify" => json!(engine::slugify(s(input, "topic"))),
        "parse_ddgs" => json!(engine::parse_ddgs(s(input, "raw"))),
        "strip_html" => json!(engine::strip_html(s(input, "body"))),
        "text_content_type" => json!(engine::text_content_type(s(input, "ct"))),
        "content_charset" => json!(engine::content_charset(s(input, "ct"))),
        "decode_body" => json!(engine::decode_body(
            &hex_bytes(s(input, "hex")),
            s(input, "charset")
        )),
        "content_hash" => json!(engine::content_hash(s(input, "text"))),
        "isoformat" => json!(pyfmt::isoformat_epoch_us(
            input["epoch_us"].as_i64().unwrap_or(0)
        )),
        "build_source" => {
            let fetch = FetchResult {
                ok: input["ok"].as_bool().unwrap_or(false),
                text: s(input, "text").to_string(),
                content_type: String::new(),
                status: None,
                reason: s(input, "reason").to_string(),
            };
            let at = pyfmt::isoformat_epoch_us(input["epoch_us"].as_i64().unwrap_or(0));
            json!(engine::build_source(s(input, "url"), &fetch, &at).to_json_line())
        }
        "read_sources" => {
            let rows: Vec<Value> = engine::parse_sources(s(input, "text"))
                .iter()
                .map(|r| json!({"line": r.to_json_line(), "verified": r.verified}))
                .collect();
            json!(rows)
        }
        "guard_resolved" => {
            let ips: Vec<IpAddr> = input["ips"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(|ip| ip.parse::<IpAddr>().expect("golden ip"))
                        .collect()
                })
                .unwrap_or_default();
            verdict(engine::check_resolved(s(input, "host"), &ips))
        }
        "guard_url" => verdict(engine::guard_url(s(input, "url"))),
        "build_brief" => {
            let sources: Vec<Source> = input["sources"]
                .as_array()
                .map(|a| a.iter().map(source).collect())
                .unwrap_or_default();
            let created = pyfmt::isoformat_epoch_us(input["epoch_us"].as_i64().unwrap_or(0));
            json!(deliverable::build_brief(
                s(input, "topic"),
                s(input, "slug"),
                &sources,
                s(input, "stopped"),
                &created
            ))
        }
        "snippet" => json!(deliverable::snippet(s(input, "text"))),
        "output_dir_unset" => match deliverable::resolve_output_dir(input["configured"].as_str()) {
            Err(DeliverError::OutputDirUnset) => json!({"err": OUTPUT_DIR_UNSET}),
            Err(DeliverError::Io(e)) => json!({"io": e}),
            Ok(_) => json!({"ok": true}),
        },
        "output_dir_from_yaml" => json!(deliverable::output_dir_from_yaml(s(input, "text"))),
        "ddgs_timeout_message" => json!(engine::ddgs_timeout_message(
            s(input, "query"),
            input["max_results"].as_i64().unwrap_or(0)
        )),
        other => panic!("no Rust answer for golden fn {other}"),
    }
}

#[test]
fn every_frozen_research_case_replays_on_the_rust_leg() {
    let cases = cases();
    assert!(cases.len() >= 385, "golden set shrank: {}", cases.len());
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
        "{} of {} research cases diverge:\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
}
