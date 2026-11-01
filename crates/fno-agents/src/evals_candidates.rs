//! `fno-agents evals candidates`: rank real failures a person can label as
//! goldens. Read-only: it writes no file and calls no model.
//!
//! Sources: rule-labelled reactions in the decision corpus (whose export
//! already kept typed turns only, through `provenance::classify_turn`), S2
//! rows in corrections.log, `Revert` commits, and plans by superseded status
//! and caused plan commits. Every row is a candidate, never a label.

use std::collections::{BTreeMap, BTreeSet};
use std::io::BufRead as _;
use std::path::{Path, PathBuf};

use regex::Regex;
use serde_json::{json, Value};

const USAGE: &str =
    "usage: fno-agents evals candidates [--limit N] [--source KIND] [--json] (KIND: reaction-correct | reaction-reject | reaction-approve | correction | revert | plan)";
const DEFAULT_LIMIT: usize = 50;
const QUOTE_CHARS: usize = 240;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Candidate {
    pub kind: String,
    pub source_ref: String,
    pub quote: String,
    pub score: u32,
}

fn clip_tail(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let total = flat.chars().count();
    flat.chars()
        .skip(total.saturating_sub(QUOTE_CHARS))
        .collect()
}

/// A reaction row: the proposal the user answered, with its rule label.
fn reaction_candidate(row: &Value) -> Option<Candidate> {
    let meta = row.get("_meta")?;
    if meta.get("source")?.as_str()? != "rule" || meta.get("kind")?.as_str()? != "reaction" {
        return None;
    }
    let label = row
        .get("questions")?
        .as_object()?
        .values()
        .find_map(|q| q.get("label").and_then(Value::as_str))?;
    let score = match label {
        "correct" => 5,
        "reject" => 4,
        "approve" => 1,
        _ => return None,
    };
    let state = row.get("state")?;
    let call = state.get("proposed_call").and_then(Value::as_str);
    let text = call
        .filter(|c| !c.trim().is_empty())
        .or_else(|| state.get("assistant").and_then(Value::as_str))?;
    Some(Candidate {
        kind: format!("reaction-{label}"),
        source_ref: format!(
            "{}:{}",
            meta.get("session")?.as_str()?,
            meta.get("turn")?.as_str()?
        ),
        quote: clip_tail(text),
        score,
    })
}

fn read_corpus(dir: &Path, out: &mut Vec<Candidate>) {
    for name in ["train.jsonl", "holdout.jsonl"] {
        let Ok(file) = std::fs::File::open(dir.join(name)) else {
            continue;
        };
        for line in std::io::BufReader::new(file).lines().map_while(Result::ok) {
            // The corpus is 150 MB; a substring test spares the JSON parse on
            // the rows that cannot match.
            if !line.contains("\"reaction\"") {
                continue;
            }
            let row = serde_json::from_str::<Value>(&line).ok();
            out.extend(row.as_ref().and_then(reaction_candidate));
        }
    }
}

fn read_corrections(log: &Path, out: &mut Vec<Candidate>) {
    let Ok(text) = std::fs::read_to_string(log) else {
        return;
    };
    let quote = Regex::new(r#"^- "(.+)" \(x\d+, ([0-9a-f]+), signal=(\w+)"#).expect("static regex");
    for line in text.lines() {
        let fields: Vec<&str> = line.splitn(5, " | ").collect();
        let [_, "S2", _, _, body] = fields.as_slice() else {
            continue;
        };
        let Some(caps) = quote.captures(body.trim()) else {
            continue;
        };
        let score = if &caps[3] == "repeated_correction" {
            6
        } else {
            5
        };
        out.push(Candidate {
            kind: "correction".into(),
            source_ref: format!("{}:{}", &caps[2], &caps[3]),
            quote: clip_tail(&caps[1]),
            score,
        });
    }
}

fn read_reverts(repo: &Path, out: &mut Vec<Candidate>) {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C")
        .arg(repo)
        .args(["log", "--no-merges", "--grep=^Revert", "--format=%h %s"]);
    let Ok(output) = crate::bounded_cmd::output_with_timeout_result(cmd, 30) else {
        return;
    };
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if let Some((sha, subject)) = line.split_once(' ') {
            out.push(Candidate {
                kind: "revert".into(),
                source_ref: sha.to_string(),
                quote: clip_tail(subject),
                score: 4,
            });
        }
    }
}

/// node id -> plan commits that name a cause, read from the plans repo.
fn caused_commits(plans: &Path) -> BTreeMap<String, u32> {
    let mut counts = BTreeMap::new();
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C")
        .arg(plans)
        .args(["log", "--format=%s", "--", "."]);
    let Ok(output) = crate::bounded_cmd::output_with_timeout_result(cmd, 30) else {
        return counts;
    };
    let subject = Regex::new(r"^blueprint\(([^)]+)\): (.+)$").expect("static regex");
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if let Some(caps) = subject.captures(line) {
            let cause = &caps[2];
            if !cause.starts_with("initial") && !cause.starts_with("finalized") {
                *counts.entry(caps[1].to_string()).or_default() += 1;
            }
        }
    }
    counts
}

/// The join a plan carries: its node from `claims:`, then the filename, and
/// its PRs from `urls:`.
fn plan_candidate(
    name: &str,
    text: &str,
    caused: &BTreeMap<String, u32>,
    superseded_nodes: &BTreeSet<String>,
    node_rx: &Regex,
    pull_rx: &Regex,
) -> Option<Candidate> {
    let body = text.strip_prefix("---\n")?;
    let (front, _) = body.split_once("\n---")?;
    let field = |key: &str| {
        front.lines().find_map(|l| {
            l.strip_prefix(key)
                .and_then(|v| v.strip_prefix(':'))
                .map(|v| v.trim().trim_matches('"').to_string())
        })
    };
    let node = field("claims")
        .filter(|c| !c.is_empty())
        .or_else(|| node_rx.find(name).map(|m| m.as_str().to_string()));
    let superseded = node.as_ref().is_some_and(|n| superseded_nodes.contains(n));
    let causes = node
        .as_ref()
        .and_then(|n| caused.get(n))
        .copied()
        .unwrap_or(0);
    let score = u32::from(superseded) * 4 + causes.min(3);
    if score == 0 {
        return None;
    }
    let prs: BTreeSet<&str> = pull_rx
        .captures_iter(front)
        .filter_map(|c| c.get(1).map(|m| m.as_str()))
        .collect();
    let prs: Vec<&str> = prs.into_iter().collect();
    Some(Candidate {
        kind: "plan".into(),
        source_ref: format!(
            "{name} node={} pr={}",
            node.as_deref().unwrap_or("none"),
            if prs.is_empty() {
                "none".to_string()
            } else {
                prs.join(",")
            }
        ),
        quote: clip_tail(&format!(
            "{} (superseded={superseded}, caused_commits={causes})",
            field("title").unwrap_or_default()
        )),
        score,
    })
}

fn read_plans(plans: &Path, superseded: &BTreeSet<String>, out: &mut Vec<Candidate>) {
    let Ok(entries) = std::fs::read_dir(plans) else {
        return;
    };
    let caused = caused_commits(plans);
    let node_rx = Regex::new(r"[a-z]{1,3}-[0-9a-f]{4,8}").expect("static regex");
    let pull_rx = Regex::new(r"/pull/(\d+)").expect("static regex");
    for path in entries.flatten().map(|e| e.path()) {
        if path.extension().map_or(true, |e| e != "md") {
            continue;
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if let Ok(text) = std::fs::read_to_string(&path) {
            out.extend(plan_candidate(
                &name, &text, &caused, superseded, &node_rx, &pull_rx,
            ));
        }
    }
}

/// Highest score first; one row per quote, since a corrected rule repeats.
pub(crate) fn rank(mut rows: Vec<Candidate>) -> Vec<Candidate> {
    rows.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.kind.cmp(&b.kind))
            .then_with(|| a.source_ref.cmp(&b.source_ref))
    });
    let mut seen = BTreeSet::new();
    rows.retain(|r| seen.insert(r.quote.to_lowercase()));
    for row in &mut rows {
        row.quote = crate::intel_corpus::scrub_quote(&row.quote);
    }
    rows
}

struct Sources {
    corpus: PathBuf,
    corrections: Option<PathBuf>,
    repo: PathBuf,
    plans: Option<PathBuf>,
    superseded: BTreeSet<String>,
}

fn mine(sources: &Sources) -> Vec<Candidate> {
    let mut rows = Vec::new();
    read_corpus(&sources.corpus, &mut rows);
    if let Some(log) = &sources.corrections {
        read_corrections(log, &mut rows);
    }
    read_reverts(&sources.repo, &mut rows);
    if let Some(plans) = &sources.plans {
        read_plans(plans, &sources.superseded, &mut rows);
    }
    rank(rows)
}

pub fn run(args: &[String]) -> i32 {
    let limit = match crate::evals::flag_lookup(args, "--limit") {
        None => DEFAULT_LIMIT,
        Some(raw) => match raw.parse::<usize>() {
            Ok(n) => n,
            Err(_) => {
                eprintln!("{USAGE}");
                return 2;
            }
        },
    };
    let source = crate::evals::flag_lookup(args, "--source");
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let state = std::env::var_os("FNO_HOME")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|h| h.join(".fno")))
        .unwrap_or_else(|| PathBuf::from(".fno"));
    let rows = mine(&Sources {
        corpus: state.join("evals").join("decision").join("v1"),
        corrections: crate::finalize::corrections_log_path(home.as_deref()),
        repo: crate::evals::canonical_repo_root(),
        plans: crate::paths_cli::plans_dir_for(&cwd),
        superseded: crate::evals::superseded_nodes(&crate::backlog::settings::graph_path_at(Some(
            &cwd,
        ))),
    });
    let rows: Vec<&Candidate> = rows
        .iter()
        .filter(|r| source.as_deref().map_or(true, |s| r.kind == s))
        .take(limit)
        .collect();
    if args.iter().any(|a| a == "--json" || a == "-J") {
        let view: Vec<Value> = rows
            .iter()
            .map(|r| json!({"kind": r.kind, "ref": r.source_ref, "quote": r.quote, "score": r.score}))
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&view).unwrap_or_default()
        );
    } else {
        for r in rows {
            println!("{}\t{}\t{}\t{}", r.score, r.kind, r.source_ref, r.quote);
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut files: Vec<(PathBuf, Vec<u8>)> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| (e.path(), std::fs::read(e.path()).unwrap()))
            .collect();
        files.sort();
        files
    }

    #[test]
    fn mine_ranks_every_source_scrubs_quotes_and_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let reaction = |label: &str, call: &str, turn: &str| {
            json!({
                "_meta": {"source": "rule", "kind": "reaction", "session": "s1", "turn": turn},
                "questions": {"reaction": {"label": label}},
                "state": {"assistant": "a", "proposed_call": call},
            })
        };
        let corpus = [
            reaction("reject", "Bash: rm -rf build", "t1"),
            reaction("reject", "Bash: rm -rf build", "t2"),
            reaction("interrupt", "Bash: sleep 9", "t3"),
            reaction("approve", "Bash: cargo fmt", "t4"),
        ]
        .map(|r| r.to_string())
        .join("\n");
        std::fs::write(tmp.path().join("train.jsonl"), corpus).unwrap();
        let log = tmp.path().join("corrections.log");
        std::fs::write(
            &log,
            "2026-10-13T09:02:42Z | S1 | target-postmortem | p.md | DonePRGreen\n\
             2026-10-13T09:02:42Z | S2 | insights-tag | r.md:1 | - \"mail me at a@b.co\" (x1, 01a0, signal=repeated_correction) #agent-correction\n",
        )
        .unwrap();
        let plan = "---\ntitle: \"Old idea\"\nclaims: n-1\nurls:\n  - https://github.com/o/r/pull/77\n---\nbody\n";
        std::fs::write(tmp.path().join("20261001-n-1.md"), plan).unwrap();
        std::fs::write(
            tmp.path().join("20261002-n-2.md"),
            "---\nstatus: ready\n---\n",
        )
        .unwrap();

        let before = snapshot(tmp.path());
        let rows = mine(&Sources {
            corpus: tmp.path().to_path_buf(),
            corrections: Some(log),
            repo: tmp.path().to_path_buf(),
            plans: Some(tmp.path().to_path_buf()),
            superseded: BTreeSet::from(["n-1".to_string()]),
        });
        assert_eq!(before, snapshot(tmp.path()), "the miner writes nothing");

        let kinds: Vec<&str> = rows.iter().map(|r| r.kind.as_str()).collect();
        assert_eq!(
            kinds,
            ["correction", "plan", "reaction-reject", "reaction-approve"],
            "ranked by score, the duplicate reject and the interrupt gone"
        );
        assert!(rows[0].quote.contains("[email]"), "{}", rows[0].quote);
        assert_eq!(rows[1].source_ref, "20261001-n-1.md node=n-1 pr=77");
    }
}
