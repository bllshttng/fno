//! `fno-agents intel --plans`: plan premises as labels. A checkable plan
//! sentence pairs with the records that came later on its node. A classifier
//! labels each pair as supports, contradicts or unrelated. Rows land under
//! `<state>/evals/plan-labels/` and become candidates, never goldens.
//!
//! No rung labels the plans until it passes the fixed labelled set, run by
//! `--plans-ladder`. The cheapest passing rung wins and no later rung runs.
//! Local rungs stay on loopback. A hosted rung runs only after both local
//! rungs scored and failed, and only on pairs that passed the scrub and a
//! person's read, because it sends the text to a vendor.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use regex::Regex;
use serde_json::{json, Value};

const PASS_BALANCED: f64 = 0.80;
const PASS_CONTRADICTS: f64 = 0.70;
const LABELS: [&str; 3] = ["supports", "contradicts", "unrelated"];
const SET_PER_LABEL: usize = 20;
const MAX_RECORDS_PER_PLAN: usize = 10;
const PREMISE_CHARS: usize = 600;
const RECORD_CHARS: usize = 2_000;
const LOCAL_TIMEOUT: Duration = Duration::from_secs(30);
const HOSTED_TIMEOUT_S: u64 = 120;
const DEFAULT_LIMIT: usize = 1_000;
const USAGE: &str = "usage: fno-agents intel --plans [--limit N] [--dry-run] | --plans-ladder [--teacher-url URL] [--hosted NAME=COMMAND]";

/// One labelled or unlabelled premise and record. `person_read` says a person
/// read this scrubbed text; only the fixed set carries it.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct Pair {
    pub id: String,
    pub claim: String,
    pub record: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub person_read: bool,
    #[serde(default)]
    pub plan: String,
    #[serde(default)]
    pub node: String,
    #[serde(default)]
    pub record_kind: String,
    #[serde(default)]
    pub overruled_by: Option<String>,
}

pub(crate) struct Answer {
    pub label: String,
    pub dollars: Option<f64>,
}

/// One classifier rung. `score` is a closure so a test can stand in for a
/// server, and so a hosted rung is only a command.
pub(crate) struct Rung<'a> {
    pub name: String,
    pub hosted: bool,
    pub url: Option<String>,
    pub score: Box<dyn FnMut(&Pair) -> Result<Answer, String> + 'a>,
}

fn clip(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// A rung's result on the fixed set. An unreachable rung has an `error` and
/// no score, and it reads as not passed.
fn score_rung(rung: &mut Rung, set: &[Pair]) -> Value {
    let mut hits: BTreeMap<&str, (u32, u32)> = BTreeMap::new();
    let mut millis = Vec::with_capacity(set.len());
    let mut dollars = 0.0;
    let mut priced = 0usize;
    for pair in set {
        let start = Instant::now();
        let answer = match (rung.score)(pair) {
            Ok(answer) => answer,
            Err(error) => {
                return json!({"name": rung.name, "hosted": rung.hosted, "url": rung.url,
                              "passed": false, "error": clip(&error, 300)});
            }
        };
        millis.push(start.elapsed().as_secs_f64() * 1000.0);
        if let Some(d) = answer.dollars {
            dollars += d;
            priced += 1;
        }
        let want = pair.label.as_deref().unwrap_or_default();
        let cell = hits.entry(want).or_default();
        cell.1 += 1;
        cell.0 += u32::from(answer.label == want);
    }
    let recall = |label: &str| {
        hits.get(label)
            .filter(|(_, total)| *total > 0)
            .map(|(hit, total)| f64::from(*hit) / f64::from(*total))
    };
    let classes: Vec<f64> = LABELS.iter().filter_map(|l| recall(l)).collect();
    let balanced = classes.iter().sum::<f64>() / classes.len().max(1) as f64;
    let contradicts = recall("contradicts").unwrap_or(0.0);
    millis.sort_by(f64::total_cmp);
    let median = millis.get(millis.len() / 2).copied().unwrap_or(0.0);
    json!({
        "name": rung.name,
        "hosted": rung.hosted,
        "url": rung.url,
        "n": set.len(),
        "balanced": balanced,
        "contradicts_recall": contradicts,
        "median_ms": median,
        "dollars_per_pair": if rung.hosted && priced == set.len() {
            json!(dollars / set.len().max(1) as f64)
        } else if rung.hosted {
            Value::Null
        } else {
            json!(0.0)
        },
        "passed": classes.len() == LABELS.len()
            && balanced >= PASS_BALANCED
            && contradicts >= PASS_CONTRADICTS,
    })
}

/// A hosted rung's gate. The refusal comes before any text can leave.
fn hosted_refusal(set: &[Pair], scores: &[Value]) -> Option<String> {
    if let Some(open) = scores
        .iter()
        .find(|s| s.get("error").is_some() || s.get("balanced").is_none())
    {
        return Some(format!(
            "local rung {} has no score; a hosted rung needs both local scores",
            open["name"].as_str().unwrap_or_default()
        ));
    }
    if scores
        .iter()
        .filter(|s| s["hosted"] == json!(false))
        .count()
        < 2
    {
        return Some("a hosted rung needs both local rungs scored first".to_string());
    }
    set.iter().find_map(|pair| {
        if !pair.person_read {
            return Some(format!("pair {} has no person's read", pair.id));
        }
        let clean = crate::intel_corpus::scrub_quote(&pair.claim) == pair.claim
            && crate::intel_corpus::scrub_quote(&pair.record) == pair.record;
        (!clean).then(|| format!("pair {} did not pass the scrub", pair.id))
    })
}

/// Rungs in cost order, stopping at the first pass.
pub(crate) fn run_ladder(set: &[Pair], rungs: &mut [Rung]) -> Result<Value, String> {
    let mut scores = Vec::new();
    let mut winner = Value::Null;
    for rung in rungs.iter_mut() {
        if rung.hosted {
            if let Some(why) = hosted_refusal(set, &scores) {
                return Err(format!("hosted rung {} refused: {why}", rung.name));
            }
        }
        let score = score_rung(rung, set);
        let passed = score["passed"] == json!(true);
        scores.push(score);
        if passed {
            winner = json!(rung.name);
            break;
        }
    }
    Ok(json!({"rungs": scores, "winner": winner}))
}

fn score_lines(ladder: &Value) -> Vec<String> {
    ladder["rungs"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|s| match s.get("error").and_then(Value::as_str) {
            Some(error) => format!("{}: no score: {error}", s["name"].as_str().unwrap_or("?")),
            None => format!(
                "{}: balanced {:.3}, contradicts recall {:.3}, {:.0} ms, dollars per pair {}",
                s["name"].as_str().unwrap_or("?"),
                s["balanced"].as_f64().unwrap_or(0.0),
                s["contradicts_recall"].as_f64().unwrap_or(0.0),
                s["median_ms"].as_f64().unwrap_or(0.0),
                s["dollars_per_pair"],
            ),
        })
        .collect()
}

/// The rung `--plans` may use, or the refusal it prints. A hosted winner still
/// refuses: the full pass sends far more than the test, so it needs its own
/// ruling, with the per-pair price and the text count stated.
pub(crate) fn winning_url(ladder: Option<&Value>) -> Result<String, String> {
    let Some(ladder) = ladder else {
        return Err("no ladder result; run fno-agents intel --plans-ladder first".to_string());
    };
    let scores = score_lines(ladder).join("\n");
    let Some(winner) = ladder["winner"].as_str() else {
        return Err(format!("no classifier rung passed the set:\n{scores}"));
    };
    let rung = ladder["rungs"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|r| r["name"].as_str() == Some(winner));
    match rung.and_then(|r| r["url"].as_str()) {
        Some(url) if rung.is_some_and(|r| r["hosted"] == json!(false)) => Ok(url.to_string()),
        _ => Err(format!(
            "hosted rung {winner} won the set, and the full pass needs its own user ruling:\n{scores}"
        )),
    }
}

/// Checkable sentences: a plan's context, why and audit lines that name a
/// path or symbol in backticks.
pub(crate) fn premises(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        if let Some(heading) = line.strip_prefix("## ") {
            inside = heading.starts_with("Context")
                || heading.starts_with("Why")
                || heading.starts_with("Existence audit");
            continue;
        }
        if !inside {
            continue;
        }
        for sentence in line.split_inclusive(". ") {
            let sentence = sentence.trim().trim_matches('|').trim();
            if sentence.contains('`') && sentence.chars().count() >= 30 {
                out.push(clip(sentence, PREMISE_CHARS));
            }
        }
    }
    out
}

#[derive(Default, Clone)]
pub(crate) struct NodeFacts {
    pub records: Vec<(String, String)>,
    pub ruled_after_plan: bool,
    pub superseded: bool,
    pub reverted: bool,
}

/// A premise reads overruled only by a mechanical fact.
pub(crate) fn overruled_by(facts: &NodeFacts) -> Option<&'static str> {
    if facts.superseded {
        Some("supersede")
    } else if facts.reverted {
        Some("revert")
    } else if facts.ruled_after_plan {
        Some("ruling")
    } else {
        None
    }
}

pub(crate) fn plan_pairs(name: &str, text: &str, node: &str, facts: &NodeFacts) -> Vec<Pair> {
    let start = facts.records.len().saturating_sub(MAX_RECORDS_PER_PLAN);
    let mut out = Vec::new();
    for claim in premises(text) {
        for (kind, record) in &facts.records[start..] {
            let id = crate::intel_corpus::row_id(name, &claim, kind, record);
            out.push(Pair {
                id,
                claim: crate::intel_corpus::scrub_quote(&claim),
                record: crate::intel_corpus::scrub_quote(&clip(record, RECORD_CHARS)),
                label: None,
                person_read: false,
                plan: name.to_string(),
                node: node.to_string(),
                record_kind: kind.clone(),
                overruled_by: overruled_by(facts).map(str::to_string),
            });
        }
    }
    out
}

type DatedTexts = Vec<(String, String)>;

/// Records per node from the graph: rulings, notes and the closing note.
fn graph_facts(graph: &Path) -> BTreeMap<String, (NodeFacts, DatedTexts, DatedTexts)> {
    let query = crate::backlog::RowQuery {
        with_blockers: true,
        ..Default::default()
    };
    let rows = crate::graph_store::read_rows_where(graph, &query).unwrap_or_default();
    let mut out = BTreeMap::new();
    for row in rows {
        let Some(id) = row.get("id").and_then(Value::as_str) else {
            continue;
        };
        let mut facts = NodeFacts {
            superseded: row.get("status").and_then(Value::as_str) == Some("superseded"),
            ..Default::default()
        };
        let mut rulings = Vec::new();
        for ruling in row
            .get("decisions")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let text = ruling
                .get("decision")
                .or_else(|| ruling.get("text"))
                .and_then(Value::as_str);
            let ts = ruling
                .get("created_at")
                .or_else(|| ruling.get("ts"))
                .and_then(Value::as_str);
            if let (Some(ts), Some(text)) = (ts, text) {
                let day = ts.get(..10).unwrap_or_default().replace('-', "");
                rulings.push((day, text.to_string()));
            }
        }
        let notes: DatedTexts = row
            .get("progress_notes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(dated_note)
            .collect();
        // A closing note the graph wrote says nothing about a premise.
        if let Some(note) = row
            .get("completion_note")
            .and_then(Value::as_str)
            .filter(|n| !n.starts_with("auto-closed"))
        {
            facts
                .records
                .push(("completion".to_string(), note.to_string()));
        }
        out.insert(id.to_string(), (facts, notes, rulings));
    }
    out
}

/// A note row's day and text. A wave row is plan scaffolding, not a record.
fn dated_note(note: &Value) -> Option<(String, String)> {
    if note.get("kind").and_then(Value::as_str) == Some("wave") {
        return None;
    }
    let text = note.get("text").and_then(Value::as_str)?;
    let ts = note.get("ts").and_then(Value::as_str).unwrap_or_default();
    let day = ts.get(..10).unwrap_or_default().replace('-', "");
    Some((day, text.to_string()))
}

/// A note before the plan's day cannot speak to the plan. An undated note
/// reads as older, so it never counts.
fn apply_notes(facts: &mut NodeFacts, notes: &[(String, String)], plan_day: &str) {
    for (day, text) in notes {
        if day.as_str() >= plan_day {
            facts.records.push(("note".to_string(), text.clone()));
        }
    }
}

/// Only a ruling from the plan's day or later is a later record. A ruling on
/// the plan's own day may predate it, so only a later day overrules.
pub(crate) fn apply_rulings(facts: &mut NodeFacts, rulings: &[(String, String)], plan_day: &str) {
    for (day, text) in rulings {
        if day.as_str() >= plan_day {
            facts.records.push(("ruling".to_string(), text.clone()));
        }
        facts.ruled_after_plan |= day.as_str() > plan_day;
    }
}

/// Nodes with a reverted merge, from the merge subjects that name the branch.
fn reverted_nodes(repo: &Path) -> BTreeSet<String> {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C")
        .arg(repo)
        .args(["log", "--grep=^Revert", "--format=%s"]);
    let mut out = BTreeSet::new();
    let Ok(output) = crate::bounded_cmd::output_with_timeout_result(cmd, 60) else {
        return out;
    };
    let node = Regex::new(r"\b[a-z]{1,3}-[0-9a-f]{4,8}\b").expect("static regex");
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        out.extend(node.find_iter(line).map(|m| m.as_str().to_string()));
    }
    out
}

fn all_pairs(plans: &Path, graph: &Path, repo: &Path) -> Vec<Pair> {
    let facts = graph_facts(graph);
    let reverted = reverted_nodes(repo);
    let Ok(entries) = std::fs::read_dir(plans) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "md"))
        .collect();
    paths.sort();
    let mut out = Vec::new();
    for path in paths {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Some(node) = crate::evals_candidates::plan_node(&name, &text) else {
            continue;
        };
        let Some((node_facts, notes, rulings)) = facts.get(&node) else {
            continue;
        };
        let mut node_facts = node_facts.clone();
        node_facts.reverted = reverted.contains(&node);
        let plan_day = name.get(..8).unwrap_or_default();
        apply_notes(&mut node_facts, notes, plan_day);
        apply_rulings(&mut node_facts, rulings, plan_day);
        out.extend(plan_pairs(&name, &text, &node, &node_facts));
    }
    out
}

/// The corpus question, word for word: a reworded question is a new input.
fn local_score(url: &str, pair: &Pair) -> Result<Answer, String> {
    if !crate::fnod::loopback(url) {
        return Err(format!("{url} is not loopback; refused"));
    }
    let body = json!({
        "state": {"claim": pair.claim, "record_call": pair.record_kind, "record": pair.record},
        "questions": {"claim_support": crate::intel_corpus::question("claim_support")},
    });
    let (status, text) = crate::opencode_serve::http_json(
        url,
        "POST",
        "/v1/systemone",
        Some(&body),
        None,
        LOCAL_TIMEOUT,
    )?;
    if status != 200 {
        return Err(format!("HTTP {status}"));
    }
    let reply: Value = serde_json::from_str(&text).map_err(|e| format!("reply: {e}"))?;
    let probabilities = reply
        .pointer("/answers/claim_support/probabilities")
        .and_then(Value::as_object)
        .ok_or("reply carries no claim_support probabilities")?;
    let label = probabilities
        .iter()
        .filter_map(|(k, v)| v.as_f64().map(|p| (k, p)))
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(k, _)| k.clone())
        .ok_or("reply carries empty probabilities")?;
    Ok(Answer {
        label,
        dollars: Some(0.0),
    })
}

/// A hosted rung is a command the user supplied. It reads the pair's JSON file
/// named as its last argument and prints `{"label": ..., "dollars": ...}`.
fn hosted_score(command: &str, scratch: &Path, pair: &Pair) -> Result<Answer, String> {
    let body = json!({"claim": pair.claim, "record": pair.record, "id": pair.id});
    std::fs::write(scratch, body.to_string()).map_err(|e| e.to_string())?;
    let mut words = command.split_whitespace();
    let program = words.next().ok_or("empty hosted command")?;
    let mut cmd = std::process::Command::new(program);
    cmd.args(words).arg(scratch);
    let output = crate::bounded_cmd::output_with_timeout_result(cmd, HOSTED_TIMEOUT_S)
        .map_err(|e| e.to_string());
    let _ = std::fs::remove_file(scratch);
    let output = output?;
    if !output.status.success() {
        return Err(format!("hosted command exit {:?}", output.status.code()));
    }
    let reply: Value = serde_json::from_slice(&output.stdout).map_err(|e| format!("reply: {e}"))?;
    Ok(Answer {
        label: reply["label"]
            .as_str()
            .ok_or("reply has no label")?
            .to_string(),
        dollars: reply["dollars"].as_f64(),
    })
}

fn labels_dir() -> PathBuf {
    crate::evals_candidates::state_root()
        .join("evals")
        .join("plan-labels")
}

fn read_set(path: &Path) -> Result<Vec<Pair>, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut set = Vec::new();
    for line in std::io::BufReader::new(file).lines() {
        let line = line.map_err(|e| e.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        set.push(serde_json::from_str::<Pair>(&line).map_err(|e| format!("set row: {e}"))?);
    }
    let per_label = |label: &str| {
        set.iter()
            .filter(|p| p.label.as_deref() == Some(label))
            .count()
    };
    if LABELS.iter().any(|l| per_label(l) != SET_PER_LABEL) {
        return Err(format!(
            "the set needs {SET_PER_LABEL} pairs per label, and {} holds {} pairs",
            path.display(),
            set.len()
        ));
    }
    if set
        .iter()
        .any(|p| !LABELS.contains(&p.label.as_deref().unwrap_or_default()))
    {
        return Err("every set row needs a label".to_string());
    }
    Ok(set)
}

fn ladder_command(args: &[String], dir: &Path) -> i32 {
    let set = match read_set(&dir.join("ladder-set.jsonl")) {
        Ok(set) => set,
        Err(e) => {
            eprintln!("fno-agents intel: {e}");
            return 1;
        }
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let fnod_url = crate::fnod::configured_url(&cwd);
    let teacher_url = crate::evals::flag_lookup(args, "--teacher-url");
    let hosted = crate::evals::flag_lookup(args, "--hosted");
    let scratch = dir.join(".hosted-pair.json");
    let teacher = teacher_url.clone().unwrap_or_default();
    let mut rungs = vec![Rung {
        name: "fnod".to_string(),
        hosted: false,
        url: Some(fnod_url.clone()),
        score: Box::new(|pair| local_score(&fnod_url, pair)),
    }];
    rungs.push(Rung {
        name: "kev-9b".to_string(),
        hosted: false,
        url: teacher_url.clone(),
        score: Box::new(|pair| {
            if teacher.is_empty() {
                return Err("no --teacher-url given".to_string());
            }
            local_score(&teacher, pair)
        }),
    });
    if let Some(spec) = &hosted {
        let Some((name, command)) = spec.split_once('=') else {
            eprintln!("{USAGE}");
            return 2;
        };
        let (command, scratch) = (command.to_string(), scratch.clone());
        rungs.push(Rung {
            name: name.to_string(),
            hosted: true,
            url: None,
            score: Box::new(move |pair| hosted_score(&command, &scratch, pair)),
        });
    }
    let ladder = match run_ladder(&set, &mut rungs) {
        Ok(ladder) => ladder,
        Err(e) => {
            eprintln!("fno-agents intel: {e}");
            return 1;
        }
    };
    let body = serde_json::to_string_pretty(&ladder).unwrap_or_default();
    if let Err(e) = std::fs::write(dir.join("ladder.json"), &body) {
        eprintln!("fno-agents intel: {e}");
        return 1;
    }
    for line in score_lines(&ladder) {
        println!("{line}");
    }
    println!("winner: {}", ladder["winner"]);
    i32::from(ladder["winner"].is_null())
}

fn plans_command(args: &[String], dir: &Path) -> i32 {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let Some(plans) = crate::paths_cli::plans_dir_for(&cwd) else {
        eprintln!("fno-agents intel: no plans directory resolves here");
        return 1;
    };
    let graph = crate::backlog::settings::graph_path_at(Some(&cwd));
    let limit = match crate::evals::flag_lookup(args, "--limit").map(|v| v.parse::<usize>()) {
        None => DEFAULT_LIMIT,
        Some(Ok(n)) => n,
        Some(Err(_)) => {
            eprintln!("{USAGE}");
            return 2;
        }
    };
    let repo = crate::evals::canonical_repo_root();
    if args.iter().any(|a| a == "--dry-run") {
        for pair in all_pairs(&plans, &graph, &repo).iter().take(limit) {
            println!("{}", serde_json::to_string(pair).unwrap_or_default());
        }
        return 0;
    }
    let ladder = std::fs::read_to_string(dir.join("ladder.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok());
    let url = match winning_url(ladder.as_ref()) {
        Ok(url) => url,
        Err(refusal) => {
            eprintln!("fno-agents intel: --plans refused: {refusal}");
            return 1;
        }
    };
    let rows_path = dir.join("rows.jsonl");
    let done: BTreeSet<String> = std::fs::read_to_string(&rows_path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter_map(|v| v["id"].as_str().map(str::to_string))
        .collect();
    let mut file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&rows_path)
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("fno-agents intel: {e}");
            return 1;
        }
    };
    let mut written = 0usize;
    for pair in all_pairs(&plans, &graph, &repo)
        .into_iter()
        .filter(|p| !done.contains(&p.id))
        .take(limit)
    {
        let answer = match local_score(&url, &pair) {
            Ok(answer) => answer,
            Err(e) => {
                eprintln!("fno-agents intel: {written} rows, then {e}");
                return 1;
            }
        };
        let row = Pair {
            label: Some(answer.label),
            ..pair
        };
        let line = serde_json::to_string(&row).unwrap_or_default();
        if let Err(e) = writeln!(file, "{line}") {
            eprintln!("fno-agents intel: {e}");
            return 1;
        }
        written += 1;
    }
    println!("{written} rows in {}", rows_path.display());
    0
}

/// A mistyped `--dry-run` must not start a real pass.
fn unknown_flag(args: &[String]) -> Option<&str> {
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--plans" | "--plans-ladder" | "--dry-run" => {}
            "--limit" | "--teacher-url" | "--hosted" => {
                rest.next();
            }
            other => return Some(other),
        }
    }
    None
}

pub fn run(args: &[String]) -> i32 {
    if let Some(flag) = unknown_flag(args) {
        eprintln!("fno-agents intel: unknown flag {flag}\n{USAGE}");
        return 2;
    }
    let dir = labels_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("fno-agents intel: {}: {e}", dir.display());
        return 1;
    }
    if args.iter().any(|a| a == "--plans-ladder") {
        ladder_command(args, &dir)
    } else {
        plans_command(args, &dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn set(person_read: bool) -> Vec<Pair> {
        LABELS
            .iter()
            .flat_map(|l| std::iter::repeat(*l).take(20))
            .enumerate()
            .map(|(i, label)| Pair {
                id: format!("p{i}"),
                claim: format!("claim {i}"),
                record: format!("record {i}"),
                label: Some(label.to_string()),
                person_read,
                plan: String::new(),
                node: String::new(),
                record_kind: String::new(),
                overruled_by: None,
            })
            .collect()
    }

    fn answering<'a>(name: &str, hosted: bool, calls: &'a Cell<u32>, right: bool) -> Rung<'a> {
        Rung {
            name: name.to_string(),
            hosted,
            url: (!hosted).then(|| "http://127.0.0.1:1".to_string()),
            score: Box::new(move |pair| {
                calls.set(calls.get() + 1);
                Ok(Answer {
                    label: if right {
                        pair.label.clone().unwrap()
                    } else {
                        "unrelated".to_string()
                    },
                    dollars: Some(0.01),
                })
            }),
        }
    }

    #[test]
    fn with_no_passing_rung_plans_refuses_and_prints_each_score() {
        let (a, b) = (Cell::new(0), Cell::new(0));
        let mut rungs = [
            answering("fnod", false, &a, false),
            answering("kev-9b", false, &b, false),
        ];
        let ladder = run_ladder(&set(false), &mut rungs).unwrap();
        assert!(ladder["winner"].is_null());
        let refusal = winning_url(Some(&ladder)).unwrap_err();
        assert!(refusal.contains("fnod: balanced 0.333"), "{refusal}");
        assert!(refusal.contains("kev-9b: balanced 0.333"), "{refusal}");
        assert!(winning_url(None).unwrap_err().contains("--plans-ladder"));
    }

    #[test]
    fn a_fnod_pass_ends_the_ladder_with_no_later_rung_called() {
        let (a, b, c) = (Cell::new(0), Cell::new(0), Cell::new(0));
        let mut rungs = [
            answering("fnod", false, &a, true),
            answering("kev-9b", false, &b, true),
            answering("vendor", true, &c, true),
        ];
        let ladder = run_ladder(&set(false), &mut rungs).unwrap();
        assert_eq!(ladder["winner"], "fnod");
        assert_eq!(ladder["rungs"].as_array().unwrap().len(), 1);
        assert_eq!((a.get(), b.get(), c.get()), (60, 0, 0));
        assert_eq!(winning_url(Some(&ladder)).unwrap(), "http://127.0.0.1:1");
    }

    #[test]
    fn a_hosted_rung_refuses_before_any_call_without_a_read_and_scrub() {
        let run = |pairs: Vec<Pair>| {
            let (a, b, c) = (Cell::new(0), Cell::new(0), Cell::new(0));
            let mut rungs = [
                answering("fnod", false, &a, false),
                answering("kev-9b", false, &b, false),
                answering("vendor", true, &c, true),
            ];
            (run_ladder(&pairs, &mut rungs), c.get())
        };
        let (unread, calls) = run(set(false));
        assert!(unread.unwrap_err().contains("person's read"));
        assert_eq!(calls, 0);

        let mut leaky = set(true);
        leaky[3].record = "mail me at a@b.co".to_string();
        let (leak, calls) = run(leaky);
        assert!(leak.unwrap_err().contains("scrub"));
        assert_eq!(calls, 0);

        let (read, calls) = run(set(true));
        let ladder = read.unwrap();
        assert_eq!(ladder["winner"], "vendor");
        assert_eq!(calls, 60);
        let refusal = winning_url(Some(&ladder)).unwrap_err();
        assert!(refusal.contains("own user ruling"), "{refusal}");
    }

    #[test]
    fn premises_pair_with_records_and_only_a_mechanical_fact_overrules() {
        let plan = "---\nclaims: n-1\n---\n## Context\n\nThe writer `a.rs` runs daily at noon. No backticks in this one at all here.\n\n## Done\n\nThe `done.rs` line is not a premise of any kind.\n";
        assert_eq!(premises(plan), ["The writer `a.rs` runs daily at noon."]);
        let mut facts = NodeFacts {
            records: vec![("note".to_string(), "mail a@b.co about a.rs".to_string())],
            ..Default::default()
        };
        let pair = &plan_pairs("20261001-n-1.md", plan, "n-1", &facts)[0];
        assert!(pair.record.contains("[email]"), "{}", pair.record);
        assert_eq!(pair.overruled_by, None);
        let row = |kind: &str, ts: &str| json!({"kind": kind, "ts": ts, "text": "t"});
        assert_eq!(dated_note(&row("wave", "2026-10-02T00:00:00Z")), None);
        let later = dated_note(&row("progress", "2026-10-02T00:00:00Z")).unwrap();
        let older = dated_note(&row("progress", "2026-09-30T00:00:00Z")).unwrap();
        let mut noted = NodeFacts::default();
        apply_notes(&mut noted, &[older, later], "20261001");
        assert_eq!(noted.records, [("note".to_string(), "t".to_string())]);
        assert_eq!(overruled_by(&noted), None, "a note is no mechanical fact");
        let rulings = [
            ("20260901".to_string(), "older".to_string()),
            ("20261002".to_string(), "newer".to_string()),
        ];
        apply_rulings(&mut facts, &rulings[..1], "20261001");
        assert_eq!(facts.records.len(), 1, "an older ruling is no later record");
        assert_eq!(overruled_by(&facts), None);
        apply_rulings(&mut facts, &rulings[1..], "20261001");
        assert_eq!(facts.records[1].1, "newer");
        assert_eq!(overruled_by(&facts), Some("ruling"));
        assert_eq!(
            unknown_flag(&["--plans".to_string(), "--dryrun".to_string()]),
            Some("--dryrun")
        );
        facts.reverted = true;
        assert_eq!(overruled_by(&facts), Some("revert"));
        facts.superseded = true;
        assert_eq!(overruled_by(&facts), Some("supersede"));
    }
}
