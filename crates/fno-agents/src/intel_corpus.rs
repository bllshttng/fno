//! `fno-agents intel --corpus <dir> [--days N] [-H claude,codex]` and
//! `fno-agents intel --corpus-ingest <dir>` - the decision corpus: Kev
//! training rows built from this machine's claude and codex transcripts.
//!
//! Every row is one Kev request (`state` plus one labelled question, metadata
//! under `_meta`) of five kinds: `claim_support` (an assistant claim that
//! tests pass, a PR merged, or a push landed, paired with the latest prior
//! tool record of that kind and labelled from its exit status and output),
//! `reaction` (the next typed user turn after a message, rule-labelled),
//! `is_ask` (did a typed reply answer the message), `help_class` (the
//! class of a `<help>` tag), and `agent_mail` (mail another agent typed into
//! the input, kept apart from user turns). Turn provenance is [`crate::provenance`]'s, with
//! one corpus-only rule: the first user row of a headless session (claude
//! entrypoint `sdk-*`, codex originator `*exec*`) is the spawn seed, never a
//! typed turn. The rule needs the row's position, which `classify_turn`
//! never sees.
//!
//! Scrub runs before any row reaches disk: secret shapes are masked with the
//! report scrub's regex, home paths collapse to `~`, email addresses become
//! `[email]`, the row is rescanned, and a residual match drops it. A row that
//! cites a tool call which read or printed a `.env*`, credentials, `.pem` or
//! keychain path is dropped whole.
//! Whole sessions go to the eval candidates, so no eval session ever lends a
//! row to train, holdout or unlabelled; the manifest's `leaks` re-reads the
//! written files to prove it. Nothing leaves the machine.

use crate::provenance::TranscriptSource;
use crate::provenance::{BusIndex, ClaudeSource, CodexSource, HarnessKind, Provenance, RelayKind};
use regex::Regex;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{BufRead, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const HOLDOUT_DAYS: u64 = 7;
const EVAL_TARGET: usize = 1_500;
/// An eval session must stay small: one huge session would spend the
/// whole eval budget on one conversation and starve train of its rows.
const EVAL_SESSION_MAX_ROWS: usize = 100;
const PAGE_ITEMS: usize = 50;
const ASK_REPLY_SECS: f64 = 1_800.0;
const TEXT_CAP: usize = 2_000;
const OUTPUT_HEAD: usize = 600;
const OUTPUT_TAIL: usize = 1_400;

const TRAIN: &str = "train.jsonl";
const HOLDOUT: &str = "holdout.jsonl";
const EVAL_CANDIDATES: &str = "eval-candidates.jsonl";
const UNLABELLED: &str = "unlabelled.jsonl";
const EVAL: &str = "eval.jsonl";
const MANIFEST: &str = "manifest.json";
const PAGES_DIR: &str = "eval-check";
const ROWS_TMP: &str = ".rows.tmp.jsonl";

/// The help classes a `help_class` row may carry, with the option text Kev
/// reads. `unclassified` is the parse residual and never a label.
const HELP_CLASSES: &[(&str, &str)] = &[
    ("stale-plan", "The plan no longer matches the code."),
    (
        "missing-prereq",
        "A needed input, tool or dependency is absent.",
    ),
    ("ci-red", "CI failed on the change."),
    ("stuck", "No progress after several tries."),
    ("held", "A rule or a person holds the work."),
    ("wait", "The work waits on an outside event."),
    ("env-denied", "The environment refused an action."),
    ("gate-deadlock", "Two gates block each other."),
    ("gate-unsatisfiable", "A gate cannot be met as written."),
    ("question", "The agent needs a ruling from a person."),
    ("budget", "A cost, time or turn budget ran out."),
];

// ---------------------------------------------------------------- patterns

fn rx(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("static regex"))
}

fn email_rx() -> &'static Regex {
    static RX: OnceLock<Regex> = OnceLock::new();
    rx(
        &RX,
        r"[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}",
    )
}

fn sensitive_rx() -> &'static Regex {
    static RX: OnceLock<Regex> = OnceLock::new();
    rx(
        &RX,
        r#"(?i)(?:^|[\s/'"=:(])\.env(?:\.[\w-]+)?\b|credentials|\.pem\b|keychain|security\s+find-(?:generic|internet)-password|\.netrc\b|\bid_(?:rsa|ed25519)\b"#,
    )
}

/// A conditional, negated, or future sentence is not a claim that something
/// happened.
fn hedge_rx() -> &'static Regex {
    static RX: OnceLock<Regex> = OnceLock::new();
    rx(
        &RX,
        r"(?i)\b(?:if|once|when|until|unless|whether|should|will|would|could|need|needs|must|after|before|not|never|no longer|pushed back|push back)\b|n't\b|\?",
    )
}

/// One claim kind a record can settle mechanically: the claim sentence, the
/// command that runs the check, output only that check prints (so a file
/// read holding it still pairs, and the veto labels it unrelated), and the
/// supports and contradicts markers. `ok_first` lets a decisive success
/// marker outrank a non-zero exit (`gh pr merge` on a merged PR fails).
struct ClaimKind {
    name: &'static str,
    claim: &'static str,
    command: &'static str,
    marker: &'static str,
    ok: &'static str,
    bad: &'static str,
    ok_first: bool,
}

const CLAIM_KINDS: [ClaimKind; 3] = [
    ClaimKind {
        name: "tests",
        claim: r"(?i)\btests?\s+(?:now\s+|all\s+|still\s+)?(?:pass(?:es|ed)?|(?:are|is)\s+(?:passing|green))\b|\b(?:test suite|cargo test|pytest|fno doctor test)\b[^.\n]{0,40}\b(?:pass(?:es|ed)?|green)\b|\b\d+\s+(?:tests?\s+)?passed\b",
        command: r"(?i)\b(?:cargo\s+(?:\+\S+\s+)?(?:test|nextest)|pytest|fno\s+doctor\s+test|(?:npm|pnpm|yarn|bun)\s+(?:run\s+)?test|go\s+test|jest|vitest|make\s+test|tox)\b",
        marker: r"test result: |(?m)^=+ .*\b(?:passed|failed)\b.* =+$",
        ok: r"test result: ok\.|\b\d+\s+passed\b",
        bad: r"test result: FAILED|\b[1-9]\d*\s+failed\b|(?m)^FAILED\b|error\[E\d{4}\]|error: could not compile",
        ok_first: false,
    },
    ClaimKind {
        name: "merged",
        claim: r"(?i)\b(?:PR|pull request)\s*#?\d*\s+(?:is\s+|was\s+|has\s+been\s+|got\s+)?(?:now\s+)?merged\b|\bmerged\s+(?:PR|pull request|#\d+)",
        command: r"(?i)\b(?:gh\s+pr\s+merge|fno\s+do\s+pr\s+merge|fno\s+do\s+pr\s+status|gh\s+pr\s+view\b[^|\n]*\b(?:state|merged)|gh\s+api\s+\S*pulls/\d+)",
        marker: r"\b\B",
        ok: r#"(?i)"state":\s*"MERGED"|\bMERGED\b|Merged pull request|was already merged|"merged":\s*true"#,
        bad: r#"(?i)"state":\s*"(?:OPEN|CLOSED)"|\bstate:\s*(?:OPEN|CLOSED)\b|not mergeable|"merged":\s*false|refus"#,
        ok_first: true,
    },
    ClaimKind {
        name: "pushed",
        claim: r"(?i)\bpushed\b|\bpush\s+(?:landed|succeeded|went through)\b",
        command: r"(?i)\b(?:git\s+push|fno\s+do\s+pr\s+push)\b",
        marker: r"\b\B",
        ok: r"(?m)[0-9a-f]{7,}\.{2,3}[0-9a-f]{7,}\s+\S+\s+->\s+\S+|\* \[new branch\]|Everything up-to-date",
        bad: r"\[rejected\]|\[remote rejected\]|error: failed to push|(?m)^fatal:",
        ok_first: false,
    },
];

/// The compiled patterns of [`CLAIM_KINDS`], in field order: claim,
/// command, marker, ok, bad.
fn kind_rx(kind: usize) -> &'static [Regex; 5] {
    static RX: OnceLock<Vec<[Regex; 5]>> = OnceLock::new();
    &RX.get_or_init(|| {
        CLAIM_KINDS
            .iter()
            .map(|k| {
                [k.claim, k.command, k.marker, k.ok, k.bad]
                    .map(|p| Regex::new(p).expect("static regex"))
            })
            .collect()
    })[kind]
}

/// The mechanical label of a tool-output record: `None` when the output
/// carries no decisive marker.
fn kind_label(kind: usize, output: &str, is_error: bool) -> Option<&'static str> {
    let [_, _, _, ok, bad] = kind_rx(kind);
    let (ok, bad) = (ok.is_match(output), bad.is_match(output));
    // A chained call (`cargo test && cargo clippy`) fails on its last
    // command, so the exit status speaks only when no marker does.
    if ok && (CLAIM_KINDS[kind].ok_first || !bad) {
        Some("supports")
    } else if bad || is_error {
        Some("contradicts")
    } else {
        None
    }
}

/// The five record classes of the claim-support provenance rule (bake-off
/// 2026-10-13, section 3), first match wins. Only `tool_output` is a run's
/// own output; `agent_input` and `file_read` are vetoed to unrelated.
fn record_class(tool: &str, command: &str) -> &'static str {
    static HEREDOC: OnceLock<Regex> = OnceLock::new();
    static MIXED: OnceLock<Regex> = OnceLock::new();
    static STORED: OnceLock<Regex> = OnceLock::new();
    static READER: OnceLock<Regex> = OnceLock::new();
    static NOT_DOC: OnceLock<Regex> = OnceLock::new();
    const AGENT_TOOLS: &[&str] = &[
        "Edit",
        "Write",
        "MultiEdit",
        "NotebookEdit",
        "TaskCreate",
        "TaskUpdate",
        "AskUserQuestion",
        "ScheduleWakeup",
        "update_plan",
        "apply_patch",
    ];
    if AGENT_TOOLS.contains(&tool)
        || (rx(&HEREDOC, r"<<-?\s*['\x22]?\w+").is_match(command)
            && (command.contains("cat >") || command.contains("tee ")))
    {
        return "agent_input";
    }
    let help = command.contains("--help");
    if !help
        && rx(
            &MIXED,
            r"--reason\b|--body\b|--details\b|--need\b|--title\b|\bmail\s+(?:send|reply)\b|\bgh\s+(?:pr|issue)\s+(?:create|comment)\b|\bbacklog\s+(?:idea|add|done|note)\b",
        )
        .is_match(command)
    {
        return "mixed";
    }
    if !help
        && rx(
            &STORED,
            r"\bbacklog\s+(?:get|show|decisions)\b|\bthink\s+inspect\b|\bgh\s+pr\s+view\b[^|\n]*(?:--comments|\bbody\b|\bcomments\b)|\bgh\s+api\s+\S*comments|\bjq\b[^|\n]*\b(?:message|content)\b",
        )
        .is_match(command)
    {
        return "stored";
    }
    if matches!(tool, "Read" | "Grep" | "Glob") {
        return if rx(&NOT_DOC, r"\.log\b|\.output\b|/\.fno/|/tmp/|/tasks/").is_match(command) {
            "tool_output"
        } else {
            "file_read"
        };
    }
    // The last command of a chain decides: `cd x && rg y` reads a file,
    // `cargo test | tail` is a run.
    let head = command
        .rsplit(['&', ';'])
        .next()
        .unwrap_or("")
        .split('|')
        .next()
        .unwrap_or("")
        .trim();
    let first = head.split_whitespace().next().unwrap_or("");
    if rx(&READER, r"^(?:cat|sed|head|awk|grep|rg|nl|less)$").is_match(first)
        && !rx(&NOT_DOC, r"\.log\b|\.output\b|/\.fno/|/tmp/|/tasks/").is_match(head)
    {
        return "file_read";
    }
    "tool_output"
}

// ------------------------------------------------------------ text helpers

fn clip(s: &str, cap: usize) -> String {
    if s.chars().count() <= cap {
        return s.to_string();
    }
    let mut out: String = s.chars().take(cap).collect();
    out.push_str(" [...]");
    out
}

/// Head and tail of a tool output: test and push summaries sit at the end.
fn clip_output(s: &str) -> String {
    let n = s.chars().count();
    if n <= OUTPUT_HEAD + OUTPUT_TAIL {
        return s.to_string();
    }
    let head: String = s.chars().take(OUTPUT_HEAD).collect();
    let tail: String = s.chars().skip(n - OUTPUT_TAIL).collect();
    format!("{head}\n[...]\n{tail}")
}

/// Assistant prose with fenced code blocks removed: quoted output is not a
/// claim the agent made.
fn prose(text: &str) -> String {
    let mut out = String::new();
    let mut fenced = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if !fenced {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

fn sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\n' {
            if !cur.trim().is_empty() {
                out.push(cur.trim().to_string());
            }
            cur.clear();
            continue;
        }
        cur.push(c);
        if matches!(c, '.' | '!' | '?') && chars.peek().is_none_or(|n| n.is_whitespace()) {
            if !cur.trim().is_empty() {
                out.push(cur.trim().to_string());
            }
            cur.clear();
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

/// The rule label of a typed reply, `None` when no rule is sure.
fn reaction_rule(text: &str) -> Option<&'static str> {
    static APPROVE: OnceLock<Regex> = OnceLock::new();
    static REJECT: OnceLock<Regex> = OnceLock::new();
    static CORRECT: OnceLock<Regex> = OnceLock::new();
    let t = text.trim().to_lowercase();
    if t.is_empty() || t.ends_with('?') {
        return None;
    }
    if rx(
        &REJECT,
        r"^(?:no|nope|nah|stop|don't|do not|cancel|abort|never ?mind)[\s.!]*$",
    )
    .is_match(&t)
    {
        return Some("reject");
    }
    if rx(
        &CORRECT,
        r"^(?:no|nope|actually|wait|instead|rather|not quite|that's not|thats not|that is not|wrong|incorrect)\b[\s,.:!-]+\S",
    )
    .is_match(&t)
    {
        return Some("correct");
    }
    if t.chars().count() <= 60
        && !t.contains(" but ")
        && !t.contains("instead")
        && rx(
            &APPROVE,
            r"^(?:y|yes|yep|yeah|yup|ok|okay|k|sure|go|go ahead|go for it|do it|ship it|lgtm|sounds good|looks good|perfect|great|nice|proceed|continue|keep going|approved?|merge it|please do|👍)\b",
        )
        .is_match(&t)
    {
        return Some("approve");
    }
    None
}

// --------------------------------------------------------------- scrubbing

#[derive(Default)]
struct ScrubCounts {
    secrets_masked: u64,
    paths_masked: u64,
    emails_masked: u64,
    residual_drops: u64,
    sensitive_drops: u64,
}

fn scrub_string(s: &str, counts: &mut ScrubCounts) -> String {
    let secret = crate::intel_html::secret_rx();
    let mut out = s.to_string();
    let n = secret.find_iter(&out).count();
    if n > 0 {
        out = secret
            .replace_all(&out, |caps: &regex::Captures| match caps.get(2) {
                Some(key) => format!("{}[redacted]", key.as_str()),
                None => "[redacted]".to_string(),
            })
            .into_owned();
        counts.secrets_masked += n as u64;
    }
    let paths = crate::intel_html::path_rx();
    let n = paths.find_iter(&out).count();
    if n > 0 {
        out = paths.replace_all(&out, "~").into_owned();
        counts.paths_masked += n as u64;
    }
    let n = email_rx().find_iter(&out).count();
    if n > 0 {
        out = email_rx().replace_all(&out, "[email]").into_owned();
        counts.emails_masked += n as u64;
    }
    out
}

fn scrub_value(v: &mut Value, counts: &mut ScrubCounts) {
    match v {
        Value::String(s) => *s = scrub_string(s, counts),
        Value::Array(items) => items.iter_mut().for_each(|i| scrub_value(i, counts)),
        Value::Object(map) => map.values_mut().for_each(|i| scrub_value(i, counts)),
        _ => {}
    }
}

/// Mask every string, then rescan the serialized row: any secret shape left
/// that is not a mask is a residual, and the row is dropped.
fn scrub_row(mut row: Value, counts: &mut ScrubCounts) -> Option<Value> {
    scrub_value(&mut row, counts);
    let text = row.to_string();
    let residual = crate::intel_html::secret_rx()
        .find_iter(&text)
        .any(|m| !m.as_str().contains("[redacted]"))
        || email_rx().is_match(&text);
    if residual {
        counts.residual_drops += 1;
        return None;
    }
    Some(row)
}

/// The Kev question each row kind asks, without its label.
fn question(kind: &str) -> Value {
    match kind {
        "claim_support" => json!({"type": "choice",
        "instructions": "Does the record support the claim?",
        "criteria": {
            "supports": "The record shows the claim is true.",
            "contradicts": "The record shows the claim is false.",
            "unrelated": "The record does not bear on the claim, or the agent wrote it itself."
        }}),
        "reaction" => json!({"type": "choice",
        "instructions": "How does the user react to this message and its proposed action?",
        "criteria": {
            "approve": "The user agrees and lets it go ahead.",
            "correct": "The user changes or corrects the plan.",
            "reject": "The user refuses it.",
            "interrupt": "The user stops the agent mid-action."
        }}),
        "is_ask" => json!({"type": "noul",
            "instructions": "Does this message ask the user for a decision or an answer?"}),
        "agent_mail" => json!({"type": "choice",
        "instructions": "What does this agent mail do with the message before it?",
        "criteria": {
            "approve": "It agrees and lets the work go ahead.",
            "correct": "It changes or corrects the plan.",
            "reject": "It refuses or stops the work.",
            "inform": "It reports status, hands over work, or asks something new."
        }}),
        _ => {
            let criteria: Map<String, Value> = HELP_CLASSES
                .iter()
                .map(|(k, v)| ((*k).to_string(), json!(v)))
                .collect();
            json!({"type": "choice", "instructions": "Why is the agent stuck at this point?",
                "criteria": criteria})
        }
    }
}

// ------------------------------------------------------- session row build

/// One tool call, kept for its result.
#[derive(Clone)]
struct Call {
    tool: String,
    command: String,
    sensitive: bool,
}

/// One tool result joined to its call: the record a claim pairs with.
#[derive(Clone)]
struct Record {
    call: Call,
    output: String,
    is_error: bool,
    sensitive: bool,
}

impl Record {
    fn kind(&self) -> Option<usize> {
        (0..CLAIM_KINDS.len()).find(|&k| {
            kind_rx(k)[1].is_match(&self.call.command) || kind_rx(k)[2].is_match(&self.output)
        })
    }
}

/// What the builder hands back per session: rows plus the counters the
/// manifest needs.
#[derive(Default)]
struct SessionOut {
    rows: Vec<Value>,
    sensitive_drops: u64,
    seeds: u64,
    typed: u64,
}

/// The per-session state machine over transcript rows in order.
struct Builder<'a> {
    harness: &'static str,
    session: String,
    bus: &'a BusIndex,
    witness: &'a mut crate::operator_witness::SubmitIndex,
    headless: bool,
    first_user_seen: bool,
    calls: HashMap<String, Call>,
    last_of_kind: [Option<Record>; 3],
    last_other: Option<Record>,
    last_record: Option<Record>,
    /// The assistant turn since the last user-shaped boundary.
    turn_text: String,
    turn_ts: Option<f64>,
    turn_uuid: String,
    turn_calls: Vec<Call>,
    turn_has_ask_call: bool,
    /// A call or result of this turn touched a sensitive path: every row
    /// built from the turn's text is dropped, not only the row citing it.
    turn_sensitive: bool,
    reacted: bool,
    pending_negative: Option<Value>,
    line: usize,
    out: SessionOut,
}

fn rfc3339(ts: Option<f64>) -> Value {
    ts.and_then(|t| chrono::DateTime::from_timestamp(t as i64, 0))
        .map(|d| Value::String(d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)))
        .unwrap_or(Value::Null)
}

fn row_id(session: &str, turn: &str, kind: &str, sub: &str) -> String {
    let key = format!("{session}\0{turn}\0{kind}\0{sub}");
    blake3::hash(key.as_bytes()).to_hex()[..16].to_string()
}

fn call_line(c: &Call) -> String {
    if c.command.is_empty() {
        c.tool.clone()
    } else {
        format!("{}: {}", c.tool, clip(&c.command, 600))
    }
}

impl<'a> Builder<'a> {
    fn new(
        harness: &'static str,
        session: &str,
        bus: &'a BusIndex,
        witness: &'a mut crate::operator_witness::SubmitIndex,
    ) -> Self {
        Builder {
            harness,
            session: session.to_string(),
            bus,
            witness,
            headless: false,
            first_user_seen: false,
            calls: HashMap::new(),
            last_of_kind: [None, None, None],
            last_other: None,
            last_record: None,
            turn_text: String::new(),
            turn_ts: None,
            turn_uuid: String::new(),
            turn_calls: Vec::new(),
            turn_has_ask_call: false,
            turn_sensitive: false,
            reacted: false,
            pending_negative: None,
            line: 0,
            out: SessionOut::default(),
        }
    }

    /// One Kev row: `label: None` writes an unlabelled question.
    #[allow(clippy::too_many_arguments)]
    fn row(
        &self,
        kind: &str,
        sub: &str,
        turn: &str,
        ts: Option<f64>,
        state: Value,
        label: Option<Value>,
        source: &str,
        extra: &[(&str, Value)],
    ) -> Value {
        let mut q = question(kind);
        if let Some(l) = label {
            q["label"] = l;
        }
        let mut meta = json!({
            "id": row_id(&self.session, turn, kind, sub), "kind": kind, "source": source,
            "harness": self.harness, "session": self.session, "turn": turn, "ts": rfc3339(ts),
        });
        for (k, v) in extra {
            meta[*k] = v.clone();
        }
        json!({"state": state, "questions": {kind: q}, "_meta": meta})
    }

    fn push(&mut self, row: Value, sensitive: bool) {
        if sensitive || self.turn_sensitive {
            self.out.sensitive_drops += 1;
        } else {
            self.out.rows.push(row);
        }
    }

    fn assistant_text(&mut self, text: &str, ts: Option<f64>, uuid: &str) {
        if text.trim().is_empty() {
            return;
        }
        if let Some(neg) = self.pending_negative.take() {
            self.out.rows.push(neg);
        }
        self.claims(text, ts, uuid);
        self.help(text, ts, uuid);
        if !self.turn_text.is_empty() {
            self.turn_text.push('\n');
        }
        self.turn_text.push_str(text);
        self.turn_ts = ts.or(self.turn_ts);
        self.turn_uuid = uuid.to_string();
    }

    fn claims(&mut self, text: &str, ts: Option<f64>, uuid: &str) {
        for sentence in sentences(&prose(text)) {
            if hedge_rx().is_match(&sentence) {
                continue;
            }
            let Some(kind) = (0..CLAIM_KINDS.len()).find(|&k| kind_rx(k)[0].is_match(&sentence))
            else {
                continue;
            };
            let pairs = [
                (self.last_of_kind[kind].clone(), "same"),
                (self.last_other.clone(), "other"),
            ];
            for (record, pairing) in pairs {
                let Some(record) = record else {
                    continue;
                };
                // A record the kind cannot name by command may still print
                // its verdict (a pr create that pushes): never unrelated.
                let [_, _, _, ok, bad] = kind_rx(kind);
                if pairing == "other"
                    && (ok.is_match(&record.output) || bad.is_match(&record.output))
                {
                    continue;
                }
                let class = record_class(&record.call.tool, &record.call.command);
                let label = if pairing == "other" {
                    Some("unrelated")
                } else if class == "agent_input" || class == "file_read" {
                    Some("unrelated")
                } else if class == "tool_output" {
                    kind_label(kind, &record.output, record.is_error)
                } else {
                    None
                };
                let state = json!({
                    "claim": clip(&sentence, 600),
                    "record_call": call_line(&record.call),
                    "record": clip_output(&record.output),
                });
                let row = self.row(
                    "claim_support",
                    &format!("{pairing}:{sentence}"),
                    uuid,
                    ts,
                    state,
                    label.map(|l| json!(l)),
                    &format!("{}/{pairing}", CLAIM_KINDS[kind].name),
                    &[("provenance", json!(class))],
                );
                self.push(row, record.sensitive || record.call.sensitive);
            }
        }
    }

    fn help(&mut self, text: &str, ts: Option<f64>, uuid: &str) {
        let text = prose(text);
        let text = text.as_str();
        let Some(at) = text.match_indices("<help").map(|(i, _)| i).find(|&i| {
            text[..i]
                .rsplit('\n')
                .next()
                .unwrap_or("")
                .trim()
                .is_empty()
        }) else {
            return;
        };
        let Some(distress) = crate::distress::extract_help_distress(&text[at..]) else {
            return;
        };
        let class = distress.class().as_str();
        let label = HELP_CLASSES
            .iter()
            .any(|(name, _)| *name == class)
            .then_some(class);
        let before = format!("{}\n{}", self.turn_text, &text[..at]);
        let last = self.last_record.clone();
        let state = json!({
            "context": clip(before.trim(), TEXT_CAP),
            "last_call": last.as_ref().map(|r| call_line(&r.call)).unwrap_or_default(),
            "last_result": last.as_ref().map(|r| clip_output(&r.output)).unwrap_or_default(),
        });
        let sensitive = last
            .as_ref()
            .is_some_and(|r| r.sensitive || r.call.sensitive);
        let row = self.row(
            "help_class",
            "0",
            uuid,
            ts,
            state,
            label.map(|l| json!(l)),
            "help_tag",
            &[],
        );
        self.push(row, sensitive);
    }

    fn call(&mut self, id: &str, tool: &str, command: &str) {
        let call = Call {
            tool: tool.to_string(),
            command: command.to_string(),
            sensitive: sensitive_rx().is_match(command),
        };
        if command.contains("outstanding ask") {
            self.turn_has_ask_call = true;
        }
        self.turn_sensitive |= call.sensitive;
        self.turn_calls.push(call.clone());
        if !id.is_empty() {
            self.calls.insert(id.to_string(), call);
        }
    }

    fn result(&mut self, id: &str, output: &str, is_error: bool, rejected: bool, ts: Option<f64>) {
        let Some(call) = self.calls.get(id).cloned() else {
            return;
        };
        let record = Record {
            sensitive: sensitive_rx().is_match(output),
            call: call.clone(),
            output: clip_output(output),
            is_error,
        };
        self.turn_sensitive |= record.sensitive;
        match record.kind() {
            Some(kind) => self.last_of_kind[kind] = Some(record.clone()),
            None => self.last_other = Some(record.clone()),
        }
        self.last_record = Some(record);
        if rejected && !self.turn_text.is_empty() && !self.reacted {
            let row = self.reaction_row(Some("reject"), &call, ts, &format!("{id}:rejected"));
            self.push(row, call.sensitive);
            self.reacted = true;
        }
    }

    fn reaction_row(&self, label: Option<&str>, call: &Call, ts: Option<f64>, turn: &str) -> Value {
        let state = json!({
            "assistant": clip(&self.turn_text, TEXT_CAP),
            "proposed_call": call_line(call),
        });
        self.row(
            "reaction",
            "0",
            turn,
            ts,
            state,
            label.map(|l| json!(l)),
            label.map_or("teacher", |_| "rule"),
            &[],
        )
    }

    /// Agent mail typed into the input: its own row class, never a user
    /// turn (user ruling 2026-10-18). The body is rule-labelled like a
    /// reaction, else left to the teacher.
    fn mail_row(&mut self, text: &str, ts: Option<f64>, uuid: &str) {
        static TAG: OnceLock<Regex> = OnceLock::new();
        let body = match rx(&TAG, r"(?s)<fno_mail[^>]*>(.*?)</fno_mail>").captures(text) {
            Some(c) => c[1].to_string(),
            None => text
                .trim_start()
                .split_once(['\n', '⏎'])
                .map_or("", |(_, b)| b)
                .to_string(),
        };
        let call = self.turn_calls.last().cloned();
        let state = json!({
            "assistant": clip(&self.turn_text, TEXT_CAP),
            "proposed_call": call.as_ref().map(call_line).unwrap_or_default(),
            "mail": clip(text.trim(), TEXT_CAP),
        });
        let label = reaction_rule(&body).map(|l| json!(l));
        let source = if label.is_some() { "rule" } else { "teacher" };
        let row = self.row("agent_mail", "0", uuid, ts, state, label, source, &[]);
        self.push(row, call.is_some_and(|c| c.sensitive));
    }

    fn ask_row(&self, label: Option<bool>, ts: Option<f64>, source: &str) -> Value {
        let heuristic = !crate::repeated_asks::asks_in(&self.turn_text).is_empty();
        self.row(
            "is_ask",
            "0",
            &self.turn_uuid,
            self.turn_ts.or(ts),
            json!({"message": clip(&self.turn_text, TEXT_CAP)}),
            label.map(Value::Bool),
            source,
            &[("heuristic", json!(heuristic))],
        )
    }

    /// One user-shaped row: classify, then close the assistant turn when
    /// the row is a boundary.
    fn user_turn(&mut self, obj: &Value, text: &str, ts: Option<f64>, uuid: &str) {
        if text.trim().is_empty() {
            return;
        }
        let mut p = crate::provenance::classify_turn(obj, self.bus, &self.session);
        if p == Provenance::Unknown || p == Provenance::Harness(HarnessKind::CommandInvocation) {
            let bound = ts
                .and_then(|t| self.witness.bind(&self.session, (t * 1000.0) as i64))
                .is_some();
            if bound && p == Provenance::Unknown {
                p = Provenance::Operator;
            }
        }
        let typed_shape = matches!(p, Provenance::Unknown | Provenance::Operator);
        let first = typed_shape && !self.first_user_seen;
        self.first_user_seen |= typed_shape;
        let seed = first && self.headless && p == Provenance::Unknown;
        if seed {
            self.out.seeds += 1;
        }
        let typed = p == Provenance::Operator || (p == Provenance::Unknown && !seed);
        let interrupt = p == Provenance::Harness(HarnessKind::InterruptMarker);
        let wake = matches!(
            p,
            Provenance::Harness(HarnessKind::StopHook)
                | Provenance::Harness(HarnessKind::LoopWakeup)
                | Provenance::Relay(RelayKind::TaskNotification)
                | Provenance::Relay(RelayKind::BusRow)
                | Provenance::Keepalive
        );
        let boundary = typed
            || interrupt
            || wake
            || seed
            || matches!(
                p,
                Provenance::Relay(_)
                    | Provenance::Harness(HarnessKind::CommandInvocation)
                    | Provenance::Harness(HarnessKind::CompactionPreamble)
            );
        if !boundary {
            return;
        }
        if p == Provenance::Relay(RelayKind::FnoMail) {
            self.mail_row(text, ts, uuid);
        }
        if typed {
            self.out.typed += 1;
            self.pending_negative = None;
        }
        if !self.turn_text.is_empty() {
            let call = self.turn_calls.last().cloned().unwrap_or(Call {
                tool: String::new(),
                command: String::new(),
                sensitive: false,
            });
            if interrupt && !self.reacted {
                let row = self.reaction_row(Some("interrupt"), &call, ts, uuid);
                self.push(row, call.sensitive);
            } else if typed && !self.reacted {
                let row = self.reaction_row(reaction_rule(text), &call, ts, uuid);
                self.push(row, call.sensitive);
            }
            let gap = ts.zip(self.turn_ts).map(|(u, a)| u - a);
            if self.turn_has_ask_call {
                let row = self.ask_row(Some(true), ts, "outstanding_ask");
                self.push(row, false);
            } else if typed && gap.is_some_and(|g| g <= ASK_REPLY_SECS) {
                let row = self.ask_row(Some(true), ts, "typed_reply");
                self.push(row, false);
            } else if wake && self.turn_sensitive {
                self.out.sensitive_drops += 1;
            } else if wake {
                self.pending_negative = Some(self.ask_row(Some(false), ts, "machine_wake"));
            }
        }
        self.turn_text.clear();
        self.turn_ts = None;
        self.turn_calls.clear();
        self.turn_has_ask_call = false;
        self.turn_sensitive = false;
        self.reacted = false;
    }

    /// One claude row.
    fn claude_row(&mut self, obj: &Value) {
        self.line += 1;
        let ts = crate::provenance::turn_ts_epoch(obj);
        let uuid = obj
            .get("uuid")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("L{}", self.line));
        let kind = obj.get("type").and_then(Value::as_str).unwrap_or("");
        let blocks = obj
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(Value::as_array);
        if kind == "assistant" {
            let Some(blocks) = blocks else {
                return;
            };
            for b in blocks {
                match b.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        let t = b.get("text").and_then(Value::as_str).unwrap_or("");
                        self.assistant_text(t, ts, &uuid);
                    }
                    Some("tool_use") => {
                        let tool = b.get("name").and_then(Value::as_str).unwrap_or("");
                        let input = b.get("input").unwrap_or(&Value::Null);
                        let command = claude_command(tool, input);
                        let id = b.get("id").and_then(Value::as_str).unwrap_or("");
                        self.call(id, tool, &command);
                    }
                    _ => {}
                }
            }
            return;
        }
        if kind != "user" {
            return;
        }
        let results: Vec<&Value> = blocks
            .map(|bs| {
                bs.iter()
                    .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
                    .collect()
            })
            .unwrap_or_default();
        if !results.is_empty() {
            for b in results {
                let id = b.get("tool_use_id").and_then(Value::as_str).unwrap_or("");
                let output = match b.get("content") {
                    Some(Value::String(s)) => s.clone(),
                    Some(c) => crate::lead_hygiene::message_text(c),
                    None => String::new(),
                };
                let is_error = crate::provenance::json_truthy(b.get("is_error"));
                let rejected =
                    is_error && crate::session_activity::error_class(&output) == "user_rejected";
                self.result(id, &output, is_error, rejected, ts);
            }
            return;
        }
        self.headless |= obj
            .get("entrypoint")
            .and_then(Value::as_str)
            .is_some_and(|e| e.starts_with("sdk"));
        let text = crate::provenance::turn_text(obj);
        self.user_turn(obj, &text, ts, &uuid);
    }

    /// One codex rollout row.
    fn codex_row(&mut self, obj: &Value) {
        self.line += 1;
        let ts = crate::provenance::turn_ts_epoch(obj);
        let uuid = format!("L{}", self.line);
        let Some(p) = obj.get("payload") else {
            return;
        };
        let ptype = p.get("type").and_then(Value::as_str).unwrap_or("");
        if obj.get("type").and_then(Value::as_str) == Some("session_meta") {
            self.headless = p
                .get("originator")
                .and_then(Value::as_str)
                .is_some_and(|o| o.contains("exec"));
            return;
        }
        match ptype {
            "message" => {
                let role = p.get("role").and_then(Value::as_str).unwrap_or("");
                let text =
                    crate::lead_hygiene::message_text(p.get("content").unwrap_or(&Value::Null));
                if role == "assistant" {
                    self.assistant_text(&text, ts, &uuid);
                } else if role == "user" {
                    let text = crate::provenance::turn_text(obj);
                    self.user_turn(obj, &text, ts, &uuid);
                }
            }
            "function_call" | "custom_tool_call" | "local_shell_call" => {
                let tool = p.get("name").and_then(Value::as_str).unwrap_or("shell");
                let raw = crate::transcript_activity::codex_row_call_text(obj).unwrap_or_default();
                let id = p.get("call_id").and_then(Value::as_str).unwrap_or("");
                self.call(id, tool, &codex_command(&raw));
            }
            "function_call_output" | "custom_tool_call_output" => {
                let id = p.get("call_id").and_then(Value::as_str).unwrap_or("");
                let output = crate::session_activity::codex_output_text(p).into_owned();
                let is_error = crate::session_activity::codex_output_is_error(&output);
                self.result(id, &output, is_error, false, ts);
            }
            "turn_aborted" => {
                let marker = json!({"type": "user", "message": {"role": "user",
                    "content": "[Request interrupted by user]"}});
                self.user_turn(&marker, "[Request interrupted by user]", ts, &uuid);
            }
            _ => {}
        }
    }
}

/// The text a claude tool call acts on: the command, the file, the pattern.
fn claude_command(tool: &str, input: &Value) -> String {
    let field = |k: &str| {
        input
            .get(k)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    match tool {
        "Bash" => field("command"),
        "Read" | "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => field("file_path"),
        "Grep" => format!("{} {}", field("pattern"), field("path"))
            .trim()
            .to_string(),
        "Glob" => field("pattern"),
        _ => clip(&input.to_string(), 600),
    }
}

/// The shell command inside a codex call's argument JSON (`cmd`, or the
/// last word of a `command` array), else the raw text.
fn codex_command(raw: &str) -> String {
    let Ok(v) = serde_json::from_str::<Value>(raw) else {
        return raw.to_string();
    };
    if let Some(cmd) = v.get("cmd").and_then(Value::as_str) {
        return cmd.to_string();
    }
    match v.get("command") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .last()
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        _ => raw.to_string(),
    }
}

/// Every row of one transcript, read line by line so a large file never
/// sits in memory whole.
fn session_rows(
    harness: &'static str,
    file: &crate::provenance::SessionFile,
    bus: &BusIndex,
    witness: &mut crate::operator_witness::SubmitIndex,
) -> SessionOut {
    let mut b = Builder::new(harness, &file.session_id, bus, witness);
    let Ok(fh) = std::fs::File::open(&file.path) else {
        return SessionOut::default();
    };
    for line in std::io::BufReader::new(fh).lines() {
        let Ok(line) = line else {
            break;
        };
        let Ok(obj) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if harness == "codex" {
            b.codex_row(&obj);
        } else {
            b.claude_row(&obj);
        }
    }
    b.out
}

// ------------------------------------------------------------------ splits

fn label_of(row: &Value) -> Option<String> {
    let q = row.get("questions")?.as_object()?.values().next()?;
    match q.get("label")? {
        Value::String(s) => Some(s.clone()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn meta_str<'a>(row: &'a Value, key: &str) -> &'a str {
    row.get("_meta")
        .and_then(|m| m.get(key))
        .and_then(Value::as_str)
        .unwrap_or("")
}

fn stratum(row: &Value) -> Option<String> {
    label_of(row).map(|l| format!("{}/{l}", meta_str(row, "kind")))
}

/// Per-stratum eval quotas by water-filling: an even share each, a stratum
/// capped at a third of its rows so train keeps the rest, the unspent
/// share spread over the strata that can take more.
fn quotas(totals: &BTreeMap<String, usize>, target: usize) -> BTreeMap<String, usize> {
    let mut quota: BTreeMap<String, usize> = BTreeMap::new();
    let mut open: Vec<(&String, usize)> = totals
        .iter()
        .map(|(k, &n)| (k, (n / 3).max(1).min(n)))
        .collect();
    let mut remaining = target;
    while !open.is_empty() && remaining > 0 {
        let share = remaining.div_ceil(open.len());
        let (small, rest): (Vec<_>, Vec<_>) = open.into_iter().partition(|(_, cap)| *cap <= share);
        if small.is_empty() {
            for (k, _) in &rest {
                quota.insert((*k).clone(), share.min(remaining));
                remaining = remaining.saturating_sub(share);
            }
            break;
        }
        for (k, cap) in small {
            quota.insert(k.clone(), cap);
            remaining = remaining.saturating_sub(cap);
        }
        open = rest;
    }
    quota
}

/// Whole sessions for the eval, in blake3 order: a session is taken when it
/// fills a stratum still short of its quota. Returns per session the rows
/// each stratum may emit as candidates.
fn select_eval(
    stats: &HashMap<String, BTreeMap<String, usize>>,
    target: usize,
) -> HashMap<String, BTreeMap<String, usize>> {
    let mut totals: BTreeMap<String, usize> = BTreeMap::new();
    for counts in stats.values() {
        for (s, n) in counts {
            *totals.entry(s.clone()).or_insert(0) += n;
        }
    }
    let quota = quotas(&totals, target);
    let mut order: Vec<&String> = stats.keys().collect();
    order.sort_by_key(|s| *blake3::hash(s.as_bytes()).as_bytes());
    let mut filled: BTreeMap<String, usize> = BTreeMap::new();
    let mut picked = HashMap::new();
    for session in order {
        let counts = &stats[session];
        if counts.values().sum::<usize>() > EVAL_SESSION_MAX_ROWS {
            continue;
        }
        let mut take = BTreeMap::new();
        for (s, &n) in counts {
            let room = quota
                .get(s)
                .copied()
                .unwrap_or(0)
                .saturating_sub(filled.get(s).copied().unwrap_or(0));
            if room > 0 {
                take.insert(s.clone(), n.min(room));
            }
        }
        if take.is_empty() {
            continue;
        }
        for (s, n) in &take {
            *filled.entry(s.clone()).or_insert(0) += n;
        }
        picked.insert(session.clone(), take);
        if quota
            .iter()
            .all(|(s, q)| filled.get(s).copied().unwrap_or(0) >= *q)
        {
            break;
        }
    }
    picked
}

// ------------------------------------------------------------------ export

pub(crate) struct ExportCtx {
    pub(crate) bus: BusIndex,
    pub(crate) witness: crate::operator_witness::SubmitIndex,
    pub(crate) now: u64,
    pub(crate) eval_target: usize,
}

fn open_out(dir: &Path, name: &str) -> Result<BufWriter<std::fs::File>, String> {
    std::fs::File::create(dir.join(name))
        .map(BufWriter::new)
        .map_err(|e| {
            format!(
                "fno-agents intel: cannot write {}: {e}",
                dir.join(name).display()
            )
        })
}

fn write_line(w: &mut impl Write, row: &Value) -> Result<(), String> {
    writeln!(w, "{row}").map_err(|e| format!("fno-agents intel: write failed: {e}"))
}

fn read_rows(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// Rows of one written file whose session is in `sessions`, read a line at
/// a time: train alone can be gigabytes.
fn rows_from_sessions(path: &Path, sessions: &HashSet<String>) -> usize {
    let Ok(fh) = std::fs::File::open(path) else {
        return 0;
    };
    std::io::BufReader::new(fh)
        .lines()
        .map_while(Result::ok)
        .filter_map(|l| serde_json::from_str::<Value>(&l).ok())
        .filter(|r| sessions.contains(meta_str(r, "session")))
        .count()
}

/// Email addresses in one written file, read a line at a time.
fn emails_in_file(path: &Path) -> usize {
    let Ok(fh) = std::fs::File::open(path) else {
        return 0;
    };
    std::io::BufReader::new(fh)
        .lines()
        .map_while(Result::ok)
        .map(|l| email_rx().find_iter(&l).count())
        .sum()
}

/// True when an earlier export's pages already carry a checked item: a new
/// export would overwrite the user's labels.
fn pages_hold_checks(dir: &Path) -> bool {
    std::fs::read_dir(dir.join(PAGES_DIR))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .any(|t| {
            t.lines()
                .any(|l| l.trim().eq_ignore_ascii_case("- [x] checked"))
        })
}

/// The export: read every session, build and scrub rows, split, write.
/// Returns the manifest it wrote.
pub(crate) fn export(
    sources: &[Box<dyn TranscriptSource>],
    days: u64,
    ctx: &mut ExportCtx,
    dir: &Path,
) -> Result<Value, String> {
    if pages_hold_checks(dir) {
        return Err(format!(
            "fno-agents intel: {} holds checked eval items; run --corpus-ingest {} and move the directory before a new export",
            dir.join(PAGES_DIR).display(),
            dir.display()
        ));
    }
    std::fs::create_dir_all(dir)
        .map_err(|e| format!("fno-agents intel: cannot create {}: {e}", dir.display()))?;
    let started = std::time::Instant::now();
    let mut scrub = ScrubCounts::default();
    let mut tmp = open_out(dir, ROWS_TMP)?;
    let mut seen_ids: HashSet<String> = HashSet::new();
    let mut stats: HashMap<String, BTreeMap<String, usize>> = HashMap::new();
    let mut by_source: BTreeMap<String, u64> = BTreeMap::new();
    let (mut sessions_read, mut duplicates, mut seeds, mut typed) = (0u64, 0u64, 0u64, 0u64);
    let mut harnesses = Vec::new();
    for source in sources {
        harnesses.push(source.harness());
        for file in source.sessions(days) {
            sessions_read += 1;
            if sessions_read % 2_000 == 0 {
                eprintln!("fno-agents intel: corpus read {sessions_read} sessions");
            }
            let out = session_rows(source.harness(), &file, &ctx.bus, &mut ctx.witness);
            scrub.sensitive_drops += out.sensitive_drops;
            seeds += out.seeds;
            typed += out.typed;
            for row in out.rows {
                let Some(row) = scrub_row(row, &mut scrub) else {
                    continue;
                };
                if !seen_ids.insert(meta_str(&row, "id").to_string()) {
                    duplicates += 1;
                    continue;
                }
                let label = label_of(&row).unwrap_or_else(|| "unlabelled".into());
                let key = format!(
                    "{}/{}/{label}",
                    meta_str(&row, "kind"),
                    meta_str(&row, "source")
                );
                *by_source.entry(key).or_insert(0) += 1;
                if let Some(s) = stratum(&row) {
                    *stats
                        .entry(meta_str(&row, "session").to_string())
                        .or_default()
                        .entry(s)
                        .or_insert(0) += 1;
                }
                write_line(&mut tmp, &row)?;
            }
        }
    }
    tmp.flush()
        .map_err(|e| format!("fno-agents intel: write failed: {e}"))?;
    drop(tmp);

    let picked = select_eval(&stats, ctx.eval_target);
    let cutoff = ctx.now.saturating_sub(HOLDOUT_DAYS * 86_400);
    let mut outs = [
        open_out(dir, TRAIN)?,
        open_out(dir, HOLDOUT)?,
        open_out(dir, UNLABELLED)?,
    ];
    let mut counts = [0u64; 3];
    let mut candidates: Vec<Value> = Vec::new();
    let (mut eval_dropped, mut eval_unlabelled) = (0u64, 0u64);
    let mut emitted: HashMap<(String, String), usize> = HashMap::new();
    let tmp_path = dir.join(ROWS_TMP);
    let fh = std::fs::File::open(&tmp_path)
        .map_err(|e| format!("fno-agents intel: cannot read {}: {e}", tmp_path.display()))?;
    for line in std::io::BufReader::new(fh).lines() {
        let Ok(line) = line else {
            break;
        };
        let Ok(row) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let session = meta_str(&row, "session").to_string();
        let st = stratum(&row);
        if let Some(take) = picked.get(&session) {
            match st {
                None => eval_unlabelled += 1,
                Some(s) => {
                    let n = emitted.entry((session.clone(), s.clone())).or_insert(0);
                    if *n < take.get(&s).copied().unwrap_or(0) {
                        *n += 1;
                        candidates.push(row);
                    } else {
                        eval_dropped += 1;
                    }
                }
            }
            continue;
        }
        let slot = match st {
            None => 2,
            Some(_) => {
                let recent =
                    crate::intel::ts_secs(meta_str(&row, "ts")).is_some_and(|t| t >= cutoff);
                usize::from(recent)
            }
        };
        write_line(&mut outs[slot], &row)?;
        counts[slot] += 1;
    }
    for w in &mut outs {
        w.flush()
            .map_err(|e| format!("fno-agents intel: write failed: {e}"))?;
    }
    drop(outs);
    let _ = std::fs::remove_file(&tmp_path);
    let mut eval_out = open_out(dir, EVAL_CANDIDATES)?;
    for row in &candidates {
        write_line(&mut eval_out, row)?;
    }
    eval_out
        .flush()
        .map_err(|e| format!("fno-agents intel: write failed: {e}"))?;
    write_pages(dir, &candidates)?;

    // The leak count re-reads what was written, independent of the router.
    let eval_sessions: HashSet<String> = read_rows(&dir.join(EVAL_CANDIDATES))
        .iter()
        .map(|r| meta_str(r, "session").to_string())
        .collect();
    let leaks: usize = [TRAIN, HOLDOUT, UNLABELLED]
        .iter()
        .map(|name| rows_from_sessions(&dir.join(name), &eval_sessions))
        .sum();

    // Same independence: count what reached disk, not what the scrub reports.
    let emails_residual: usize = [TRAIN, HOLDOUT, UNLABELLED, EVAL_CANDIDATES]
        .iter()
        .map(|name| emails_in_file(&dir.join(name)))
        .sum();

    let reaction = |label: bool| -> u64 {
        by_source
            .iter()
            .filter(|(k, _)| k.starts_with("reaction/"))
            .filter(|(k, _)| k.ends_with("/unlabelled") != label)
            .map(|(_, v)| v)
            .sum()
    };
    let (labelled, unlabelled) = (reaction(true), reaction(false));
    let share = if labelled + unlabelled == 0 {
        0.0
    } else {
        labelled as f64 / (labelled + unlabelled) as f64
    };
    let manifest = json!({
        "version": 1,
        "created": rfc3339(Some(ctx.now as f64)),
        "host": host_name(),
        "days": days,
        "harnesses": harnesses,
        "holdout_days": HOLDOUT_DAYS,
        "sessions": {
            "read": sessions_read,
            "with_rows": stats.len(),
            "eval": eval_sessions.len(),
            "seed_turns": seeds,
            "typed_turns": typed,
        },
        "rows": {
            "train": counts[0],
            "holdout": counts[1],
            "unlabelled": counts[2],
            "eval_candidates": candidates.len(),
            "eval_overflow_dropped": eval_dropped,
            "eval_unlabelled_dropped": eval_unlabelled,
            "duplicates": duplicates,
        },
        "rows_by_source": by_source,
        "reaction_rule_coverage": {
            "labelled": labelled,
            "unlabelled": unlabelled,
            "share": share,
        },
        "scrub": {
            "secrets_masked": scrub.secrets_masked,
            "paths_masked": scrub.paths_masked,
            "emails_masked": scrub.emails_masked,
            "emails_residual": emails_residual,
            "residual_drops": scrub.residual_drops,
            "sensitive_drops": scrub.sensitive_drops,
        },
        "leaks": leaks,
        "wall_s": started.elapsed().as_secs(),
    });
    std::fs::write(
        dir.join(MANIFEST),
        serde_json::to_string_pretty(&manifest).unwrap_or_default() + "\n",
    )
    .map_err(|e| format!("fno-agents intel: cannot write manifest: {e}"))?;
    if emails_residual > 0 {
        return Err(format!(
            "fno-agents intel: {emails_residual} email address(es) survive the scrub in {}; no row may leave this machine",
            dir.display()
        ));
    }
    Ok(manifest)
}

pub(crate) fn host_name() -> String {
    std::process::Command::new("hostname")
        .arg("-s")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// A fence longer than any backtick run in the text.
fn fence(text: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for c in text.chars() {
        run = if c == '`' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    "`".repeat((longest + 1).max(3))
}

fn options(q: &Value) -> Vec<String> {
    match q.get("type").and_then(Value::as_str) {
        Some("noul") => vec!["true".into(), "false".into()],
        _ => q
            .get("criteria")
            .and_then(Value::as_object)
            .map(|c| c.keys().cloned().collect())
            .unwrap_or_default(),
    }
}

/// `eval-check/NN.md`, 50 items a page: the state, the options, a checkbox
/// and one `label:` line holding the mechanical label to confirm or change.
fn write_pages(dir: &Path, candidates: &[Value]) -> Result<(), String> {
    let pages = dir.join(PAGES_DIR);
    let _ = std::fs::remove_dir_all(&pages);
    std::fs::create_dir_all(&pages)
        .map_err(|e| format!("fno-agents intel: cannot create {}: {e}", pages.display()))?;
    for (n, chunk) in candidates.chunks(PAGE_ITEMS).enumerate() {
        let mut page = format!(
            "# Eval check {:02}\n\nFor each item, set `label:` to the right option and tick the box. `fno-agents intel --corpus-ingest` reads ticked items only.\n",
            n + 1
        );
        for row in chunk {
            let (kind, q) = row
                .get("questions")
                .and_then(Value::as_object)
                .and_then(|m| m.iter().next())
                .map(|(k, v)| (k.as_str(), v.clone()))
                .unwrap_or(("", Value::Null));
            let state = serde_json::to_string_pretty(row.get("state").unwrap_or(&Value::Null))
                .unwrap_or_default();
            let f = fence(&state);
            page.push_str(&format!(
                "\n## {}\n\n{kind}: {}\n\noptions: {}\n\n{f}json\n{state}\n{f}\n\n- [ ] checked\nlabel: {}\n",
                meta_str(row, "id"),
                q.get("instructions").and_then(Value::as_str).unwrap_or(""),
                options(&q).join(", "),
                label_of(row).unwrap_or_default(),
            ));
        }
        std::fs::write(pages.join(format!("{:02}.md", n + 1)), page)
            .map_err(|e| format!("fno-agents intel: cannot write page: {e}"))?;
    }
    Ok(())
}

/// Read the checked pages and write `eval.jsonl`: one row per ticked item
/// whose label is one of its options.
pub(crate) fn ingest(dir: &Path) -> Result<Value, String> {
    let cand_path = dir.join(EVAL_CANDIDATES);
    if !cand_path.is_file() {
        return Err(format!(
            "fno-agents intel: {} is missing; run --corpus {} first",
            cand_path.display(),
            dir.display()
        ));
    }
    let by_id: HashMap<String, Value> = read_rows(&cand_path)
        .into_iter()
        .map(|r| (meta_str(&r, "id").to_string(), r))
        .collect();
    let mut pages: Vec<PathBuf> = std::fs::read_dir(dir.join(PAGES_DIR))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("md"))
        .collect();
    pages.sort();
    let mut out = open_out(dir, EVAL)?;
    let (mut checked, mut written, mut invalid, mut changed) = (0u64, 0u64, 0u64, 0u64);
    for page in pages {
        let text = std::fs::read_to_string(&page).unwrap_or_default();
        for item in text.split("\n## ").skip(1) {
            let id = item.lines().next().unwrap_or("").trim();
            if !item
                .lines()
                .any(|l| l.trim().eq_ignore_ascii_case("- [x] checked"))
            {
                continue;
            }
            checked += 1;
            let label = item
                .lines()
                .filter_map(|l| l.strip_prefix("label:"))
                .last()
                .map(|l| l.trim().to_string())
                .unwrap_or_default();
            let Some(mut row) = by_id.get(id).cloned() else {
                invalid += 1;
                continue;
            };
            let Some((kind, q)) = row
                .get("questions")
                .and_then(Value::as_object)
                .and_then(|m| m.iter().next())
                .map(|(k, v)| (k.clone(), v.clone()))
            else {
                invalid += 1;
                continue;
            };
            if !options(&q).contains(&label) {
                invalid += 1;
                continue;
            }
            let proposed = label_of(&row).unwrap_or_default();
            if proposed != label {
                changed += 1;
            }
            let value = match q.get("type").and_then(Value::as_str) {
                Some("noul") => Value::Bool(label == "true"),
                _ => Value::String(label),
            };
            row["questions"][&kind]["label"] = value;
            row["_meta"]["checked"] = json!(true);
            row["_meta"]["proposed"] = json!(proposed);
            write_line(&mut out, &row)?;
            written += 1;
        }
    }
    out.flush()
        .map_err(|e| format!("fno-agents intel: write failed: {e}"))?;
    Ok(
        json!({"checked": checked, "written": written, "invalid": invalid, "changed": changed,
              "eval": dir.join(EVAL).display().to_string()}),
    )
}

/// `fno-agents intel --corpus <dir> [--days N] [-H claude,codex]` or
/// `--corpus-ingest <dir>`.
pub fn run(args: &[String]) -> i32 {
    let mut dir: Option<PathBuf> = None;
    let mut ingest_dir: Option<PathBuf> = None;
    let mut days = 0u64;
    let mut harnesses: Vec<&'static str> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let value = args.get(i + 1);
        match args[i].as_str() {
            "--corpus" | "--corpus-ingest" => {
                let Some(v) = value else {
                    eprintln!("fno-agents intel: {} needs a directory", args[i]);
                    return 2;
                };
                let path = crate::cargo_build_dirs::expand_home(Path::new(v));
                if args[i] == "--corpus" {
                    dir = Some(path);
                } else {
                    ingest_dir = Some(path);
                }
                i += 1;
            }
            "--days" => match value.and_then(|v| v.parse::<u64>().ok()) {
                Some(v) => {
                    days = v;
                    i += 1;
                }
                None => {
                    eprintln!("fno-agents intel: --days needs a non-negative integer");
                    return 2;
                }
            },
            "-H" | "--harness" => {
                let Some(v) = value else {
                    eprintln!("fno-agents intel: --harness needs a comma-separated list");
                    return 2;
                };
                for part in v.split(',').map(str::trim).filter(|p| !p.is_empty()) {
                    let h = match part {
                        "claude" => "claude",
                        "codex" => "codex",
                        other => {
                            eprintln!(
                                "fno-agents intel: --corpus reads claude and codex only, not {other}"
                            );
                            return 2;
                        }
                    };
                    if !harnesses.contains(&h) {
                        harnesses.push(h);
                    }
                }
                i += 1;
            }
            other => {
                eprintln!("fno-agents intel: unknown flag {other} beside --corpus");
                return 2;
            }
        }
        i += 1;
    }
    let result = match (dir, ingest_dir) {
        (Some(_), Some(_)) => {
            eprintln!("fno-agents intel: --corpus and --corpus-ingest are exclusive");
            return 2;
        }
        (None, Some(d)) => ingest(&d),
        (Some(d), None) => {
            if harnesses.is_empty() {
                harnesses = vec!["claude", "codex"];
            }
            let mut sources: Vec<Box<dyn TranscriptSource>> = Vec::new();
            if harnesses.contains(&"claude") {
                sources.push(Box::new(ClaudeSource {
                    projects_dir: crate::claude_drive::claude_projects_dir(),
                    roots: None,
                }));
            }
            if harnesses.contains(&"codex") {
                sources.push(Box::new(CodexSource {
                    sessions_dir: None,
                    roots: None,
                }));
            }
            let home = crate::paths::AgentsHome::from_env();
            let fno_dir = home
                .root()
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from(".fno"));
            let mut ctx = ExportCtx {
                bus: BusIndex::load(&crate::intel::bus_log_path(&fno_dir)),
                witness: crate::operator_witness::SubmitIndex::load(&home.events_jsonl()),
                now: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
                eval_target: EVAL_TARGET,
            };
            export(&sources, days, &mut ctx, &d)
        }
        (None, None) => {
            eprintln!("fno-agents intel: --corpus needs a directory");
            return 2;
        }
    };
    match result {
        Ok(v) => {
            println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
            0
        }
        Err(why) => {
            eprintln!("{why}");
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        root: PathBuf,
        projects: PathBuf,
        out: PathBuf,
    }

    fn fixture(tag: &str) -> Fixture {
        let root =
            std::env::temp_dir().join(format!("fno-intel-corpus-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let projects = root.join("projects");
        std::fs::create_dir_all(projects.join("-repo")).unwrap();
        Fixture {
            out: root.join("out"),
            projects,
            root,
        }
    }

    fn write_session(fx: &Fixture, session: &str, rows: &[Value]) {
        let body: String = rows.iter().map(|r| format!("{r}\n")).collect();
        std::fs::write(
            fx.projects.join("-repo").join(format!("{session}.jsonl")),
            body,
        )
        .unwrap();
    }

    fn assistant(uuid: &str, ts: &str, blocks: Value) -> Value {
        json!({"type": "assistant", "uuid": uuid, "timestamp": ts,
               "message": {"role": "assistant", "content": blocks}})
    }

    fn user(uuid: &str, ts: &str, content: Value) -> Value {
        json!({"type": "user", "uuid": uuid, "timestamp": ts, "entrypoint": "cli",
               "message": {"role": "user", "content": content}})
    }

    /// A session where cargo test fails, the agent claims the tests pass,
    /// and the user approves; the failing output carries a token.
    fn claim_session(token: &str) -> Vec<Value> {
        vec![
            user("u0", "2026-01-01T00:00:00Z", json!("fix the parser please")),
            assistant(
                "a1",
                "2026-01-01T00:00:05Z",
                json!([
                    {"type": "tool_use", "id": "t1", "name": "Bash",
                     "input": {"command": "cargo test -p parser"}}
                ]),
            ),
            user(
                "r1",
                "2026-01-01T00:00:30Z",
                json!([
                    {"type": "tool_result", "tool_use_id": "t1", "is_error": true,
                     "content": format!("Exit code 101\nGITHUB_TOKEN={token}\ntest parse_one ... FAILED\ntest result: FAILED. 3 passed; 1 failed")}
                ]),
            ),
            assistant(
                "a2",
                "2026-01-01T00:01:00Z",
                json!([
                    {"type": "text", "text": "All tests pass now. Shall I open the PR?"}
                ]),
            ),
            user("u1", "2026-01-01T00:02:00Z", json!("yes")),
        ]
    }

    fn ctx(target: usize) -> ExportCtx {
        ExportCtx {
            bus: BusIndex::empty(),
            witness: crate::operator_witness::SubmitIndex::empty(),
            now: chrono::DateTime::parse_from_rfc3339("2026-02-01T00:00:00Z")
                .unwrap()
                .timestamp() as u64,
            eval_target: target,
        }
    }

    fn sources(fx: &Fixture) -> Vec<Box<dyn TranscriptSource>> {
        vec![Box::new(ClaudeSource {
            projects_dir: fx.projects.clone(),
            roots: None,
        })]
    }

    #[test]
    fn a_failed_cargo_test_contradicts_the_later_claim_and_the_token_never_reaches_disk() {
        let fx = fixture("claim");
        let token = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
        write_session(&fx, "s-claim", &claim_session(token));
        let manifest = export(&sources(&fx), 0, &mut ctx(0), &fx.out).unwrap();
        let rows: Vec<Value> = [TRAIN, HOLDOUT, UNLABELLED, EVAL_CANDIDATES]
            .iter()
            .flat_map(|n| read_rows(&fx.out.join(n)))
            .collect();
        let claim = rows
            .iter()
            .find(|r| {
                meta_str(r, "kind") == "claim_support" && meta_str(r, "source") == "tests/same"
            })
            .expect("a same-kind claim row");
        assert_eq!(label_of(claim).as_deref(), Some("contradicts"));
        assert_eq!(meta_str(claim, "provenance"), "tool_output");
        let reaction = rows
            .iter()
            .find(|r| meta_str(r, "kind") == "reaction")
            .expect("a reaction row");
        assert_eq!(label_of(reaction).as_deref(), Some("approve"));
        let ask = rows
            .iter()
            .find(|r| meta_str(r, "kind") == "is_ask")
            .expect("an is_ask row");
        assert_eq!(label_of(ask).as_deref(), Some("true"));
        for entry in std::fs::read_dir(&fx.out).unwrap().flatten() {
            if entry.path().is_file() {
                let text = std::fs::read_to_string(entry.path()).unwrap();
                assert!(
                    !text.contains(token),
                    "token leaked into {}",
                    entry.path().display()
                );
            }
        }
        assert!(manifest["scrub"]["secrets_masked"].as_u64().unwrap() >= 1);
        assert_eq!(manifest["leaks"], json!(0));
        assert_eq!(manifest["scrub"]["emails_residual"], json!(0));
        let _ = std::fs::remove_dir_all(&fx.root);
    }

    #[test]
    fn an_email_address_is_masked_in_every_string_and_none_survives_the_rescan() {
        let mut counts = ScrubCounts::default();
        let row = json!({"text": "mail jane.doe+x@example.co.uk now", "meta": {"to": ["a@b.io"]}});
        let out = scrub_row(row, &mut counts).expect("masked rows are kept");
        assert!(!email_rx().is_match(&out.to_string()));
        assert!(out.to_string().contains("[email]"));
        assert_eq!(counts.emails_masked, 2);
        assert_eq!(counts.residual_drops, 0);
    }

    #[test]
    fn an_eval_session_lends_no_row_to_train_or_holdout_and_ingest_keeps_checked_labels() {
        let fx = fixture("split");
        for n in 0..8 {
            write_session(&fx, &format!("s-{n}"), &claim_session("x"));
        }
        let manifest = export(&sources(&fx), 0, &mut ctx(4), &fx.out).unwrap();
        let eval = read_rows(&fx.out.join(EVAL_CANDIDATES));
        assert!(!eval.is_empty(), "the target picks eval sessions");
        let eval_sessions: HashSet<&str> = eval.iter().map(|r| meta_str(r, "session")).collect();
        for name in [TRAIN, HOLDOUT, UNLABELLED] {
            for row in read_rows(&fx.out.join(name)) {
                assert!(
                    !eval_sessions.contains(meta_str(&row, "session")),
                    "{name} holds a row from eval session {}",
                    meta_str(&row, "session")
                );
            }
        }
        assert!(manifest["rows"]["train"].as_u64().unwrap() > 0);
        assert_eq!(manifest["leaks"], json!(0));

        // Tick the first item with a changed label, leave the rest unticked.
        let page_path = fx.out.join(PAGES_DIR).join("01.md");
        let page = std::fs::read_to_string(&page_path).unwrap();
        let first = meta_str(&eval[0], "id").to_string();
        let opts = options(
            eval[0]["questions"]
                .as_object()
                .unwrap()
                .values()
                .next()
                .unwrap(),
        );
        let proposed = label_of(&eval[0]).unwrap();
        let other = opts.iter().find(|o| **o != proposed).unwrap().clone();
        let (head, tail) = page.split_once(&format!("## {first}\n")).unwrap();
        let tail = tail.replacen("- [ ] checked", "- [x] checked", 1).replacen(
            &format!("label: {proposed}"),
            &format!("label: {other}"),
            1,
        );
        std::fs::write(&page_path, format!("{head}## {first}\n{tail}")).unwrap();
        let receipt = ingest(&fx.out).unwrap();
        assert_eq!(receipt["written"], json!(1));
        assert_eq!(receipt["changed"], json!(1));
        let checked = read_rows(&fx.out.join(EVAL));
        assert_eq!(label_of(&checked[0]).as_deref(), Some(other.as_str()));
        // A second export refuses while checked pages would be overwritten.
        assert!(export(&sources(&fx), 0, &mut ctx(4), &fx.out).is_err());
        let _ = std::fs::remove_dir_all(&fx.root);
    }

    #[test]
    fn the_seed_and_the_rules_label_only_what_they_are_sure_of() {
        assert_eq!(reaction_rule("yes go"), Some("approve"));
        assert_eq!(reaction_rule("nope"), Some("reject"));
        assert_eq!(reaction_rule("no, use the other branch"), Some("correct"));
        assert_eq!(reaction_rule("what about the tests?"), None);
        assert_eq!(reaction_rule("yes but rename it first"), None);
        assert_eq!(record_class("Read", "/repo/src/lib.rs"), "file_read");
        assert_eq!(
            record_class("Bash", "cd /repo && cargo test | tail -3"),
            "tool_output"
        );
        assert_eq!(
            record_class("Bash", "gh pr create --title x --body y"),
            "mixed"
        );

        // A headless session's first row is a seed: it closes no turn and
        // counts as no typed reply.
        let mut witness = crate::operator_witness::SubmitIndex::empty();
        let bus = BusIndex::empty();
        let mut b = Builder::new("claude", "s-seed", &bus, &mut witness);
        let mut seed = user(
            "u0",
            "2026-01-01T00:00:00Z",
            json!("You are a reviewer. Review x."),
        );
        seed["entrypoint"] = json!("sdk-cli");
        b.claude_row(&seed);
        b.claude_row(&user(
            "u1",
            "2026-01-01T00:00:10Z",
            json!("now this one is typed"),
        ));
        assert_eq!((b.out.seeds, b.out.typed), (1, 1));

        // Agent mail is its own row class and never a typed turn.
        b.claude_row(&assistant(
            "a1",
            "2026-01-01T00:00:20Z",
            json!([
                {"type": "text", "text": "Plan ready. Ship it?"}
            ]),
        ));
        b.claude_row(&user(
            "m1",
            "2026-01-01T00:00:30Z",
            json!("`@lead · fmail-0123456789ab · re plan`\nyes go"),
        ));
        assert_eq!(b.out.typed, 1);
        let mail = b
            .out
            .rows
            .iter()
            .find(|r| meta_str(r, "kind") == "agent_mail")
            .expect("a mail row");
        assert_eq!(label_of(mail).as_deref(), Some("approve"));
        assert!(!b.out.rows.iter().any(|r| meta_str(r, "kind") == "reaction"));

        // A chained call whose tests passed still supports the claim.
        assert_eq!(
            kind_label(0, "test result: ok. 5 passed", true),
            Some("supports")
        );
        assert_eq!(
            kind_label(0, "Exit code 1\nkilled", true),
            Some("contradicts")
        );

        // A turn that read a sensitive path lends no row, is_ask included.
        let rows = b.out.rows.len();
        b.claude_row(&assistant("a2", "2026-01-01T00:01:00Z", json!([
            {"type": "tool_use", "id": "t9", "name": "Read", "input": {"file_path": "/repo/.env"}},
            {"type": "text", "text": "Read the config. Want me to rotate it?"}
        ])));
        b.claude_row(&user("u2", "2026-01-01T00:01:30Z", json!("yes")));
        assert_eq!(b.out.rows.len(), rows);
        assert!(b.out.sensitive_drops >= 2);
    }
}
