//! The Rust question intake behind `fno inbox outstanding ask`.
//!
//! Transport-only `question-intake` arm: one JSON request on stdin (what the
//! Python shim resolved: text, flags, identity, live law rows, storage root),
//! one JSON answer on stdout, and the exit code carries the verdict. It
//! registers no client command - the shrink law (d-fe66560a) allows no new
//! action - and callers reach it through `command_call`, like the patch door.
//!
//! Owns: the law refusal (`law_match`), the context parse
//! (`escalation::parse` with the question-file sections), the node-pointer
//! rule (one line plus a node pointer, law d-59af3235), the dedup on
//! subject plus node, the 2000-character cut, the id mint, the dual write
//! (project journal fatal, index best-effort) and the render-position
//! receipt. The exit CODE lives in the answer (the transport always exits 0
//! once the request parsed, the question-intake shape); refusal MESSAGES stay
//! in the shim, which owns the user's name.

use crate::decision_trace::{actor_kind_in, emit_span_to, new_span_id, Trace};
use crate::paths::AgentsHome;
use crate::provider_cap::questions_path;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};

/// `cli/src/fno/events/__init__.py: QUESTION_CAP`.
pub const QUESTION_CAP: usize = 2000;

/// The filled question file the template arm prints: the gate's own shape,
/// sourced from the file that enforces it so neither can drift.
pub const QUESTION_TEMPLATE: &str = "\
---
recommend: 1
---

<the question, one line>

## Options
1. <first choice>.
    What happens next: <what starts the moment this is chosen>
2. <second choice>.
    What happens next: <what starts the moment this is chosen>

## Blocked because
<what stays stuck until this is answered>

## Why these options
<the reading that produced exactly these options>

## Downside
<the cost of the option you recommend>

## Recommendation
Option 1, <the reason in one line>.

## Not thought through
<the part you did not check>

## Reversible
costly

## Cost if wrong
<what breaks if the recommendation is wrong>

## Meanwhile
<what keeps moving while the question waits>
";

#[derive(Deserialize)]
pub struct IntakeRequest {
    pub question: String,
    /// `--template`: print the filled question file, record nothing.
    #[serde(default)]
    pub template: bool,
    #[serde(default)]
    pub ask: Option<String>,
    #[serde(default)]
    pub options: Vec<String>,
    #[serde(default)]
    pub blocks: Vec<String>,
    #[serde(default)]
    pub node: Option<String>,
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub asker: Option<String>,
    #[serde(default)]
    pub laws: Vec<crate::law_match::LawRow>,
    pub storage_root: PathBuf,
    /// The machine index, resolved by the Python side (`paths.questions_jsonl()`)
    /// so the caller's sandbox stays the single path authority. Falls back to
    /// `questions_path(home)` when absent.
    #[serde(default)]
    pub index_path: Option<PathBuf>,
    /// The project journal (`paths.project_log("events.jsonl")`), same
    /// authority rule as `index_path`. Falls back to the legacy
    /// `<storage_root>/.fno/events.jsonl`.
    #[serde(default)]
    pub journal_path: Option<PathBuf>,
    /// The user's stated name, interpolated into the law-refusal line.
    #[serde(default)]
    pub display_name: Option<String>,
    /// `fno inbox outstanding`'s render cap, for the does-not-render line.
    #[serde(default)]
    pub render_cap: Option<usize>,
    /// `append`: the machine-writer door (the lead wake, the branch hook).
    /// `event` is a caller-built `operator_question` envelope; it is
    /// validated and written to both sinks, and no ask gate runs. The wake
    /// and hook lanes ask questions the ask gate would refuse (machine
    /// markers, option lists with no node pointer), and their rows were
    /// always raw appends.
    #[serde(default)]
    pub op: Option<String>,
    #[serde(default)]
    pub event: Option<Value>,
    /// `--image`: screenshots the page embeds under `## Screens`. Each is
    /// copied into `screens_dir` before anything is recorded.
    #[serde(default)]
    pub images: Vec<PathBuf>,
    /// Where the copies land: `<questions dir>/screens`, so the vault holds
    /// them next to the page. Required when `images` is not empty.
    #[serde(default)]
    pub screens_dir: Option<PathBuf>,
}

#[derive(Serialize)]
pub struct IntakeAnswer {
    pub exit_code: i32,
    /// The stderr lines, in print order: the shim echoes them verbatim.
    pub lines: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qid: Option<String>,
    /// True only on a template answer; the shim reads it to refuse a binary
    /// that predates the template arm.
    #[serde(default)]
    pub template: bool,
    /// `law` | `node_pointer` | `context` | `decide_yourself` | `dedup` | `write` | `index`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refusal: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_id: Option<String>,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<usize>,
}

impl IntakeAnswer {
    fn exit(code: i32) -> Self {
        IntakeAnswer {
            exit_code: code,
            lines: Vec::new(),
            qid: None,
            template: false,
            refusal: None,
            open_id: None,
            truncated: false,
            position: None,
            total: None,
        }
    }
}

/// The binary transport: request on stdin, answer on stdout. Exits 0
/// whenever an answer was computed; the verdict rides `exit_code` in it.
pub fn run_question_intake() -> i32 {
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        eprintln!("fno-agents question-intake: could not read stdin");
        return 2;
    }
    let req: IntakeRequest = match serde_json::from_str(&input) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("fno-agents question-intake: bad request: {e}");
            return 2;
        }
    };
    let home = AgentsHome::from_env();
    let answer = run_intake(&req, &home);
    match serde_json::to_string(&answer) {
        Ok(s) => println!("{s}"),
        Err(e) => {
            eprintln!("fno-agents question-intake: could not serialize answer: {e}");
            return 1;
        }
    }
    0
}

fn str_field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get("data")
        .and_then(|d| d.get(key))
        .or_else(|| v.get(key))
        .and_then(Value::as_str)
}

/// One open question seen in the index, for dedup and the receipt.
#[derive(Debug)]
struct OpenRow {
    id: String,
    ts: String,
    blocks: usize,
    subject: Option<String>,
    node: Option<String>,
}

/// Latest ask per id minus closes, folded from the machine index. A
/// malformed line is skipped; the receipt is advisory and dedup is best
/// effort against the rows that do parse.
pub fn question_rows(index: &Path) -> (Map<String, Value>, HashSet<String>) {
    // Store rows first: Python commits closes to questions.db without
    // touching the raw journal, so a raw read marks closed questions open.
    let raw =
        crate::event_store::journal_text(index, &["operator_question", "operator_question_closed"]);
    let mut asked: Map<String, Value> = Map::new();
    let mut closed = HashSet::new();
    for line in raw.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let qid = match str_field(&v, "question_id") {
            Some(q) => q.to_string(),
            None => continue,
        };
        match v.get("type").and_then(Value::as_str) {
            Some("operator_question") => {
                asked.insert(qid, v);
            }
            Some("operator_question_closed") => {
                closed.insert(qid);
            }
            _ => {}
        }
    }
    (asked, closed)
}

fn open_rows(index: &Path) -> Vec<OpenRow> {
    let (asked, closed) = question_rows(index);
    let mut out: Vec<OpenRow> = asked
        .into_iter()
        .filter(|(id, _)| !closed.contains(id))
        .map(|(id, v)| OpenRow {
            blocks: v
                .get("data")
                .and_then(|d| d.get("blocks"))
                .or_else(|| v.get("blocks"))
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(0),
            ts: v
                .get("ts")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            subject: str_field(&v, "subject").map(str::to_string),
            node: str_field(&v, "node").map(str::to_string),
            id,
        })
        .collect();
    // Newest first, then more blocks, then id (mirrors read_open_questions
    // with a zero liveness budget: one lane, no probes).
    out.sort_by(|a, b| {
        b.ts.cmp(&a.ts)
            .then_with(|| b.blocks.cmp(&a.blocks))
            .then_with(|| b.id.cmp(&a.id))
    });
    out
}

/// True when the text carries any question-file section (so the title line
/// becomes the question), per the template in docs/architecture/
/// attention-items.md.
fn is_question_file(text: &str) -> bool {
    const SECTIONS: [&str; 9] = [
        "## Options",
        "## Recommendation",
        "## Blocked because",
        "## Why these options",
        "## Downside",
        "## Not thought through",
        "## Reversible",
        "## Cost if wrong",
        "## Meanwhile",
    ];
    SECTIONS.iter().any(|s| text.contains(s))
}

/// The exact word the Reversible section must hold: one of the three,
/// nothing else. Extra text under the heading reads as missing.
fn is_reversible_word(word: &str) -> bool {
    matches!(word, "yes" | "costly" | "no")
}

/// A question that would clear a visual-approval hold: the gate's own
/// subject (`visual approval PR <n> at <head>`), or a title that names a PR
/// and the visual hold, the shape a lead writes by hand.
fn asks_visual_approval(subject: Option<&str>, title: &str) -> bool {
    if subject.is_some_and(|s| s.trim().to_ascii_lowercase().starts_with("visual approval")) {
        return true;
    }
    let t = title.to_ascii_lowercase();
    let visual = ["visual hold", "visual approval", "visual-approval"]
        .iter()
        .any(|w| t.contains(w));
    visual && names_a_pr(&t)
}

/// A word-start `pr`, an optional `#`/space run, then a digit.
fn names_a_pr(lower: &str) -> bool {
    lower.match_indices("pr").any(|(i, _)| {
        let start = i == 0 || !lower.as_bytes()[i - 1].is_ascii_alphanumeric();
        let rest = lower[i + 2..].trim_start_matches(['#', ' ']);
        start && rest.starts_with(|c: char| c.is_ascii_digit())
    })
}

/// An image the viewer and Obsidian both show: a png, jpg, gif or webp
/// extension, and a regular file whose first bytes carry that format's
/// signature. A renamed text file would satisfy the screens refusal and
/// still show nothing.
fn is_image_file(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    if !matches!(
        ext.as_deref(),
        Some("png" | "jpg" | "jpeg" | "gif" | "webp")
    ) || !path.is_file()
    {
        return false;
    }
    let mut head = [0u8; 12];
    let n = std::fs::File::open(path)
        .and_then(|mut f| f.read(&mut head))
        .unwrap_or(0);
    let head = &head[..n];
    head.starts_with(b"\x89PNG\r\n\x1a\n")
        || head.starts_with(b"\xff\xd8\xff")
        || head.starts_with(b"GIF8")
        || (head.starts_with(b"RIFF") && head.get(8..12) == Some(b"WEBP"))
}

/// Copy each screenshot to `<dir>/<qid>-<n>-<name>`, so the vault holds it
/// next to the page and Obsidian's `![[name]]` resolves it by its unique
/// name. Returns the copies' absolute paths.
fn copy_screens(images: &[PathBuf], dir: Option<&Path>, qid: &str) -> Result<Vec<String>, String> {
    let dir = dir.ok_or("no screens directory was resolved")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut out = Vec::new();
    for (i, src) in images.iter().enumerate() {
        let name = src
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let dest = dir.join(format!("{qid}-{}-{name}", i + 1));
        std::fs::copy(src, &dest).map_err(|e| format!("{}: {e}", src.display()))?;
        let abs = std::fs::canonicalize(&dest).unwrap_or(dest);
        out.push(abs.to_string_lossy().into_owned());
    }
    Ok(out)
}

fn title_of(text: &str, parsed_title: &str) -> String {
    if !parsed_title.trim().is_empty() {
        return parsed_title.trim().to_string();
    }
    let mut lines = text.lines().map(str::trim).peekable();
    if lines.peek() == Some(&"---") {
        // Skip a leading frontmatter block; the title is the first prose line.
        lines.next();
        for line in lines.by_ref() {
            if line == "---" {
                break;
            }
        }
    }
    lines
        .find(|l| !l.is_empty() && !l.starts_with("---"))
        .unwrap_or("")
        .to_string()
}

/// `q-` + 8 hex from the OS entropy pool; a time+pid hash when it cannot be
/// read, so a mint never fails the ask.
fn mint_id() -> String {
    let mut bytes = [0u8; 4];
    let ok = std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .is_ok();
    if !ok {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let mix = nanos as u64 ^ ((std::process::id() as u64) << 32);
        bytes.copy_from_slice(&mix.to_le_bytes()[..4]);
    }
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("q-{hex}")
}

/// True when the question carries retract intent and names one of `ids`.
/// The id citation is what keeps the exemption narrow - "revoke the
/// credentials" alone never qualifies - and mirrors the nearby tier's own
/// remedy ("name every id above and ask again").
fn cites_retraction(question: &str, ids: &[String]) -> bool {
    let lower = question.to_lowercase();
    let intent = ["retract", "repeal", "rescind", "revoke", "overturn"]
        .iter()
        .any(|w| lower.contains(w));
    intent && ids.iter().any(|id| lower.contains(&id.to_lowercase()))
}

/// True when the question's closing action is the retraction of a live law
/// whose retraction only the operator may record. The lane verdict is the
/// retract gate's own rule (`_decision_lane`), consumed as data.
fn targets_operator_retraction(question: &str, laws: &[crate::law_match::LawRow]) -> bool {
    laws.iter().any(|l| {
        l.retraction_needs_operator()
            && cites_retraction(question, std::slice::from_ref(&l.decision_id))
    })
}

/// The machine-writer append door: validate one caller-built
/// `operator_question` envelope, then the dual write - project journal
/// fatal, machine index reported. The id must ride the envelope: a writer
/// that cannot name its row has a bug, and minting here would hide it.
fn run_append(req: &IntakeRequest) -> IntakeAnswer {
    let mut answer = IntakeAnswer::exit(0);
    let Some(event) = req.event.as_ref().filter(|e| e.is_object()) else {
        answer.lines.push(
            "outstanding: failed to record question: the append door needs an event envelope"
                .to_string(),
        );
        answer.refusal = Some("write".to_string());
        answer.exit_code = 2;
        return answer;
    };
    let Some(qid) = event
        .get("data")
        .and_then(|d| d.get("question_id"))
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
    else {
        answer.lines.push(
            "outstanding: failed to record question: question event has no question_id".to_string(),
        );
        answer.refusal = Some("write".to_string());
        answer.exit_code = 2;
        return answer;
    };
    let qid = qid.to_string();
    let journal_path = req
        .journal_path
        .clone()
        .unwrap_or_else(|| req.storage_root.join(".fno").join("events.jsonl"));
    let index = req
        .index_path
        .clone()
        .unwrap_or_else(|| questions_path(&AgentsHome::from_env()));
    let line = match serde_json::to_string(event) {
        Ok(l) => l,
        Err(e) => {
            answer.lines.push(format!(
                "outstanding: failed to record question: could not serialize the envelope: {e}"
            ));
            answer.refusal = Some("write".to_string());
            answer.exit_code = 1;
            return answer;
        }
    };
    if let Err(e) = crate::event_store::append_envelope(&journal_path, &line, None) {
        answer.lines.push(format!(
            "outstanding: failed to append question to project journal: {e}"
        ));
        answer.refusal = Some("write".to_string());
        answer.exit_code = 1;
        return answer;
    }
    if let Err(e) = crate::provider_cap::append_questions_row(&index, event) {
        answer.lines.push(format!(
            "outstanding: recorded {qid} in the project journal, but the recall index \
write failed: {e}. Run `fno inbox outstanding reindex`."
        ));
        answer.qid = Some(qid);
        answer.exit_code = 1;
        return answer;
    }
    answer.qid = Some(qid);
    answer
}

pub fn run_intake(req: &IntakeRequest, home: &AgentsHome) -> IntakeAnswer {
    if req.op.as_deref() == Some("append") {
        return run_append(req);
    }
    if req.template {
        let mut answer = IntakeAnswer::exit(0);
        answer.lines = vec![QUESTION_TEMPLATE.to_string()];
        answer.template = true;
        return answer;
    }
    // The user's name for the refusal line, and the render cap for the
    // receipt; both absent read as the plain shapes.
    let who = req.display_name.as_deref().unwrap_or("the user");
    let render_cap = req.render_cap.unwrap_or(10);

    let truncated = req.question.chars().count() > QUESTION_CAP;
    let mut answer = IntakeAnswer::exit(0);
    if truncated {
        answer.truncated = true;
        answer.lines.push(format!(
            "outstanding: recorded truncated: the question is {} characters, the event stores {QUESTION_CAP}.",
            req.question.chars().count()
        ));
    }

    // The law refusal first: a live law on this subject means the question
    // is never recorded (fail-closed, d-0fa92eb9) - unless the question's
    // closing action is the retraction of that law. A law-lane row refuses
    // every non-operator retraction (`retract_decision`), so "act on the
    // law; do not ask" has no agent-side remedy there and the user is the
    // only door. The closing action may ride the pin line (`--ask`), so it
    // scans with the question, not after it.
    let closing_text = match req.ask.as_deref() {
        Some(ask) if !ask.trim().is_empty() => format!("{}\n{}", req.question, ask),
        _ => req.question.clone(),
    };
    let retraction_ask = targets_operator_retraction(&closing_text, &req.laws);
    let law_verdict = crate::law_match::ask_answer(&crate::law_match::AskRequest {
        question: req.question.clone(),
        subject: req.subject.clone(),
        node: req.node.clone(),
        laws: req.laws.clone(),
    });
    if (!law_verdict.exact.is_empty() && !retraction_ask) || law_verdict.nearby_refusal.is_some() {
        for hit in &law_verdict.exact {
            let mut line = format!(
                "outstanding: refused: live law already rules on '{}' ({}). Read it: \
fno inbox decisions {} --lane law --state live. Act on the law; do not ask {who}.",
                hit.subject,
                hit.ids.join(", "),
                hit.subject
            );
            if req.subject.is_none() {
                line += " If the question is about another subject, name it with --subject.";
            }
            line += " If the question asks to retract this law, name its id in the \
question and ask again: only the user may record that retraction.";
            answer.lines.push(line);
        }
        if let Some(refusal) = law_verdict.nearby_refusal {
            answer.lines.push(refusal);
        }
        answer.refusal = Some("law".to_string());
        answer.exit_code = 2;
        return answer;
    }

    let parsed = crate::escalation::parse(&req.question);
    // A mistyped class would land as an unroutable hop, so it refuses the
    // way the reversible field does: exit 2, the six values named, nothing
    // written.
    let asked_class = parsed.class.trim();
    if !asked_class.is_empty() && !crate::decision_trace::DECISION_CLASSES.contains(&asked_class) {
        // Quote the class: line as read; the recurring wrong value is
        // 'decision', and a bare "unknown class" sent askers hunting
        // for a list instead of fixing the one word.
        let read_line = req
            .question
            .lines()
            .find(|l| l.split_once(':').is_some_and(|(k, _)| k.trim() == "class"))
            .unwrap_or("class:");
        answer.lines.push(format!(
            "outstanding: refused: read '{read_line}'. class must be one of six \
words: {}. 'decision' is not a class.",
            crate::decision_trace::DECISION_CLASSES.join(", ")
        ));
        answer.refusal = Some("class".to_string());
        answer.exit_code = 2;
        return answer;
    }
    let from_file = is_question_file(&req.question);
    // File options carry a per-option next; flag options stay bare strings.
    let file_options = !parsed.options.is_empty();
    let has_options = file_options || !req.options.is_empty();

    // Node-pointer rule: a question with choices names its node.
    let node = req.node.as_deref().map(str::trim).filter(|n| !n.is_empty());
    if has_options && node.is_none() {
        answer.lines.push(
            "outstanding: refused: a question with options names its node (--node). \
The ask line stays one line plus the pointer (law d-59af3235); a question \
file carries the context sections and is not a violation, so pass the file \
with --question-file and --node together."
                .to_string(),
        );
        answer.refusal = Some("node_pointer".to_string());
        answer.exit_code = 2;
        return answer;
    }

    // The context refusal (user design 2026-09-22): the page writer cannot
    // invent context, so a question carries what, why, two options and a
    // recommendation with its reason. One action with no choice is a pin
    // and passes.
    let is_pin = !has_options && req.ask.as_deref().is_some_and(|a| !a.trim().is_empty());
    if !is_pin {
        let mut missing: Vec<&str> = Vec::new();
        if title_of(&req.question, &parsed.title).trim().is_empty() {
            missing.push("what");
        }
        if parsed.blocked_because.trim().is_empty() {
            missing.push("why");
        }
        if parsed.options.len() < 2 && req.options.len() < 2 {
            missing.push("two options");
        }
        let recommendation_ok = parsed
            .recommend
            .is_some_and(|r| (1..=parsed.options.len()).contains(&r))
            && !parsed.recommendation.trim().is_empty();
        if !recommendation_ok {
            missing.push("a recommendation");
        }
        // The readiness mirror: the mux renders only ready items
        // (attention::problems), so every section it reads is required here.
        // A question admitted half-checked lands exit 0 and the user never
        // sees it, which is the failure this door exists to prevent.
        if parsed.options_rationale.trim().is_empty() {
            missing.push("## Why these options");
        }
        if file_options {
            if parsed.options.iter().any(|o| o.next.trim().is_empty()) {
                missing.push("What happens next under every option");
            }
        } else if has_options {
            missing.push("a question file (flag options carry no What happens next)");
        }
        if parsed.unknowns.trim().is_empty() {
            missing.push("## Not thought through");
        }
        if !is_reversible_word(parsed.reversible.trim()) {
            missing.push("## Reversible (yes, costly, or no)");
        }
        if parsed.reversible.trim() == "costly" && parsed.cost_if_wrong.trim().is_empty() {
            missing.push("## Cost if wrong");
        }
        if parsed.meanwhile.trim().is_empty() {
            missing.push("## Meanwhile");
        }
        if !missing.is_empty() {
            answer.lines.push(format!(
                "outstanding: refused: a question needs {}. Write a question file \
(docs/architecture/attention-items.md, \"Asking with context\") and pass \
--question-file. One action with no choice is a pin: pass --ask \"<the action>\".",
                missing.join(", ")
            ));
            // The quote lines: where a value was present to read, name it
            // and the shape wanted, so one pass fixes the file.
            let reversible_read = parsed.reversible.trim();
            if !reversible_read.is_empty() && !is_reversible_word(reversible_read) {
                answer.lines.push(format!(
                    "outstanding: the Reversible section reads '{reversible_read}'; \
it must hold exactly one word: yes, costly, or no. Any extra text under the \
heading reads as missing."
                ));
            }
            for (i, o) in parsed.options.iter().enumerate() {
                if o.next.trim().is_empty() {
                    answer.lines.push(format!(
                        "outstanding: option {} '{}' carries no 'What happens next:' \
line; each option needs an indented 'What happens next: <the action this \
choice starts>'.",
                        i + 1,
                        o.text
                    ));
                }
            }
            answer.refusal = Some("context".to_string());
            answer.exit_code = 2;
            return answer;
        }
        // The asker-must-decide refusal (user ruling 2026-09-22): a
        // reversible question with a recommendation is one the asker or its
        // lead settles itself; it reaches the user only with a user-only
        // reason in why_user. A question whose closing action is an
        // operator-only retraction is not the asker's to settle, whatever
        // the file says: the same authority rule decides here.
        if parsed.reversible.trim().eq_ignore_ascii_case("yes") {
            let user_only = crate::escalation::why_user_is_user_only(&parsed.why_user);
            if !user_only && !retraction_ask {
                let decide = match node {
                    Some(n) => format!("fno backlog decide {n} \"<ruling>\""),
                    None => "fno backlog decide <node> \"<ruling>\"".to_string(),
                };
                answer.lines.push(format!(
                    "outstanding: refused: reversible with a recommendation is yours \
to rule. Decide door first: {decide} (--authority agent) - record the \
ruling, then continue. \
The user answers only what only a user can: irreversible, spends money or a \
credential, reaches outside the machine, or a product or taste call - name it \
with why_user: in the question file."
                ));
                answer.refusal = Some("decide_yourself".to_string());
                answer.exit_code = 2;
                return answer;
            }
        }
    }

    // The screens refusal: the user cannot judge paint they cannot see, so
    // a question that clears a visual-approval hold carries its shots.
    if let Some(bad) = req.images.iter().find(|p| !is_image_file(p)) {
        answer.lines.push(format!(
            "outstanding: refused: --image {} is not a readable png, jpg, gif or webp file.",
            bad.display()
        ));
        answer.refusal = Some("image".to_string());
        answer.exit_code = 2;
        return answer;
    }
    if req.images.is_empty()
        && asks_visual_approval(
            req.subject.as_deref(),
            &title_of(&req.question, &parsed.title),
        )
    {
        answer.lines.push(
            "outstanding: refused: a visual-approval question carries the screens the user \
judges. Pass each screenshot with --image <path>; the page embeds them under ## Screens."
                .to_string(),
        );
        answer.refusal = Some("screens".to_string());
        answer.exit_code = 2;
        return answer;
    }

    // Dedup: an open question on the same subject and node already waits.
    let subject = req
        .subject
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let index = req
        .index_path
        .clone()
        .unwrap_or_else(|| questions_path(home));
    // One read serves the dedup and the supersede after the write: the
    // open rows a re-ask closes are the ones this read saw.
    let open = open_rows(&index);
    if let (Some(s), Some(n)) = (subject, node) {
        let dup = open.iter().find(|row| {
            row.subject.as_deref().map(str::trim) == Some(s) && row.node.as_deref() == Some(n)
        });
        if let Some(row) = dup {
            answer.lines.push(format!(
                "outstanding: refused: an open question on subject '{s}' and node '{n}' \
already waits ({}). Answer it or clear it; do not ask twice.",
                row.id
            ));
            answer.refusal = Some("dedup".to_string());
            answer.open_id = Some(row.id.clone());
            answer.exit_code = 2;
            return answer;
        }
    }

    let stored_question: String = if from_file {
        title_of(&req.question, &parsed.title)
    } else if truncated {
        req.question.chars().take(QUESTION_CAP).collect()
    } else {
        req.question.clone()
    };

    // The escalate route: an `answers_ask:` pointer turns the intake into the
    // lead's route span, so the question links the hop it answers. Same
    // journal as the question row.
    let journal_path = req
        .journal_path
        .clone()
        .unwrap_or_else(|| req.storage_root.join(".fno").join("events.jsonl"));
    let mut route_span_id: Option<String> = None;
    let mut pending_route: Option<(Trace, Map<String, Value>)> = None;
    if let Some(ask_ref) = parsed
        .answers_ask
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        // A span id parents verbatim; any other reference names the ask's
        // bus message, whose endpoints re-derive the ask span. An
        // unresolvable pointer still routes: the lead's escalation is the
        // fact, only parentless.
        let ask_span = crate::decision_trace::resolve_ask_span(&journal_path, ask_ref);
        if ask_span.is_none() {
            eprintln!(
                "outstanding: answers_ask '{ask_ref}' names no ask span; the route records unparented"
            );
        }
        let trace = Trace {
            trace_id: node.unwrap_or("none").to_string(),
            span_id: new_span_id(),
            parent_span_id: ask_span,
            actor_session: req.session_id.clone(),
            actor_kind: actor_kind_in(home, req.session_id.as_deref(), "question"),
            comms: "question",
            recipient_session: None,
            recipient_kind: None,
        };
        let mut attrs = Map::new();
        attrs.insert("route".to_string(), json!("escalate"));
        if !asked_class.is_empty() {
            attrs.insert("class".to_string(), json!(asked_class));
        }
        if let Some(rec) = parsed.recommend {
            attrs.insert("recommendation".to_string(), json!(rec.to_string()));
        }
        pending_route = Some((trace, attrs));
    }
    // The route lands BEFORE the question row: the trace query reads in ts
    // order, and a parent must not postdate its child. A failed route emit
    // leaves the question parentless rather than dangling; a failed question
    // write below leaves the route orphaned, reported on stderr.
    let mut route_recorded = false;
    if let Some((trace, attrs)) = pending_route.take() {
        match emit_span_to(&journal_path, "route", &trace, &attrs) {
            Ok(()) => {
                route_span_id = Some(trace.span_id.clone());
                route_recorded = true;
            }
            Err(e) => eprintln!("outstanding: route span skipped: {e}"),
        }
    }

    let mut data = Map::new();
    let qid = mint_id();
    data.insert("question_id".into(), json!(&qid));
    data.insert("question".into(), json!(stored_question));
    for (key, value) in [
        ("session_id", &req.session_id),
        ("cwd", &req.cwd),
        ("asker", &req.asker),
        ("ask", &req.ask),
    ] {
        if let Some(v) = value.as_deref().filter(|v| !v.is_empty()) {
            data.insert(key.into(), json!(v));
        }
    }
    if let Some(n) = node {
        data.insert("node".into(), json!(n));
    }
    if !req.blocks.is_empty() {
        data.insert("blocks".into(), json!(req.blocks));
    }
    if !req.images.is_empty() {
        match copy_screens(&req.images, req.screens_dir.as_deref(), &qid) {
            Ok(copies) => {
                data.insert("images".into(), json!(copies));
            }
            Err(e) => {
                answer.lines.push(format!(
                    "outstanding: failed to record question: the screens could not be copied: {e}"
                ));
                answer.refusal = Some("write".to_string());
                answer.exit_code = 1;
                return answer;
            }
        }
    }
    if has_options {
        if file_options {
            let options: Vec<Value> = parsed
                .options
                .iter()
                .enumerate()
                .map(|(i, o)| {
                    let mut m = Map::new();
                    m.insert("n".into(), json!(i + 1));
                    m.insert("text".into(), json!(o.text));
                    if !o.next.is_empty() {
                        m.insert("next".into(), json!(o.next));
                    }
                    Value::Object(m)
                })
                .collect();
            data.insert("options".into(), Value::Array(options));
        } else {
            data.insert("options".into(), json!(req.options));
        }
    }
    if let Some(subject) = subject {
        data.insert("subject".into(), json!(subject));
    }
    let why_user = parsed.why_user.trim();
    if !why_user.is_empty() {
        // The sweep reads this to spare a user-only question from its node's
        // closing.
        data.insert("why_user".into(), json!(why_user));
    }

    // The structured context: every non-empty field the projection reads.
    let mut context = Map::new();
    for (key, value) in [
        ("blocked_because", &parsed.blocked_because),
        ("options_rationale", &parsed.options_rationale),
        ("unknowns", &parsed.unknowns),
        ("reversible", &parsed.reversible),
        ("cost_if_wrong", &parsed.cost_if_wrong),
        ("meanwhile", &parsed.meanwhile),
    ] {
        if !value.trim().is_empty() {
            context.insert(key.into(), json!(value.trim()));
        }
    }
    if let Some(rec) = parsed.recommend {
        if rec >= 1 && rec <= parsed.options.len() {
            let mut recommendation = Map::new();
            recommendation.insert("option".into(), json!(rec));
            recommendation.insert("why".into(), json!(parsed.recommendation.trim()));
            if !parsed.downside.trim().is_empty() {
                recommendation.insert("downside".into(), json!(parsed.downside.trim()));
            }
            context.insert("recommendation".into(), Value::Object(recommendation));
        }
    }
    if !context.is_empty() {
        data.insert("context".into(), Value::Object(context));
    }

    // One envelope, two sinks: the journal and the recall index must carry
    // the identical line, so the ts is stamped once. The journal is the
    // durable half and its failure is fatal. The journal path travels in
    // the request (the Python sandbox stays the single path authority, as
    // with index_path); the legacy .fno path is the fallback.
    // The question span: the question row IS the span (span_id = question_id),
    // parented at the route span an answers_ask pointer produced.
    data.insert(
        "trace".into(),
        serde_json::to_value(&Trace {
            trace_id: node.unwrap_or("none").to_string(),
            span_id: qid.clone(),
            parent_span_id: route_span_id,
            actor_session: req.session_id.clone(),
            actor_kind: actor_kind_in(home, req.session_id.as_deref(), "question"),
            comms: "question",
            recipient_session: None,
            recipient_kind: Some("user"),
        })
        .unwrap_or(Value::Null),
    );
    let event = json!({
        "ts": crate::events::now_rfc3339(),
        "type": "operator_question",
        "source": "target",
        "data": Value::Object(data),
    });
    let line = match serde_json::to_string(&event) {
        Ok(l) => l,
        Err(e) => {
            answer.lines.push(format!(
                "outstanding: failed to record question: could not serialize the envelope: {e}"
            ));
            answer.refusal = Some("write".to_string());
            answer.exit_code = 1;
            return answer;
        }
    };
    if let Err(e) = crate::event_store::append_envelope(&journal_path, &line, None) {
        if route_recorded {
            eprintln!("outstanding: the route span landed alone; the question row did not");
        }
        answer.lines.push(format!(
            "outstanding: failed to record question: failed to append question to project journal: {e}"
        ));
        answer.refusal = Some("write".to_string());
        answer.exit_code = 1;
        return answer;
    }

    // Machine-wide recall index: best-effort, reported.
    let index_error = crate::provider_cap::append_questions_row(&index, &event).err();
    if let Some(e) = index_error {
        answer.lines.push(format!(
            "outstanding: recorded {qid} in the project journal, but the recall index \
write failed: {e}. Run `fno inbox outstanding reindex`; do not retry ask, which \
would mint a second id for the same question."
        ));
        answer.qid = Some(qid);
        answer.exit_code = 1;
        return answer;
    }

    answer.lines.push(format!(
        "outstanding: recorded {qid}. The user answers it on the question board, \
and the answer closes it. To withdraw it yourself: fno inbox outstanding clear {qid}"
    ));

    // A re-ask at a new head closes the older head's open question on the
    // same node, whoever asked: the older head can no longer be approved
    // or refused, so one open question per head family stays. Best-effort;
    // a close that fails here waits for the hourly sweep.
    if let (Some(s), Some(n)) = (subject, node) {
        if let Some((new_family, new_head)) = crate::question_sweep::head_scoped(s) {
            let olds: Vec<(&str, &str)> = open
                .iter()
                .filter_map(|row| {
                    if row.node.as_deref() != Some(n) {
                        return None;
                    }
                    let (family, old_head) =
                        crate::question_sweep::head_scoped(row.subject.as_deref()?.trim())?;
                    (family == new_family && !crate::question_sweep::same_head(old_head, new_head))
                        .then_some((row.id.as_str(), old_head))
                })
                .collect();
            if !olds.is_empty() {
                let pairs: Vec<(String, String)> = olds
                    .iter()
                    .map(|(id, _)| {
                        (
                            id.to_string(),
                            format!("superseded: {qid} re-asks at head {new_head}"),
                        )
                    })
                    .collect();
                let now = crate::claims::now_ms() / 1000;
                let (closed, errors) =
                    crate::question_sweep::close_questions(&index, &pairs, now, "question-intake");
                for (id, old_head) in &olds {
                    if closed.iter().any(|c| c == id) {
                        answer.lines.push(format!(
                            "outstanding: superseded {id} (same subject at older head \
{old_head}); one question stays open on this subject."
                        ));
                    } else {
                        let error = errors
                            .iter()
                            .find(|e| e.contains(id))
                            .map(String::as_str)
                            .unwrap_or("index write failed");
                        answer.lines.push(format!(
                            "outstanding: {id} could not be superseded ({error}); the next \
sweep closes it."
                        ));
                    }
                }
            }
        }
    }

    let (position, total) = receipt_position(&index, &qid);
    let total = total.unwrap_or(0);
    answer.position = position;
    answer.total = Some(total);
    match position {
        None => answer.lines.push(
            "outstanding: recorded, but its render position could not be read; \
run fno inbox outstanding to check."
                .to_string(),
        ),
        Some(p) if p <= render_cap => {
            answer.lines.push(format!(
                "outstanding: {qid} renders at position {p} of {total}."
            ));
        }
        Some(p) => answer.lines.push(format!(
            "outstanding: {qid} does NOT render: position {p} of {total}, and \
fno inbox outstanding prints {render_cap}. Nothing will show it to the operator; \
raise it another way or answer it yourself."
        )),
    }
    answer.qid = Some(qid);
    answer
}

/// Where the new id lands in the render: (position 1-based, total). None
/// means the read failed and the receipt says so instead of guessing.
fn receipt_position(index: &Path, qid: &str) -> (Option<usize>, Option<usize>) {
    let rows = open_rows(index);
    let total = rows.len();
    let position = rows.iter().position(|r| r.id == qid).map(|i| i + 1);
    match position {
        Some(p) => (Some(p), Some(total)),
        None => (None, Some(total)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::law_match::LawRow;

    fn tmp_home(tag: &str) -> AgentsHome {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "fno-agents-qintake-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        // Mirror the prod shape (<home>/.fno/agents): questions_path reads
        // root.parent(), and a home rooted at p itself would land the index
        // in the shared temp dir.
        let home = AgentsHome::at(&p.join(".fno").join("agents"));
        home.ensure_root().unwrap();
        home
    }

    fn tmp_root(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "fno-agents-qintake-root-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn req(question: &str, root: &Path) -> IntakeRequest {
        IntakeRequest {
            question: question.to_string(),
            template: false,
            ask: None,
            options: vec![],
            blocks: vec![],
            node: None,
            subject: None,
            session_id: Some("s1".to_string()),
            cwd: Some("/repo/fno".to_string()),
            asker: Some("worker-1".to_string()),
            laws: vec![],
            storage_root: root.to_path_buf(),
            index_path: None,
            journal_path: None,
            display_name: None,
            render_cap: None,
            op: None,
            event: None,
            images: Vec::new(),
            screens_dir: None,
        }
    }

    fn journal_text(root: &Path) -> String {
        // Writes commit to the store beside the journal; the raw file is
        // never appended, so tests read committed rows.
        crate::events::committed_journal_text(&root.join(".fno/events.jsonl"))
    }

    const QUESTION_FILE: &str = "\
---
recommend: 1
---

Is a net-zero Python repair legal with no grant?

## Options
1. Yes, net zero or less needs no grant.
    What happens next: unblocks four fixes today
2. Stay strict.
    What happens next: every Python fix waits on a grant

## Blocked because
the reconcile fix and the merge_close fix are both Python edits

## Why these options
the three readings leads have acted on

## Downside
a repair can hide a feature

## Recommendation
Option 1, the narrowest door: every later fix needs it.

## Not thought through
whether a net-zero move between files counts

## Reversible
costly

## Cost if wrong
the push allowance drops to 0

## Meanwhile
stops
";

    #[test]
    fn ac7_hp_question_file_records_context_and_object_options() {
        let home = tmp_home("hp");
        let root = tmp_root("hp");
        let mut r = req(QUESTION_FILE, &root);
        r.node = Some("x-aaaa".to_string());
        let answer = run_intake(&r, &home);
        assert_eq!(answer.exit_code, 0, "lines: {:?}", answer.lines);
        let qid = answer.qid.clone().unwrap();
        assert!(qid.starts_with("q-"));
        assert_eq!(qid.len(), 10);

        let row: Value = journal_text(&root)
            .lines()
            .last()
            .map(|l| serde_json::from_str(l).unwrap())
            .unwrap();
        let data = row.get("data").unwrap();
        assert_eq!(
            data.get("question").and_then(Value::as_str),
            Some("Is a net-zero Python repair legal with no grant?")
        );
        let options = data.get("options").and_then(Value::as_array).unwrap();
        assert_eq!(options.len(), 2);
        assert_eq!(
            options[0].get("next").and_then(Value::as_str),
            Some("unblocks four fixes today")
        );
        let ctx = data.get("context").unwrap();
        for key in [
            "blocked_because",
            "options_rationale",
            "recommendation",
            "unknowns",
            "reversible",
            "cost_if_wrong",
            "meanwhile",
        ] {
            assert!(ctx.get(key).is_some(), "context missing {key}");
        }
        assert_eq!(
            ctx.get("recommendation")
                .and_then(|r| r.get("option"))
                .and_then(Value::as_u64),
            Some(1)
        );
        // The index row landed too.
        let index =
            crate::event_store::journal_text(&questions_path(&home), &["operator_question"]);
        assert!(index.contains(&qid));
        assert_eq!(answer.position, Some(1));
        assert_eq!(answer.total, Some(1));
    }

    #[test]
    fn a_visual_approval_ask_needs_screens_and_copies_them_beside_the_page() {
        let home = tmp_home("screens");
        let root = tmp_root("screens");
        let mut r = req(QUESTION_FILE, &root);
        r.node = Some("x-aaaa".to_string());
        r.subject = Some("visual approval PR 7 at 05ea5bcc".to_string());
        let refused = run_intake(&r, &home);
        assert_eq!(
            refused.refusal.as_deref(),
            Some("screens"),
            "{:?}",
            refused.lines
        );
        // A hand-written title naming the PR and the hold reads the same.
        let mut by_title = req(
            &QUESTION_FILE.replace(
                "Is a net-zero Python repair legal with no grant?",
                "Approve PR 4030 at f5068f08 past the visual hold?",
            ),
            &root,
        );
        by_title.node = Some("x-aaaa".to_string());
        assert_eq!(
            run_intake(&by_title, &home).refusal.as_deref(),
            Some("screens")
        );
        r.images = vec![root.join("missing.png")];
        assert_eq!(run_intake(&r, &home).refusal.as_deref(), Some("image"));
        assert!(journal_text(&root).is_empty());

        let shot = root.join("bell.png");
        std::fs::write(&shot, b"png").unwrap();
        r.images = vec![shot.clone()];
        assert_eq!(
            run_intake(&r, &home).refusal.as_deref(),
            Some("image"),
            "a renamed text file is no image"
        );
        let png = b"\x89PNG\r\n\x1a\n-pixels";
        std::fs::write(&shot, png).unwrap();
        r.screens_dir = Some(root.join("questions/screens"));
        let answer = run_intake(&r, &home);
        assert_eq!(answer.exit_code, 0, "{:?}", answer.lines);
        let qid = answer.qid.unwrap();
        let copy = root.join(format!("questions/screens/{qid}-1-bell.png"));
        assert_eq!(std::fs::read(&copy).unwrap(), png);
        let row: Value = serde_json::from_str(journal_text(&root).lines().last().unwrap()).unwrap();
        let images = row["data"]["images"].as_array().unwrap();
        assert_eq!(images.len(), 1);
        assert!(images[0]
            .as_str()
            .unwrap()
            .ends_with(&format!("{qid}-1-bell.png")));
    }

    #[test]
    fn ac7_err_file_options_without_node_is_refused_and_records_nothing() {
        let home = tmp_home("err");
        let root = tmp_root("err");
        let answer = run_intake(&req(QUESTION_FILE, &root), &home);
        assert_eq!(answer.exit_code, 2);
        assert_eq!(answer.refusal.as_deref(), Some("node_pointer"));
        assert!(journal_text(&root).is_empty());
    }

    #[test]
    fn rule10_reversible_recommended_question_refused_without_why_user() {
        let home = tmp_home("rule10");
        let root = tmp_root("rule10");
        let question = QUESTION_FILE.replace("## Reversible\ncostly", "## Reversible\nyes");
        let mut r = req(&question, &root);
        r.node = Some("x-aaaa".to_string());
        let answer = run_intake(&r, &home);
        assert_eq!(answer.exit_code, 2, "lines: {:?}", answer.lines);
        assert_eq!(answer.refusal.as_deref(), Some("decide_yourself"));
        assert!(
            answer
                .lines
                .iter()
                .any(|l| l.contains("fno backlog decide")),
            "the refusal names the decide door: {:?}",
            answer.lines
        );
        let line = answer
            .lines
            .iter()
            .find(|l| l.contains("fno backlog decide"))
            .unwrap();
        let door = line.find("fno backlog decide").unwrap();
        let tail = line.find("The user answers only").unwrap();
        assert!(door < tail, "the decide door comes first: {line}");
        assert!(journal_text(&root).is_empty());
        // An empty why_user reads the same as an absent one: not user-only.
        let question = QUESTION_FILE
            .replace("## Reversible\ncostly", "## Reversible\nyes")
            .replace(
                "## Meanwhile\nstops\n",
                "## Meanwhile\nstops\n\n## Why user\n\n",
            );
        let mut r2 = req(&question, &root);
        r2.node = Some("x-aaaa".to_string());
        let answer2 = run_intake(&r2, &home);
        assert_eq!(answer2.exit_code, 2);
        assert_eq!(answer2.refusal.as_deref(), Some("decide_yourself"));
    }

    #[test]
    fn a_question_missing_a_readiness_section_is_refused_and_named() {
        // The gate mirrors the mux's readiness read: each stripped section
        // is named in the refusal, nothing is recorded, no variant exits 0.
        let cases: [(&str, &str, &str, &str); 6] = [
            (
                "unknowns",
                "## Not thought through\nwhether a net-zero move between files counts\n",
                "",
                "## Not thought through",
            ),
            ("meanwhile", "## Meanwhile\nstops\n", "", "## Meanwhile"),
            (
                "why-these",
                "## Why these options\nthe three readings leads have acted on\n",
                "",
                "## Why these options",
            ),
            (
                "option-next",
                "    What happens next: unblocks four fixes today\n",
                "",
                "What happens next under every option",
            ),
            (
                "cost",
                "## Cost if wrong\nthe push allowance drops to 0\n",
                "",
                "## Cost if wrong",
            ),
            (
                "bad-reversible",
                "## Reversible\ncostly",
                "## Reversible\nmaybe",
                "## Reversible (yes, costly, or no)",
            ),
        ];
        for (tag, needle, hole, want) in cases {
            let home = tmp_home(tag);
            let root = tmp_root(tag);
            let question = QUESTION_FILE.replace(needle, hole);
            let mut r = req(&question, &root);
            r.node = Some("x-aaaa".to_string());
            let answer = run_intake(&r, &home);
            assert_eq!(answer.exit_code, 2, "{tag}: lines: {:?}", answer.lines);
            assert_eq!(answer.refusal.as_deref(), Some("context"), "{tag}");
            assert!(
                answer.lines.iter().any(|l| l.contains(want)),
                "{tag}: the refusal must name {want}: {:?}",
                answer.lines
            );
            assert!(journal_text(&root).is_empty(), "{tag}: nothing recorded");
        }
    }

    fn junk_law() -> crate::law_match::LawRow {
        crate::law_match::LawRow {
            decision_id: "d-junk0001".to_string(),
            subject: Some("junk-law".to_string()),
            decision: Some("a placeholder ruling".to_string()),
            ts: Some("2026-08-29T19:20:10Z".to_string()),
            lane: Some("law".to_string()),
        }
    }

    #[test]
    fn a_question_to_retract_a_law_is_accepted() {
        // The closing action is a retraction, and the retract gate refuses
        // every non-operator authority on a law-lane row: the ask is the
        // only door, so neither the law refusal nor decide_yourself fires.
        let home = tmp_home("retract-accept");
        let root = tmp_root("retract-accept");
        let question = QUESTION_FILE
            .replace("## Reversible\ncostly", "## Reversible\nyes")
            .replace(
                "Is a net-zero Python repair legal with no grant?",
                "Retract d-junk0001: the junk law ruling on junk-law?",
            );
        let mut r = req(&question, &root);
        r.subject = Some("junk-law".to_string());
        r.node = Some("x-aaaa".to_string());
        r.laws = vec![junk_law()];
        let answer = run_intake(&r, &home);
        assert_eq!(answer.exit_code, 0, "lines: {:?}", answer.lines);
        assert!(journal_text(&root).lines().count() == 1);
    }

    #[test]
    fn ac9_edge_second_ask_on_same_subject_and_node_names_the_open_id() {
        let home = tmp_home("dedup");
        let root = tmp_root("dedup");
        let mut first = req("first", &root);
        first.subject = Some("subject-s".to_string());
        first.node = Some("x-aaaa".to_string());
        first.ask = Some("finish the lane".to_string());
        assert_eq!(run_intake(&first, &home).exit_code, 0);

        let mut second = req("second", &root);
        second.subject = Some("subject-s".to_string());
        second.node = Some("x-aaaa".to_string());
        second.ask = Some("finish the lane".to_string());
        let answer = run_intake(&second, &home);
        assert_eq!(answer.exit_code, 2);
        assert_eq!(answer.refusal.as_deref(), Some("dedup"));
        assert!(answer
            .open_id
            .as_deref()
            .is_some_and(|id| id.starts_with("q-")));
        // Nothing new recorded.
        assert_eq!(journal_text(&root).lines().count(), 1);
    }

    #[test]
    fn a_re_ask_at_a_new_head_closes_the_older_open_question() {
        // One open question per head family per node: the second ask at a
        // new head succeeds AND closes the first ask with an empty-answer
        // superseded close row in the index; the same family on another
        // node closes nothing.
        let home = tmp_home("supersede");
        let root = tmp_root("supersede");
        let ask = |subject: &str, node: &str| {
            let mut r = req("does the paint look right?", &root);
            r.subject = Some(subject.to_string());
            r.node = Some(node.to_string());
            r.ask = Some("approve the paint at this head".to_string());
            run_intake(&r, &home)
        };
        let first = ask("visual approval PR 7 at 1111111a", "x-aaaa");
        assert_eq!(first.exit_code, 0, "lines: {:?}", first.lines);
        let q1 = first.qid.clone().unwrap();
        let second = ask("visual approval PR 7 at 2222222b", "x-aaaa");
        assert_eq!(second.exit_code, 0, "lines: {:?}", second.lines);
        assert!(
            second.lines.iter().any(|l| l.contains(&format!(
                "superseded {q1} (same subject at older head 1111111a)"
            ))),
            "lines: {:?}",
            second.lines
        );
        let index = crate::provider_cap::questions_path(&home);
        let closed_text = crate::event_store::journal_text(&index, &["operator_question_closed"]);
        let close_row: Value = closed_text
            .lines()
            .find(|l| l.contains(&q1))
            .map(|l| serde_json::from_str(l).unwrap())
            .unwrap();
        assert_eq!(
            close_row.pointer("/data/answer").and_then(Value::as_str),
            Some("")
        );
        assert!(close_row
            .pointer("/data/reason")
            .and_then(Value::as_str)
            .unwrap_or("")
            .starts_with("superseded:"));
        // The third ask joins the same family on another node: nothing
        // closes there, so exactly one superseded close exists.
        let third = ask("visual approval PR 7 at 3333333c", "x-bbbb");
        assert_eq!(third.exit_code, 0, "lines: {:?}", third.lines);
        let closed_text = crate::event_store::journal_text(&index, &["operator_question_closed"]);
        assert_eq!(
            closed_text
                .lines()
                .filter(|l| l.contains("superseded:"))
                .count(),
            1
        );
    }

    #[test]
    fn law_refusal_records_nothing() {
        let home = tmp_home("law");
        let root = tmp_root("law");
        let mut r = req("test subject again?", &root);
        r.laws = vec![LawRow {
            decision_id: "d-test0001".to_string(),
            subject: Some("test-subject".to_string()),
            decision: Some("stay strict".to_string()),
            ts: Some("2026-09-01T00:00:00Z".to_string()),
            lane: Some("law".to_string()),
        }];
        let answer = run_intake(&r, &home);
        assert_eq!(answer.exit_code, 2);
        assert_eq!(answer.refusal.as_deref(), Some("law"));
        assert!(
            answer.lines.iter().any(|l| l.contains("d-test0001")),
            "the refusal names the law id: {:?}",
            answer.lines
        );
        assert!(journal_text(&root).is_empty());
    }

    #[test]
    fn over_cap_question_is_cut_at_the_cap_and_flagged() {
        let home = tmp_home("cap");
        let root = tmp_root("cap");
        let long = "x".repeat(QUESTION_CAP + 50);
        let mut r = req(&long, &root);
        r.ask = Some("note the cap".to_string());
        let answer = run_intake(&r, &home);
        assert_eq!(answer.exit_code, 0);
        assert!(answer.truncated);
        let row: Value = journal_text(&root)
            .lines()
            .last()
            .map(|l| serde_json::from_str(l).unwrap())
            .unwrap();
        assert_eq!(
            row.pointer("/data/question")
                .and_then(Value::as_str)
                .unwrap()
                .chars()
                .count(),
            QUESTION_CAP
        );
    }

    #[test]
    fn ac5_hp_a_store_only_close_unmarks_the_question() {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "fno-agents-qintake-openrows-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        let index = p.join("questions.jsonl");
        let ask = r#"{"ts":"2026-09-23T01:00:00Z","type":"operator_question","source":"test","data":{"question_id":"q-t1","question":"ship?","blocks":[]}}"#;
        std::fs::write(&index, format!("{ask}\n")).unwrap();
        let close = json!({
            "ts": "2026-09-23T01:05:00Z",
            "type": "operator_question_closed",
            "source": "test",
            "data": {"question_id": "q-t1"}
        });
        crate::event_store::append_envelope(&index, &close.to_string(), None).unwrap();
        let open = open_rows(&index);
        assert!(
            open.iter().all(|r| r.id != "q-t1"),
            "a store-only close must unmark the question: {open:?}"
        );
    }

    #[test]
    fn the_append_door_writes_the_envelope_to_both_sinks() {
        let root = tmp_root("append-ok");
        let index = root.join("questions.jsonl");
        let journal = root.join("journal").join("events.jsonl");
        let event = json!({
            "ts": "2026-10-24T12:00:00Z",
            "type": "operator_question",
            "source": "daemon",
            "data": {"question_id": "q-ap1", "question": "wake?", "ask": "answer inherit or clean"}
        });
        let req = IntakeRequest {
            op: Some("append".to_string()),
            event: Some(event),
            journal_path: Some(journal.clone()),
            index_path: Some(index.clone()),
            storage_root: root.clone(),
            ..req("", &root)
        };
        let answer = run_intake(&req, &tmp_home("append-ok"));
        assert_eq!(answer.exit_code, 0, "{:?}", answer.lines);
        assert_eq!(answer.qid.as_deref(), Some("q-ap1"));
        let row = crate::events::committed_journal_text(&journal)
            .lines()
            .last()
            .map(|l| serde_json::from_str::<Value>(l).unwrap())
            .unwrap();
        assert_eq!(
            row.pointer("/data/question_id").and_then(Value::as_str),
            Some("q-ap1")
        );
        assert_eq!(
            row.pointer("/source").and_then(Value::as_str),
            Some("daemon")
        );
        let indexed = crate::events::committed_journal_text(&index);
        assert!(indexed.contains("\"q-ap1\""), "{indexed}");
    }

    #[test]
    fn the_append_door_refuses_an_envelope_with_no_question_id() {
        let root = tmp_root("append-noid");
        let event = json!({
            "ts": "2026-10-24T12:00:00Z",
            "type": "operator_question",
            "source": "hook",
            "data": {"question": "no id here"}
        });
        let req = IntakeRequest {
            op: Some("append".to_string()),
            event: Some(event),
            journal_path: Some(root.join("events.jsonl")),
            index_path: Some(root.join("questions.jsonl")),
            storage_root: root.clone(),
            ..req("", &root)
        };
        let answer = run_intake(&req, &tmp_home("append-noid"));
        assert_eq!(answer.exit_code, 2);
        assert_eq!(answer.qid, None);
        assert!(
            answer.lines[0].contains("no question_id"),
            "{:?}",
            answer.lines
        );
        assert!(
            crate::events::committed_journal_text(&root.join("events.jsonl")).is_empty(),
            "a refused envelope writes nothing"
        );
    }

    #[test]
    fn a_failed_journal_append_is_fatal_and_reported() {
        let root = tmp_root("append-jfail");
        let event = json!({
            "ts": "2026-10-24T12:00:00Z",
            "type": "operator_question",
            "source": "daemon",
            "data": {"question_id": "q-ap2", "question": "wake?"}
        });
        // A journal parked under a plain FILE cannot take the append: both
        // the store and the journal need that parent to be a directory.
        std::fs::write(root.join("no-such-dir"), b"not a directory").unwrap();
        let req = IntakeRequest {
            op: Some("append".to_string()),
            event: Some(event),
            journal_path: Some(root.join("no-such-dir").join("events.jsonl")),
            index_path: Some(root.join("questions.jsonl")),
            storage_root: root.clone(),
            ..req("", &root)
        };
        let answer = run_intake(&req, &tmp_home("append-jfail"));
        assert_eq!(answer.exit_code, 1);
        assert!(
            answer.lines[0].contains("failed to append question to project journal"),
            "{:?}",
            answer.lines
        );
    }
}
