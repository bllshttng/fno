//! The finding classifier, the bounded attestation record, and the
//! per-round disposition comment, ported from the deleted Python leg
//! (`fno.review.findings` + the `fno do review post-dispositions` leaf) and
//! frozen byte-for-byte against its goldens
//! (`tests/review_findings_parity.rs`).
//!
//! The output contract is Python's: the record is ONE line with keys in the
//! Python dict order, serialized with json.dumps default separators
//! (`, ` / `: `), `null` for none, non-ASCII left raw. Hand-rolled like
//! `review_level::render`, because a serde struct would sort keys and break
//! byte parity. Line rendering (`finding_key`) prints the JSON `line` value
//! the way Python `str()` does; whitespace trimming follows Python
//! `str.strip()` (a Unicode superset of Rust `trim`).
//!
//! The door nests under `review-level` (`findings record|categories|post`)
//! so fno-agents gains no root verb. `disposition_gate` reads the shipped
//! category list from here: one copy of the blocking rule's allowlist, the
//! producer and the gate both.

use crate::publish_review::Gh;
use serde_json::Value;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// The categories an ALLOWLIST admits as harmless. Shipped default for
/// `config.review.nonblocking_categories`; a configured list EXTENDS this
/// rather than replacing it (a partial list must never drop a shipped
/// harmless category, or a `typo` finding would start blocking every PR
/// that names one extra entry). `disposition_gate` reads this same constant.
pub(crate) const DEFAULT_NONBLOCKING_CATEGORIES: &[&str] = &[
    "style",
    "formatting",
    "naming",
    "docs",
    "typo",
    "nit",
    "simplification",
    "test-coverage",
];

const DISPOSITIONS: &[&str] = &["fixed", "declined", "nonblocking"];
const SUMMARY_MAX: usize = 240;
const SUMMARY_ELLIPSIS: &str = "...";
const RECORD_CHAR_BUDGET: usize = 48_000;
const FINDINGS_COUNT_CAP: usize = 200;

// --- Python string semantics the port must reproduce ----------------------

/// Python `str.isspace()`: Rust `char::is_whitespace` misses U+0085 and the
/// file/ group/group separators U+001C..=U+001F, and `str.strip()` strips
/// those. The goldens pin a U+001F-padded category that must clean to
/// `typo`, so trimming follows Python's set, not Rust's.
fn py_isspace(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n'
            | '\u{0b}'
            | '\u{0c}'
            | '\r'
            | '\u{1c}'..='\u{1f}'
            | ' '
            | '\u{85}'
            | '\u{a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
    )
}

fn py_trim(s: &str) -> &str {
    s.trim_matches(py_isspace)
}

/// A string value stripped, or None for absent/empty/non-string
/// (Python `_clean`).
fn clean_str(item: &Value, key: &str) -> Option<String> {
    match item.get(key) {
        Some(Value::String(s)) => {
            let trimmed = py_trim(s);
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        _ => None,
    }
}

/// Python `repr` of a str, close enough for the container shapes the
/// `line` field's goldens pin (single quotes preferred, the quote
/// backslash-escaped).
fn py_repr_str(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::new();
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Python `str()` of a JSON value, for the `line` component of a finding
/// key: `4` -> `4`, `4.0` -> `4.0`, `true` -> `True`, `null` -> `None`,
/// `"7"` -> `7` (a top-level string is itself; repr only applies inside
/// containers), `[1, 2]` -> `[1, 2]`, `{"a": 1}` -> `{'a': 1}`. Map keys
/// print in serde's (sorted) order; the goldens pin single-key maps.
fn py_str_value(v: &Value) -> String {
    match v {
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.to_string()
            } else if let Some(u) = n.as_u64() {
                u.to_string()
            } else {
                format!("{:?}", n.as_f64().unwrap_or_default())
            }
        }
        Value::String(s) => s.to_string(),
        Value::Array(rows) => {
            let parts: Vec<String> = rows.iter().map(py_repr_value).collect();
            format!("[{}]", parts.join(", "))
        }
        Value::Object(map) => {
            let parts: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{}: {}", py_repr_str(k), py_repr_value(v)))
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
    }
}

/// Python `repr()` of a JSON value: the shape a container prints its
/// members with.
fn py_repr_value(v: &Value) -> String {
    match v {
        Value::String(s) => py_repr_str(s),
        other => py_str_value(other),
    }
}

// --- The record vocabulary -------------------------------------------------

#[derive(Debug, Clone)]
struct FindingRecord {
    category: Option<String>,
    verdict: Option<String>,
    file: Option<String>,
    line: Option<Value>,
    summary: Option<String>,
    failure_scenario: Option<String>,
    autofix_class: Option<String>,
    owner: Option<String>,
    /// A non-object element, or an object carrying none of the vocabulary:
    /// the fail-closed answer is BLOCKING, never zero findings.
    unmappable: bool,
}

impl FindingRecord {
    fn from_item(item: &Value) -> Self {
        if !item.is_object() {
            return FindingRecord {
                category: None,
                verdict: None,
                file: None,
                line: None,
                summary: None,
                failure_scenario: None,
                autofix_class: None,
                owner: None,
                unmappable: true,
            };
        }
        let category = clean_str(item, "category");
        let verdict = clean_str(item, "verdict");
        let file = clean_str(item, "file");
        // Python `dict.get` reads an explicit JSON null as absent, so the
        // key's middle stays empty (`f1.py::perf`, the record_line_null
        // golden); keeping Some(Value::Null) would print `None` there.
        let line = match item.get("line") {
            None | Some(Value::Null) => None,
            other => other.cloned(),
        };
        let summary = clean_str(item, "summary");
        let failure_scenario = clean_str(item, "failure_scenario");
        let autofix_class = clean_str(item, "autofix_class").and_then(|cleaned| {
            let lowered = cleaned.to_lowercase();
            if lowered == "gated_auto" || lowered == "manual" || lowered == "advisory" {
                Some(lowered)
            } else {
                None
            }
        });
        let owner = clean_str(item, "owner");
        let mut record = FindingRecord {
            category,
            verdict,
            file,
            line,
            summary,
            failure_scenario,
            autofix_class,
            owner,
            unmappable: false,
        };
        if record.category.is_none()
            && record.verdict.is_none()
            && record.file.is_none()
            && record.line.is_none()
            && record.summary.is_none()
            && record.failure_scenario.is_none()
        {
            record.unmappable = true;
        }
        record
    }

    fn has_required_fields(&self) -> bool {
        self.file.is_some() && self.summary.is_some() && self.failure_scenario.is_some()
    }

    /// BLOCKING or not, by one rule evaluated in order: unmappable or
    /// missing a required field blocks; a CONFIRMED verdict blocks whatever
    /// the category says; a category in the allowlist is nonblocking;
    /// everything else blocks.
    fn blocking(&self, allow: &[String]) -> bool {
        if self.unmappable || !self.has_required_fields() {
            return true;
        }
        if let Some(verdict) = &self.verdict {
            if verdict.to_lowercase() == "confirmed" {
                return true;
            }
        }
        match &self.category {
            Some(category) => {
                let lowered = category.to_lowercase();
                !allow.iter().any(|a| *a == lowered)
            }
            None => true,
        }
    }

    fn key(&self) -> String {
        let line = self.line.as_ref().map(py_str_value).unwrap_or_default();
        let category = self
            .category
            .as_deref()
            .map(|c| c.to_lowercase())
            .unwrap_or_default();
        format!(
            "{}:{}:{}",
            self.file.as_deref().unwrap_or(""),
            line,
            category
        )
    }
}

#[derive(Debug, Clone)]
struct Primitive {
    category: Option<String>,
    verdict: Option<String>,
    blocking: bool,
    has_required_fields: bool,
    finding_key: String,
    summary: Option<String>,
    autofix_class: Option<String>,
    owner: Option<String>,
}

#[derive(Debug, Clone)]
struct Disposition {
    finding_key: String,
    disposition: String,
    reason: String,
}

#[derive(Debug)]
struct Record {
    blocking: usize,
    nonblocking: usize,
    /// (category, count) in first-seen order; a missing category counts
    /// under "".
    histogram: Vec<(String, u64)>,
    findings: Vec<Primitive>,
    truncated: bool,
    /// An empty dispositions array leaves the key off the record.
    dispositions: Option<Vec<Disposition>>,
    review_round: Option<i64>,
}

impl Record {
    fn record_json(&self) -> String {
        let esc = |s: &str| Value::String(s.to_string()).to_string();
        let opt = |v: &Option<String>| v.as_deref().map(esc).unwrap_or_else(|| "null".into());
        let hist: Vec<String> = self
            .histogram
            .iter()
            .map(|(k, n)| format!("{}: {}", esc(k), n))
            .collect();
        let prims: Vec<String> = self
            .findings
            .iter()
            .map(|p| {
                format!(
                    "{{\"category\": {}, \"verdict\": {}, \"blocking\": {}, \
                     \"has_required_fields\": {}, \"finding_key\": {}, \"summary\": {}, \
                     \"autofix_class\": {}, \"owner\": {}}}",
                    opt(&p.category),
                    opt(&p.verdict),
                    p.blocking,
                    p.has_required_fields,
                    esc(&p.finding_key),
                    opt(&p.summary),
                    opt(&p.autofix_class),
                    opt(&p.owner),
                )
            })
            .collect();
        let mut out = format!(
            "{{\"findings_blocking\": {}, \"findings_nonblocking\": {}, \
             \"category_histogram\": {{{}}}, \"findings\": [{}]",
            self.blocking,
            self.nonblocking,
            hist.join(", "),
            prims.join(", "),
        );
        if self.truncated {
            out.push_str(", \"findings_truncated\": true");
        }
        if let Some(dispositions) = &self.dispositions {
            let rows: Vec<String> = dispositions
                .iter()
                .map(|d| {
                    format!(
                        "{{\"finding_key\": {}, \"disposition\": {}, \"reason\": {}}}",
                        esc(&d.finding_key),
                        esc(&d.disposition),
                        esc(&d.reason),
                    )
                })
                .collect();
            out.push_str(&format!(", \"dispositions\": [{}]", rows.join(", ")));
        }
        if let Some(round) = self.review_round {
            out.push_str(&format!(", \"review_round\": {round}"));
        }
        out.push('}');
        out
    }

    /// The length Python measured: `len(json.dumps(record,
    /// ensure_ascii=False))` - characters, never bytes.
    fn serialized_len(&self) -> usize {
        self.record_json().chars().count()
    }
}

/// Re-cap an already-bounded summary to `limit`, or drop it at 0: detail is
/// shed first and findings are dropped only after every summary is gone.
fn shrink_summary(value: Option<String>, limit: usize) -> Option<String> {
    let value = value?;
    if limit == 0 {
        return None;
    }
    let count = value.chars().count();
    if count <= limit {
        return Some(value);
    }
    let cut: String = value.chars().take(limit - SUMMARY_ELLIPSIS.len()).collect();
    Some(format!("{cut}{SUMMARY_ELLIPSIS}"))
}

// --- Config ----------------------------------------------------------------

/// The effective harmless-category set: the shipped default EXTENDED by
/// `review.nonblocking_categories` (stripped, lower-cased). The config read
/// rides `agents_config::config_value_deep`; a value the typed Python
/// settings model would refuse to load (anything but a list of strings)
/// yields the shipped list with no line, exactly as the Python producer's
/// `except` arm did.
fn resolved_categories(cwd: &Path) -> Vec<String> {
    let configured =
        crate::agents_config::config_value_deep(cwd, &["review", "nonblocking_categories"]);
    extend_categories(default_categories(), configured.as_ref())
}

fn default_categories() -> Vec<String> {
    DEFAULT_NONBLOCKING_CATEGORIES
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// The pure resolver: the shipped list, extended in order by a configured
/// list of strings; any other configured shape (a non-list, a non-string
/// entry) leaves the shipped list standing.
fn extend_categories(mut out: Vec<String>, configured: Option<&toml::Value>) -> Vec<String> {
    if let Some(toml::Value::Array(rows)) = configured {
        if rows.iter().all(|r| matches!(r, toml::Value::String(_))) {
            for row in rows {
                if let toml::Value::String(s) = row {
                    let cleaned = py_trim(s).to_lowercase();
                    if !cleaned.is_empty() && !out.contains(&cleaned) {
                        out.push(cleaned);
                    }
                }
            }
        }
    }
    out
}

// --- The record builder ----------------------------------------------------

/// One findings payload into the bounded record. Refusals carry the exact
/// Python sentence: a refusal, never an empty record.
fn build_record(payload: &Value, allow: &[String]) -> Result<Record, String> {
    let mut dispositions: Vec<Disposition> = Vec::new();
    let mut review_round: Option<i64> = None;
    let items: Vec<Value> = match payload {
        Value::Object(map) => {
            if let Some(raw) = map.get("dispositions").filter(|v| !v.is_null()) {
                let Some(rows) = raw.as_array() else {
                    return Err("dispositions must be an array".to_string());
                };
                for entry in rows {
                    let Some(entry) = entry.as_object() else {
                        return Err("each disposition must be an object".to_string());
                    };
                    let key = entry.get("finding_key");
                    let key = match key {
                        Some(Value::String(k)) if !py_trim(k).is_empty() => k.clone(),
                        _ => {
                            return Err(format!(
                                "each disposition needs finding_key, a disposition in \
                                 ['fixed', 'declined', 'nonblocking'], and a non-empty reason"
                            ))
                        }
                    };
                    let disposition = entry
                        .get("disposition")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if !DISPOSITIONS.contains(&disposition) {
                        return Err(format!(
                            "each disposition needs finding_key, a disposition in \
                             ['fixed', 'declined', 'nonblocking'], and a non-empty reason"
                        ));
                    }
                    let reason = entry.get("reason");
                    let reason = match reason {
                        Some(Value::String(r)) if !py_trim(r).is_empty() => r.clone(),
                        _ => {
                            return Err(format!(
                                "each disposition needs finding_key, a disposition in \
                                 ['fixed', 'declined', 'nonblocking'], and a non-empty reason"
                            ))
                        }
                    };
                    if disposition == "declined" && !reason.contains('"') {
                        return Err(
                            "a declined disposition's reason must cite the finding: a double-quoted \
                             proving line at the current head, plus the failure scenario that does \
                             not hold"
                                .to_string(),
                        );
                    }
                    dispositions.push(Disposition {
                        finding_key: key,
                        disposition: disposition.to_string(),
                        reason,
                    });
                }
            }
            if let Some(raw) = map.get("review_round").filter(|v| !v.is_null()) {
                let Some(round) = raw.as_i64() else {
                    return Err("review_round must be a non-negative integer".to_string());
                };
                if round < 0 {
                    return Err("review_round must be a non-negative integer".to_string());
                }
                review_round = Some(round);
            }
            let Some(rows) = map.get("findings").and_then(Value::as_array) else {
                return Err("'findings' is absent or not an array".to_string());
            };
            rows.clone()
        }
        Value::Array(rows) => rows.clone(),
        _ => return Err("records payload must be an array".to_string()),
    };

    let mut histogram: Vec<(String, u64)> = Vec::new();
    let mut records = Vec::new();
    for item in &items {
        let record = FindingRecord::from_item(item);
        let hist_key = record
            .category
            .as_deref()
            .map(|c| c.to_lowercase())
            .unwrap_or_default();
        match histogram.iter_mut().find(|(k, _)| *k == hist_key) {
            Some((_, n)) => *n += 1,
            None => histogram.push((hist_key, 1)),
        }
        records.push(record);
    }
    let blocking = records.iter().filter(|r| r.blocking(allow)).count();
    let nonblocking = records.len() - blocking;
    let findings: Vec<Primitive> = records
        .iter()
        .map(|r| {
            // The record's own summary, stripped and capped at the primitive
            // so one pathological finding cannot eat the array budget.
            let summary = r.summary.as_deref().map(|text| {
                if text.chars().count() <= SUMMARY_MAX {
                    text.to_string()
                } else {
                    let cut: String = text
                        .chars()
                        .take(SUMMARY_MAX - SUMMARY_ELLIPSIS.len())
                        .collect();
                    format!("{cut}{SUMMARY_ELLIPSIS}")
                }
            });
            Primitive {
                category: r.category.clone(),
                verdict: r.verdict.clone(),
                blocking: r.blocking(allow),
                has_required_fields: r.has_required_fields(),
                finding_key: r.key(),
                summary,
                autofix_class: r.autofix_class.clone(),
                owner: r.owner.clone(),
            }
        })
        .collect();

    let mut record = Record {
        blocking,
        nonblocking,
        histogram,
        findings,
        truncated: false,
        dispositions: if dispositions.is_empty() {
            None
        } else {
            Some(dispositions)
        },
        review_round,
    };

    // Shed summary DETAIL before dropping any finding: a truncated array
    // sets findings_truncated, which the gate reads as BLOCKING for the
    // remainder, so losing a sentence is recoverable and losing a finding
    // is not.
    if record.findings.len() > FINDINGS_COUNT_CAP {
        record.findings.truncate(FINDINGS_COUNT_CAP);
        record.truncated = true;
    }
    for limit in [120usize, 60, 30, 0] {
        if record.serialized_len() <= RECORD_CHAR_BUDGET {
            break;
        }
        for prim in record.findings.iter_mut() {
            prim.summary = shrink_summary(prim.summary.clone(), limit);
        }
    }
    while record.serialized_len() > RECORD_CHAR_BUDGET && !record.findings.is_empty() {
        record.findings.pop();
        record.truncated = true;
    }
    Ok(record)
}

// --- The round comment ------------------------------------------------------

fn render_round_comment(record: &Record, head: &str, round_no: i64, reviewer: &str) -> String {
    let dispositions: HashSet<&str> = record
        .dispositions
        .iter()
        .flatten()
        .map(|d| d.finding_key.as_str())
        .collect();
    let disposed = record
        .findings
        .iter()
        .filter(|f| dispositions.contains(f.finding_key.as_str()))
        .count();
    let head7: String = head.chars().take(7).collect();
    let mut lines = vec![
        format!("<!-- fno-review-round head={head} round={round_no} reviewer={reviewer} -->"),
        format!(
            "Review round {round_no} at {head7} ({reviewer}): {} findings, {disposed} disposed.",
            record.findings.len()
        ),
    ];
    for finding in &record.findings {
        let key = if finding.finding_key.is_empty() {
            "(unkeyed)".to_string()
        } else {
            finding.finding_key.clone()
        };
        // Last disposition wins: a corrected entry appended later is the
        // one the gate reads, and Python's dict renderer overwrote the
        // same way. First-match here would show the stale status.
        match record
            .dispositions
            .iter()
            .flatten()
            .filter(|d| d.finding_key == key)
            .last()
        {
            None => lines.push(format!("- {key}: no disposition")),
            Some(d) => lines.push(format!("- {key}: {} ({})", d.disposition, d.reason)),
        }
    }
    lines.join("\n")
}

/// The verified round the comment names: a nonzero `--review-round` wins,
/// else a non-empty `FNO_REVIEW_ROUND` (so `0` there means round 0), else a
/// nonzero payload round, else 1.
fn resolve_round(flag: Option<i64>, payload_round: Option<i64>) -> Result<i64, String> {
    if let Some(n) = flag.filter(|n| *n != 0) {
        return Ok(n);
    }
    if let Ok(raw) = std::env::var("FNO_REVIEW_ROUND") {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return trimmed
                .parse::<i64>()
                .map_err(|_| format!("FNO_REVIEW_ROUND is not an integer: {trimmed}"));
        }
    }
    if let Some(n) = payload_round.filter(|n| *n != 0) {
        return Ok(n);
    }
    Ok(1)
}

// --- The gh seam -----------------------------------------------------------

/// A `publish_review::Gh` viewed as a `pr_status_facts::GhProbe`, so REST
/// failures classify through the one `rest_reason` without a second gh
/// runner.
struct GhAsProbe<'a> {
    gh: &'a dyn crate::publish_review::Gh,
    cwd: PathBuf,
}

impl crate::pr_status_facts::GhProbe for GhAsProbe<'_> {
    fn run_gh(&self, _cwd: &Path, args: &[String]) -> Result<(bool, String, String), String> {
        let out = self.gh.run(args, None, &self.cwd);
        Ok((out.ok, out.stdout, out.stderr))
    }
}

fn gh_args(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

/// The current branch's open PR, the one REST query Python asked
/// (`state=all&head={owner}:{branch}&per_page=2`), served by
/// `pr_status::logs::resolve_current_pr` - no second reader. The `why`
/// carries the resolver's own failure sentence, the one Python echoed.
fn resolve_branch_pr(
    gh: &dyn crate::publish_review::Gh,
    cwd: &Path,
    slug: &str,
) -> Result<u64, String> {
    crate::pr_status::logs::resolve_current_pr(
        &GhAsProbe {
            gh,
            cwd: cwd.to_path_buf(),
        },
        cwd,
        slug,
    )
}

// --- The door ---------------------------------------------------------------

const USAGE: &str = "usage: fno-agents review-level findings <record|categories|post>";

/// The flags the door's actions accept, in either spelling the typer door
/// read: `--flag value` and `--flag=value`.
const KNOWN_FLAGS: &[&str] = &[
    "--findings-file",
    "--head",
    "--reviewer",
    "--pr",
    "--pr-number",
    "--review-round",
    "--root",
];

/// Normalize the argv into (flag, value) pairs; `--flag value` and
/// `--flag=value` spell the same option. Anything else is the usage
/// refusal, exit 2, the typer door's own posture.
fn flag_pairs(args: &[String]) -> Result<Vec<(&str, &str)>, String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let token = args[i].as_str();
        if let Some(eq) = token.find('=') {
            let (name, value) = token.split_at(eq);
            if !KNOWN_FLAGS.contains(&name) {
                return Err(format!("unknown flag: {token}"));
            }
            out.push((name, &value[1..]));
            i += 1;
        } else if KNOWN_FLAGS.contains(&token) {
            let value = args
                .get(i + 1)
                .ok_or_else(|| format!("{token} needs a value"))?;
            out.push((token, value.as_str()));
            i += 2;
        } else {
            return Err(format!("unknown flag: {token}"));
        }
    }
    Ok(out)
}

struct PostArgs {
    findings_file: PathBuf,
    head: String,
    reviewer: String,
    pr: Option<i64>,
    review_round: Option<i64>,
}

fn parse_post_args(args: &[String]) -> Result<PostArgs, String> {
    let mut out = PostArgs {
        findings_file: PathBuf::new(),
        head: String::new(),
        reviewer: "code-review".to_string(),
        pr: None,
        review_round: None,
    };
    for (flag, value) in flag_pairs(args)? {
        match flag {
            "--findings-file" => out.findings_file = PathBuf::from(value),
            "--head" => out.head = value.to_string(),
            "--reviewer" => out.reviewer = value.to_string(),
            "--pr" | "--pr-number" => {
                out.pr = Some(
                    value
                        .parse::<i64>()
                        .map_err(|_| format!("{flag} needs a number"))?,
                );
            }
            "--review-round" => {
                out.review_round = Some(
                    value
                        .parse::<i64>()
                        .map_err(|_| format!("{flag} needs a number"))?,
                );
            }
            _ => unreachable!(),
        }
    }
    if out.findings_file.as_os_str().is_empty() {
        return Err("--findings-file is required".to_string());
    }
    if out.head.is_empty() {
        return Err("--head is required".to_string());
    }
    Ok(out)
}

fn read_payload(path: &Path) -> Result<Value, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{} is not valid JSON: {e}", path.display()))
}

/// The `findings record` action: the bounded record as one line, or the
/// refusal reason alone on stderr with exit 2. This is the door the Python
/// `classify` leaf reads.
fn run_record(args: &[String]) -> i32 {
    let mut findings_file: Option<&str> = None;
    let pairs = match flag_pairs(args) {
        Ok(pairs) => pairs,
        Err(msg) => {
            eprintln!("{USAGE} ({msg})");
            return 2;
        }
    };
    for (flag, value) in pairs {
        match flag {
            "--findings-file" => findings_file = Some(value),
            _ => {
                eprintln!("{USAGE} (unknown flag: {flag})");
                return 2;
            }
        }
    }
    let Some(file) = findings_file else {
        eprintln!("{USAGE} (--findings-file is required)");
        return 2;
    };
    let path = PathBuf::from(file);
    let payload = match read_payload(&path) {
        Ok(p) => p,
        Err(reason) => {
            eprintln!("{reason}");
            return 2;
        }
    };
    let allow =
        resolved_categories(&std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    match build_record(&payload, &allow) {
        Ok(record) => {
            println!("{}", record.record_json());
            0
        }
        Err(reason) => {
            eprintln!("{reason}");
            2
        }
    }
}

/// The `findings categories` action: the resolved allowlist, sorted, for
/// the Python coverage gate.
fn run_categories(args: &[String]) -> i32 {
    let mut root: Option<&str> = None;
    let pairs = match flag_pairs(args) {
        Ok(pairs) => pairs,
        Err(msg) => {
            eprintln!("{USAGE} ({msg})");
            return 2;
        }
    };
    for (flag, value) in pairs {
        match flag {
            "--root" => root = Some(value),
            _ => {
                eprintln!("{USAGE} (unknown flag: {flag})");
                return 2;
            }
        }
    }
    let cwd = root
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let mut categories = resolved_categories(&cwd);
    categories.sort();
    let rows: Vec<String> = categories
        .iter()
        .map(|c| Value::String(c.clone()).to_string())
        .collect();
    println!("{{\"categories\": [{}]}}", rows.join(", "));
    0
}

/// The `findings post` action: the ported `post-dispositions` leaf. Exit
/// order is Python's: read and build refuse at 2, REST failures at 3, and
/// every skip (no dispositions, no PR, failed comments read, marker
/// already present) says why on stdout at 0.
fn run_post(args: &[String]) -> i32 {
    let parsed = match parse_post_args(args) {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("{USAGE} ({msg})");
            return 2;
        }
    };
    let payload = match read_payload(&parsed.findings_file) {
        Ok(p) => p,
        Err(reason) => {
            eprintln!("post-dispositions: {reason}");
            return 2;
        }
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let allow = resolved_categories(&cwd);
    let record = match build_record(&payload, &allow) {
        Ok(r) => r,
        Err(reason) => {
            eprintln!(
                "post-dispositions: {}: {reason}",
                parsed.findings_file.display()
            );
            return 2;
        }
    };
    if record.dispositions.is_none() {
        println!("post-dispositions: no dispositions in the record; nothing posted");
        return 0;
    }

    let gh = crate::publish_review::GhReal;
    let (slug, why) = match crate::pr_list::origin_slug(&cwd, None) {
        Ok(slug) => (slug, String::new()),
        Err(reason) => (
            String::new(),
            format!("could not resolve owner/repo: {reason}"),
        ),
    };
    if slug.is_empty() {
        eprintln!("post-dispositions: {why}");
        return 3;
    }
    let pr: i64 = match parsed.pr {
        Some(n) => n,
        None => match resolve_branch_pr(&gh, &cwd, &slug) {
            Ok(n) => n as i64,
            Err(why) => {
                println!("post-dispositions: no PR to post on ({why}); nothing posted");
                return 0;
            }
        },
    };
    // Head-pin check: the comment describes one head and must never attach
    // to another. Full shas on both sides.
    let info = gh.run(
        &gh_args(&["api", &format!("repos/{slug}/pulls/{pr}")]),
        None,
        &cwd,
    );
    if !info.ok {
        let probe = GhAsProbe {
            gh: &gh,
            cwd: cwd.clone(),
        };
        let reason = crate::pr_status::rest_reason(&probe, &cwd, &info.stderr);
        eprintln!("post-dispositions: PR read failed: {}", reason.text);
        return 3;
    }
    // The payload validates fail-closed, Python's `fetch_pr_info_rest`
    // shape for shape: the comment must never post off an unreadable PR
    // read, so a malformed body or a missing head sha refuses at 3.
    let pr_read_failed = |why: &str| {
        eprintln!("post-dispositions: PR read failed: {why}");
    };
    let pr_json: Value = match serde_json::from_str(info.stdout.trim()) {
        Ok(v) => v,
        Err(_) => {
            pr_read_failed("gh api pulls/<n> returned output that is not JSON");
            return 3;
        }
    };
    if !pr_json.is_object() {
        pr_read_failed("gh api pulls/<n> returned a JSON value that is not an object");
        return 3;
    }
    let head_obj = pr_json.get("head");
    if !head_obj.is_some_and(Value::is_object) {
        pr_read_failed("gh api pulls/<n> carried malformed head/base objects");
        return 3;
    }
    let pr_head = head_obj
        .and_then(|h| h.get("sha"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let Some(pr_head) = pr_head else {
        pr_read_failed("gh api pulls/<n> carried no head sha");
        return 3;
    };
    if pr_head != parsed.head {
        eprintln!(
            "post-dispositions: refusing: PR {pr} head {pr_head} is not the attested head {} \
             (--head takes the full sha); re-request review at the new head",
            parsed.head
        );
        return 3;
    }

    // Idempotent at (pr, head): the marker line is the key. A failed read
    // skips the post rather than risking a duplicate: a duplicate is the
    // one outcome this command cannot undo. --paginate walks every page, so
    // a busy PR cannot push its marker past the scan.
    let existing = gh.run(
        &gh_args(&[
            "api",
            "--paginate",
            &format!("repos/{slug}/issues/{pr}/comments?per_page=100"),
        ]),
        None,
        &cwd,
    );
    if !existing.ok {
        let tail: String = existing.stderr.trim().chars().take(200).collect();
        println!(
            "post-dispositions: comments read failed; nothing posted (re-run the emit to retry): {tail}"
        );
        return 0;
    }
    let marker = format!("<!-- fno-review-round head={} ", parsed.head);
    if existing.stdout.contains(&marker) {
        let head9: String = parsed.head.chars().take(9).collect();
        println!("post-dispositions: round comment for {head9} already posted");
        return 0;
    }

    let round_no = match resolve_round(parsed.review_round, record.review_round) {
        Ok(n) => n,
        Err(reason) => {
            eprintln!("post-dispositions: {reason}");
            return 1;
        }
    };
    let body = render_round_comment(&record, &parsed.head, round_no, &parsed.reviewer);
    let write = gh.run(
        &gh_args(&[
            "api",
            "-X",
            "POST",
            &format!("repos/{slug}/issues/{pr}/comments"),
            "-f",
            &format!("body={body}"),
        ]),
        None,
        &cwd,
    );
    if !write.ok {
        eprintln!(
            "post-dispositions: comment post failed: {}",
            write.stderr.trim()
        );
        return 3;
    }
    let head9: String = parsed.head.chars().take(9).collect();
    println!("post-dispositions: posted round comment on PR {pr} at {head9}");
    0
}

/// The `review-level findings` door. Dispatched from `review_doors` before
/// the level parser; `findings` is not a level word, so no valid level call
/// changes.
pub fn run_findings_door(rest: &[String]) -> i32 {
    let Some(action) = rest.first().map(String::as_str) else {
        eprintln!("{USAGE}");
        return 2;
    };
    let tail = &rest[1..];
    if tail.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        println!();
        println!("actions:");
        println!("  record --findings-file F             print the bounded attestation record");
        println!(
            "  categories [--root DIR]              print the resolved nonblocking categories"
        );
        println!(
            "  post --findings-file F --head SHA [--reviewer R] [--pr-number N] [--review-round N]"
        );
        println!("                                       post ONE per-round disposition comment");
        return 0;
    }
    match action {
        "record" => run_record(tail),
        "categories" => run_categories(tail),
        "post" => run_post(tail),
        other => {
            eprintln!("{USAGE} (unknown action: {other})");
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The pure table: the blocking rule in evaluation order, the category
    /// resolver's extend-and-degrade semantics, and the Python-`str` line
    /// printer. Everything else about this module is pinned by the goldens.
    #[test]
    fn blocking_rule_resolver_and_line_printer() {
        let allow = default_categories();
        // Fail closed: unmappable, missing required fields, CONFIRMED, and
        // unknown categories all block.
        let v = |payload: Value| FindingRecord::from_item(&payload);
        assert!(v(json!(7)).blocking(&allow));
        assert!(v(json!({"autofix_class": "manual"})).blocking(&allow));
        assert!(v(json!({"category": "typo", "file": "a.py", "line": 1})).blocking(&allow));
        assert!(
            v(json!({"category": "typo", "summary": "s", "failure_scenario": "f"}))
                .blocking(&allow)
        );
        assert!(v(json!({"category": "typo", "verdict": "CONFIRMED", "file": "a.py", "line": 1, "summary": "s", "failure_scenario": "f"})).blocking(&allow));
        assert!(v(json!({"category": "vibes", "file": "a.py", "line": 1, "summary": "s", "failure_scenario": "f"})).blocking(&allow));
        // The harmless paths: an allowlisted category without CONFIRMED.
        assert!(!v(json!({"category": "Typo", "file": "a.py", "line": 1, "summary": "s", "failure_scenario": "f"})).blocking(&allow));
        assert!(!v(json!({"category": "typo", "verdict": "PLAUSIBLE", "file": "a.py", "line": 1, "summary": "s", "failure_scenario": "f"})).blocking(&allow));
        // A CONFIRMED verdict blocks whatever the category says; a padded
        // verdict is the same verdict.
        assert!(v(json!({"category": "typo", "verdict": " confirmed ", "file": "a.py", "line": 1, "summary": "s", "failure_scenario": "f"})).blocking(&allow));

        // The resolver: extend, never replace; trim and lower; a malformed
        // shape degrades to the shipped list.
        let cfg = |rows: Value| toml::Value::try_from(rows).ok();
        assert_eq!(extend_categories(default_categories(), None), allow);
        let extended = extend_categories(
            default_categories(),
            cfg(json!([" Perf ", "vibes", "", "perf"])).as_ref(),
        );
        assert!(extended.contains(&"perf".to_string()));
        assert!(extended.contains(&"vibes".to_string()));
        assert_eq!(extended.len(), allow.len() + 1);
        assert_eq!(
            extend_categories(default_categories(), cfg(json!("perf")).as_ref()),
            allow
        );
        assert_eq!(
            extend_categories(default_categories(), cfg(json!(["perf", 5])).as_ref()),
            allow
        );

        // The line printer: Python str() shapes the finding_key's middle.
        let key = |line: Value| {
            v(json!({"category": "perf", "file": "c.py", "line": line, "summary": "s", "failure_scenario": "f"}))
                .key()
        };
        assert_eq!(key(json!(4)), "c.py:4:perf");
        assert_eq!(key(json!("7")), "c.py:7:perf");
        assert_eq!(key(json!(4.0)), "c.py:4.0:perf");
        assert_eq!(key(json!(true)), "c.py:True:perf");
        assert_eq!(key(json!(Value::Null)), "c.py::perf");
        assert_eq!(key(json!([1, 2])), "c.py:[1, 2]:perf");
        assert_eq!(key(json!({"a": 1})), "c.py:{'a': 1}:perf");

        // The comment shows the LAST disposition for a duplicated key: the
        // gate's insert overwrites and Python's dict renderer overwrote, so
        // a corrected entry appended later is the one the human reads.
        let dup = Record {
            blocking: 1,
            nonblocking: 0,
            histogram: vec![("correctness".to_string(), 1)],
            findings: vec![Primitive {
                category: Some("correctness".to_string()),
                verdict: None,
                blocking: true,
                has_required_fields: true,
                finding_key: "f.py:1:correctness".to_string(),
                summary: None,
                autofix_class: None,
                owner: None,
            }],
            truncated: false,
            dispositions: Some(vec![
                Disposition {
                    finding_key: "f.py:1:correctness".to_string(),
                    disposition: "declined".to_string(),
                    reason: "first".to_string(),
                },
                Disposition {
                    finding_key: "f.py:1:correctness".to_string(),
                    disposition: "fixed".to_string(),
                    reason: "second".to_string(),
                },
            ]),
            review_round: None,
        };
        let rendered = render_round_comment(&dup, "abc1234def", 2, "code-review");
        assert!(rendered.contains("- f.py:1:correctness: fixed (second)"));

        // Python str.strip()'s set, not Rust trim's: the unit separator
        // strips, like the golden case pins.
        assert_eq!(py_trim("\x1ftypo\x1f"), "typo");
        assert_eq!(py_trim(" typo "), "typo");

        // shrink_summary sheds characters and marks the cut.
        assert_eq!(
            shrink_summary(Some("s".repeat(240)), 240).unwrap().len(),
            240
        );
        let shrunk = shrink_summary(Some("s".repeat(241)), 240).unwrap();
        assert_eq!(shrunk.chars().count(), 240);
        assert!(shrunk.ends_with("..."));
        assert_eq!(shrink_summary(Some("abc".to_string()), 0), None);
    }
}
