//! `fno inbox outstanding`: what is waiting on the operator, in one read.
//!
//! Five streams fold into one report: the operator lane, this project's
//! carve-outs, open operator questions, open prove-it FAIL verdicts and open
//! `fu-` captures across every known project. `--json` is the contract
//! `org_board` and `finalize` read, so its key order and shape are fixed.

use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::io::Write as _;

pub const RENDER_CAP: usize = 3;
pub const QUESTION_RENDER_CAP: usize = 10;
pub const QUESTION_BODY_CAP: usize = 240;
pub const BACKFILL_KIND: &str = "backfill";

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Question {
    pub id: String,
    pub ts: String,
    pub question: String,
    pub session_id: Option<String>,
    pub cwd: Option<String>,
    pub node: Option<String>,
    pub asker: Option<String>,
    pub ask: Option<String>,
    pub options: Vec<String>,
    pub blocks: Vec<String>,
    pub subject: Option<String>,
    pub live: Option<bool>,
}

impl Question {
    pub fn as_json(&self, rank: usize) -> Value {
        json!({
            "id": self.id,
            "ts": self.ts,
            "question": self.question,
            "session_id": self.session_id,
            "cwd": self.cwd,
            "node": self.node,
            "asker": self.asker,
            "ask": self.ask,
            "options": self.options,
            "blocks": self.blocks,
            "subject": self.subject,
            "live": self.live,
            "rank": rank,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Capture {
    pub fu_id: String,
    pub title: String,
    pub project: String,
    pub added_at: Option<String>,
}

impl Capture {
    pub fn as_json(&self) -> Value {
        json!({
            "id": self.fu_id,
            "title": self.title,
            "project": self.project,
            "added_at": self.added_at,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Verdict {
    pub node: String,
    pub report: String,
    pub verdict: String,
    pub claim: String,
    pub status: Option<String>,
    pub mtime: Option<String>,
}

impl Verdict {
    pub fn as_json(&self) -> Value {
        json!({
            "node": self.node,
            "report": self.report,
            "verdict": self.verdict,
            "claim": self.claim,
            "status": self.status,
            "mtime": self.mtime,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LaneRow {
    pub text: String,
    pub line: usize,
}

#[derive(Clone, Debug, Default)]
pub struct Outstanding {
    pub carveout_total: u64,
    /// Insertion order follows first sight in the ledger, as the Python dict did.
    pub carveout_by_kind: Vec<(String, u64)>,
    pub carveout_oldest_ts: Option<String>,
    pub carveout_root: Option<String>,
    pub questions: Vec<Question>,
    pub questions_store: String,
    pub captures: Vec<Capture>,
    pub capture_file_total: u64,
    pub capture_row_total: u64,
    pub lane: Vec<LaneRow>,
    pub lane_parked: u64,
    pub lane_store: String,
    pub verdicts: Vec<Verdict>,
    pub verdicts_error: Option<String>,
}

impl Outstanding {
    pub fn empty(&self) -> bool {
        self.carveout_total == 0
            && self.questions.is_empty()
            && self.captures.is_empty()
            && self.lane.is_empty()
            && self.verdicts.is_empty()
            && self.verdicts_error.is_none()
    }

    pub fn as_json(&self) -> Value {
        let mut by_project: Map<String, Value> = Map::new();
        let mut counts: BTreeMap<&str, u64> = BTreeMap::new();
        let mut order: Vec<&str> = Vec::new();
        for c in &self.captures {
            let n = counts.entry(c.project.as_str()).or_insert(0);
            if *n == 0 {
                order.push(c.project.as_str());
            }
            *n += 1;
        }
        for p in order {
            by_project.insert(p.to_string(), json!(counts[p]));
        }
        let mut by_kind: Map<String, Value> = Map::new();
        for (kind, n) in &self.carveout_by_kind {
            by_kind.insert(kind.clone(), json!(n));
        }
        let questions: Vec<Value> = self
            .questions
            .iter()
            .enumerate()
            .map(|(i, q)| q.as_json(i + 1))
            .collect();
        let captures: Vec<Value> = self.captures.iter().map(Capture::as_json).collect();
        let lane: Vec<Value> = self
            .lane
            .iter()
            .map(|i| json!({"text": i.text, "line": i.line}))
            .collect();
        let verdicts: Vec<Value> = self.verdicts.iter().map(Verdict::as_json).collect();
        let total = self.captures.len() as u64;
        json!({
            "roots": {
                "carveouts": {"scope": "project", "root": self.carveout_root},
                "questions": {"scope": "machine", "store": self.questions_store},
                "captures": {"scope": "machine", "files_read": self.capture_file_total},
                "lane": {"scope": "machine", "store": self.lane_store},
            },
            "carveouts": {
                "total": self.carveout_total,
                "by_kind": Value::Object(by_kind),
                "oldest_ts": self.carveout_oldest_ts,
            },
            "questions": questions,
            "captures": {
                "total": total,
                "resolved_files": self.capture_file_total,
                "parsed_open_rows": self.capture_row_total,
                "repeated_ids": self.capture_row_total.saturating_sub(total),
                "by_project": Value::Object(by_project),
                "items": captures,
            },
            "lane": {
                "total": self.lane.len(),
                "parked": self.lane_parked,
                "items": lane,
            },
            "verdicts": {
                "total": self.verdicts.len(),
                "error": self.verdicts_error,
                "items": verdicts,
            },
        })
    }
}

/// Collect every stream. The skeleton reads nothing yet.
pub fn collect() -> Result<Outstanding, String> {
    Ok(Outstanding::default())
}

/// The human block. Empty when nothing is outstanding.
pub fn render(o: &Outstanding) -> String {
    if o.empty() {
        return String::new();
    }
    "## Outstanding for you\n".to_string()
}

/// `fno-agents outstanding [--json|-J]`.
pub fn run_outstanding(args: &[String]) -> i32 {
    let mut as_json = false;
    for a in args {
        match a.as_str() {
            "--json" | "-J" => as_json = true,
            "-h" | "--help" => {
                println!("usage: fno inbox outstanding [--json|-J]");
                return 0;
            }
            other => {
                eprintln!("outstanding: unknown argument {other:?}");
                return 2;
            }
        }
    }
    let o = match collect() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("outstanding: failed to read: {e}");
            return 1;
        }
    };
    let mut out = std::io::stdout().lock();
    if as_json {
        let _ = writeln!(out, "{}", o.as_json());
    } else {
        let _ = write!(out, "{}", render(&o));
    }
    0
}
