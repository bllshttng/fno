//! Native backlog maintenance: the recurring hygiene pass (`maintain`).
//!
//! Deterministic legs apply under `--apply`: re-scope (only `project`/`cwd`
//! change), temp-leak prune, pr_url backfill, session twin drops, harness
//! shape fixes, failure defers, stale-ready quarantine and abandoned do rows.
//! Judgment legs only ever propose: dedup, shared-plan cost, rollup, stale
//! ideas, the Now cap and the validity sweep. The pass runs under a
//! wall-clock budget checked between legs; an overrun exits 4 with a partial
//! receipt and a `complete: false` health-history row.

use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::Instant;

#[path = "maintenance_validity.rs"]
mod validity;

use crate::backlog_ready::{days_between, get_str, parse_iso_ms, truthy};
use crate::graph_store::StoreError;
use validity::{normalize_title, py_str, SweepSeams};

/// Every leg the partial receipt can count ("12/14 legs completed").
const MAINTAIN_LEG_TOTAL: usize = 14;
/// Never auto-defer more than this many nodes in one pass, so a provider
/// outage cannot defer half the board. The truncation is always printed.
const AUTO_DEFER_BLAST_CAP: usize = 10;
const ROLLUP_PROPOSAL_CAP: usize = 20;
const STALE_QUARANTINE_REASON: &str = "stale-quarantine (guard)";
/// Markers of a pytest or test-home temp dir. A bare temp ROOT is not
/// enough: a real checkout can live under /tmp, and pruning removes nodes.
const TEMP_CWD_MARKERS: [&str; 3] = ["pytest-of-", "/pytest-", "fno-test-home-"];

const USAGE: &str = "Usage: fno backlog maintain [--apply] [--json|-J] [--recheck] [--no-validity] [--suspect-reverts]

Keep the backlog and the kanban board clean. Deterministic legs apply under
--apply; the judgment legs (dedup, drain-stale, cap-Now, validity) only ever
propose. The pass runs under backlog.maintain.budget_seconds (default 300s);
an overrun exits 4 with a partial receipt naming the leg it stopped in.

Options:
  --apply            Apply the deterministic legs.
  --json, -J         Emit structured JSON instead of a human summary.
  --recheck          Validity sweep: re-review watermarked ideas.
  --no-validity      Skip the validity sweep (the leg that calls the analyzer).
  --suspect-reverts  Read-only: print drained nodes carrying evidence of a
                     human curation decision, then exit.
  -h, --help         Show this message and exit.";

// ---------------------------------------------------------------------------
// Flags, output and settings
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Flags {
    pub apply: bool,
    pub json: bool,
    pub recheck: bool,
    pub no_validity: bool,
    pub suspect_reverts: bool,
}

fn parse_flags(tail: &[String]) -> Result<Flags, i32> {
    let mut flags = Flags::default();
    for arg in tail {
        match arg.as_str() {
            "--apply" => flags.apply = true,
            "--json" | "-J" => flags.json = true,
            "--recheck" => flags.recheck = true,
            "--no-validity" => flags.no_validity = true,
            "--suspect-reverts" => flags.suspect_reverts = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                return Err(0);
            }
            other => {
                eprintln!("{USAGE}\n\nError: No such option: {other}");
                return Err(2);
            }
        }
    }
    Ok(flags)
}

/// Where the pass writes: straight to the terminal, or into buffers a test
/// reads back.
pub(crate) struct Out {
    live: bool,
    pub stdout: String,
    pub stderr: String,
}

impl Out {
    fn live() -> Self {
        Out {
            live: true,
            stdout: String::new(),
            stderr: String::new(),
        }
    }

    #[cfg(test)]
    fn captured() -> Self {
        Out {
            live: false,
            stdout: String::new(),
            stderr: String::new(),
        }
    }

    fn line(&mut self, text: impl AsRef<str>) {
        if self.live {
            println!("{}", text.as_ref());
        } else {
            self.stdout.push_str(text.as_ref());
            self.stdout.push('\n');
        }
    }

    fn err(&mut self, text: impl AsRef<str>) {
        if self.live {
            eprintln!("{}", text.as_ref());
        } else {
            self.stderr.push_str(text.as_ref());
            self.stderr.push('\n');
        }
    }
}

/// The `backlog.maintain.*` knobs plus the two readers outside the block.
/// A missing or nonpositive value takes its default.
#[derive(Clone, Debug)]
pub(crate) struct Settings {
    pub staleness_days: i64,
    pub max_failed_attempts: i64,
    pub abandoned_do_row_hours: i64,
    pub budget_seconds: i64,
    /// `backlog.staleness_days`: ready work goes stale faster than ideas.
    pub ready_staleness_days: i64,
    /// Raw, so the sweep names a bad value instead of hiding it.
    pub validity_days: Value,
    pub validity_batch_size: Value,
    /// The global `kanban.wip_caps.now`.
    pub now_wip_cap: i64,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            staleness_days: 30,
            max_failed_attempts: 3,
            abandoned_do_row_hours: 24,
            budget_seconds: 300,
            ready_staleness_days: 21,
            validity_days: json!(validity::VALIDITY_DAYS_DEFAULT),
            validity_batch_size: json!(validity::VALIDITY_BATCH_DEFAULT),
            now_wip_cap: 20,
        }
    }
}

impl Settings {
    fn load() -> Self {
        let d = Settings::default();
        let doc = super::advance_settings::load_merged(None);
        let maintain = &doc["backlog"]["maintain"];
        let positive = |v: &Value, default: i64| v.as_i64().filter(|n| *n > 0).unwrap_or(default);
        let (global, _) = super::advance_settings::load_global_merged();
        Settings {
            staleness_days: positive(&maintain["staleness_days"], d.staleness_days),
            max_failed_attempts: positive(&maintain["max_failed_attempts"], d.max_failed_attempts),
            abandoned_do_row_hours: positive(
                &maintain["abandoned_do_row_hours"],
                d.abandoned_do_row_hours,
            ),
            budget_seconds: positive(&maintain["budget_seconds"], d.budget_seconds),
            ready_staleness_days: positive(
                &doc["backlog"]["staleness_days"],
                d.ready_staleness_days,
            ),
            validity_days: maintain
                .get("validity_days")
                .cloned()
                .unwrap_or(d.validity_days),
            validity_batch_size: maintain
                .get("validity_batch_size")
                .cloned()
                .unwrap_or(d.validity_batch_size),
            now_wip_cap: positive(&global["kanban"]["wip_caps"]["now"], d.now_wip_cap),
        }
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

/// Python's float spelling (`0.0`, `301.4`) for the receipts.
fn py_float(x: f64) -> String {
    format!("{x:?}")
}

fn entry_id(e: &Value) -> Option<&str> {
    e.get("id").and_then(Value::as_str)
}

// ---------------------------------------------------------------------------
// Re-scope drift
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
struct RescopeFix {
    node_id: String,
    new_project: String,
    new_cwd: String,
}

/// The repo segment of a worktree cwd: `<repo>/.claude/worktrees/<name>`,
/// `.../conductor/workspaces/<repo>/<name>`, or `<base>/<repo>/<name>` under
/// a configured worktrees_base. The caller guards the hint with the
/// workspace map, so an unknown segment declines.
fn worktree_repo_hint(norm_cwd: &str, worktrees_base: Option<&str>) -> Option<String> {
    let probe = format!("{norm_cwd}/");
    if let Some(idx) = probe.find("/.claude/worktrees/") {
        let head = &probe[..idx];
        if let Some(repo) = head.rsplit('/').next().filter(|r| !r.is_empty()) {
            return Some(repo.to_string());
        }
    }
    if let Some(idx) = probe.find("/conductor/workspaces/") {
        let rest = &probe[idx + "/conductor/workspaces/".len()..];
        if let Some((repo, _)) = rest.split_once('/').filter(|(r, _)| !r.is_empty()) {
            return Some(repo.to_string());
        }
    }
    let base = worktrees_base?.trim_end_matches('/');
    let rest = norm_cwd.strip_prefix(&format!("{base}/"))?;
    let parts: Vec<&str> = rest.split('/').collect();
    (parts.len() >= 2 && !parts[0].is_empty() && parts[0] != "..").then(|| parts[0].to_string())
}

/// Nodes whose `project`/`cwd` disagree with the workspace map, each with
/// the canonical values. A consistent node yields nothing; a node whose
/// project cannot be determined is left for a human.
fn detect_rescope_fixes(
    entries: &[Value],
    workspaces: &HashMap<String, String>,
    worktrees_base: Option<&str>,
) -> Vec<RescopeFix> {
    if workspaces.is_empty() {
        return Vec::new();
    }
    let path_to_project: HashMap<&str, &str> = workspaces
        .iter()
        .map(|(proj, path)| (path.as_str(), proj.as_str()))
        .collect();
    let mut fixes = Vec::new();
    for e in entries {
        let Some(node_id) = entry_id(e) else {
            continue;
        };
        if !truthy(e.get("cwd")) {
            continue;
        }
        let norm_cwd = crate::territory::normalize_path(&py_str(e.get("cwd")));
        let proj = get_str(e, "project");
        let candidate = path_to_project.get(norm_cwd.as_str()).copied();
        let target: Option<String> = match proj.filter(|p| workspaces.contains_key(*p)) {
            Some(p) => (norm_cwd != workspaces[p]).then(|| p.to_string()),
            None => match candidate {
                Some(c) => (Some(c) != proj).then(|| c.to_string()),
                None if !truthy(e.get("project")) => worktree_repo_hint(&norm_cwd, worktrees_base)
                    .filter(|hint| workspaces.contains_key(hint)),
                None => None,
            },
        };
        let Some(target) = target else {
            continue;
        };
        let new_cwd = workspaces[&target].clone();
        if proj == Some(target.as_str()) && norm_cwd == new_cwd {
            continue;
        }
        fixes.push(RescopeFix {
            node_id: node_id.to_string(),
            new_project: target,
            new_cwd,
        });
    }
    fixes
}

// ---------------------------------------------------------------------------
// Temp leaks and url-less PRs
// ---------------------------------------------------------------------------

fn is_temp_cwd(cwd: Option<&Value>) -> bool {
    let Some(raw) = cwd.and_then(Value::as_str).filter(|c| !c.is_empty()) else {
        return false;
    };
    let norm = crate::territory::normalize_path(raw);
    TEMP_CWD_MARKERS.iter().any(|m| norm.contains(m))
}

fn detect_temp_leaks(entries: &[Value]) -> Vec<String> {
    entries
        .iter()
        .filter(|e| is_temp_cwd(e.get("cwd")))
        .filter_map(|e| entry_id(e).map(str::to_string))
        .collect()
}

#[derive(Clone, Debug)]
struct PrUrlFix {
    node_id: String,
    pr_number: i64,
    cwd: Option<String>,
    pr_url: Option<String>,
}

/// The repo slug for a node's recorded cwd, or None when it is gone. Never
/// degrades to the invocation cwd: a bulk pass that did would stamp every
/// stale-cwd row with the sweeping repo's slug.
fn slug_from_node_cwd(cwd: Option<&str>) -> Option<String> {
    let path = crate::territory::normalize_path(cwd.filter(|c| !c.is_empty())?);
    if !Path::new(&path).is_dir() {
        return None;
    }
    super::pr_link::resolve_current_repo_slug(Some(&path))
}

/// Rows carrying a `pr_number` with no `pr_url`, keyed off the node's own
/// `cwd`; an unresolvable repo comes back url-less so the caller reports it
/// instead of guessing. One resolution per distinct cwd.
fn detect_url_less_prs(
    entries: &[Value],
    resolver: &dyn Fn(Option<&str>) -> Option<String>,
) -> Vec<PrUrlFix> {
    let mut slugs: HashMap<Option<String>, Option<String>> = HashMap::new();
    let mut fixes = Vec::new();
    for e in entries {
        let (Some(nid), Some(pr)) = (entry_id(e), e.get("pr_number").and_then(Value::as_i64))
        else {
            continue;
        };
        if truthy(e.get("pr_url")) {
            continue;
        }
        let cwd = get_str(e, "cwd").map(str::to_string);
        let slug = slugs
            .entry(cwd.clone())
            .or_insert_with(|| resolver(cwd.as_deref()))
            .clone();
        fixes.push(PrUrlFix {
            node_id: nid.to_string(),
            pr_number: pr,
            cwd,
            pr_url: slug.map(|s| super::pr_link::pr_url_from_slug(&s, pr)),
        });
    }
    fixes
}

// ---------------------------------------------------------------------------
// Dedup, rollup and shared-plan cost (propose-only)
// ---------------------------------------------------------------------------

/// Groups of more than one idea sharing a normalized title. Scoped to the
/// idea pile, so distinct ready or done work is never flagged.
fn detect_dup_groups(entries: &[Value]) -> Vec<Vec<String>> {
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    for e in entries {
        if get_str(e, "status") != Some("idea") {
            continue;
        }
        let Some(nid) = entry_id(e) else {
            continue;
        };
        let key = normalize_title(e.get("title"));
        if key.is_empty() {
            continue;
        }
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, ids)) => ids.push(nid.to_string()),
            None => groups.push((key, vec![nid.to_string()])),
        }
    }
    groups
        .into_iter()
        .filter(|(_, ids)| ids.len() > 1)
        .map(|(_, ids)| ids)
        .collect()
}

/// Open orphans whose best epic candidate is worth a human look, best-first
/// and capped. Propose-only even under `--apply`: a bulk reparent has no
/// human reading a receipt the way intake's one-at-a-time link does.
fn detect_rollup_candidates(entries: &[Value], limit: usize) -> Vec<(String, String, f64)> {
    let index: BTreeMap<String, Value> = entries
        .iter()
        .filter_map(|e| entry_id(e).map(|id| (id.to_string(), e.clone())))
        .collect();
    let mut proposals: Vec<(String, String, f64)> = Vec::new();
    for (nid, entry) in &index {
        if matches!(
            get_str(entry, "status"),
            Some("done") | Some("superseded") | Some("deferred")
        ) {
            continue;
        }
        if !super::autolink::is_orphan(entry, &index) {
            continue;
        }
        if let Some((epic, score, _)) = super::relatedness::epic_candidates(entry, entries, 1, None)
            .into_iter()
            .next()
        {
            proposals.push((nid.clone(), epic, score));
        }
    }
    proposals.sort_by(|a, b| {
        b.2.partial_cmp(&a.2)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    proposals.truncate(limit);
    proposals
}

/// Plans whose cost more than one node claims. One costed holder plus
/// contained children is the legal shape; only the double-count reports.
fn detect_shared_plan_cost_violations(entries: &[Value]) -> Vec<(String, Vec<String>)> {
    let mut holders: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for e in entries {
        if e.get("cost_usd").is_none_or(Value::is_null) {
            continue;
        }
        let Some(plan) = crate::graph_store::normalize_plan_path(get_str(e, "plan_path")) else {
            continue;
        };
        if let Some(nid) = entry_id(e) {
            holders.entry(plan).or_default().push(nid.to_string());
        }
    }
    holders
        .into_iter()
        .filter(|(_, ids)| ids.len() > 1)
        .map(|(plan, mut ids)| {
            ids.sort();
            (plan, ids)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Mis-harnessed session rows
// ---------------------------------------------------------------------------

/// One sessions[] row whose harness its own id's shape contradicts.
#[derive(Clone, Debug, PartialEq)]
struct HarnessMismatch {
    node_id: String,
    session_id: String,
    phase: String,
    wrong: String,
    right: String,
}

impl HarnessMismatch {
    fn keyed(&self) -> (String, String, String, String) {
        (
            self.node_id.clone(),
            self.session_id.clone(),
            self.phase.clone(),
            self.wrong.clone(),
        )
    }
}

/// Every wrong-shape row, split by whether the node also carries the
/// shape-correct twin for the same (session_id, phase). A twinned row is a
/// phantom to drop; a lone row is the node's only record of the session,
/// so its harness is corrected and the row kept.
fn detect_harness_mismatches(entries: &[Value]) -> (Vec<HarnessMismatch>, Vec<HarnessMismatch>) {
    let (mut twins, mut lone) = (Vec::new(), Vec::new());
    for e in entries {
        let (Some(nid), Some(rows)) = (entry_id(e), e.get("sessions").and_then(Value::as_array))
        else {
            continue;
        };
        let mut groups: Vec<((&str, &str), Vec<&str>)> = Vec::new();
        for r in rows {
            let (Some(sid), Some(phase), Some(harness)) = (
                get_str(r, "session_id").filter(|s| !s.is_empty()),
                get_str(r, "phase").filter(|s| !s.is_empty()),
                get_str(r, "harness").filter(|s| !s.is_empty()),
            ) else {
                continue;
            };
            match groups.iter_mut().find(|(k, _)| *k == (sid, phase)) {
                Some((_, hs)) => hs.push(harness),
                None => groups.push(((sid, phase), vec![harness])),
            }
        }
        for ((sid, phase), harnesses) in groups {
            let Some(shape) = crate::identity::harness_of_session_id(sid) else {
                continue;
            };
            let twinned = harnesses.contains(&shape);
            if twinned && harnesses.len() < 2 {
                continue;
            }
            for h in harnesses.into_iter().filter(|h| *h != shape) {
                let m = HarnessMismatch {
                    node_id: nid.to_string(),
                    session_id: sid.to_string(),
                    phase: phase.to_string(),
                    wrong: h.to_string(),
                    right: shape.to_string(),
                };
                if twinned {
                    twins.push(m);
                } else {
                    lone.push(m);
                }
            }
        }
    }
    twins.sort_by_key(HarnessMismatch::keyed);
    lone.sort_by_key(HarnessMismatch::keyed);
    (twins, lone)
}

fn is_session_row(r: &Value, sid: &str, phase: &str, harness: &str) -> bool {
    get_str(r, "session_id") == Some(sid)
        && get_str(r, "phase") == Some(phase)
        && get_str(r, "harness") == Some(harness)
}

/// Drop each twin inside the caller's locked mutation, re-checked there so
/// a row settled since the scan never drops provenance.
fn apply_twin_drops(
    ents: &mut [Value],
    drops: &[HarnessMismatch],
    claimed: &BTreeSet<String>,
    skipped: &mut Vec<String>,
) -> Vec<Value> {
    let mut applied = Vec::new();
    for d in drops {
        if claimed.contains(&d.node_id) {
            skipped.push(d.node_id.clone());
            continue;
        }
        let Some(rows) = ents
            .iter_mut()
            .find(|e| entry_id(e) == Some(d.node_id.as_str()))
            .and_then(|n| n.get_mut("sessions"))
            .and_then(Value::as_array_mut)
        else {
            continue;
        };
        let has_twin = rows
            .iter()
            .any(|o| is_session_row(o, &d.session_id, &d.phase, &d.right));
        let before = rows.len();
        if has_twin {
            rows.retain(|r| !is_session_row(r, &d.session_id, &d.phase, &d.wrong));
        }
        if rows.len() != before {
            applied.push(json!({
                "node_id": d.node_id, "session_id": d.session_id, "phase": d.phase,
                "dropped_harness": d.wrong, "kept_harness": d.right,
            }));
        }
    }
    applied
}

/// Correct each lone wrong harness inside the caller's locked mutation; a
/// row dropped, gone or already correct since the scan is never rewritten.
fn apply_harness_shape_fixes(
    ents: &mut [Value],
    fixes: &[HarnessMismatch],
    claimed: &BTreeSet<String>,
    skipped: &mut Vec<String>,
) -> Vec<Value> {
    let mut applied = Vec::new();
    for f in fixes {
        if claimed.contains(&f.node_id) {
            skipped.push(f.node_id.clone());
            continue;
        }
        let Some(rows) = ents
            .iter_mut()
            .find(|e| entry_id(e) == Some(f.node_id.as_str()))
            .and_then(|n| n.get_mut("sessions"))
            .and_then(Value::as_array_mut)
        else {
            continue;
        };
        let mut touched = false;
        for r in rows.iter_mut() {
            if is_session_row(r, &f.session_id, &f.phase, &f.wrong) {
                r["harness"] = json!(f.right);
                touched = true;
            }
        }
        if touched {
            applied.push(json!({
                "node_id": f.node_id, "session_id": f.session_id, "phase": f.phase,
                "was_harness": f.wrong, "now_harness": f.right,
            }));
        }
    }
    applied
}

fn twin_payload(drops: &[HarnessMismatch], applied: &[Value], apply: bool) -> Value {
    json!({
        "applied": if apply { applied.to_vec() } else { Vec::new() },
        "candidates": drops.iter().map(|t| json!({
            "node_id": t.node_id, "session_id": t.session_id, "phase": t.phase,
            "dropped_harness": t.wrong, "kept_harness": t.right,
        })).collect::<Vec<_>>(),
    })
}

fn shape_fix_payload(fixes: &[HarnessMismatch], applied: &[Value], apply: bool) -> Value {
    json!({
        "applied": if apply { applied.to_vec() } else { Vec::new() },
        "candidates": fixes.iter().map(|f| json!({
            "node_id": f.node_id, "session_id": f.session_id, "phase": f.phase,
            "was_harness": f.wrong, "now_harness": f.right,
        })).collect::<Vec<_>>(),
    })
}

// ---------------------------------------------------------------------------
// Stale ideas, stale ready work and suspect reverts
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
struct Stale {
    node_id: String,
    age_days: i64,
}

/// Ideas STRICTLY older than `staleness_days` since their last curation
/// touch (`touched_at`, else `created_at`), with no movement.
fn detect_stale_ideas(entries: &[Value], staleness_days: i64, now_ms: i64) -> Vec<Stale> {
    let mut out = Vec::new();
    for e in entries {
        if get_str(e, "status") != Some("idea") {
            continue;
        }
        let Some(nid) = entry_id(e) else {
            continue;
        };
        if crate::backlog_ready::node_has_movement(e, now_ms, staleness_days) {
            continue;
        }
        let touched = e
            .get("touched_at")
            .and_then(parse_iso_ms)
            .or_else(|| e.get("created_at").and_then(parse_iso_ms));
        let Some(touched) = touched else {
            continue;
        };
        let age_days = days_between(now_ms, touched);
        if age_days > staleness_days {
            out.push(Stale {
                node_id: nid.to_string(),
                age_days,
            });
        }
    }
    out
}

/// Ready nodes quarantine-eligible under the one predicate live selection
/// also reads, so the two can never disagree about what is stale.
fn detect_stale_ready(entries: &[Value], staleness_days: i64, now_ms: i64) -> Vec<Stale> {
    entries
        .iter()
        .filter(|e| get_str(e, "status") == Some("ready"))
        .filter(|e| crate::backlog_ready::is_stale_ready(e, now_ms, staleness_days))
        .filter_map(|e| {
            let age_days = e
                .get("created_at")
                .and_then(parse_iso_ms)
                .map(|c| days_between(now_ms, c))
                .unwrap_or(-1);
            entry_id(e).map(|id| Stale {
                node_id: id.to_string(),
                age_days,
            })
        })
        .collect()
}

/// The drain's reason at any configured staleness_days.
fn is_drain_reason(reason: &str) -> bool {
    let Some(rest) = reason.strip_prefix("stale >") else {
        return false;
    };
    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    digits > 0 && rest[digits..].starts_with("d, drained by maintain")
}

#[derive(Clone, Debug)]
struct SuspectRevert {
    node_id: String,
    title: String,
    priority: String,
    deferred_at: String,
    signal: String,
}

/// Drained nodes carrying evidence a human curated them. Read-only: a
/// second automated reversal would repeat the defect, so the operator rules.
fn detect_suspect_reverts(entries: &[Value], events: &[Value]) -> Vec<SuspectRevert> {
    let mut undeferred_at: BTreeMap<String, String> = BTreeMap::new();
    for rec in events {
        let kind = get_str(rec, "type")
            .filter(|t| !t.is_empty())
            .or_else(|| get_str(rec, "kind"));
        if kind != Some("node_undeferred") {
            continue;
        }
        let data = rec.get("data").filter(|d| d.is_object()).unwrap_or(rec);
        let nid = get_str(data, "unit_id")
            .filter(|s| !s.is_empty())
            .or_else(|| get_str(data, "node_id"));
        let (Some(nid), Some(ts)) = (nid, get_str(rec, "ts")) else {
            continue;
        };
        let slot = undeferred_at
            .entry(nid.to_string())
            .or_insert_with(|| ts.to_string());
        if ts < slot.as_str() {
            *slot = ts.to_string();
        }
    }
    let mut out = Vec::new();
    for e in entries {
        if !get_str(e, "deferred_reason").is_some_and(is_drain_reason) {
            continue;
        }
        let Some(nid) = entry_id(e) else {
            continue;
        };
        let deferred_at = if truthy(e.get("deferred_at")) {
            py_str(e.get("deferred_at"))
        } else {
            String::new()
        };
        // Parsed, never compared raw: the event log stamps `Z` while
        // deferred_at carries `+00:00` and microseconds.
        let reversed = match (
            undeferred_at
                .get(nid)
                .and_then(|ts| parse_iso_ms(&json!(ts))),
            parse_iso_ms(&json!(deferred_at)),
        ) {
            (Some(undef), Some(deferred)) => undef < deferred,
            _ => false,
        };
        let priority = get_str(e, "priority").unwrap_or("");
        let signal = if reversed {
            "undeferred before this defer".to_string()
        } else if priority == "p0" || priority == "p1" {
            format!("priority {priority} (non-default ranking)")
        } else if e.get("rank").is_some_and(|r| !r.is_null()) {
            "curated board rank".into()
        } else if truthy(e.get("sessions")) {
            "session history".into()
        } else if truthy(e.get("progress_notes")) {
            "progress notes".into()
        } else if truthy(e.get("plan_path")) {
            "reached planning (plan_path set)".into()
        } else if truthy(e.get("pr_number")) {
            "reached delivery (pr_number set)".into()
        } else {
            continue;
        };
        out.push(SuspectRevert {
            node_id: nid.to_string(),
            title: if truthy(e.get("title")) {
                py_str(e.get("title"))
            } else {
                String::new()
            },
            priority: if truthy(e.get("priority")) {
                py_str(e.get("priority"))
            } else {
                String::new()
            },
            deferred_at,
            signal,
        });
    }
    out
}

/// `(count, cap)` when the Now column exceeds its WIP cap. The column is
/// the board's own authority, with its whole-graph overlays bound once.
fn now_overflow(entries: &[Value], cap: i64) -> Option<(i64, i64)> {
    use crate::backlog_ready::{
        epics_with_child_progress, in_progress_epic_ids, make_effective_priority,
    };
    let by_id: BTreeMap<String, Value> = entries
        .iter()
        .filter_map(|e| entry_id(e).map(|id| (id.to_string(), e.clone())))
        .collect();
    let child_progress = epics_with_child_progress(&by_id);
    let in_progress = in_progress_epic_ids(entries, &by_id, &child_progress, &BTreeSet::new());
    let effective = make_effective_priority(&by_id, &child_progress);
    let count = entries
        .iter()
        .filter(|e| {
            super::projections::kanban_column(e, &in_progress, &effective, &by_id, &child_progress)
                == Some("Now")
        })
        .count() as i64;
    (count > cap).then_some((count, cap))
}

// ---------------------------------------------------------------------------
// Failure-prone ready nodes
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct FailureDefer {
    node_id: String,
    streak: i64,
    error: String,
}

impl FailureDefer {
    /// Sentinel first (the prefix is load-bearing), then the streak, then
    /// the truncated spawn error the drains died on.
    fn reason(&self) -> String {
        let base = format!(
            "{} {} consecutive failed attempts",
            super::triage_health::AUTO_FAILURE_SENTINEL,
            self.streak
        );
        if self.error.is_empty() {
            base
        } else {
            format!(
                "{base}: {}",
                self.error.chars().take(200).collect::<String>()
            )
        }
    }
}

/// The newest `advance_failed` error inside the window the streak counts,
/// so a cause never outlives the streak it explains.
fn last_advance_failed_error(node_id: &str, events: &[Value]) -> String {
    use super::advance::{classify_streak_row, StreakSignal};
    for raw in events.iter().rev() {
        if let Some((event_node, signal)) = classify_streak_row(raw) {
            if event_node != node_id {
                continue;
            }
            if signal == StreakSignal::Reset {
                break;
            }
            continue;
        }
        if get_str(raw, "type") != Some("advance_failed") {
            continue;
        }
        let data = raw.get("data").filter(|d| d.is_object()).unwrap_or(raw);
        let owner = get_str(data, "node_id")
            .filter(|s| !s.is_empty())
            .or_else(|| get_str(data, "unit_id"));
        if owner != Some(node_id) {
            continue;
        }
        let error = if truthy(data.get("error")) {
            py_str(data.get("error"))
        } else {
            String::new()
        };
        if !error.trim().is_empty() {
            return error.trim().to_string();
        }
    }
    String::new()
}

/// Ready, undeferred nodes whose consecutive-failure streak reaches the
/// threshold: the picks `next` would still burn an iteration on.
fn detect_failure_defers(entries: &[Value], events: &[Value], threshold: i64) -> Vec<FailureDefer> {
    if threshold < 1 {
        return Vec::new();
    }
    let mut out = Vec::new();
    for e in entries {
        if get_str(e, "status") != Some("ready") || truthy(e.get("deferred_at")) {
            continue;
        }
        let Some(nid) = entry_id(e) else {
            continue;
        };
        let streak = super::advance::consecutive_failures(nid, events);
        if streak >= threshold {
            out.push(FailureDefer {
                node_id: nid.to_string(),
                streak,
                error: last_advance_failed_error(nid, events),
            });
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Abandoned do rows
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
struct AbandonedRow {
    node: String,
    harness: Option<String>,
    session_id: Option<String>,
    verdict: &'static str,
    reason: String,
}

/// Seconds since the row last showed life: its start, or a progress note
/// its own session wrote. None when neither stamp parses.
fn do_row_idle_s(entry: &Value, row: &Value, now_ms: i64) -> Option<i64> {
    let sid = row.get("session_id");
    let notes = entry
        .get("progress_notes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|n| n.is_object() && n.get("source_session_id") == sid)
        .filter_map(|n| n.get("ts").and_then(parse_iso_ms));
    let newest = row
        .get("started_at")
        .and_then(parse_iso_ms)
        .into_iter()
        .chain(notes)
        .max()?;
    Some((now_ms - newest) / 1000)
}

fn open_do_rows(entry: &Value) -> impl Iterator<Item = &Value> {
    entry
        .get("sessions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|r| crate::graph_store::is_open_do_row(r))
}

/// Every non-terminal, unclaimed node's open do rows, gone or held. A live
/// claim or a reachable worker on the node outranks the idle clock.
fn detect_abandoned_do_rows(
    entries: &[Value],
    claimed: &BTreeSet<String>,
    worked: &BTreeMap<String, Vec<String>>,
    now_ms: i64,
    quiet_after_s: i64,
) -> Vec<AbandonedRow> {
    let mut out = Vec::new();
    for e in entries {
        let Some(nid) = entry_id(e).filter(|id| !id.is_empty()) else {
            continue;
        };
        if truthy(e.get("locked_by"))
            || get_str(e, "status").is_some_and(|s| crate::graph_store::TERMINAL_RUNGS.contains(&s))
            || truthy(e.get("superseded_by"))
        {
            continue;
        }
        for row in open_do_rows(e) {
            let workers = worked.get(nid).filter(|w| !w.is_empty());
            let (verdict, reason) = if claimed.contains(nid) {
                ("held", "live claim".to_string())
            } else if let Some(workers) = workers {
                (
                    "held",
                    format!("reachable worker on node {}", workers.join(", ")),
                )
            } else {
                let idle = do_row_idle_s(e, row, now_ms);
                match idle {
                    Some(idle) if idle > quiet_after_s => (
                        "gone",
                        format!(
                            "row idle {}h, no reachable worker on the node",
                            idle.div_euclid(3600)
                        ),
                    ),
                    _ => (
                        "held",
                        format!(
                            "row idle {}h, inside the {}h bound",
                            idle.map(|i| i.div_euclid(3600).to_string())
                                .unwrap_or_else(|| "?".into()),
                            quiet_after_s / 3600
                        ),
                    ),
                }
            };
            out.push(AbandonedRow {
                node: nid.to_string(),
                harness: get_str(row, "harness").map(str::to_string),
                session_id: get_str(row, "session_id").map(str::to_string),
                verdict,
                reason,
            });
        }
    }
    out
}

pub(crate) type Roster<'a> = &'a dyn Fn() -> Result<BTreeMap<String, Vec<String>>, String>;

/// The leg's read: the roster is consulted only when some open do row is
/// already past the bound. An unread roster refuses (`strict`) or holds
/// every row with the reason, never reaps on a blind fleet read.
fn abandoned_do_rows(
    entries: &[Value],
    claimed: &BTreeSet<String>,
    hours: i64,
    roster: Roster,
    now_ms: i64,
    strict: bool,
) -> Result<Vec<AbandonedRow>, String> {
    let bound = hours * 3600;
    let past_bound = entries
        .iter()
        .any(|e| open_do_rows(e).any(|r| do_row_idle_s(e, r, now_ms).unwrap_or(0) > bound));
    let worked = if past_bound {
        match roster() {
            Ok(map) => map,
            Err(reason) if strict => return Err(format!("roster unread ({reason})")),
            Err(reason) => {
                return Ok(entries
                    .iter()
                    .flat_map(|e| {
                        let reason = reason.clone();
                        open_do_rows(e).map(move |r| AbandonedRow {
                            node: entry_id(e).unwrap_or_default().to_string(),
                            harness: get_str(r, "harness").map(str::to_string),
                            session_id: get_str(r, "session_id").map(str::to_string),
                            verdict: "held",
                            reason: format!("roster unread ({reason})"),
                        })
                    })
                    .collect())
            }
        }
    } else {
        BTreeMap::new()
    };
    Ok(detect_abandoned_do_rows(
        entries, claimed, &worked, now_ms, bound,
    ))
}

/// Detect, reap (under `--apply`, blast-capped) and render the leg. A gone
/// row's open execute window is closed through the session reap door; the
/// row and its provenance stay.
fn abandoned_leg(
    entries: &[Value],
    claimed: &BTreeSet<String>,
    graph: &Path,
    apply: bool,
    hours: i64,
    roster: Roster,
    now_ms: i64,
) -> (Vec<String>, Option<String>) {
    let rows = match abandoned_do_rows(entries, claimed, hours, roster, now_ms, true) {
        Ok(rows) => rows,
        Err(e) => {
            return (
                Vec::new(),
                Some(format!("abandoned-do-row leg skipped: {e}")),
            )
        }
    };
    let mut reaped: BTreeMap<String, Result<Value, String>> = BTreeMap::new();
    let mut reaped_rows = 0usize;
    let mut truncated = 0usize;
    if apply {
        let gone: Vec<&AbandonedRow> = rows.iter().filter(|r| r.verdict == "gone").collect();
        truncated = gone.len().saturating_sub(AUTO_DEFER_BLAST_CAP);
        for cand in gone.into_iter().take(AUTO_DEFER_BLAST_CAP) {
            let receipt = std::cell::RefCell::new(Value::Null);
            let outcome = super::mutate_single_row(graph, "session_reap_open", |rows| {
                let r = crate::graph_keeper::session_reap_open(
                    rows,
                    Some(&cand.node),
                    "execute",
                    cand.harness.as_deref().unwrap_or_default(),
                    cand.session_id.as_deref().unwrap_or_default(),
                    None,
                )
                .map_err(|e| e.to_string())?;
                receipt.replace(r);
                Ok(true)
            });
            match outcome {
                Ok(_) => {
                    reaped.insert(cand.node.clone(), Ok(receipt.into_inner()));
                    reaped_rows += 1;
                }
                Err(e) => {
                    reaped.insert(cand.node.clone(), Err(e));
                }
            }
        }
    }
    let mut lines = vec![if apply {
        format!(
            "abandoned-do-rows reaped {reaped_rows} of {} candidate(s)",
            rows.len()
        )
    } else {
        format!("abandoned-do-row candidates {}", rows.len())
    }];
    for r in &rows {
        let sid: String = r
            .session_id
            .clone()
            .unwrap_or_else(|| "None".into())
            .chars()
            .take(8)
            .collect();
        let tag = format!("{} {sid}", r.harness.as_deref().unwrap_or("None"));
        match reaped.get(&r.node) {
            Some(Err(e)) => lines.push(format!("  warning: do-row reap of {} failed: {e}", r.node)),
            Some(Ok(rep)) => lines.push(format!(
                "  settled do row {} ({tag}): row_closed {}, status_after {} ({})",
                r.node,
                rep.get("row_closed")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                rep.get("status_after")
                    .and_then(Value::as_str)
                    .unwrap_or("None"),
                r.reason
            )),
            None => {
                let verb = if r.verdict == "gone" {
                    "would reap"
                } else {
                    "held"
                };
                lines.push(format!("  {verb} do row {} ({tag}): {}", r.node, r.reason));
            }
        }
    }
    if truncated > 0 {
        lines.push(format!(
            "  NOTE: abandoned-do-row blast cap hit - {truncated} gone row(s) not reaped (cap {AUTO_DEFER_BLAST_CAP}); re-run to continue"
        ));
    }
    (lines, None)
}

// ---------------------------------------------------------------------------
// The pass
// ---------------------------------------------------------------------------

/// Every source the pass reads, injected so the whole verb runs hermetic
/// under test; `run_maintain` wires the production readers.
pub(crate) struct Seams<'a> {
    pub graph: PathBuf,
    pub settings: Settings,
    pub workspaces: HashMap<String, String>,
    pub worktrees_base: Option<String>,
    /// Live `node:<id>` claims; an Err fails an apply closed.
    pub claims: &'a dyn Fn() -> Result<BTreeSet<String>, String>,
    /// The failure-event history (the streak and undefer reader).
    pub events: &'a dyn Fn() -> Vec<Value>,
    pub slug: &'a dyn Fn(Option<&str>) -> Option<String>,
    pub roster: Roster<'a>,
    pub history: Option<PathBuf>,
    pub deck_dir: Option<PathBuf>,
    pub sweep: SweepSeams<'a>,
}

/// The wall-clock budget, checked between legs.
struct Budget {
    deadline: Option<Instant>,
}

impl Budget {
    fn new(seconds: i64) -> Self {
        Budget {
            deadline: (seconds > 0)
                .then(|| Instant::now() + std::time::Duration::from_secs(seconds as u64)),
        }
    }

    fn expired(&self) -> bool {
        self.deadline.is_some_and(|d| Instant::now() > d)
    }

    /// Seconds left, floored at 0; None when unbounded.
    fn remaining(&self) -> Option<f64> {
        self.deadline
            .map(|d| d.saturating_duration_since(Instant::now()).as_secs_f64())
    }
}

struct Pass<'o> {
    out: &'o mut Out,
    budget: Budget,
    started: Instant,
    legs_done: Vec<(String, String)>,
    flags: Flags,
    history: Option<PathBuf>,
}

impl Pass<'_> {
    fn duration_s(&self) -> f64 {
        round1(self.started.elapsed().as_secs_f64())
    }

    fn record_history(&mut self, report: &Value) {
        match &self.history {
            Some(path) => super::triage_health::append_history(report, &[], path, 90),
            None => self
                .out
                .err("warning: maintain health-history append failed: no state directory resolves"),
        }
    }

    fn done(&mut self, leg: &str, detail: impl ToString) {
        self.legs_done.push((leg.to_string(), detail.to_string()));
    }

    /// Enter one leg. Past the deadline: land a `complete: false` history
    /// row, print the partial receipt, and answer exit 4.
    fn enter(&mut self, leg: &str) -> Result<(), i32> {
        if !self.budget.expired() {
            return Ok(());
        }
        let duration_s = self.duration_s();
        self.record_history(&json!({
            "scope": "maintain",
            "applied": self.flags.apply,
            "complete": false,
            "incomplete_leg": leg,
            "duration_s": duration_s,
        }));
        // The receipt divides only by the legs this run could complete.
        let leg_total = MAINTAIN_LEG_TOTAL - usize::from(self.flags.no_validity);
        self.out.err(format!(
            "budget exceeded in leg '{leg}' after {}s; {}/{leg_total} legs completed; results partial",
            py_float(duration_s),
            self.legs_done.len()
        ));
        if self.flags.json {
            let legs: Map<String, Value> = self
                .legs_done
                .iter()
                .map(|(k, v)| (k.clone(), json!(v)))
                .collect();
            self.out.line(crate::graph_store::to_python_json(&json!({
                "complete": false,
                "incomplete_leg": leg,
                "duration_s": duration_s,
                "legs_completed": legs,
            })));
        } else {
            for (name, detail) in self.legs_done.clone() {
                self.out.line(format!("  {name}: {detail}"));
            }
            self.out
                .line("results partial: the remaining legs never ran; re-run to finish");
        }
        Err(4)
    }
}

/// Live claims: an apply fails closed on an unreadable claim store; a
/// report degrades to no claims.
fn live_claims(seams: &Seams, strict: bool, out: &mut Out) -> Result<BTreeSet<String>, i32> {
    match (seams.claims)() {
        Ok(ids) => Ok(ids),
        Err(_) if strict => {
            out.err("Error: live claim state is unavailable; backlog maintain --apply refused.");
            Err(1)
        }
        Err(_) => Ok(BTreeSet::new()),
    }
}

/// The working graph: archive residents are history, never a maintenance
/// candidate.
fn read_graph(graph: &Path) -> Result<Vec<Value>, StoreError> {
    let live = super::RowQuery {
        include_archived: false,
        ..Default::default()
    };
    let mut rows = crate::graph_store::read_rows_where_strict(graph, &live)?;
    crate::graph_store::recompute_statuses(&mut rows);
    Ok(rows)
}

/// What the one locked apply mutation landed. Rebuilt on every attempt,
/// because a conflict retry reruns the mutation over a fresh read.
#[derive(Default)]
struct Applied {
    rescope: Vec<String>,
    prune: Vec<String>,
    pr_urls: Vec<Value>,
    twins: Vec<Value>,
    shapes: Vec<Value>,
    defers: Vec<Value>,
    stale_ready: Vec<Value>,
    skipped: Vec<String>,
}

impl Applied {
    fn any(&self) -> bool {
        !(self.rescope.is_empty()
            && self.prune.is_empty()
            && self.pr_urls.is_empty()
            && self.twins.is_empty()
            && self.shapes.is_empty()
            && self.defers.is_empty()
            && self.stale_ready.is_empty())
    }
}

/// The deterministic legs' candidates, applied as one locked mutation.
struct Candidates<'c> {
    rescope: &'c [RescopeFix],
    prune: &'c [String],
    pr_urls: &'c [&'c PrUrlFix],
    twins: &'c [HarnessMismatch],
    shapes: &'c [HarnessMismatch],
    defers: &'c [FailureDefer],
    stale_ready: &'c [Stale],
    ready_staleness_days: i64,
}

impl Candidates<'_> {
    fn is_empty(&self) -> bool {
        self.rescope.is_empty()
            && self.prune.is_empty()
            && self.pr_urls.is_empty()
            && self.twins.is_empty()
            && self.shapes.is_empty()
            && self.defers.is_empty()
            && self.stale_ready.is_empty()
    }

    /// One pass over the fresh rows. Every candidate is re-checked here, in
    /// the lock: a node that raced to a claim, a close or a defer is left.
    fn apply(&self, ents: &mut Vec<Value>, claimed: &BTreeSet<String>) -> Applied {
        let mut a = Applied::default();
        let index = |ents: &[Value], id: &str| super::node_ref::find_node_index(ents, id);
        for fix in self.rescope {
            if claimed.contains(&fix.node_id) {
                a.skipped.push(fix.node_id.clone());
                continue;
            }
            if let Some(i) = index(ents, &fix.node_id) {
                ents[i]["project"] = json!(fix.new_project);
                ents[i]["cwd"] = json!(fix.new_cwd);
                a.rescope.push(fix.node_id.clone());
            }
        }
        let mut prune: BTreeSet<&str> = BTreeSet::new();
        for nid in self.prune {
            if claimed.contains(nid) {
                a.skipped.push(nid.clone());
                continue;
            }
            prune.insert(nid);
            a.prune.push(nid.clone());
        }
        if !prune.is_empty() {
            // Mirror `remove`: drop the node AND its dangling blocked_by refs.
            for e in ents.iter_mut() {
                if let Some(blocked) = e.get_mut("blocked_by").and_then(Value::as_array_mut) {
                    blocked.retain(|b| !b.as_str().is_some_and(|b| prune.contains(b)));
                }
            }
            ents.retain(|e| !entry_id(e).is_some_and(|id| prune.contains(id)));
        }
        for fix in self.pr_urls {
            if claimed.contains(&fix.node_id) {
                a.skipped.push(fix.node_id.clone());
                continue;
            }
            let (Some(i), Some(url)) = (index(ents, &fix.node_id), &fix.pr_url) else {
                continue;
            };
            // A present url outranks the derived one.
            if truthy(ents[i].get("pr_url"))
                || ents[i].get("pr_number").and_then(Value::as_i64) != Some(fix.pr_number)
            {
                continue;
            }
            ents[i]["pr_url"] = json!(url);
            a.pr_urls
                .push(json!({"node_id": fix.node_id, "pr_url": url}));
        }
        a.twins = apply_twin_drops(ents, self.twins, claimed, &mut a.skipped);
        a.shapes = apply_harness_shape_fixes(ents, self.shapes, claimed, &mut a.skipped);
        let now_iso = crate::graph_store::now_isoformat();
        for cand in self.defers {
            if claimed.contains(&cand.node_id) {
                a.skipped.push(cand.node_id.clone());
                continue;
            }
            let Some(i) = index(ents, &cand.node_id) else {
                continue;
            };
            if truthy(ents[i].get("completed_at")) || truthy(ents[i].get("deferred_at")) {
                continue;
            }
            let reason = cand.reason();
            let n = ents[i].as_object_mut().expect("graph rows are objects");
            n.insert("completed_at".into(), Value::Null);
            n.insert("deferred_at".into(), json!(now_iso));
            n.insert("deferred_reason".into(), json!(reason));
            // Sentinel vocabulary, not an expired drift: no kind.
            n.shift_remove("deferred_kind");
            a.defers
                .push(json!({"node_id": cand.node_id, "streak": cand.streak, "reason": reason}));
        }
        let now = now_ms();
        for cand in self.stale_ready {
            if claimed.contains(&cand.node_id) {
                a.skipped.push(cand.node_id.clone());
                continue;
            }
            let Some(i) = index(ents, &cand.node_id) else {
                continue;
            };
            if truthy(ents[i].get("completed_at")) || truthy(ents[i].get("deferred_at")) {
                continue;
            }
            // A candidate that moved since the scan is live work now.
            if !crate::backlog_ready::is_stale_ready(&ents[i], now, self.ready_staleness_days) {
                continue;
            }
            let n = ents[i].as_object_mut().expect("graph rows are objects");
            n.insert("completed_at".into(), Value::Null);
            n.insert("deferred_at".into(), json!(now_iso));
            n.insert("deferred_reason".into(), json!(STALE_QUARANTINE_REASON));
            n.insert("deferred_kind".into(), json!("expired"));
            a.stale_ready.push(json!({
                "node_id": cand.node_id, "age_days": cand.age_days, "reason": STALE_QUARANTINE_REASON,
            }));
        }
        a
    }
}

fn cap_by<T: Clone>(cands: Vec<T>, key: impl Fn(&T) -> (i64, String)) -> (Vec<T>, usize) {
    if cands.len() <= AUTO_DEFER_BLAST_CAP {
        return (cands, 0);
    }
    let mut sorted = cands;
    sorted.sort_by_key(|c| key(c));
    let truncated = sorted.len() - AUTO_DEFER_BLAST_CAP;
    sorted.truncate(AUTO_DEFER_BLAST_CAP);
    (sorted, truncated)
}

/// Run the whole pass: detect, apply, validity, history, report. Returns
/// the exit code.
pub(crate) fn run_pass(flags: Flags, seams: &Seams, out: &mut Out) -> i32 {
    let entries = match read_graph(&seams.graph) {
        Ok(rows) => rows,
        Err(e) => {
            out.err(format!(
                "Error: the graph is unreadable ({e}); backlog maintain refused."
            ));
            return 1;
        }
    };
    let now = now_ms();
    let s = &seams.settings;

    if flags.suspect_reverts {
        // A read-only retro sweep, not another leg.
        let reverts = detect_suspect_reverts(&entries, &(seams.events)());
        out.line(format!(
            "reversals: {} of the drained pile carry evidence of a human decision",
            reverts.len()
        ));
        for r in &reverts {
            out.line(format!(
                "  {}  {}  {}  {}  {}",
                r.node_id,
                r.priority,
                r.deferred_at.chars().take(10).collect::<String>(),
                r.signal,
                r.title.chars().take(60).collect::<String>()
            ));
        }
        out.line(
            "(read-only: no node was changed. Rule on these yourself with `fno backlog undefer <id>...`)",
        );
        return 0;
    }

    // Apply legs never touch a node a live target session is driving.
    let claimed = match live_claims(seams, flags.apply, out) {
        Ok(c) => c,
        Err(code) => return code,
    };
    let mut pass = Pass {
        out,
        budget: Budget::new(s.budget_seconds),
        started: Instant::now(),
        legs_done: Vec::new(),
        flags,
        history: seams.history.clone(),
    };
    macro_rules! enter {
        ($leg:expr) => {
            if let Err(code) = pass.enter($leg) {
                return code;
            }
        };
    }

    enter!("rescope");
    let rescope_fixes =
        detect_rescope_fixes(&entries, &seams.workspaces, seams.worktrees_base.as_deref());
    pass.done("rescope", rescope_fixes.len());
    enter!("temp-leaks");
    let prune_ids = detect_temp_leaks(&entries);
    pass.done("temp-leaks", prune_ids.len());
    enter!("pr-url");
    let pr_url_fixes = detect_url_less_prs(&entries, seams.slug);
    pass.done("pr-url", pr_url_fixes.len());
    let pr_url_writable: Vec<&PrUrlFix> =
        pr_url_fixes.iter().filter(|f| f.pr_url.is_some()).collect();
    let pr_url_unresolvable: Vec<&PrUrlFix> =
        pr_url_fixes.iter().filter(|f| f.pr_url.is_none()).collect();
    let (twin_drops, shape_fixes) = detect_harness_mismatches(&entries);
    enter!("session-twins");
    pass.done("session-twins", twin_drops.len());
    enter!("harness-shape");
    pass.done("harness-shape", shape_fixes.len());
    enter!("dedup");
    let dup_groups = detect_dup_groups(&entries);
    pass.done("dedup", dup_groups.len());
    enter!("shared-plan-cost");
    let plan_cost = detect_shared_plan_cost_violations(&entries);
    pass.done("shared-plan-cost", plan_cost.len());
    enter!("rollup");
    let rollup = detect_rollup_candidates(&entries, ROLLUP_PROPOSAL_CAP);
    pass.done("rollup", rollup.len());
    enter!("stale-ideas");
    let stale = detect_stale_ideas(&entries, s.staleness_days, now);
    pass.done("stale-ideas", stale.len());

    // The propose-only mirror of the failure-defer leg over READY rows, with
    // the same blast cap.
    enter!("stale-ready");
    let (stale_ready, stale_ready_truncated) = cap_by(
        detect_stale_ready(&entries, s.ready_staleness_days, now),
        |c| (-c.age_days, c.node_id.clone()),
    );
    pass.done("stale-ready", stale_ready.len());
    enter!("now-cap");
    let overflow = now_overflow(&entries, s.now_wip_cap);
    pass.done("now-cap", if overflow.is_some() { 2 } else { 0 });

    enter!("failure-defers");
    let events = (seams.events)();
    let (defer_cands, defer_truncated) = cap_by(
        detect_failure_defers(&entries, &events, s.max_failed_attempts),
        |d| (-d.streak, d.node_id.clone()),
    );
    pass.done("failure-defers", defer_cands.len());

    enter!("abandoned");
    let (ab_lines, ab_warn) = abandoned_leg(
        &entries,
        &claimed,
        &seams.graph,
        flags.apply,
        s.abandoned_do_row_hours,
        seams.roster,
        now,
    );
    if let Some(w) = ab_warn {
        pass.out.err(format!("warning: {w}"));
    }
    pass.done("abandoned", ab_lines.len());

    enter!("apply");
    let mut applied = Applied::default();
    let cands = Candidates {
        rescope: &rescope_fixes,
        prune: &prune_ids,
        pr_urls: &pr_url_writable,
        twins: &twin_drops,
        shapes: &shape_fixes,
        defers: &defer_cands,
        stale_ready: &stale_ready,
        ready_staleness_days: s.ready_staleness_days,
    };
    if flags.apply && !cands.is_empty() {
        let published = crate::graph_store::mutate_rows(
            &seams.graph,
            std::time::Duration::from_secs(30),
            None,
            None,
            |ents| {
                let fresh = (seams.claims)().map_err(StoreError::ClaimsUnavailable)?;
                let current: BTreeSet<String> = claimed.union(&fresh).cloned().collect();
                applied = cands.apply(ents, &current);
                Ok(applied.any())
            },
        );
        match published {
            Ok(_) => {}
            Err(StoreError::ClaimsUnavailable(_)) => {
                pass.out.err(
                    "Error: live claim state is unavailable; backlog maintain --apply refused.",
                );
                return 1;
            }
            Err(e) => {
                pass.out
                    .err(format!("Error: backlog maintain --apply write failed: {e}"));
                return 1;
            }
        }
    }

    // The validity sweep reviews the oldest stale ideas into an immutable
    // deck; watermarked ideas never re-enter, so a later run finds 0
    // eligible and skips the analyzer call.
    enter!("validity");
    let mut validity_result = None;
    if !flags.no_validity {
        if let Some(deck_dir) = &seams.deck_dir {
            let second = match live_claims(seams, flags.apply, pass.out) {
                Ok(c) => c,
                Err(code) => return code,
            };
            let all_claimed: BTreeSet<String> = claimed.union(&second).cloned().collect();
            let reread = || read_graph(&seams.graph).map_err(|e| e.to_string());
            let input = validity::SweepInput {
                entries: &entries,
                validity_days: &s.validity_days,
                batch_size: &s.validity_batch_size,
                out_dir: deck_dir,
                claimed: &all_claimed,
                recheck: flags.recheck,
                now_ms: now,
                run_timeout: pass.budget.remaining(),
                reread: Some(&reread),
            };
            let res = validity::run_validity_sweep(&input, &seams.sweep);
            if let Some(err) = &res.error {
                if !flags.json {
                    pass.out.err(format!("validity: {err}"));
                    return 1;
                }
            }
            pass.done("validity", format!("{} eligible", res.eligible));
            validity_result = Some(res);
        }
    }

    let apply = flags.apply;
    let pick = |done: usize, proposed: usize| if apply { done } else { proposed };
    let overflow_json = overflow
        .map(|(c, cap)| json!([c, cap]))
        .unwrap_or(Value::Null);
    let defer_nodes = if apply {
        applied.defers.clone()
    } else {
        defer_cands
            .iter()
            .map(|c| json!({"node_id": c.node_id, "streak": c.streak}))
            .collect()
    };
    let stale_ready_nodes = if apply {
        applied.stale_ready.clone()
    } else {
        stale_ready
            .iter()
            .map(|c| json!({"node_id": c.node_id, "age_days": c.age_days}))
            .collect()
    };
    let report = json!({
        "scope": "maintain",
        "applied": apply,
        "complete": true,
        "duration_s": pass.duration_s(),
        "rescoped": pick(applied.rescope.len(), rescope_fixes.len()),
        "pruned": pick(applied.prune.len(), prune_ids.len()),
        "pr_url_backfilled": pick(applied.pr_urls.len(), pr_url_writable.len()),
        "pr_url_unresolvable": pr_url_unresolvable.len(),
        "dedup_groups": dup_groups.len(),
        "shared_plan_cost_violations": plan_cost.len(),
        "rollup_candidates": rollup.len(),
        "stale_ideas": stale.len(),
        "now_overflow": overflow_json,
        "skipped_claimed": applied.skipped.len(),
        "auto_deferred": pick(applied.defers.len(), defer_cands.len()),
        "auto_deferred_nodes": defer_nodes,
        "auto_defer_truncated": defer_truncated,
        "stale_ready": pick(applied.stale_ready.len(), stale_ready.len()),
        "stale_ready_nodes": stale_ready_nodes,
        "stale_ready_truncated": stale_ready_truncated,
        "incomplete_leg": null,
    });
    pass.record_history(&report);

    if flags.json {
        let mut payload = json!({
            "applied": apply,
            "rescope": {
                "applied": if apply { applied.rescope.clone() } else { Vec::new() },
                "candidates": rescope_fixes.iter().map(|f| json!({
                    "node_id": f.node_id, "new_project": f.new_project, "new_cwd": f.new_cwd,
                })).collect::<Vec<_>>(),
            },
            "prune": {
                "applied": if apply { applied.prune.clone() } else { Vec::new() },
                "candidates": prune_ids,
            },
            "pr_url_backfill": {
                "applied": if apply { applied.pr_urls.clone() } else { Vec::new() },
                "candidates": pr_url_writable.iter().map(|f| json!({
                    "node_id": f.node_id, "pr_number": f.pr_number, "pr_url": f.pr_url,
                })).collect::<Vec<_>>(),
                "unresolvable": pr_url_unresolvable.iter().map(|f| json!({
                    "node_id": f.node_id, "pr_number": f.pr_number, "cwd": f.cwd,
                })).collect::<Vec<_>>(),
            },
            "dedup_groups": dup_groups,
            "shared_plan_cost_violations": plan_cost.iter().map(|(plan, nodes)| json!({
                "plan_path": plan, "nodes": nodes,
            })).collect::<Vec<_>>(),
            "rollup_candidates": rollup.iter().map(|(n, e, score)| json!({
                "node_id": n, "epic_id": e, "score": score,
            })).collect::<Vec<_>>(),
            "stale_ideas": stale.iter().map(|s| json!({"node_id": s.node_id, "age_days": s.age_days})).collect::<Vec<_>>(),
            "now_overflow": overflow_json,
            "skipped_claimed": applied.skipped,
            "auto_defer": {
                "applied": if apply { applied.defers.clone() } else { Vec::new() },
                "candidates": defer_cands.iter().map(|c| json!({"node_id": c.node_id, "streak": c.streak})).collect::<Vec<_>>(),
                "truncated": defer_truncated,
            },
            "stale_ready": {
                "applied": if apply { applied.stale_ready.clone() } else { Vec::new() },
                "candidates": stale_ready.iter().map(|c| json!({"node_id": c.node_id, "age_days": c.age_days})).collect::<Vec<_>>(),
                "truncated": stale_ready_truncated,
            },
            "session_twins": twin_payload(&twin_drops, &applied.twins, apply),
            "session_harness_fixes": shape_fix_payload(&shape_fixes, &applied.shapes, apply),
            "complete": true,
            "duration_s": pass.duration_s(),
        });
        if let Some(v) = &validity_result {
            payload["validity"] = json!({
                "eligible": v.eligible,
                "counts": v.counts,
                "deck": v.deck_md,
                "degraded": v.degraded,
                "stale": v.stale,
                "error": v.error,
            });
        }
        pass.out.line(crate::graph_store::to_python_json(&payload));
        return if validity_result.as_ref().is_some_and(|v| v.error.is_some()) {
            1
        } else {
            0
        };
    }

    let ctx = Report {
        apply,
        settings: s,
        rescope_fixes: &rescope_fixes,
        prune_ids: &prune_ids,
        pr_url_writable: &pr_url_writable,
        pr_url_unresolvable: &pr_url_unresolvable,
        dup_groups: &dup_groups,
        plan_cost: &plan_cost,
        rollup: &rollup,
        stale: &stale,
        stale_ready: &stale_ready,
        stale_ready_truncated,
        defer_cands: &defer_cands,
        defer_truncated,
        overflow,
        twin_drops: &twin_drops,
        shape_fixes: &shape_fixes,
        ab_lines: &ab_lines,
        applied: &applied,
        validity: validity_result.as_ref(),
    };
    ctx.render(pass.out);
    0
}

/// The human per-leg summary. Every category prints its count, zero
/// included: a silent category reads as "nothing to do".
struct Report<'r> {
    apply: bool,
    settings: &'r Settings,
    rescope_fixes: &'r [RescopeFix],
    prune_ids: &'r [String],
    pr_url_writable: &'r [&'r PrUrlFix],
    pr_url_unresolvable: &'r [&'r PrUrlFix],
    dup_groups: &'r [Vec<String>],
    plan_cost: &'r [(String, Vec<String>)],
    rollup: &'r [(String, String, f64)],
    stale: &'r [Stale],
    stale_ready: &'r [Stale],
    stale_ready_truncated: usize,
    defer_cands: &'r [FailureDefer],
    defer_truncated: usize,
    overflow: Option<(i64, i64)>,
    twin_drops: &'r [HarnessMismatch],
    shape_fixes: &'r [HarnessMismatch],
    ab_lines: &'r [String],
    applied: &'r Applied,
    validity: Option<&'r validity::SweepResult>,
}

impl Report<'_> {
    fn render(&self, out: &mut Out) {
        let a = self.applied;
        let overflow = if self.overflow.is_some() { "yes" } else { "no" };
        if self.apply {
            // "written N of M", never a bare N: the in-lock pass skips raced rows.
            out.line(format!(
                "pr-url written {} of {} | pr-url unresolvable {}",
                a.pr_urls.len(),
                self.pr_url_writable.len(),
                self.pr_url_unresolvable.len()
            ));
        } else {
            out.line(format!(
                "pr-url proposed {} | pr-url unresolvable {}",
                self.pr_url_writable.len(),
                self.pr_url_unresolvable.len()
            ));
        }
        for f in self.pr_url_unresolvable {
            out.line(format!(
                "  unresolvable pr_url {} (PR #{}, cwd={})",
                f.node_id,
                f.pr_number,
                f.cwd
                    .as_deref()
                    .filter(|c| !c.is_empty())
                    .unwrap_or("unset")
            ));
        }
        if self.apply {
            out.line(format!(
                "re-scoped {} | pruned {} | auto-deferred {} | stale-ready-deferred {} | \
                 dedup-groups {} | rollup-candidates {} | stale-ideas {} | now-overflow {overflow} | \
                 skipped-claimed {}",
                a.rescope.len(),
                a.prune.len(),
                a.defers.len(),
                a.stale_ready.len(),
                self.dup_groups.len(),
                self.rollup.len(),
                self.stale.len(),
                a.skipped.len()
            ));
        } else {
            out.line(format!(
                "re-scope candidates {} | prune candidates {} | auto-defer candidates {} | \
                 stale-ready candidates {} | dedup-groups {} | rollup-candidates {} | \
                 stale-ideas {} | now-overflow {overflow}  (run with --apply to apply the deterministic legs)",
                self.rescope_fixes.len(),
                self.prune_ids.len(),
                self.defer_cands.len(),
                self.stale_ready.len(),
                self.dup_groups.len(),
                self.rollup.len(),
                self.stale.len()
            ));
        }
        for rf in self.rescope_fixes {
            let verb = if self.apply && a.rescope.contains(&rf.node_id) {
                "re-scoped"
            } else {
                "would re-scope"
            };
            out.line(format!(
                "  {verb} {} -> project={} cwd={}",
                rf.node_id, rf.new_project, rf.new_cwd
            ));
        }
        for nid in self.prune_ids {
            let verb = if self.apply && a.prune.contains(nid) {
                "pruned"
            } else {
                "would prune (temp-cwd leak)"
            };
            out.line(format!("  {verb} {nid}"));
        }
        if self.apply {
            for d in &a.defers {
                out.line(format!(
                    "  auto-deferred {} ({} consecutive failures): {}",
                    py_str(d.get("node_id")),
                    py_str(d.get("streak")),
                    py_str(d.get("reason"))
                ));
            }
        } else {
            for c in self.defer_cands {
                out.line(format!(
                    "  would auto-defer {} ({} consecutive failures, >= {}): fno backlog undefer {} to recover",
                    c.node_id, c.streak, self.settings.max_failed_attempts, c.node_id
                ));
            }
        }
        if self.defer_truncated > 0 {
            out.line(format!(
                "  NOTE: auto-defer blast cap hit - {} further candidate(s) NOT deferred this run \
                 (cap {AUTO_DEFER_BLAST_CAP}); re-run to continue",
                self.defer_truncated
            ));
        }
        if self.apply {
            for d in &a.stale_ready {
                out.line(format!(
                    "  stale-ready deferred {} ({}d unmoved): {}",
                    py_str(d.get("node_id")),
                    py_str(d.get("age_days")),
                    py_str(d.get("reason"))
                ));
            }
        } else {
            for c in self.stale_ready {
                out.line(format!(
                    "  would quarantine stale-ready {} ({}d unmoved, >{}d): fno backlog undefer {} to recover",
                    c.node_id, c.age_days, self.settings.ready_staleness_days, c.node_id
                ));
            }
        }
        if self.stale_ready_truncated > 0 {
            out.line(format!(
                "  NOTE: stale-ready blast cap hit - {} further candidate(s) NOT quarantined this run \
                 (cap {AUTO_DEFER_BLAST_CAP}); re-run to continue",
                self.stale_ready_truncated
            ));
        }
        for group in self.dup_groups {
            out.line(format!(
                "  near-duplicate ideas (merge/supersede by hand): {}",
                group.join(", ")
            ));
        }
        for (plan, nodes) in self.plan_cost {
            out.line(format!(
                "  shared-plan cost double-count {plan}: {} all carry cost_usd (a plan is one PR \
                 is one node; one node is the delivery unit, the rest are contained). Read-only: \
                 pick the unit and `fno backlog update <other> plan_path=null` by hand.",
                nodes.join(", ")
            ));
        }
        if self.apply {
            for d in &a.twins {
                out.line(format!(
                    "  dropped twin {} ({}, phase {}): harness {} contradicts the id shape, {} twin kept",
                    py_str(d.get("node_id")),
                    py_str(d.get("session_id")),
                    py_str(d.get("phase")),
                    py_str(d.get("dropped_harness")),
                    py_str(d.get("kept_harness"))
                ));
            }
            for d in &a.shapes {
                out.line(format!(
                    "  fixed harness {} ({}, phase {}): {} -> {} (the id's own shape)",
                    py_str(d.get("node_id")),
                    py_str(d.get("session_id")),
                    py_str(d.get("phase")),
                    py_str(d.get("was_harness")),
                    py_str(d.get("now_harness"))
                ));
            }
        } else {
            for t in self.twin_drops {
                out.line(format!(
                    "  would drop twin {} ({}, phase {}): harness {} contradicts the id shape, {} twin kept",
                    t.node_id, t.session_id, t.phase, t.wrong, t.right
                ));
            }
            for f in self.shape_fixes {
                out.line(format!(
                    "  would fix harness {} ({}, phase {}): {} -> {} (the id's own shape)",
                    f.node_id, f.session_id, f.phase, f.wrong, f.right
                ));
            }
        }
        if !self.ab_lines.is_empty() {
            out.line(self.ab_lines.join("\n"));
        }
        for (nid, epic, score) in self.rollup {
            out.line(format!(
                "  rollup candidate {nid} -> {epic} ({score:.2}): fno backlog update {nid} parent={epic}"
            ));
        }
        self.render_stale(out);
        if let Some((count, cap)) = self.overflow {
            out.line(format!(
                "  Now over WIP cap ({count} > {cap}): run `fno backlog triage propose` to demote \
                 lower-priority work (never auto-reprioritized)"
            ));
        }
        if !a.skipped.is_empty() {
            out.line(format!(
                "  skipped {} live-claimed node(s): {}",
                a.skipped.len(),
                a.skipped.join(", ")
            ));
        }
        if let Some(v) = self.validity {
            for w in &v.warnings {
                out.err(format!("  validity config: {w}"));
            }
            if v.eligible == 0 {
                out.line("validity: 0 eligible ideas");
            } else {
                let count = |k: &str| v.counts.get(k).and_then(Value::as_i64).unwrap_or(0);
                let tag = if v.degraded {
                    " (DEGRADED: analyzer unavailable)"
                } else {
                    ""
                };
                let stale_note = if v.stale > 0 {
                    format!(", {} stale", v.stale)
                } else {
                    String::new()
                };
                out.line(format!(
                    "validity: reviewed {} ideas{tag} -> promote {} | keep {} | supersede {} | \
                     needs-human {}{stale_note}",
                    v.eligible,
                    count("promote"),
                    count("keep"),
                    count("supersede"),
                    count("needs-human")
                ));
                out.line(format!(
                    "  deck: {}",
                    v.deck_md.as_deref().unwrap_or("None")
                ));
            }
        }
    }

    /// A bounded receipt: summary, the 10 oldest, and one drain command
    /// (`--no-validity` skips the analyzer call that made `-J` hang).
    fn render_stale(&self, out: &mut Out) {
        if self.stale.is_empty() {
            out.line("  stale ideas: 0");
            return;
        }
        let mut ages: Vec<i64> = self.stale.iter().map(|s| s.age_days).collect();
        ages.sort();
        let mut oldest: Vec<&Stale> = self.stale.iter().collect();
        oldest.sort_by(|a, b| b.age_days.cmp(&a.age_days));
        out.line(format!(
            "  stale ideas: {} (age {}-{}d) - drain in one locked write:",
            self.stale.len(),
            ages[0],
            ages[ages.len() - 1]
        ));
        for s in oldest.iter().take(10) {
            out.line(format!("    {} ({}d)", s.node_id, s.age_days));
        }
        if self.stale.len() > 10 {
            out.line(format!("    (showing 10 of {} oldest)", self.stale.len()));
        }
        out.line(format!(
            "    fno backlog maintain --no-validity -J | jq -r '.stale_ideas[].node_id' | xargs \
             fno backlog defer -R 'stale >{}d, drained by maintain' --kind expired",
            self.settings.staleness_days
        ));
    }
}

/// `fno backlog maintain`: parse the verb's flags, wire the production
/// readers, run the pass, and return the exit code (4 on a budget overrun,
/// with the partial receipt already printed).
pub fn run_maintain(tail: &[String]) -> i32 {
    let flags = match parse_flags(tail) {
        Ok(flags) => flags,
        Err(code) => return code,
    };
    let cwd = std::env::current_dir().unwrap_or_default();
    let claims = || super::next::live_claimed_node_ids();
    let events = || super::advance::read_failure_events(None);
    let roster = || super::worked::reachable_workers_by_node();
    let worktrees_base = crate::agents_config::config_lookup(&cwd, &["paths", "worktrees_base"])
        .and_then(|v| v.as_str().map(str::to_string))
        .filter(|b| !b.is_empty())
        .map(|b| crate::territory::normalize_path(&b));
    let seams = Seams {
        graph: super::settings::graph_path(),
        settings: Settings::load(),
        workspaces: crate::territory::workspace_paths(&cwd),
        worktrees_base,
        claims: &claims,
        events: &events,
        slug: &slug_from_node_cwd,
        roster: &roster,
        history: super::triage_health::history_path(&Value::Null),
        deck_dir: super::settings::state_dir().map(|d| d.join("validity-decks")),
        sweep: SweepSeams {
            exists_for: &validity::validity_exists_for,
            search: &validity::validity_search,
            retro: &validity::validity_retro_source,
            analyze: &validity::run_validity_analysis,
        },
    };
    let mut out = Out::live();
    run_pass(flags, &seams, &mut out)
}

#[cfg(test)]
#[path = "maintenance_tests.rs"]
mod tests;
