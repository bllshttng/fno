//! The skill-diff proposer loop: observer failure patterns -> cited SKILL.md
//! diff -> PR. Ported from `cli/src/fno/skill_diff/` (engine, guards,
//! synthesize, cli); the Python leg is deleted in the same change.
//!
//! `tick` runs one proposer iteration over the newest unprocessed observer
//! run: level-gated (report = dry-run and default; assisted = actually open
//! the PR through the normal review gate - a human always merges). When no
//! diff helps it files a backlog node instead. `reconcile` is the loop
//! closer: it finds merged proposer PRs with no eval-closed receipt, replays
//! the targeted corpus items at the merge commit, folds a before/after delta
//! on the top failure dimension, and emits `skill_diff_eval_closed`.
//! All coordination routes through the event store - no new state files.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Map, Value};

pub const LOOP_NAME: &str = "skill_diff_proposer";

/// Terminal records that mark a run_id as already handled (idempotency key is
/// (run_id, skill_id)). `skill_eval_run_complete` is the trigger; these three
/// are the proposer's own outcomes.
const TERMINAL_TYPES: [&str; 3] = [
    "skill_diff_proposed",
    "skill_diff_no_diff_helps",
    "skill_diff_noop",
];

/// Local-maxima ceiling: how many consecutive run_complete cycles a skill's
/// top failure dimension may stay unchanged (with >=1 proposal in between)
/// before the proposer stops diffing that dimension and files a node.
const LOCAL_MAXIMA_WINDOW: usize = 3;

/// Additive-only diffs adding more than this need a justification.
const ADDITIVE_LINE_THRESHOLD: i64 = 15;
/// Trailing skill_diff_proposed events per skill the bloat flag sums over.
const BLOAT_WINDOW: usize = 5;
/// Net (added-removed) lines over the window that flags bloat.
const BLOAT_NET_GROWTH_THRESHOLD: i64 = 120;

/// Replayable blueprint dimensions; judge dimensions are never replayable (a
/// missing judge answer would read as a regression). Everything else closes
/// outcome-pending.
const REPLAYABLE_DIMS: [&str; 1] = ["collision_free"];

/// Judgment-about-judgment tier for the synthesis one-shot.
const SYNTH_MODEL: &str = "claude-fable-5";
const SYNTH_TIMEOUT_SECS: u64 = 900;
const REPLAY_TIMEOUT_SECS: u64 = 900;

/// Both spellings remain drivers during the one-release compatibility window.
const DRIVER_SKILLS: [&str; 6] = [
    "fno:think",
    "fno:target",
    "fno:megawalk",
    "fno:do",
    "fno:execute",
    "fno:blueprint",
];

const LEVELS: [&str; 3] = ["report", "assisted", "unattended"];

/// One synthesized edit. `old_text == ""` is a pure addition; a non-empty
/// old_text is a modification applied by literal replacement.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Hunk {
    pub file: String,
    pub old_text: String,
    pub new_text: String,
    pub cited_finding_ids: Vec<String>,
    pub rationale: String,
}

#[derive(Debug, Clone)]
pub struct Proposal {
    pub verdict: String,
    pub hunks: Vec<Hunk>,
    pub justification: Option<String>,
    pub no_diff_reason: Option<String>,
}

/// A redaction hit refused the PR open: terminal (a content problem won't fix
/// itself). Transient git/gh failures are `OpenError::Transient` so the caller
/// leaves the run unprocessed for retry.
#[derive(Debug)]
pub(crate) enum OpenError {
    Redaction(String),
    Transient(String),
}

// ---------------------------------------------------------------------------
// entry
// ---------------------------------------------------------------------------

/// `fno-agents loops skill-diff-tick|skill-diff-reconcile`, exec'd by the
/// front for `fno doctor skill-diff` (and the deprecated root spelling).
pub fn run(args: &[String]) -> i32 {
    if args
        .iter()
        .any(|a| matches!(a.as_str(), "--help" | "-h" | "help"))
    {
        print_help();
        return 0;
    }
    let op = args.first().map(String::as_str).unwrap_or("");
    let rest = if args.is_empty() { &[][..] } else { &args[1..] };
    match op {
        "tick" | "skill-diff-tick" => {}
        "reconcile" | "skill-diff-reconcile" => {}
        "-h" | "--help" | "help" | "" => {
            print_help();
            return 0;
        }
        other => {
            eprintln!("fno doctor skill-diff: expected tick or reconcile, got {other}");
            return 2;
        }
    }
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(e) => {
            eprintln!("fno doctor skill-diff: cannot resolve cwd: {e}");
            return 2;
        }
    };
    if op == "tick" || op == "skill-diff-tick" {
        let skill = match flag_value(rest, "--skill") {
            Some(skill) => skill,
            None => {
                eprintln!("fno doctor skill-diff tick: --skill is required");
                return 2;
            }
        };
        tick(&cwd, &skill)
    } else {
        let pr = match flag_value(rest, "--pr-number").or_else(|| flag_value(rest, "--pr")) {
            None => None,
            Some(v) => match v.parse::<i64>() {
                Ok(n) => Some(n),
                Err(_) => {
                    eprintln!("fno doctor skill-diff reconcile: --pr-number must be an integer");
                    return 2;
                }
            },
        };
        reconcile(&cwd, pr)
    }
}

fn print_help() {
    println!(
        "usage: fno doctor skill-diff tick --skill <name> | reconcile [--pr-number N]\n\
         \n\
         tick       one proposer iteration over the newest unprocessed observer run\n\
                    (report = dry-run and default; assisted = actually open the PR)\n\
         reconcile  re-eval merged proposer PRs with no eval-closed receipt and emit\n\
                    skill_diff_eval_closed; --pr-number scopes to one PR"
    );
}

fn flag_value(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

// ---------------------------------------------------------------------------
// paths, events io
// ---------------------------------------------------------------------------

/// Project log first (canonical), then the global log (best-effort); one path
/// when they resolve the same.
fn events_paths(cwd: &Path) -> Vec<PathBuf> {
    let project = crate::paths::events_path(cwd);
    let global = crate::paths::AgentsHome::from_env().events_jsonl();
    if project == global {
        vec![project]
    } else {
        vec![project, global]
    }
}

/// Committed events, skipping unreadable rows. The store commit is the write
/// boundary: a missing store reads empty, a corrupt row is logged, never a
/// crash - a standing loop tick must never die on one malformed append.
fn read_events(journal: &Path) -> Vec<Value> {
    let _ = crate::event_store::sync(journal);
    if !crate::event_store::store_path(journal).is_file() {
        return Vec::new();
    }
    match crate::event_store::query_events(journal, &crate::event_store::EventQuery::default()) {
        Ok(rows) => rows
            .iter()
            .enumerate()
            .filter_map(|(i, row)| match serde_json::from_str::<Value>(&row.line) {
                Ok(event) => Some(event),
                Err(e) => {
                    eprintln!("skill-diff: skipping corrupt events row {}: {e}", i + 1);
                    None
                }
            })
            .collect(),
        Err(e) => {
            eprintln!("skill-diff: events read failed, continuing with none: {e}");
            Vec::new()
        }
    }
}

/// Emit through the house envelope. Returns canonical-log success.
fn emit(journals: &[PathBuf], type_name: &str, data: Value) -> bool {
    let mut canonical_ok = false;
    for (i, p) in journals.iter().enumerate() {
        let emitter = crate::events::EventEmitter::new(p, "skill_diff");
        if let Err(e) = emitter.emit(type_name, &data) {
            eprintln!(
                "skill-diff: emit {type_name} to {} failed: {e}",
                p.display()
            );
        } else if i == 0 {
            canonical_ok = true;
        }
    }
    canonical_ok
}

fn emit_tick(journals: &[PathBuf], level: &str, outcome: &str) {
    emit(
        journals,
        "loop_tick",
        json!({"name": LOOP_NAME, "level": level, "outcome": outcome}),
    );
}

// ---------------------------------------------------------------------------
// engine: the pure event-processing core
// ---------------------------------------------------------------------------

fn data_of(event: &Value) -> &Map<String, Value> {
    static EMPTY: std::sync::OnceLock<Map<String, Value>> = std::sync::OnceLock::new();
    event
        .get("data")
        .and_then(Value::as_object)
        .unwrap_or_else(|| EMPTY.get_or_init(Map::new))
}

fn data_owned(event: &Value) -> Map<String, Value> {
    event
        .get("data")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

fn str_of<'a>(map: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    map.get(key).and_then(Value::as_str)
}

/// The event's `type` field. Events arrive as bare `Value`s; only their
/// `data` object is a map, so this is `str_of`'s event-level sibling.
fn ev_type(event: &Value) -> Option<&str> {
    event.get("type").and_then(Value::as_str)
}

/// run_ids with a run_complete for `skill_id` but no terminal proposer record,
/// oldest-first. A run_complete carrying a `skill_ref` KEY is a replay or
/// candidate eval, never a bare sweep, so it must not trigger the proposer.
/// The discriminator is FIELD PRESENCE, not truthiness: a HEAD replay emits
/// `skill_ref: null` (the key present), and only a sweep omits the key.
pub fn unprocessed_runs(events: &[Value], skill_id: &str) -> Vec<String> {
    let mut handled: BTreeSet<(String, String)> = BTreeSet::new();
    let mut complete: Vec<String> = Vec::new();
    for e in events {
        let ty = ev_type(e).unwrap_or("");
        let d = data_of(e);
        if TERMINAL_TYPES.contains(&ty) {
            if let (Some(rid), Some(sid)) = (str_of(d, "run_id"), str_of(d, "skill_id")) {
                handled.insert((rid.to_string(), sid.to_string()));
            }
        } else if ty == "skill_eval_run_complete"
            && str_of(d, "skill_id") == Some(skill_id)
            && !d.contains_key("skill_ref")
        {
            if let Some(rid) = str_of(d, "run_id") {
                if !complete.iter().any(|r| r == rid) {
                    complete.push(rid.to_string());
                }
            }
        }
    }
    complete
        .into_iter()
        .filter(|rid| !handled.contains(&(rid.clone(), skill_id.to_string())))
        .collect()
}

pub fn run_complete_event<'e>(events: &'e [Value], run_id: &str) -> Option<&'e Value> {
    events.iter().find(|e| {
        ev_type(e) == Some("skill_eval_run_complete")
            && str_of(data_of(e), "run_id") == Some(run_id)
    })
}

/// skill_eval_finding data dicts for `run_id`, excluding tool faults and,
/// when `skill_id` is given, every other skill's findings.
pub fn findings_for_run(
    events: &[Value],
    run_id: &str,
    skill_id: Option<&str>,
) -> Vec<Map<String, Value>> {
    let mut out = Vec::new();
    for e in events {
        if ev_type(e) != Some("skill_eval_finding") {
            continue;
        }
        let d = data_of(e);
        if str_of(d, "run_id") != Some(run_id) {
            continue;
        }
        if skill_id.is_some_and(|sid| str_of(d, "skill_id") != Some(sid)) {
            continue;
        }
        if d.get("tool_fault") == Some(&Value::Bool(true)) {
            continue;
        }
        out.push(d.clone());
    }
    out
}

/// [{dimension, fail_count}] descending. Prefer the run_complete's own
/// ranking; fall back to counting non-tool-fault fail findings.
pub fn failure_ranking(
    events: &[Value],
    run_id: &str,
    skill_id: Option<&str>,
) -> Vec<(String, i64)> {
    if let Some(rc) = run_complete_event(events, run_id) {
        if let Some(ranking) = data_of(rc).get("failure_ranking").and_then(Value::as_array) {
            if !ranking.is_empty() {
                return ranking
                    .iter()
                    .filter_map(|r| {
                        Some((
                            r.get("dimension")?.as_str()?.to_string(),
                            r.get("fail_count")?.as_i64()?,
                        ))
                    })
                    .collect();
            }
        }
    }
    let mut counts: BTreeMap<String, i64> = BTreeMap::new();
    for f in findings_for_run(events, run_id, skill_id) {
        let dim = str_of(&f, "dimension").unwrap_or("");
        if str_of(&f, "verdict") == Some("fail") && !dim.is_empty() {
            *counts.entry(dim.to_string()).or_insert(0) += 1;
        }
    }
    let mut rows: Vec<(String, i64)> = counts.into_iter().collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    rows
}

/// True when at least one non-tool-fault finding is fail or degraded: a run
/// whose findings are all pass is a no-op.
pub fn has_actionable_findings(events: &[Value], run_id: &str, skill_id: Option<&str>) -> bool {
    findings_for_run(events, run_id, skill_id)
        .iter()
        .any(|f| matches!(str_of(&f, "verdict"), Some("fail") | Some("degraded")))
}

pub fn top_dimension(events: &[Value], run_id: &str, skill_id: Option<&str>) -> Option<String> {
    failure_ranking(events, run_id, skill_id)
        .into_iter()
        .next()
        .map(|(d, _)| d)
}

/// The top failure dimension has resisted proposals across the window.
/// Mechanical, not judged: the last LOCAL_MAXIMA_WINDOW run_complete cycles
/// (including the current run) all share one top dimension, and at least one
/// proposal landed in that span.
pub fn local_maxima_tripped(events: &[Value], skill_id: &str, run_id: &str) -> bool {
    let Some(current_top) = top_dimension(events, run_id, Some(skill_id)) else {
        return false;
    };
    let mut seq: Vec<String> = Vec::new();
    for e in events {
        if ev_type(e) == Some("skill_eval_run_complete")
            && str_of(data_of(e), "skill_id") == Some(skill_id)
        {
            if let Some(rid) = str_of(data_of(e), "run_id") {
                if !seq.iter().any(|r| r == rid) {
                    seq.push(rid.to_string());
                }
            }
            if str_of(data_of(e), "run_id") == Some(run_id) {
                break;
            }
        }
    }
    if seq.len() < LOCAL_MAXIMA_WINDOW {
        return false;
    }
    let window = &seq[seq.len() - LOCAL_MAXIMA_WINDOW..];
    if window
        .iter()
        .any(|rid| top_dimension(events, rid, Some(skill_id)).as_deref() != Some(&current_top))
    {
        return false;
    }
    events.iter().any(|e| {
        ev_type(e) == Some("skill_diff_proposed")
            && str_of(data_of(e), "skill_id") == Some(skill_id)
            && data_of(e)
                .get("run_id")
                .and_then(Value::as_str)
                .is_some_and(|rid| window.iter().any(|w| w == rid))
    })
}

/// This skill's skill_diff_proposed data dicts, append order (bloat input).
pub fn prior_proposed(events: &[Value], skill_id: &str) -> Vec<Map<String, Value>> {
    events
        .iter()
        .filter(|e| {
            ev_type(e) == Some("skill_diff_proposed")
                && str_of(data_of(e), "skill_id") == Some(skill_id)
        })
        .map(data_owned)
        .collect()
}

/// The Python list repr (`['a', 'b']`) two stdout lines print through str().
fn py_list(items: &[String]) -> String {
    let inner: Vec<String> = items.iter().map(|i| format!("'{i}'")).collect();
    format!("[{}]", inner.join(", "))
}

/// Assemble the PR body: one section per hunk naming its cited finding IDs,
/// the justification section (if additive-only), the bloat flag (if tripped),
/// and both skill hashes. Pattern-level prose only - the redaction guard runs
/// over the returned text before the PR opens.
pub fn build_pr_body(
    run_id: &str,
    skill_id: &str,
    hunks: &[Hunk],
    justification: Option<&str>,
    bloat: Option<(i64, usize, i64)>,
    version_observed: &str,
    version_against: &str,
    is_review_skill: bool,
) -> String {
    let mut lines: Vec<String> = vec![
        format!("## Skill-diff proposal: `{skill_id}`"),
        String::new(),
        format!(
            "Synthesized from observer run `{run_id}`. Every hunk below cites the \
             observed failure it responds to; a reviewer judges the evidence the \
             same way they would any PR description."
        ),
        String::new(),
        format!("- skill version observed by the sweep: `{version_observed}`"),
        format!("- skill version this diff is against: `{version_against}`"),
    ];
    if version_observed != version_against {
        lines.push(
            "- note: the skill file moved between eval and proposal; the diff is \
             computed against current content (AC9-FR)."
                .to_string(),
        );
    }
    lines.push(String::new());
    if let Some((net, window, threshold)) = bloat {
        lines.push(format!(
            "> **bloat_review_needed**: this skill's last {window} proposals net \
             +{net} lines (threshold {threshold}). Flagged, not blocked - the merge \
             gate is the control point."
        ));
        lines.push(String::new());
    }
    if let Some(justification) = justification.map(str::trim).filter(|j| !j.is_empty()) {
        lines.push("### Justification (additive-only diff)".to_string());
        lines.push(String::new());
        lines.push(justification.to_string());
        lines.push(String::new());
    }
    if is_review_skill {
        lines.push(
            "> Replay v1 covers /blueprint only. This /review diff is validated by \
             sweep-mode evidence and post-merge sweeps, not a pre-merge replay \
             comparison."
                .to_string(),
        );
        lines.push(String::new());
    }
    lines.push("### Cited hunks".to_string());
    lines.push(String::new());
    for (i, h) in hunks.iter().enumerate() {
        let cites = h.cited_finding_ids.join(", ");
        lines.push(format!(
            "**{}. `{}`** \u{2014} addresses: {}",
            i + 1,
            h.file,
            cites
        ));
        let rationale = h.rationale.trim();
        if !rationale.is_empty() {
            lines.push(format!("  {rationale}"));
        }
        lines.push(String::new());
    }
    lines.push("---".to_string());
    lines.push(
        "_Structurally motivated by replay/sweep evidence; outcome confirmation \
         accrues from post-merge observer sweeps. A human merges - this loop is \
         assisted, never unattended._"
            .to_string(),
    );
    lines.join("\n")
}

// ---------------------------------------------------------------------------
// guards: deterministic mechanical checks
// ---------------------------------------------------------------------------

/// Count non-empty content lines; a blank string is zero lines, not one.
fn content_lines(text: &str) -> i64 {
    text.lines().filter(|l| !l.trim().is_empty()).count() as i64
}

/// (added, removed) across all hunks. removed == 0 means additive-only.
pub fn count_lines(hunks: &[Hunk]) -> (i64, i64) {
    let mut added = 0;
    let mut removed = 0;
    for h in hunks {
        added += content_lines(&h.new_text);
        removed += content_lines(&h.old_text);
    }
    (added, removed)
}

/// Drop hunks that cite zero findings; every surviving hunk points at a
/// specific observed failure. Returns (kept, dropped).
pub fn filter_cited_hunks(hunks: &[Hunk]) -> (Vec<Hunk>, Vec<Hunk>) {
    let mut kept = Vec::new();
    let mut dropped = Vec::new();
    for h in hunks {
        if h.cited_finding_ids.iter().any(|c| !c.is_empty()) {
            kept.push(h.clone());
        } else {
            dropped.push(h.clone());
        }
    }
    (kept, dropped)
}

/// True when the diff only adds lines and adds more than the threshold: a
/// proposer that only ever appends bloats a skill, so the synthesis output
/// must carry a justification for such a diff to open a PR.
pub fn additive_only_needs_justification(hunks: &[Hunk]) -> bool {
    let (added, removed) = count_lines(hunks);
    removed == 0 && added > ADDITIVE_LINE_THRESHOLD
}

/// Cumulative bloat over a trailing window of this skill's prior proposals.
/// Flags, never blocks: the return renders into the PR body.
pub fn bloat_flag(prior_proposed: &[Map<String, Value>]) -> Option<(i64, usize, i64)> {
    let recent_len = prior_proposed.len().min(BLOAT_WINDOW);
    let recent = &prior_proposed[prior_proposed.len() - recent_len..];
    let net: i64 = recent
        .iter()
        .map(|d| {
            d.get("added_lines").and_then(Value::as_i64).unwrap_or(0)
                - d.get("removed_lines").and_then(Value::as_i64).unwrap_or(0)
        })
        .sum();
    (net > BLOAT_NET_GROWTH_THRESHOLD).then_some((net, recent_len, BLOAT_NET_GROWTH_THRESHOLD))
}

/// Scan PR-body / commit-message text for surfaces that must never go public:
/// proposer PRs land on the PUBLIC repo but cite findings distilled from a
/// corpus spanning private client work. Refuse the open on any hit; opaque
/// finding IDs remain the citation mechanism.
pub fn redaction_violations(text: &str, project_names: &[String]) -> Vec<String> {
    let mut hits = Vec::new();
    for needle in ["internal/", "~/.fno", "/.fno/"] {
        if text.contains(needle) {
            hits.push(needle.to_string());
        }
    }
    let lowered = text.to_lowercase();
    for name in project_names {
        let name = name.trim();
        if name.is_empty() || name == "fno" {
            continue;
        }
        if contains_token(&lowered, &name.to_lowercase()) {
            hits.push(format!("project:{name}"));
        }
    }
    hits
}

/// Word-ish boundary check: the char before and after the match must not be
/// a lowercase alnum, so a short project name never matches inside an
/// unrelated word.
fn contains_token(haystack: &str, token: &str) -> bool {
    if token.is_empty() {
        return false;
    }
    let mut start = 0;
    while let Some(pos) = haystack[start..].find(token) {
        let at = start + pos;
        let end = at + token.len();
        let before_ok = haystack[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_ascii_lowercase() && !c.is_ascii_digit());
        let after_ok = haystack[end..]
            .chars()
            .next()
            .is_none_or(|c| !c.is_ascii_lowercase() && !c.is_ascii_digit());
        if before_ok && after_ok {
            return true;
        }
        // Advance one full char, not one byte: a multibyte first char would
        // make `at + 1` a non-boundary and the next slice a panic.
        start = at + token.chars().next().map_or(1, char::len_utf8);
    }
    false
}

// ---------------------------------------------------------------------------
// synthesis: prompt build, dispatch, parse
// ---------------------------------------------------------------------------

/// Compose the synthesis prompt. Bounded evidence only (each finding's
/// first 500 evidence chars, never a full transcript).
pub fn build_prompt(
    skill_id: &str,
    skill_files: &[(String, String)],
    findings: &[Map<String, Value>],
    ranking: &[(String, i64)],
    history: &[Map<String, Value>],
) -> String {
    let driver_note = if DRIVER_SKILLS.contains(&skill_id) {
        "\nThis is a DRIVER skill: your diff must not introduce a self-containment \
         CI violation - no ${REPO_ROOT}/scripts/ refs, no ../../ path escapes.\n"
    } else {
        ""
    };
    let ev = findings
        .iter()
        .map(|f| {
            let dim = str_of(&f, "dimension").unwrap_or("None");
            let verdict = str_of(&f, "verdict").unwrap_or("None");
            let cid = match f.get("corpus_item_id") {
                Some(Value::String(s)) => s.clone(),
                _ => "None".to_string(),
            };
            let evidence = str_of(&f, "evidence").unwrap_or("");
            let evidence: String = evidence.chars().take(500).collect();
            format!("- [{dim}/{verdict}] finding_id={cid}: {evidence}")
        })
        .collect::<Vec<_>>()
        .join("\n");
    let rank = if ranking.is_empty() {
        "(none)".to_string()
    } else {
        ranking
            .iter()
            .map(|(d, n)| format!("{d}={n}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let files = skill_files
        .iter()
        .map(|(p, b)| format!("### FILE: {p}\n{b}"))
        .collect::<Vec<_>>()
        .join("\n\n");
    let prior = if history.is_empty() {
        "(no prior proposals)".to_string()
    } else {
        history
            .iter()
            .map(|d| {
                let rid = str_of(&d, "run_id").unwrap_or("None");
                let added = d.get("added_lines").and_then(Value::as_i64).unwrap_or(0);
                let removed = d.get("removed_lines").and_then(Value::as_i64).unwrap_or(0);
                let cited: Vec<String> = d
                    .get("cited_finding_ids")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                format!("- run {rid}: +{added}/-{removed} cited {}", py_list(&cited))
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "You improve a Claude skill by editing its files in response to OBSERVED failures.\n\
         \n\
         Skill: {skill_id}\n\
         Failure ranking (dimension=fail_count): {rank}\n\
         {driver_note}\n\
         Cited failure evidence (bounded, one line each):\n\
         {ev}\n\
         \n\
         Prior proposals for this skill (do not re-propose what already did not help):\n\
         {prior}\n\
         \n\
         Current skill file contents:\n\
         {files}\n\
         \n\
         RULES:\n\
         - Prefer EDITS and DELETIONS over pure additions. A diff that only adds text is\n\
         \u{20}\u{20}suspect: if you must add more than {ADDITIVE_LINE_THRESHOLD} net new lines with zero\n\
         \u{20}\u{20}removals, you MUST fill \"justification\".\n\
         - Every hunk MUST cite at least one finding_id from the evidence above. An\n\
         \u{20}\u{20}uncited hunk will be dropped before the PR opens.\n\
         - old_text must match the current file content EXACTLY (it is applied by literal\n\
         \u{20}\u{20}replacement). Use \"\" for a pure addition appended to the file.\n\
         - If no edit will help - the failure is architectural, not a wording problem -\n\
         \u{20}\u{20}return verdict \"no_diff_helps\" with a \"no_diff_reason\".\n\
         \n\
         Return ONLY a JSON object:\n\
         {{\"verdict\":\"propose_pr\"|\"no_diff_helps\",\"hunks\":[{{\"file\",\"old_text\",\"new_text\",\"cited_finding_ids\",\"rationale\"}}],\"justification\":null,\"no_diff_reason\":null}}\n"
    )
}

/// Parse the agent's structured output, fenced ```json or bare, into a
/// Proposal. Fail closed: an unparseable or out-of-enum verdict is an error
/// rather than a silently empty PR. A no_diff_helps verdict wins even with
/// hunks present.
pub fn parse_proposal(text: &str) -> Result<Proposal, String> {
    let obj = extract_json(text)?;
    let verdict = obj
        .get("verdict")
        .and_then(Value::as_str)
        .unwrap_or("(missing)");
    if verdict != "propose_pr" && verdict != "no_diff_helps" {
        return Err(format!("unknown verdict: '{verdict}'"));
    }
    let mut hunks = Vec::new();
    if let Some(list) = obj.get("hunks") {
        let list = list
            .as_array()
            .ok_or_else(|| "hunks must be a list".to_string())?;
        for h in list {
            let map = h.as_object().ok_or("malformed hunk")?;
            let file = map
                .get("file")
                .ok_or("malformed hunk")?
                .as_str()
                .ok_or("hunk.file must be a string")?
                .to_string();
            for key in ["old_text", "new_text", "rationale"] {
                if let Some(v) = map.get(key) {
                    if !v.is_null() && v.as_str().is_none() {
                        return Err(format!("hunk.{key} must be a string"));
                    }
                }
            }
            let cites = match map.get("cited_finding_ids") {
                None | Some(Value::Null) => Vec::new(),
                Some(Value::Array(a)) => {
                    let mut out = Vec::new();
                    for c in a {
                        out.push(
                            c.as_str()
                                .ok_or("hunk.cited_finding_ids must be a list of strings")?
                                .to_string(),
                        );
                    }
                    out
                }
                Some(_) => {
                    return Err("hunk.cited_finding_ids must be a list of strings".to_string())
                }
            };
            let s = |k: &str| map.get(k).and_then(Value::as_str).unwrap_or("").to_string();
            hunks.push(Hunk {
                file,
                old_text: s("old_text"),
                new_text: s("new_text"),
                cited_finding_ids: cites,
                rationale: s("rationale"),
            });
        }
    }
    Ok(Proposal {
        verdict: verdict.to_string(),
        hunks,
        justification: obj
            .get("justification")
            .and_then(Value::as_str)
            .map(str::to_string),
        no_diff_reason: obj
            .get("no_diff_reason")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

/// Prefer a fenced block's inner text, else the whole text; then take the
/// outermost { .. } by first-brace / last-brace. A brace-span extraction, not
/// a single lazy regex (the hunks list is nested).
fn extract_json(text: &str) -> Result<Value, String> {
    let fence = text.find("```").and_then(|start| {
        let after = start + 3;
        let after = text[after..].strip_prefix("json").unwrap_or(&text[after..]);
        let inner_start = after.strip_prefix(['\n', '\r', ' ', '\t']).unwrap_or(after);
        inner_start.find("```").map(|rel| {
            let inner = &inner_start[..rel];
            inner.trim().to_string()
        })
    });
    let region = fence.as_deref().unwrap_or(text);
    let start = region.find('{');
    let end = region.rfind('}');
    let (Some(start), Some(end)) = (start, end) else {
        return Err("no JSON object found in synthesis output".to_string());
    };
    if end <= start {
        return Err("no JSON object found in synthesis output".to_string());
    }
    serde_json::from_str::<Value>(&region[start..=end])
        .map_err(|e| format!("invalid JSON: {e}"))
        .and_then(|v| {
            if v.is_object() {
                Ok(v)
            } else {
                Err("synthesis output is not a JSON object".to_string())
            }
        })
}

/// Dispatch the fable synthesis agent (a bare `claude -p` one-shot, behind
/// `--` so a leading-flag prompt stays the positional) and parse its output.
/// Any timeout, spawn failure or non-zero exit is an error; the caller treats
/// it as a tool fault and takes no action this tick.
fn synthesize(prompt: &str) -> Result<Proposal, String> {
    let output = bounded_run(
        Command::new("claude")
            .arg("-p")
            .arg("--model")
            .arg(SYNTH_MODEL)
            .arg("--")
            .arg(prompt),
        SYNTH_TIMEOUT_SECS,
    )?;
    let (rc, stdout, stderr) = output;
    if rc != 0 {
        return Err(format!(
            "synthesis agent exited {rc}: {}",
            stderr.chars().take(300).collect::<String>()
        ));
    }
    parse_proposal(&stdout)
}

/// Run a command under the shared poll-then-kill bound: spawn it in its own
/// process group, then wait through `blueprint_judge::bounded_wait`, the one
/// implementation both the judge's headless spawn and this family's one-shots
/// share. Returns (exit code, stdout, stderr); a timeout reports code 124.
fn bounded_run(cmd: &mut Command, timeout_secs: u64) -> Result<(i32, String, String), String> {
    let child = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|e| format!("spawn failed: {e}"))?;
    crate::blueprint_judge::bounded_wait(child, timeout_secs)
}

// ---------------------------------------------------------------------------
// plumbing: skill files, work map, git identity
// ---------------------------------------------------------------------------

fn skill_dir(repo_root: &Path, skill_id: &str) -> PathBuf {
    let short = skill_id.split(':').next_back().unwrap_or(skill_id);
    repo_root.join("skills").join(short)
}

/// SKILL.md only: a whole references/ tree would blow the synthesis prompt.
fn read_skill_files(repo_root: &Path, skill_id: &str) -> Vec<(String, String)> {
    let md = skill_dir(repo_root, skill_id).join("SKILL.md");
    match std::fs::read_to_string(&md) {
        Ok(body) => vec![(
            format!(
                "skills/{}/SKILL.md",
                skill_id.split(':').next_back().unwrap_or(skill_id)
            ),
            body,
        )],
        Err(_) => Vec::new(),
    }
}

/// git blob hash of the working-tree file, or "unknown" (best-effort
/// skill-version identity: an absent hash is never a blocker).
fn blob_hash(repo_root: &Path, rel_path: &str) -> String {
    match Command::new("git")
        .args(["hash-object", rel_path])
        .current_dir(repo_root)
        .output()
    {
        Ok(out) if out.status.success() => {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if s.is_empty() {
                "unknown".to_string()
            } else {
                s
            }
        }
        _ => "unknown".to_string(),
    }
}

/// Every project name in the workspace work-map (redaction guard input). A
/// malformed work-map must not uncap redaction, so errors read as no names.
fn project_names(cwd: &Path) -> Vec<String> {
    let mut names = Vec::new();
    if let Some(work) = crate::agents_config::config_lookup(cwd, &["work"])
        .and_then(|v| v.get("workspaces").and_then(|w| w.as_table()).cloned())
    {
        for ws in work.values() {
            if let Some(projects) = ws.get("projects").and_then(|p| p.as_array()) {
                for p in projects {
                    if let Some(name) = p.get("name").and_then(|n| n.as_str()) {
                        if !name.is_empty() {
                            names.push(name.to_string());
                        }
                    }
                }
            }
        }
    }
    names
}

/// The configured autonomy level for the proposer loop; an unset or invalid
/// level fails safe to "report" (observe only).
fn loop_level(cwd: &Path) -> String {
    crate::agents_config::config_lookup(cwd, &["loops", LOOP_NAME, "level"])
        .and_then(|v| v.as_str().map(str::to_string))
        .filter(|level| LEVELS.contains(&level.as_str()))
        .unwrap_or_else(|| "report".to_string())
}

fn loops_paused() -> bool {
    crate::loops_pause::is_paused()
}

/// A fresh after-run id sharing the observer's obs-<skill_id>-<UTC> shape but
/// no lineage with the before run.
fn mint_run_id(skill_id: &str) -> String {
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    format!("obs-{skill_id}-{stamp}")
}

fn pr_merged(repo_root: &Path, pr: i64) -> Option<bool> {
    let mut cmd = Command::new("gh");
    cmd.args([
        "pr",
        "view",
        &pr.to_string(),
        "--json",
        "state",
        "-q",
        ".state",
    ])
    .current_dir(repo_root);
    let out = crate::gh_budget::output(cmd).ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim() == "MERGED")
}

/// Best-effort merge commit SHA; None falls back to origin/main at the caller.
fn merge_sha(repo_root: &Path, pr: i64) -> Option<String> {
    let mut cmd = Command::new("gh");
    cmd.args([
        "pr",
        "view",
        &pr.to_string(),
        "--json",
        "mergeCommit",
        "-q",
        ".mergeCommit.oid",
    ])
    .current_dir(repo_root);
    let out = crate::gh_budget::output(cmd).ok()?;
    if !out.status.success() {
        return None;
    }
    let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
    // A null mergeCommit (squash race) prints the literal "null": treat it as
    // unrecoverable rather than `git worktree add null`.
    if sha.is_empty() || sha == "null" {
        None
    } else {
        Some(sha)
    }
}

/// Spawn one observer replay of a corpus item at the candidate ref, under the
/// shared after-run id. The heavy machinery is the observer's; reconcile only
/// loops it. Returns the subprocess rc (non-zero == that item did not score).
fn run_replay(repo_root: &Path, corpus_item: &str, skill_ref: &str, run_id_after: &str) -> i32 {
    let output = bounded_run(
        Command::new(crate::scrape::fno_bin())
            .args([
                "doctor",
                "observer",
                "replay",
                "--skill",
                "blueprint",
                "--corpus-item",
                corpus_item,
                "--skill-ref",
                skill_ref,
                "--run-id",
                run_id_after,
            ])
            .current_dir(repo_root),
        REPLAY_TIMEOUT_SECS,
    );
    match output {
        Ok((rc, _, stderr)) => {
            if rc != 0 {
                eprintln!(
                    "skill-diff: replay of {corpus_item} failed (rc={rc}): {}",
                    stderr.trim().chars().take(300).collect::<String>()
                );
            }
            rc
        }
        Err(e) => {
            eprintln!("skill-diff: replay of {corpus_item} failed: {e}");
            1
        }
    }
}

// ---------------------------------------------------------------------------
// node filing (the no-diff-helps path)
// ---------------------------------------------------------------------------

/// The last JSON object on the idea receipt's stdout. Reuses the scratch
/// helper rather than growing a second parser for the same shape.
fn idea_receipt(stdout: &str) -> Option<Map<String, Value>> {
    crate::scratch::last_json_object(stdout).and_then(|v| v.as_object().cloned())
}

/// The choice_required receipt decision: fold onto this skill's own node,
/// mint separate, or defer (no usable candidate).
enum FoldDecision {
    Fold(String),
    Separate,
}

/// A near-duplicate exists and NOTHING was filed by that call: fold onto this
/// skill's own node via --wave-of; a different node's title gets a separate
/// mint; no usable candidate defers. Reporting the candidate id as filed
/// without either would silently lose the finding.
fn fold_decision(receipt: &Map<String, Value>, skill_id: &str) -> Option<FoldDecision> {
    let candidate = receipt
        .get("candidates")
        .and_then(Value::as_array)
        .and_then(|c| c.first());
    let fold_id = candidate
        .and_then(|c| c.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string)?;
    let title_match = candidate
        .and_then(|c| c.get("title").and_then(Value::as_str))
        .is_some_and(|t| t.starts_with(&format!("skill-diff: {skill_id} failure")));
    Some(if title_match {
        FoldDecision::Fold(fold_id)
    } else {
        FoldDecision::Separate
    })
}

/// File a backlog node for an architectural finding, WITH implementation
/// guidance so it is a real follow-up. Returns the node id, or None on
/// failure (retried next tick - degrade, don't crash).
fn file_no_diff_node(
    repo_root: &Path,
    skill_id: &str,
    run_id: &str,
    reason: &str,
) -> Option<String> {
    let title = format!("skill-diff: {skill_id} failure looks architectural (run {run_id})");
    let details = format!(
        "The skill-diff proposer hit its local-maxima ceiling for `{skill_id}`: the top \
         failure dimension resisted proposals across the window ({reason}). Diffing that \
         dimension again is chasing a local maximum. Needs a design look, not another wording \
         tweak - the failure is likely architectural. See observer run {run_id}'s failure ranking."
    );
    let argv: Vec<String> = [
        "fno",
        "backlog",
        "idea",
        &title,
        "-d",
        &details,
        "-p",
        "p2",
        "-t",
        "feature",
        "--difficulty",
        "medium",
        "--json",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let out = Command::new(&argv[0])
        .args(&argv[1..])
        .current_dir(repo_root)
        .output();
    let out = match out {
        Ok(o) if o.status.success() => o,
        Ok(o) => {
            eprintln!(
                "skill-diff: filing no-diff-helps node failed (rc={}): {}",
                o.status.code().unwrap_or(1),
                String::from_utf8_lossy(&o.stderr)
                    .trim()
                    .chars()
                    .take(200)
                    .collect::<String>()
            );
            return None;
        }
        Err(e) => {
            eprintln!("skill-diff: filing no-diff-helps node failed: {e}");
            return None;
        }
    };
    let receipt = match idea_receipt(&String::from_utf8_lossy(&out.stdout)) {
        Some(r) => r,
        None => {
            eprintln!(
                "skill-diff: unreadable idea receipt: {:?}",
                String::from_utf8_lossy(&out.stdout)
                    .chars()
                    .take(120)
                    .collect::<String>()
            );
            return None;
        }
    };
    if str_of(&receipt, "outcome") == Some("choice_required") {
        // A near-duplicate exists and NOTHING was filed by that call. Fold into
        // the candidate via --wave-of (the non-interactive fold) - but only onto
        // this skill's own node; a different node gets a separate mint.
        // Reporting the candidate id as filed without either would silently
        // lose the finding.
        match fold_decision(&receipt, skill_id) {
            Some(FoldDecision::Fold(fold_id)) => {
                let argv2: Vec<String> = vec![
                    "fno".into(),
                    "backlog".into(),
                    "idea".into(),
                    title.clone(),
                    "-d".into(),
                    details.clone(),
                    "--wave-of".into(),
                    fold_id.clone(),
                    "--difficulty".into(),
                    "medium".into(),
                    "--json".into(),
                ];
                let out2 = Command::new(&argv2[0])
                    .args(&argv2[1..])
                    .current_dir(repo_root)
                    .output();
                let out2 = match out2 {
                    Ok(o) if o.status.success() => o,
                    Ok(o) => {
                        eprintln!(
                            "skill-diff: folding no-diff-helps node failed (rc={})",
                            o.status.code().unwrap_or(1)
                        );
                        return None;
                    }
                    Err(e) => {
                        eprintln!("skill-diff: folding no-diff-helps node failed: {e}");
                        return None;
                    }
                };
                let receipt2 = idea_receipt(&String::from_utf8_lossy(&out2.stdout));
                let wave_ok = receipt2
                    .as_ref()
                    .is_some_and(|r| str_of(r, "outcome") == Some("wave"));
                if !wave_ok {
                    eprintln!("skill-diff: unexpected fold receipt");
                    return None;
                }
                return receipt2
                    .as_ref()
                    .and_then(|r| str_of(r, "node_id").map(str::to_string))
                    .or(Some(fold_id));
            }
            Some(FoldDecision::Separate) => {
                let mut separate = argv.clone();
                separate.push("--separate".to_string());
                let out2 = Command::new(&separate[0])
                    .args(&separate[1..])
                    .current_dir(repo_root)
                    .output();
                let out2 = match out2 {
                    Ok(o) if o.status.success() => o,
                    Ok(o) => {
                        eprintln!(
                            "skill-diff: folding no-diff-helps node failed (rc={})",
                            o.status.code().unwrap_or(1)
                        );
                        return None;
                    }
                    Err(e) => {
                        eprintln!("skill-diff: folding no-diff-helps node failed: {e}");
                        return None;
                    }
                };
                let receipt2 = idea_receipt(&String::from_utf8_lossy(&out2.stdout));
                return receipt2.and_then(|r| str_of(&r, "id").map(str::to_string));
            }
            None => return None,
        }
    }
    str_of(&receipt, "id").map(str::to_string)
}

// ---------------------------------------------------------------------------
// PR open (assisted level)
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub(crate) struct OpenArgs {
    pub repo_root: PathBuf,
    pub skill_id: String,
    pub run_id: String,
    pub hunks: Vec<Hunk>,
    pub body: String,
    pub cited: Vec<String>,
    pub project_names: Vec<String>,
}

/// Apply hunks, branch, commit, push, `gh pr create`. Never merges, never
/// touches a default branch. Returns (pr_number, branch); raises on any
/// git/gh failure so the caller takes no action rather than reporting a
/// phantom PR.
fn apply_and_open_pr(args: &OpenArgs) -> Result<(Option<i64>, String), OpenError> {
    let OpenArgs {
        repo_root,
        skill_id,
        run_id,
        hunks,
        body,
        cited,
        project_names,
    } = args;
    let skill_root = skill_dir(repo_root, skill_id);
    let short_run: String = run_id
        .rsplit('-')
        .next()
        .unwrap_or(run_id)
        .chars()
        .take(12)
        .collect();
    let short_skill = skill_id.split(':').next_back().unwrap_or(skill_id);
    let branch = format!("skill-diff/{short_skill}-{short_run}");
    let commit_msg = format!(
        "docs(skill): {skill_id} diff from observer run {short_run}\n\ncites: {}",
        cited.join(", ")
    );

    // Redaction guard over EVERY surface that gets committed or pushed: the PR
    // body, the commit message, AND the hunk content itself - GitHub metadata
    // and skills/ files are surfaces no-internal-refs CI never scans. Refuse
    // before writing anything.
    let mut scan = String::new();
    scan.push_str(body);
    scan.push('\n');
    scan.push_str(&commit_msg);
    for h in hunks {
        scan.push('\n');
        scan.push_str(&h.new_text);
    }
    let hits = redaction_violations(&scan, project_names);
    if !hits.is_empty() {
        return Err(OpenError::Redaction(format!(
            "redaction guard refused open: {}",
            py_list(&hits)
        )));
    }

    let mut touched: Vec<String> = Vec::new();
    for h in hunks {
        // h.file comes from untrusted LLM output: resolve it and refuse any
        // path outside the TARGET skill's own directory, and any non-markdown
        // file (skill content is markdown - a .py/.sh path is never legitimate).
        let f = normalize_path(repo_root, &h.file);
        let under = f == skill_root || f.starts_with(&skill_root);
        if !under || f.extension().map(|e| e != "md").unwrap_or(true) {
            return Err(OpenError::Transient(format!(
                "hunk path not a .md under {skill_id}'s dir: {}",
                h.file
            )));
        }
        let current = if f.exists() {
            std::fs::read_to_string(&f).map_err(|e| OpenError::Transient(e.to_string()))?
        } else {
            String::new()
        };
        let updated = if !h.old_text.is_empty() {
            if !current.contains(&h.old_text) {
                return Err(OpenError::Transient(format!(
                    "hunk old_text not found in {} (file drifted)",
                    h.file
                )));
            }
            current.replacen(&h.old_text, &h.new_text, 1)
        } else if !current.is_empty() {
            format!(
                "{}\n{}\n",
                current.trim_end_matches('\n'),
                h.new_text.trim_end_matches('\n')
            )
        } else {
            format!("{}\n", h.new_text.trim_end_matches('\n'))
        };
        std::fs::write(&f, updated).map_err(|e| OpenError::Transient(e.to_string()))?;
        touched.push(
            f.strip_prefix(repo_root)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| h.file.clone()),
        );
    }

    let git = |git_args: &[&str]| -> Result<(), OpenError> {
        let out = Command::new("git")
            .args(git_args)
            .current_dir(repo_root)
            .output()
            .map_err(|e| OpenError::Transient(e.to_string()))?;
        if !out.status.success() {
            return Err(OpenError::Transient(format!(
                "git {} failed: {}",
                git_args.first().unwrap_or(&""),
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(())
    };
    // -B resets the branch if it exists (idempotent retry).
    git(&["checkout", "-B", &branch])?;
    git(&["add"]
        .into_iter()
        .chain(touched.iter().map(String::as_str))
        .collect::<Vec<_>>())?;
    git(&["commit", "-m", &commit_msg])?;
    git(&["push", "-u", "origin", &branch])?;

    let body_file = repo_root
        .join(".fno")
        .join(format!(".skill-diff-body-{short_run}.md"));
    if let Some(parent) = body_file.parent() {
        std::fs::create_dir_all(parent).map_err(|e| OpenError::Transient(e.to_string()))?;
    }
    std::fs::write(&body_file, body).map_err(|e| OpenError::Transient(e.to_string()))?;
    let mut cmd = Command::new("gh");
    cmd.args([
        "pr",
        "create",
        "--title",
        &format!("skill-diff: {skill_id} (run {short_run})"),
        "--body-file",
    ])
    .arg(&body_file)
    .current_dir(repo_root);
    let pr = crate::gh_budget::output(cmd);
    let _ = std::fs::remove_file(&body_file);
    let out = pr.map_err(|e| OpenError::Transient(e.to_string()))?;
    if !out.status.success() {
        return Err(OpenError::Transient(format!(
            "gh pr create failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let pr_number = stdout.split("/pull/").nth(1).and_then(|tail| {
        tail.chars()
            .take_while(|c| c.is_ascii_digit())
            .collect::<String>()
            .parse::<i64>()
            .ok()
    });
    Ok((pr_number, branch))
}

/// Lexical join-and-normalize (Python Path.resolve's non-strict shape): no
/// filesystem access, `..` folded, so a not-yet-existing hunk path still
/// normalizes for the containment check.
fn normalize_path(root: &Path, rel: &str) -> PathBuf {
    let mut out = root.to_path_buf();
    for comp in Path::new(rel).components() {
        match comp {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// tick
// ---------------------------------------------------------------------------

/// Test seams for the tick decision surface: no test spawns an agent or
/// touches the network. The real `run` wires the production closures.
pub(crate) struct TickDeps<'a> {
    pub events: Vec<Value>,
    pub paused: bool,
    pub level: String,
    pub skill_files: Vec<(String, String)>,
    pub blob_hash: String,
    pub synthesize: Box<dyn Fn(&str) -> Result<Proposal, String> + 'a>,
    pub open_pr: Box<dyn Fn(&OpenArgs) -> Result<(Option<i64>, String), OpenError> + 'a>,
    pub file_node: Box<dyn Fn(&str, &str, &str) -> Option<String> + 'a>,
    /// stdout lines in order; the run prints them as they are produced.
    pub lines: Vec<String>,
}

impl<'a> TickDeps<'a> {
    fn say(&mut self, line: String) {
        self.lines.push(line);
    }
}

/// One proposer iteration over the newest unprocessed observer run. States on
/// stdout, one word each: paused | no-work | noop | no-diff-helps | report |
/// proposed. Never exits 0 silently with no state word.
fn tick_core(deps: &mut TickDeps, journals: &[PathBuf], skill: &str, skill_id: &str) -> i32 {
    if deps.paused {
        deps.say("paused".to_string());
        emit_tick(journals, &deps.level, "paused");
        return 0;
    }
    let runs = unprocessed_runs(&deps.events, skill_id);
    if runs.is_empty() {
        deps.say("no-work".to_string());
        emit_tick(journals, &deps.level, "no-work");
        return 0;
    }
    let run_id = runs[0].clone();

    // An all-pass (or empty) run is a no-op: nothing to fix.
    if !has_actionable_findings(&deps.events, &run_id, Some(skill_id)) {
        let reason = if findings_for_run(&deps.events, &run_id, Some(skill_id)).is_empty() {
            "zero_findings"
        } else {
            "zero_failures"
        };
        emit(
            journals,
            "skill_diff_noop",
            json!({"run_id": run_id, "skill_id": skill_id, "reason": reason}),
        );
        emit_tick(journals, &deps.level, "noop");
        deps.say(format!("noop {run_id} ({reason})"));
        return 0;
    }

    // Local-maxima ceiling: file a node, do not diff again.
    if local_maxima_tripped(&deps.events, skill_id, &run_id) {
        no_diff_helps(deps, journals, &run_id, skill_id, "local_maxima");
        return 0;
    }

    let ranking = failure_ranking(&deps.events, &run_id, Some(skill_id));
    let findings = findings_for_run(&deps.events, &run_id, Some(skill_id));

    // report (default): dry-run. Show what it WOULD process and stop - no
    // synthesis spend, no PR.
    if deps.level != "assisted" {
        let top = ranking
            .first()
            .map(|(d, _)| d.clone())
            .unwrap_or_else(|| "(none)".to_string());
        deps.say(format!(
            "report {run_id} skill={skill_id} findings={} top_failure={top} \
             (level={}; set config.{LOOP_NAME}.level=assisted to open PRs)",
            findings.len(),
            deps.level
        ));
        emit_tick(journals, &deps.level, "report");
        return 0;
    }

    // assisted: synthesize -> guards -> PR (or no-diff-helps).
    let version_observed = run_complete_event(&deps.events, &run_id)
        .and_then(|e| str_of(data_of(e), "skill_version").map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string());
    let prompt = build_prompt(
        skill_id,
        &deps.skill_files,
        &findings,
        &ranking,
        &prior_proposed(&deps.events, skill_id),
    );
    let proposal = match (deps.synthesize)(&prompt) {
        Ok(p) => p,
        // A synthesis crash/timeout is a tool fault, not a skill verdict:
        // take no action this tick, leave the run unprocessed, retry next tick.
        Err(e) => {
            deps.say(format!(
                "report {run_id} (synthesis failed, no action: {e})"
            ));
            emit_tick(journals, &deps.level, "report");
            return 0;
        }
    };

    if proposal.verdict == "no_diff_helps" {
        no_diff_helps(deps, journals, &run_id, skill_id, "synth_no_diff");
        return 0;
    }

    let (kept, _dropped) = filter_cited_hunks(&proposal.hunks);
    if kept.is_empty() {
        // Every hunk uncited: no PR, take the no-diff-helps path.
        no_diff_helps(deps, journals, &run_id, skill_id, "all_hunks_uncited");
        return 0;
    }

    // Additive-only over the threshold requires justification.
    let justification = proposal.justification.clone().unwrap_or_default();
    if additive_only_needs_justification(&kept) && justification.trim().is_empty() {
        no_diff_helps(
            deps,
            journals,
            &run_id,
            skill_id,
            "additive_only_no_justification",
        );
        return 0;
    }

    let bloat = bloat_flag(&prior_proposed(&deps.events, skill_id));
    let (added, removed) = count_lines(&kept);
    let cited: Vec<String> = kept
        .iter()
        .flat_map(|h| h.cited_finding_ids.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let body = build_pr_body(
        &run_id,
        skill_id,
        &kept,
        if justification.is_empty() {
            None
        } else {
            Some(justification.as_str())
        },
        bloat,
        &version_observed,
        &deps.blob_hash,
        skill == "review",
    );
    let open_args = OpenArgs {
        repo_root: PathBuf::new(),
        skill_id: skill_id.to_string(),
        run_id: run_id.clone(),
        hunks: kept,
        body: body.clone(),
        cited: cited.clone(),
        project_names: Vec::new(),
    };
    match (deps.open_pr)(&open_args) {
        Err(OpenError::Redaction(e)) => {
            // A leak in the synthesized content is a hard, terminal refusal
            // (retrying would re-run synthesis on the same corpus and likely
            // leak again).
            emit(
                journals,
                "skill_diff_no_diff_helps",
                json!({"run_id": run_id, "skill_id": skill_id, "reason": "redaction_refused", "filed_node_id": null}),
            );
            emit_tick(journals, &deps.level, "no-diff-helps");
            deps.say(format!("no-diff-helps {run_id} (redaction refused: {e})"));
        }
        Err(OpenError::Transient(e)) => {
            // A transient git/gh failure must NOT become terminal: no terminal
            // event, so the run stays unprocessed and the next tick retries.
            emit_tick(journals, &deps.level, "open-failed");
            deps.say(format!(
                "open-failed {run_id} (transient, will retry next tick: {e})"
            ));
        }
        Ok((pr_number, branch)) => {
            // A report-level dry run opens nothing, so the number can be
            // None; render it the way the Python leg's f-string did.
            let pr_txt = pr_number
                .map(|n| n.to_string())
                .unwrap_or_else(|| "None".to_string());
            let proposed_ok = emit(
                journals,
                "skill_diff_proposed",
                json!({
                    "run_id": run_id, "skill_id": skill_id,
                    "skill_version_observed": version_observed,
                    "skill_version_proposed_against": deps.blob_hash,
                    "pr_number": pr_number, "branch": branch,
                    "cited_finding_ids": cited,
                    "added_lines": added, "removed_lines": removed,
                    "justification": if justification.is_empty() { Value::Null } else { json!(justification) },
                }),
            );
            if !proposed_ok {
                // The proposed event IS the idempotency record; a failed log
                // after the PR opened could open a second PR next tick.
                eprintln!(
                    "CRITICAL {run_id}: PR#{pr_txt} opened but skill_diff_proposed failed to log canonically - reconcile before the next tick to avoid a duplicate PR"
                );
            }
            emit_tick(journals, &deps.level, "proposed");
            deps.say(format!(
                "proposed {run_id} PR#{pr_txt} branch={branch} +{added}/-{removed} cited={}",
                py_list(&cited)
            ));
        }
    }
    0
}

/// Terminal no-diff-helps path: file a backlog node, then emit the terminal
/// event ONLY if the node was filed, so a failed filing stays unprocessed and
/// the next tick retries (degrade, not crash).
fn no_diff_helps(
    deps: &mut TickDeps,
    journals: &[PathBuf],
    run_id: &str,
    skill_id: &str,
    reason: &str,
) {
    let node = (deps.file_node)(skill_id, run_id, reason);
    match node {
        None => {
            emit_tick(journals, &deps.level, "no-diff-helps-defer");
            deps.say(format!(
                "no-diff-helps {run_id} ({reason}) - node filing failed, will retry next tick"
            ));
        }
        Some(node) => {
            emit(
                journals,
                "skill_diff_no_diff_helps",
                json!({"run_id": run_id, "skill_id": skill_id, "reason": reason, "filed_node_id": node}),
            );
            emit_tick(journals, &deps.level, "no-diff-helps");
            deps.say(format!("no-diff-helps {run_id} ({reason}, node={node})"));
        }
    }
}

fn tick(cwd: &Path, skill: &str) -> i32 {
    let journals = events_paths(cwd);
    let paused = loops_paused();
    let level = loop_level(cwd);
    let repo_root = crate::paths::worktree_repo_root(cwd);
    let events = if paused {
        Vec::new()
    } else {
        read_events(&journals[0])
    };
    let skill_id = if skill.contains(':') {
        skill.to_string()
    } else {
        format!("fno:{skill}")
    };
    let skill_files = read_skill_files(&repo_root, &skill_id);
    let rel = skill_files
        .first()
        .map(|(p, _)| p.clone())
        .unwrap_or_else(|| format!("skills/{skill}/SKILL.md"));
    let hash = blob_hash(&repo_root, &rel);
    let names = project_names(cwd);
    let file_root = repo_root.clone();
    let mut deps = TickDeps {
        events,
        paused,
        level,
        skill_files,
        blob_hash: hash,
        synthesize: Box::new(|prompt| synthesize(prompt)),
        open_pr: Box::new(move |args: &OpenArgs| {
            apply_and_open_pr(&OpenArgs {
                repo_root: repo_root.clone(),
                project_names: names.clone(),
                ..args.clone()
            })
        }),
        file_node: Box::new(move |sid, rid, reason| {
            file_no_diff_node(&file_root, sid, rid, reason)
        }),
        lines: Vec::new(),
    };
    let rc = tick_core(&mut deps, &journals, skill, &skill_id);
    for line in &deps.lines {
        println!("{line}");
        let _ = std::io::stdout().flush();
    }
    rc
}

// ---------------------------------------------------------------------------
// reconcile (the loop closer)
// ---------------------------------------------------------------------------

/// Test seams for the reconcile decision surface.
pub(crate) struct ReconcileDeps<'a> {
    pub events: Vec<Value>,
    pub paused: bool,
    pub pr_merged: Box<dyn Fn(i64) -> Option<bool> + 'a>,
    pub merge_sha: Box<dyn Fn(i64) -> Option<String> + 'a>,
    pub run_replay: Box<dyn Fn(&str, &str, &str) -> i32 + 'a>,
    /// The after-run id minter: a seam so a test can pin the id its canned
    /// re-read rows carry (the real one stamps the wall clock).
    pub mint_after: Box<dyn Fn(&str) -> String + 'a>,
    pub reread: Box<dyn Fn() -> Vec<Value> + 'a>,
    pub lines: Vec<String>,
}

/// Close the skill-diff loop: for each merged proposer PR with no
/// skill_diff_eval_closed receipt, replay the exact corpus items the diff
/// targeted at the merge commit, fold a before/after score_delta on the top
/// failure dimension, and emit the receipt. --pr-number scopes to the
/// just-merged PR (the fast merge-trigger path); the bare form sweeps. Two
/// callers key idempotency on the receipt's presence, so firing both never
/// double-emits.
fn reconcile(cwd: &Path, pr_number: Option<i64>) -> i32 {
    let journals = events_paths(cwd);
    let repo_root = crate::paths::worktree_repo_root(cwd);
    let events = read_events(&journals[0]);
    let merged_root = repo_root.clone();
    let sha_root = repo_root.clone();
    let replay_root = repo_root;
    let mut deps = ReconcileDeps {
        events,
        paused: loops_paused(),
        pr_merged: Box::new(move |pr| pr_merged(&merged_root, pr)),
        merge_sha: Box::new(move |pr| merge_sha(&sha_root, pr)),
        run_replay: Box::new(move |ci, rref, rid| run_replay(&replay_root, ci, rref, rid)),
        mint_after: Box::new(mint_run_id),
        reread: {
            let journals = journals.clone();
            Box::new(move || read_events(&journals[0]))
        },
        lines: Vec::new(),
    };
    reconcile_core(&mut deps, &journals, pr_number);
    for line in &deps.lines {
        println!("reconcile: {line}");
        let _ = std::io::stdout().flush();
    }
    0
}

fn reconcile_core(deps: &mut ReconcileDeps, journals: &[PathBuf], pr_number: Option<i64>) {
    // pr_number -> its proposed data, last event wins, first-seen order kept.
    let mut order: Vec<i64> = Vec::new();
    let mut proposed: BTreeMap<i64, Map<String, Value>> = BTreeMap::new();
    for e in &deps.events {
        if ev_type(e) == Some("skill_diff_proposed") {
            let d = data_of(e);
            if let Some(pr) = d.get("pr_number").and_then(Value::as_i64) {
                if !order.contains(&pr) {
                    order.push(pr);
                }
                proposed.insert(pr, d.clone());
            }
        }
    }
    let closed: BTreeSet<i64> = deps
        .events
        .iter()
        .filter(|e| ev_type(e) == Some("skill_diff_eval_closed"))
        .filter_map(|e| data_of(e).get("pr_number").and_then(Value::as_i64))
        .collect();
    let open_gaps: Vec<i64> = match pr_number {
        Some(pr) => {
            if !proposed.contains_key(&pr) {
                deps.lines.push(format!(
                    "PR#{pr} is not a known proposer PR (no skill_diff_proposed event)"
                ));
                return;
            }
            if closed.contains(&pr) {
                deps.lines.push(format!(
                    "PR#{pr} already has an eval-closed receipt (no-op)"
                ));
                return;
            }
            vec![pr]
        }
        None => {
            let gaps: Vec<i64> = order
                .into_iter()
                .filter(|pr| !closed.contains(pr))
                .collect();
            if gaps.is_empty() {
                deps.lines.push("no un-closed proposer PRs".to_string());
                return;
            }
            gaps
        }
    };
    for pr in open_gaps {
        let data = proposed.get(&pr).cloned().unwrap_or_default();
        let line = reeval_pr(deps, journals, pr, &data);
        deps.lines.push(line);
    }
}

/// Re-eval one merged-unclosed proposer PR. Emits the receipt on success.
/// Emits NOTHING (leaving the PR detectable as unclosed for the next tick) on
/// not-yet-merged, gh-offline, paused, a replay with no comparable top-dim
/// verdict, or a canonical-log emit failure - all degrade-not-crash.
fn reeval_pr(
    deps: &mut ReconcileDeps,
    journals: &[PathBuf],
    pr: i64,
    proposed: &Map<String, Value>,
) -> String {
    let skill_id = str_of(proposed, "skill_id")
        .unwrap_or("unknown")
        .to_string();
    let run_id_before = str_of(proposed, "run_id").map(str::to_string);

    // /review proposals have no historical thread state to score against:
    // log-and-skip, no replay, no receipt.
    if skill_id == "fno:review" {
        return format!("PR#{pr} ({skill_id}) outcome-pending (review-skill)");
    }

    match (deps.pr_merged)(pr) {
        Some(false) => {
            return format!(
                "PR#{pr} ({skill_id}, run {}) not-yet-merged, no eval-closed",
                run_id_before.as_deref().unwrap_or("None")
            );
        }
        // gh unreachable: never re-eval a PR we can't confirm merged - it
        // could still be open, and replaying against origin/main would score
        // a diff that never landed. An offline blip is not a genuinely open PR.
        None => {
            return format!(
                "PR#{pr} ({skill_id}) merge status unknown (gh offline), left unclosed for retry"
            );
        }
        Some(true) => {}
    }

    // Pause gate BEFORE any replay spend.
    if deps.paused {
        return format!("PR#{pr} ({skill_id}) paused, left unclosed for a later tick");
    }

    let Some(run_id_before) = run_id_before else {
        return format!("PR#{pr} ({skill_id}) malformed proposed event (no run_id), skipped");
    };

    let top = top_dimension(&deps.events, &run_id_before, Some(&skill_id));
    let replay_set = replay_set(&deps.events, &run_id_before, &skill_id, top.as_deref());

    // Nothing failing on the top dimension: replay nothing; close with a null
    // after-run and a zero delta.
    if replay_set.is_empty() {
        if already_closed(deps, pr) {
            return format!(
                "PR#{pr} ({skill_id}) already closed by a concurrent reconcile (no-op)"
            );
        }
        if !emit_receipt(deps, journals, pr, &skill_id, &run_id_before, None, Some(0)) {
            return format!(
                "PR#{pr} ({skill_id}) receipt emit failed (canonical log), left unclosed for retry"
            );
        }
        return format!(
            "PR#{pr} ({skill_id}) re-evaluated: no failing items on {} dim, delta=0",
            top.as_deref().unwrap_or("top")
        );
    }

    // The top failure dimension is not structurally replayable: close as
    // outcome-pending (null delta, no replay) so it neither spins the replay
    // fleet forever nor reports a fabricated structural win.
    if !REPLAYABLE_DIMS.contains(&top.as_deref().unwrap_or("")) {
        if already_closed(deps, pr) {
            return format!(
                "PR#{pr} ({skill_id}) already closed by a concurrent reconcile (no-op)"
            );
        }
        if !emit_receipt(deps, journals, pr, &skill_id, &run_id_before, None, None) {
            return format!(
                "PR#{pr} ({skill_id}) receipt emit failed (canonical log), left unclosed for retry"
            );
        }
        return format!(
            "PR#{pr} ({skill_id}) outcome-pending: top dim {} not structurally replayable (delta=null)",
            top.as_deref().unwrap_or("None")
        );
    }

    let merge_sha = (deps.merge_sha)(pr).unwrap_or_else(|| "origin/main".to_string());
    let run_id_after = (deps.mint_after)(&skill_id);
    for corpus_item in &replay_set {
        (deps.run_replay)(corpus_item, &merge_sha, &run_id_after);
    }

    // Re-read and scope the fold to the TOP dimension both sides: a replay
    // emits a finding on EVERY structural dimension, but the delta is only
    // honest on the dimension the diff targeted.
    let events_after = (deps.reread)();
    let top_after: Vec<Map<String, Value>> =
        findings_for_run(&events_after, &run_id_after, Some(&skill_id))
            .into_iter()
            .filter(|f| str_of(f, "dimension") == top.as_deref())
            .collect();

    // The replay produced no comparable top-dimension verdict: every spawn
    // tool-faulted, or the top dim came back unscorable. Emit no receipt; the
    // PR stays unclosed so the next tick retries (never a fabricated delta).
    if top_after.is_empty() {
        return format!(
            "PR#{pr} ({skill_id}) replay produced no {} verdict, left unclosed for retry",
            top.as_deref().unwrap_or("top")
        );
    }

    // before_fail = the targeted slice size (every replay-set item failed the
    // top dim before, by construction); after_fail = how many still fail.
    let before_fail = replay_set.len() as i64;
    let after_fail = top_after
        .iter()
        .filter(|f| str_of(f, "verdict") == Some("fail"))
        .count() as i64;
    let score_delta = before_fail - after_fail;
    if already_closed(deps, pr) {
        return format!("PR#{pr} ({skill_id}) already closed by a concurrent reconcile (no-op)");
    }
    if !emit_receipt(
        deps,
        journals,
        pr,
        &skill_id,
        &run_id_before,
        Some(&run_id_after),
        Some(score_delta),
    ) {
        return format!(
            "PR#{pr} ({skill_id}) receipt emit failed (canonical log), left unclosed for retry"
        );
    }
    format!(
        "PR#{pr} ({skill_id}) re-evaluated: delta={score_delta} (before={before_fail} after={after_fail}, ref={})",
        &merge_sha[..merge_sha.len().min(12)]
    )
}

/// The corpus items the diff was supposed to fix: the before run's failing
/// findings on the top dimension only. Order-preserving dedup.
fn replay_set(
    events: &[Value],
    run_id_before: &str,
    skill_id: &str,
    top_dim: Option<&str>,
) -> Vec<String> {
    let Some(top_dim) = top_dim else {
        return Vec::new();
    };
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for f in findings_for_run(events, run_id_before, Some(skill_id)) {
        if str_of(&f, "dimension") == Some(top_dim) && str_of(&f, "verdict") == Some("fail") {
            if let Some(cid) = str_of(&f, "corpus_item_id") {
                if seen.insert(cid.to_string()) {
                    out.push(cid.to_string());
                }
            }
        }
    }
    out
}

fn emit_receipt(
    deps: &ReconcileDeps,
    journals: &[PathBuf],
    pr: i64,
    skill_id: &str,
    run_id_before: &str,
    run_id_after: Option<&str>,
    score_delta: Option<i64>,
) -> bool {
    // The close receipt must be durably recorded: the caller never reports a
    // close it did not write.
    let _ = deps;
    emit(
        journals,
        "skill_diff_eval_closed",
        json!({
            "pr_number": pr, "skill_id": skill_id, "run_id_before": run_id_before,
            "run_id_after": run_id_after.map(str::to_string),
            "score_delta": score_delta,
        }),
    )
}

/// A skill_diff_eval_closed for this PR already landed. A cheap re-read right
/// before emitting narrows the merge-trigger vs periodic-sweep race; it
/// shrinks, not eliminates, a simultaneous double-emit.
fn already_closed(deps: &ReconcileDeps, pr: i64) -> bool {
    (deps.reread)().iter().any(|e| {
        ev_type(e) == Some("skill_diff_eval_closed")
            && data_of(e).get("pr_number").and_then(Value::as_i64) == Some(pr)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A per-call temp root: the pre-state journal lives at
    /// `dir/events.jsonl`, and every emitted event commits into the store
    /// beside it without touching the file, so `emitted_types` reads the
    /// store un-synced and sees only what the run under test emitted.
    struct Scratch {
        dir: PathBuf,
    }

    impl Scratch {
        fn new(name: &str) -> Self {
            static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir = std::env::temp_dir()
                .join(format!("fno-skill-diff-{}-{n}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("scratch dir");
            Scratch { dir }
        }

        fn journals(&self) -> Vec<PathBuf> {
            vec![self.dir.join("events.jsonl")]
        }

        fn emitted_types(&self) -> Vec<String> {
            let journal = self.dir.join("events.jsonl");
            if !crate::event_store::store_path(&journal).is_file() {
                return Vec::new();
            }
            match crate::event_store::query_events(
                &journal,
                &crate::event_store::EventQuery::default(),
            ) {
                Ok(rows) => rows.iter().map(|r| r.r#type.clone()).collect(),
                Err(_) => Vec::new(),
            }
        }
    }

    fn ev(ty: &str, data: Value) -> Value {
        json!({"type": ty, "data": data})
    }

    fn rc(run_id: &str, top: &str, fails: i64) -> Value {
        ev(
            "skill_eval_run_complete",
            json!({"run_id": run_id, "skill_id": "fno:blueprint",
                   "failure_ranking": [{"dimension": top, "fail_count": fails}]}),
        )
    }

    fn finding(run_id: &str, verdict: &str) -> Value {
        ev(
            "skill_eval_finding",
            json!({"run_id": run_id, "skill_id": "fno:blueprint", "dimension": "collision_free",
                   "verdict": verdict, "corpus_item_id": format!("{run_id}-item")}),
        )
    }

    fn proposed(run_id: &str, pr: i64) -> Value {
        ev(
            "skill_diff_proposed",
            json!({"run_id": run_id, "skill_id": "fno:blueprint", "pr_number": pr,
                   "branch": format!("skill-diff-{run_id}"), "cited_finding_ids": [],
                   "added_lines": 1, "removed_lines": 0}),
        )
    }

    fn hunk(cites: &[&str]) -> Hunk {
        Hunk {
            file: "skills/blueprint/SKILL.md".into(),
            old_text: String::new(),
            new_text: "x".into(),
            cited_finding_ids: cites.iter().map(|s| s.to_string()).collect(),
            rationale: String::new(),
        }
    }

    fn write_journal(dir: &Path, rows: &[Value]) -> PathBuf {
        let journal = dir.join("events.jsonl");
        let text: String = rows
            .iter()
            .map(|e| serde_json::to_string(e).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&journal, text + "\n").unwrap();
        journal
    }

    fn empty_reconcile_deps<'a>(events: Vec<Value>) -> ReconcileDeps<'a> {
        ReconcileDeps {
            events,
            paused: false,
            pr_merged: Box::new(|_| Some(true)),
            merge_sha: Box::new(|_| Some("mergesha12345".to_string())),
            run_replay: Box::new(|_, _, _| 1),
            mint_after: Box::new(|skill| format!("obs-{skill}-x")),
            reread: Box::new(Vec::new),
            lines: Vec::new(),
        }
    }

    #[test]
    fn unprocessed_runs_idempotency_and_the_skill_ref_discriminator() {
        // AC8-FR: a run_id that already produced a proposal / node / noop is
        // never reprocessed; the key is (run_id, skill_id), so a shared
        // run_id across skills stays open for the other skill.
        let evs = vec![rc("r1", "t", 1), rc("r2", "t", 1)];
        assert_eq!(unprocessed_runs(&evs, "fno:blueprint"), ["r1", "r2"]);
        let evs = [
            evs,
            vec![
                ev(
                    "skill_diff_proposed",
                    json!({"run_id": "r1", "skill_id": "fno:blueprint"}),
                ),
                ev(
                    "skill_diff_noop",
                    json!({"run_id": "r2", "skill_id": "fno:blueprint"}),
                ),
            ],
        ]
        .concat();
        assert!(unprocessed_runs(&evs, "fno:blueprint").is_empty());
        let evs = vec![
            rc("shared", "t", 1),
            ev(
                "skill_eval_run_complete",
                json!({"run_id": "shared", "skill_id": "fno:review",
                "failure_ranking": [{"dimension": "t", "fail_count": 1}]}),
            ),
            ev(
                "skill_diff_proposed",
                json!({"run_id": "shared", "skill_id": "fno:blueprint"}),
            ),
        ];
        assert!(unprocessed_runs(&evs, "fno:blueprint").is_empty());
        assert_eq!(unprocessed_runs(&evs, "fno:review"), ["shared"]);

        // FIELD PRESENCE, not truthiness: a HEAD replay's `skill_ref: null`
        // is excluded exactly like a ref'd one (Locked Decision 5).
        let evs = vec![
            rc("sweep1", "t", 1),
            ev(
                "skill_eval_run_complete",
                json!({"run_id": "after1", "skill_id": "fno:blueprint",
                "skill_ref": "deadbeef"}),
            ),
            ev(
                "skill_eval_run_complete",
                json!({"run_id": "after2", "skill_id": "fno:blueprint",
                "skill_ref": null}),
            ),
        ];
        assert_eq!(unprocessed_runs(&evs, "fno:blueprint"), ["sweep1"]);
    }

    #[test]
    fn findings_exclude_tool_faults_and_ranking_sorts() {
        let evs = vec![
            finding("r1", "fail"),
            ev(
                "skill_eval_finding",
                json!({"run_id": "r1", "skill_id": "fno:blueprint",
                "dimension": "collision_free", "verdict": "fail", "tool_fault": true}),
            ),
            finding("r1", "pass"),
        ];
        let got = findings_for_run(&evs, "r1", None);
        assert_eq!(got.len(), 2);
        assert!(got
            .iter()
            .all(|f| f.get("tool_fault") != Some(&Value::Bool(true))));

        // The run_complete's own ranking wins; the fallback counts non-tool
        // fault fails, descending by count then ascending by dimension.
        let evs = vec![rc("r1", "collision_free", 3)];
        assert_eq!(
            failure_ranking(&evs, "r1", Some("fno:blueprint")),
            vec![("collision_free".to_string(), 3)]
        );
        let evs = vec![
            finding("r1", "fail"),
            finding("r1", "pass"),
            ev(
                "skill_eval_finding",
                json!({"run_id": "r1", "skill_id": "fno:blueprint",
                "dimension": "naming", "verdict": "fail"}),
            ),
            ev(
                "skill_eval_finding",
                json!({"run_id": "r1", "skill_id": "fno:blueprint",
                "dimension": "naming", "verdict": "fail"}),
            ),
        ];
        assert_eq!(
            failure_ranking(&evs, "r1", Some("fno:blueprint")),
            vec![("naming".to_string(), 2), ("collision_free".to_string(), 1)]
        );
        assert!(!has_actionable_findings(
            &[finding("r1", "pass"), finding("r1", "pass")],
            "r1",
            Some("fno:blueprint")
        ));
        assert!(has_actionable_findings(
            &[finding("r1", "degraded")],
            "r1",
            Some("fno:blueprint")
        ));
    }

    #[test]
    fn local_maxima_needs_a_full_window_and_a_proposal_in_span() {
        let same_top = |runs: &[&str]| -> Vec<Value> {
            runs.iter()
                .map(|r| rc(r, "collision_free", 1))
                .chain(std::iter::once(finding("r3", "fail")))
                .chain(std::iter::once(proposed("r1", 1)))
                .collect()
        };
        assert!(!local_maxima_tripped(
            &[rc("r1", "t", 1), rc("r2", "t", 1), rc("r3", "t", 1)],
            "fno:blueprint",
            "r3"
        ));
        assert!(!local_maxima_tripped(
            &[rc("r1", "t", 1), rc("r2", "t", 1), proposed("r1", 1)],
            "fno:blueprint",
            "r2"
        ));
        assert!(local_maxima_tripped(
            &same_top(&["r1", "r2", "r3"]),
            "fno:blueprint",
            "r3"
        ));
        // The dimension moved in the window: no ceiling.
        let moved: Vec<Value> = vec![
            rc("r1", "collision_free", 1),
            rc("r2", "collision_free", 1),
            rc("r3", "naming", 1),
            finding("r3", "fail"),
            proposed("r1", 1),
        ];
        assert!(!local_maxima_tripped(&moved, "fno:blueprint", "r3"));
    }

    #[test]
    fn guards_and_the_redaction_boundary() {
        let (kept, dropped) = filter_cited_hunks(&[
            hunk(&["f1"]),
            hunk(&[]),
            Hunk {
                cited_finding_ids: vec!["".into()],
                ..hunk(&[]).clone()
            },
            hunk(&["s1"]),
        ]);
        assert_eq!(kept.len(), 2);
        assert_eq!(dropped.len(), 2);
        let over = vec![Hunk {
            new_text: "x\n".repeat(16),
            ..hunk(&["s1"]).clone()
        }];
        let under = vec![Hunk {
            old_text: "y".into(),
            new_text: "x\n".repeat(16),
            ..hunk(&["s1"]).clone()
        }];
        assert!(additive_only_needs_justification(&over));
        assert!(!additive_only_needs_justification(&under));
        let prior = vec![json!({"added_lines": 200, "removed_lines": 10})
            .as_object()
            .unwrap()
            .clone()];
        assert_eq!(bloat_flag(&prior), Some((190, 1, 120)));
        let small = vec![json!({"added_lines": 10, "removed_lines": 5})
            .as_object()
            .unwrap()
            .clone()];
        assert_eq!(bloat_flag(&small), None);
        assert_eq!(
            redaction_violations("see internal/fno/plan.md and ~/.fno/x and /.fno/ y", &[]),
            ["internal/", "~/.fno", "/.fno/"]
        );
        // A foreign project name is flagged at a word boundary; fno (self)
        // never is; a name inside a longer word is not a hit.
        assert_eq!(
            redaction_violations(
                "touches acme and fno and acme-corp",
                &["acme".into(), "fno".into()]
            ),
            ["project:acme"]
        );
    }

    #[test]
    fn parse_proposal_fails_closed() {
        let nested = r#"{"verdict":"propose_pr","hunks":[
            {"file":"a","new_text":"x","cited_finding_ids":["s1"]},
            {"file":"b","new_text":"y","cited_finding_ids":["s2"]}],"justification":"j"}"#;
        let p = parse_proposal(nested).unwrap();
        assert_eq!(p.hunks.len(), 2);
        assert_eq!(p.justification.as_deref(), Some("j"));
        // A no_diff_helps verdict wins at the caller (tick checks the verdict
        // before the guards), but the parser still parses the hunks.
        let fenced = "```json\n{\"verdict\":\"no_diff_helps\",\"hunks\":[{\"file\":\"a\",\"cited_finding_ids\":[\"s1\"]}],\"no_diff_reason\":\"arch\"}\n```";
        let p = parse_proposal(fenced).unwrap();
        assert_eq!(p.verdict, "no_diff_helps");
        assert_eq!(p.hunks.len(), 1);
        assert_eq!(p.no_diff_reason.as_deref(), Some("arch"));
        assert!(parse_proposal("{\"verdict\":\"ship_it\"}").is_err());
        assert!(parse_proposal(
            "{\"verdict\":\"propose_pr\",\"hunks\":[{\"file\":\"a\",\"cited_finding_ids\":\"s1\"}]}"
        )
        .is_err());
        assert!(parse_proposal("the model refused").is_err());
    }

    #[test]
    fn tick_paused_answers_the_word_and_one_loop_tick() {
        let scratch = Scratch::new("paused");
        let journal = write_journal(&scratch.dir, &[rc("r1", "t", 1), finding("r1", "fail")]);
        let mut deps = TickDeps {
            events: Vec::new(),
            paused: true,
            level: "report".into(),
            skill_files: vec![],
            blob_hash: "unknown".into(),
            synthesize: Box::new(|_| panic!("no synthesis while paused")),
            open_pr: Box::new(|_| panic!("no PR while paused")),
            file_node: Box::new(|_, _, _| panic!("no node while paused")),
            lines: Vec::new(),
        };
        let journals = vec![journal];
        assert_eq!(
            tick_core(&mut deps, &journals, "blueprint", "fno:blueprint"),
            0
        );
        assert_eq!(deps.lines, ["paused"]);
        assert_eq!(scratch.emitted_types(), ["loop_tick"]);
    }

    fn dry_tick_deps<'a>(events: Vec<Value>, level: &str) -> TickDeps<'a> {
        TickDeps {
            events,
            paused: false,
            level: level.into(),
            skill_files: vec![],
            blob_hash: "aaa".into(),
            synthesize: Box::new(|prompt| {
                assert!(prompt.contains("OBSERVED failures"));
                Err("not called".into())
            }),
            open_pr: Box::new(|_| panic!("no PR at report level")),
            file_node: Box::new(|_, _, _| panic!("no node at report level")),
            lines: Vec::new(),
        }
    }

    #[test]
    fn tick_report_level_is_a_dry_run() {
        let mut deps = dry_tick_deps(
            vec![rc("r1", "collision_free", 2), finding("r1", "fail")],
            "report",
        );
        assert_eq!(tick_core(&mut deps, &[], "blueprint", "fno:blueprint"), 0);
        assert!(deps.lines[0]
            .starts_with("report r1 skill=fno:blueprint findings=1 top_failure=collision_free"));
        assert!(deps.lines[0].ends_with(
            "(level=report; set config.skill_diff_proposer.level=assisted to open PRs)"
        ));
    }

    #[test]
    fn tick_local_maxima_files_the_node_and_emits_once() {
        let events = vec![
            rc("r1", "collision_free", 1),
            rc("r2", "collision_free", 1),
            rc("r3", "collision_free", 1),
            finding("r3", "fail"),
            proposed("r1", 1),
            proposed("r2", 2),
        ];
        let mut deps = dry_tick_deps(events, "assisted");
        deps.file_node = Box::new(|_, _, _| Some("fno-dead".into()));
        assert_eq!(tick_core(&mut deps, &[], "blueprint", "fno:blueprint"), 0);
        assert!(deps.lines[0].starts_with("no-diff-helps r3 (local_maxima, node=fno-dead)"));
    }

    #[test]
    fn tick_uncited_hunks_take_the_no_diff_path_and_filing_failure_defers() {
        let base = || vec![rc("r1", "collision_free", 1), finding("r1", "fail")];
        // Every hunk uncited: no PR, no-diff-helps.
        let mut deps = dry_tick_deps(base(), "assisted");
        deps.synthesize = Box::new(|_| {
            Ok(Proposal {
                verdict: "propose_pr".into(),
                hunks: vec![hunk(&[])],
                justification: None,
                no_diff_reason: None,
            })
        });
        deps.file_node = Box::new(|_, _, _| Some("fno-y".into()));
        assert_eq!(tick_core(&mut deps, &[], "blueprint", "fno:blueprint"), 0);
        assert_eq!(
            deps.lines[0],
            "no-diff-helps r1 (all_hunks_uncited, node=fno-y)"
        );

        // Filing the node fails: NO terminal event posture - the retry word.
        let mut deps = dry_tick_deps(base(), "assisted");
        deps.synthesize = Box::new(|_| {
            Ok(Proposal {
                verdict: "propose_pr".into(),
                hunks: vec![hunk(&[])],
                justification: None,
                no_diff_reason: None,
            })
        });
        deps.file_node = Box::new(|_, _, _| None);
        assert_eq!(tick_core(&mut deps, &[], "blueprint", "fno:blueprint"), 0);
        assert!(deps.lines[0].contains("will retry next tick"));
    }

    #[test]
    fn tick_redaction_refusal_is_terminal_not_transient() {
        let mut deps = dry_tick_deps(
            vec![rc("r1", "collision_free", 1), finding("r1", "fail")],
            "assisted",
        );
        deps.synthesize = Box::new(|_| {
            Ok(Proposal {
                verdict: "propose_pr".into(),
                hunks: vec![hunk(&["s1"])],
                justification: None,
                no_diff_reason: None,
            })
        });
        deps.open_pr = Box::new(|_| {
            Err(OpenError::Redaction(
                "redaction guard refused open: ['internal/']".into(),
            ))
        });
        assert_eq!(tick_core(&mut deps, &[], "blueprint", "fno:blueprint"), 0);
        assert!(deps.lines[0].starts_with("no-diff-helps r1 (redaction refused:"));
    }

    #[test]
    fn reconcile_structural_receipt_and_the_unclosed_paths() {
        // AC1-HP: both replay items pass the top dimension, delta = 2.
        let events = vec![
            rc("r1", "collision_free", 2),
            finding("r1", "fail"),
            ev(
                "skill_eval_finding",
                json!({"run_id": "r1", "skill_id": "fno:blueprint",
                "dimension": "collision_free", "verdict": "fail",
                "corpus_item_id": "sid-b"}),
            ),
            proposed_pr(201),
        ];
        let after: Value = ev(
            "skill_eval_finding",
            json!({"run_id": "obs-fno:blueprint-x",
            "skill_id": "fno:blueprint", "dimension": "collision_free", "verdict": "pass",
            "corpus_item_id": "sid-a"}),
        );
        let after_b: Value = ev(
            "skill_eval_finding",
            json!({"run_id": "obs-fno:blueprint-x",
            "skill_id": "fno:blueprint", "dimension": "collision_free", "verdict": "pass",
            "corpus_item_id": "sid-b"}),
        );
        let scratch = Scratch::new("reconcile");
        let mut deps = empty_reconcile_deps(events);
        let played = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let played_log = std::rc::Rc::clone(&played);
        deps.run_replay = Box::new(move |ci, rref, rid| {
            played_log
                .borrow_mut()
                .push((ci.to_string(), rref.to_string(), rid.to_string()));
            0
        });
        let after_all = vec![after.clone(), after_b.clone()];
        let counter = std::cell::Cell::new(0);
        deps.reread = Box::new(move || {
            counter.set(counter.get() + 1);
            after_all.clone()
        });
        reconcile_core(&mut deps, &scratch.journals(), Some(201));
        assert_eq!(deps.lines.len(), 1);
        assert!(
            deps.lines[0].contains("re-evaluated: delta=2"),
            "{:?}",
            deps.lines
        );
        assert_eq!(played.borrow().len(), 2);

        // Every replay tool-faults: no comparable verdict, no receipt.
        let events = vec![
            rc("r1", "collision_free", 2),
            finding("r1", "fail"),
            proposed_pr(201),
        ];
        let scratch = Scratch::new("reconcile");
        let mut deps = empty_reconcile_deps(events);
        deps.run_replay = Box::new(|_, _, _| 1);
        deps.reread = Box::new(Vec::new);
        reconcile_core(&mut deps, &scratch.journals(), Some(201));
        assert!(deps.lines[0].contains("left unclosed for retry"));

        // gh offline (None) never re-evals against origin/main.
        let events = vec![
            rc("r1", "collision_free", 1),
            finding("r1", "fail"),
            proposed_pr(201),
        ];
        let scratch = Scratch::new("reconcile");
        let mut deps = empty_reconcile_deps(events);
        deps.pr_merged = Box::new(|_| None);
        reconcile_core(&mut deps, &scratch.journals(), Some(201));
        assert!(deps.lines[0].contains("merge status unknown"));
    }

    #[test]
    fn reconcile_concurrent_close_is_a_noop() {
        let events = vec![
            rc("r1", "collision_free", 1),
            finding("r1", "fail"),
            proposed_pr(201),
        ];
        let receipt = ev(
            "skill_diff_eval_closed",
            json!({"pr_number": 201,
            "skill_id": "fno:blueprint", "run_id_before": "r1"}),
        );
        let scratch = Scratch::new("reconcile");
        let mut deps = empty_reconcile_deps(events);
        let after: Vec<Value> = vec![
            ev(
                "skill_eval_finding",
                json!({"run_id": "obs-fno:blueprint-x", "skill_id": "fno:blueprint",
                "dimension": "collision_free", "verdict": "pass", "corpus_item_id": "r1-item"}),
            ),
            receipt,
        ];
        deps.reread = Box::new(move || after.clone());
        reconcile_core(&mut deps, &scratch.journals(), Some(201));
        assert!(deps.lines[0].contains("already closed by a concurrent reconcile"));
    }

    fn proposed_pr(pr: i64) -> Value {
        ev(
            "skill_diff_proposed",
            json!({"run_id": "r1", "skill_id": "fno:blueprint", "pr_number": pr,
                   "branch": "skill-diff-r1", "cited_finding_ids": [],
                   "added_lines": 1, "removed_lines": 0}),
        )
    }
}
