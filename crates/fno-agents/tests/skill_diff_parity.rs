//! parity-stage: characterization
//! parity-oracle: cli/src/fno/skill_diff/engine.py
//!
//! The goldens under tests/golden/skill_diff/ were frozen from the Python
//! legs by tests/fixtures/skill_diff/capture_driver.py before the leg was
//! deleted in the same change (engine, guards, synthesize and the tick and
//! reconcile wiring all moved to crates/fno-agents/src/skill_diff.rs; the
//! driver is deleted with the leg, so a golden can only be re-frozen by
//! reverting the deletion, never from this binary). This test is the Rust
//! half of the protocol: it feeds every golden's frozen input through the
//! ported functions and asserts the frozen expected values byte for byte
//! where the contract is bytes.

use std::fs;

use fno_agents::skill_diff::{
    additive_only_needs_justification, bloat_flag, build_pr_body, build_prompt, failure_ranking,
    filter_cited_hunks, findings_for_run, has_actionable_findings, local_maxima_tripped,
    parse_proposal, redaction_violations, top_dimension, unprocessed_runs, Hunk,
};

const GOLDEN_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/skill_diff");

struct Case {
    slug: String,
    input: serde_json::Value,
    expected: serde_json::Value,
}

fn cases() -> Vec<Case> {
    let mut out: Vec<Case> = fs::read_dir(GOLDEN_DIR)
        .expect("golden dir present")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|x| x == "json").unwrap_or(false))
        .map(|e| e.path())
        .map(|path| {
            let raw = fs::read_to_string(&path).expect("golden readable");
            let v: serde_json::Value = serde_json::from_str(&raw).expect("golden parses");
            let slug = path.file_stem().unwrap().to_string_lossy().into_owned();
            Case {
                slug,
                input: v.get("input").cloned().unwrap_or_default(),
                expected: v.get("expected").cloned().unwrap_or_default(),
            }
        })
        .collect();
    out.sort_by(|a, b| a.slug.cmp(&b.slug));
    out
}

fn slug_starts(case: &Case, prefix: &str) -> bool {
    case.slug.starts_with(prefix)
}

fn hunk_from(v: &serde_json::Value) -> Hunk {
    let s = |k: &str| {
        v.get(k)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    Hunk {
        file: s("file"),
        old_text: s("old_text"),
        new_text: s("new_text"),
        cited_finding_ids: v
            .get("cited_finding_ids")
            .and_then(serde_json::Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        rationale: s("rationale"),
    }
}

fn hunks_from(v: &serde_json::Value) -> Vec<Hunk> {
    v.as_array()
        .map(|a| a.iter().map(hunk_from).collect())
        .unwrap_or_default()
}

fn strings(v: &serde_json::Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn events(v: &serde_json::Value) -> Vec<serde_json::Value> {
    v.as_array().cloned().unwrap_or_default()
}

#[test]
fn unprocessed_goldens() {
    for case in cases().iter().filter(|c| slug_starts(c, "unprocessed_")) {
        let evs = events(&case.input);
        if case.slug == "unprocessed_pair_key" {
            assert_eq!(
                serde_json::to_value(unprocessed_runs(&evs, "fno:blueprint")).unwrap(),
                case.expected["blueprint"],
                "case {}",
                case.slug
            );
            assert_eq!(
                serde_json::to_value(unprocessed_runs(&evs, "fno:review")).unwrap(),
                case.expected["review"],
                "case {}",
                case.slug
            );
        } else {
            assert_eq!(
                serde_json::to_value(unprocessed_runs(&evs, "fno:blueprint")).unwrap(),
                case.expected,
                "case {}",
                case.slug
            );
        }
    }
}

#[test]
fn findings_ranking_and_actionable_goldens() {
    for case in cases() {
        let evs = events(&case.input);
        if slug_starts(&case, "findings_") {
            let got = findings_for_run(&evs, "r1", None);
            assert_eq!(
                serde_json::to_value(got).unwrap(),
                case.expected,
                "case {}",
                case.slug
            );
        } else if slug_starts(&case, "ranking_from") {
            // The goldens freeze the Python dicts; the Rust pairs render as
            // objects, not two-element arrays.
            let got: Vec<serde_json::Value> = failure_ranking(&evs, "r1", Some("fno:blueprint"))
                .into_iter()
                .map(|(dim, n)| serde_json::json!({"dimension": dim, "fail_count": n}))
                .collect();
            assert_eq!(
                serde_json::to_value(got).unwrap(),
                case.expected,
                "case {}",
                case.slug
            );
        } else if slug_starts(&case, "ranking_fallback") {
            let got: Vec<serde_json::Value> = failure_ranking(&evs, "r1", Some("fno:blueprint"))
                .into_iter()
                .map(|(dim, n)| serde_json::json!({"dimension": dim, "fail_count": n}))
                .collect();
            assert_eq!(
                serde_json::to_value(got).unwrap(),
                case.expected,
                "case {}",
                case.slug
            );
        } else if slug_starts(&case, "has_actionable") {
            assert_eq!(
                serde_json::Value::Bool(has_actionable_findings(&evs, "r1", Some("fno:blueprint"))),
                case.expected["actionable"],
                "case {}",
                case.slug
            );
        } else if slug_starts(&case, "top_dimension") {
            assert_eq!(
                serde_json::to_value(top_dimension(&evs, "r1", Some("fno:blueprint"))).unwrap(),
                case.expected,
                "case {}",
                case.slug
            );
        }
    }
}

#[test]
fn local_maxima_goldens() {
    // The short-window case froze its verdict at run id r2; the dedicated
    // test below carries it. The generic loop runs only the r3 cases.
    for case in cases()
        .iter()
        .filter(|c| slug_starts(c, "local_maxima_") && c.slug != "local_maxima_short_window")
    {
        let evs = events(&case.input);
        assert_eq!(
            serde_json::Value::Bool(local_maxima_tripped(&evs, "fno:blueprint", "r3")),
            case.expected,
            "case {}",
            case.slug
        );
    }
}

#[test]
fn local_maxima_short_window_golden_uses_its_own_run() {
    for case in cases()
        .iter()
        .filter(|c| c.slug == "local_maxima_short_window")
    {
        let evs = events(&case.input);
        assert_eq!(
            serde_json::Value::Bool(local_maxima_tripped(&evs, "fno:blueprint", "r2")),
            case.expected,
            "case {}",
            case.slug
        );
    }
}

#[test]
fn build_pr_body_goldens() {
    for case in cases().iter().filter(|c| slug_starts(c, "build_pr_body_")) {
        let hunks = hunks_from(&case.input["hunks"]);
        let justification = case.input["justification"].as_str().map(str::to_string);
        let bloat = case.input["bloat"].as_object().map(|b| {
            (
                b["net_growth"].as_i64().unwrap_or(0),
                b["window"].as_u64().unwrap_or(0) as usize,
                b["threshold"].as_i64().unwrap_or(0),
            )
        });
        let body = build_pr_body(
            case.input["run_id"].as_str().unwrap(),
            case.input["skill_id"].as_str().unwrap(),
            &hunks,
            justification.as_deref(),
            bloat,
            case.input["version_observed"].as_str().unwrap(),
            case.input["version_against"].as_str().unwrap(),
            // The driver froze no flag; the review skill carries the note.
            case.input["skill_id"].as_str() == Some("fno:review"),
        );
        assert_eq!(body, case.expected.as_str().unwrap(), "case {}", case.slug);
    }
}

#[test]
fn guards_goldens() {
    for case in cases() {
        if slug_starts(&case, "filter_cited_hunks") {
            let hunks = hunks_from(&case.input["hunks"]);
            let (kept, dropped) = filter_cited_hunks(&hunks);
            assert_eq!(
                serde_json::to_value(kept).unwrap(),
                case.expected["kept"],
                "case {}",
                case.slug
            );
            let dropped_new: Vec<String> = dropped.iter().map(|h| h.new_text.clone()).collect();
            assert_eq!(
                serde_json::to_value(dropped_new).unwrap(),
                case.expected["dropped_new"],
                "case {}",
                case.slug
            );
        } else if slug_starts(&case, "additive_threshold") {
            let threshold = case.input["threshold"].as_i64().unwrap();
            let mk = |old: bool| -> Vec<Hunk> {
                vec![Hunk {
                    old_text: if old { "y".into() } else { String::new() },
                    new_text: "x\n".repeat((threshold + 1) as usize),
                    ..hunk_from(&serde_json::json!({}))
                }]
            };
            assert!(
                additive_only_needs_justification(&mk(false)),
                "case {}",
                case.slug
            );
            assert!(
                !additive_only_needs_justification(&mk(true)),
                "case {}",
                case.slug
            );
        } else if slug_starts(&case, "bloat_flag") {
            // The golden freezes the Python dict; the Rust triple renders as
            // an object, not a three-element array.
            let as_map = |key: &str| -> Vec<serde_json::Map<String, serde_json::Value>> {
                case.input[key]
                    .as_array()
                    .map(|a| a.iter().filter_map(|x| x.as_object().cloned()).collect())
                    .unwrap_or_default()
            };
            let flag_json = |flag: Option<(i64, usize, i64)>| {
                flag.map(|(net, window, threshold)| {
                    serde_json::json!({"net_growth": net, "window": window, "threshold": threshold})
                })
            };
            assert_eq!(
                serde_json::to_value(flag_json(bloat_flag(&as_map("over")))).unwrap(),
                case.expected["over"],
                "case {}",
                case.slug
            );
            assert_eq!(
                serde_json::to_value(flag_json(bloat_flag(&as_map("under")))).unwrap(),
                case.expected["under"],
                "case {}",
                case.slug
            );
        } else if slug_starts(&case, "redaction_") {
            let names = strings(&case.input["project_names"]);
            let text = case.input["text"].as_str().unwrap();
            assert_eq!(
                serde_json::to_value(redaction_violations(text, &names)).unwrap(),
                case.expected,
                "case {}",
                case.slug
            );
        }
    }
}

#[test]
fn parse_proposal_goldens() {
    for case in cases().iter().filter(|c| slug_starts(c, "parse_")) {
        let text = case.input["text"].as_str().unwrap();
        match parse_proposal(text) {
            Ok(p) => {
                assert!(case.expected.get("error").is_none(), "case {}", case.slug);
                if let Some(verdict) = case.expected.get("verdict").and_then(|v| v.as_str()) {
                    assert_eq!(p.verdict, verdict, "case {}", case.slug);
                }
                if let Some(files) = case.expected.get("files") {
                    let got: Vec<String> = p.hunks.iter().map(|h| h.file.clone()).collect();
                    assert_eq!(
                        serde_json::to_value(got).unwrap(),
                        *files,
                        "case {}",
                        case.slug
                    );
                }
                if let Some(n) = case.expected.get("hunks").and_then(|v| v.as_i64()) {
                    assert_eq!(p.hunks.len() as i64, n, "case {}", case.slug);
                }
                if let Some(j) = case.expected.get("justification") {
                    assert_eq!(
                        serde_json::to_value(p.justification).unwrap(),
                        *j,
                        "case {}",
                        case.slug
                    );
                }
                if let Some(r) = case.expected.get("no_diff_reason") {
                    assert_eq!(
                        serde_json::to_value(p.no_diff_reason).unwrap(),
                        *r,
                        "case {}",
                        case.slug
                    );
                }
            }
            Err(e) => {
                assert_eq!(
                    Some(e.as_str()),
                    case.expected.get("error").and_then(|v| v.as_str()),
                    "case {}",
                    case.slug
                );
            }
        }
    }
}

#[test]
fn build_prompt_goldens() {
    for case in cases().iter().filter(|c| slug_starts(c, "build_prompt_")) {
        let files: Vec<(String, String)> = case.input["skill_files"]
            .as_object()
            .map(|m| {
                m.iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let findings: Vec<serde_json::Map<String, serde_json::Value>> = case.input["findings"]
            .as_array()
            .map(|a| a.iter().filter_map(|x| x.as_object().cloned()).collect())
            .unwrap_or_default();
        let ranking: Vec<(String, i64)> = case.input["ranking"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|r| {
                        (
                            r["dimension"].as_str().unwrap_or_default().to_string(),
                            r["fail_count"].as_i64().unwrap_or(0),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let history: Vec<serde_json::Map<String, serde_json::Value>> = case.input["history"]
            .as_array()
            .map(|a| a.iter().filter_map(|x| x.as_object().cloned()).collect())
            .unwrap_or_default();
        let prompt = build_prompt(
            case.input["skill_id"].as_str().unwrap(),
            &files,
            &findings,
            &ranking,
            &history,
        );
        assert_eq!(
            prompt,
            case.expected.as_str().unwrap(),
            "case {}",
            case.slug
        );
    }
}

// The golden set is the whole corpus; a missing file is a test failure, so
// pin the count here.
#[test]
fn golden_corpus_is_complete() {
    assert_eq!(cases().len(), 27);
}
