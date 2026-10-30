//! `fno backlog carveout`: capture left-out work to the session ledger.
//!
//! A carve-out is a raw signal that an executor consciously left work undone.
//! It is not a node: the retro harvest reads the ledger, dedups, classifies,
//! and decides what becomes one. The `backfill` kind survives the harvest for
//! `/fno:ship pr merged`.
//!
//! Machine-first surface: the id goes to stdout, warnings and errors to
//! stderr. Exit codes: 0 ok, 2 invalid arguments, 1 write or read failure.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::retro::classify::py_splitlines;
use crate::retro::ledger::{self, BACKFILL_KIND, VALID_KINDS};
use crate::retro::py_str;

const VALID_SEVERITIES: [&str; 4] = ["critical", "high", "medium", "low"];
const DESCRIPTION_CAP: usize = 8000;
const DIFFICULTY_LADDER: [&str; 3] = ["low", "medium", "high"];

const USAGE: &str = "Usage: fno backlog carveout <add|list|update|resolve> [OPTIONS]

  LAST RESORT for work too big to land in this PR. Fix what you find instead:
  a problem you can fix here gets fixed here, in this PR, as its own commit -
  SIZE is the only justification for filing instead. The retro-triage harvest
  at merge turns deferred/oos-bug into backlog nodes (deduped, classified),
  while backfill is handled by /fno:ship pr merged. That harvest is manual:
  `fno backlog retro sweep-carveouts --apply` is the only thing that clears
  the ledger, so every row you file is a chore for a human. See
  `fno inbox outstanding` for what has piled up.

Commands:
  add      Last resort: record work too big for this PR.
  list     List recorded carve-outs (read-only).
  update   Correct a carve-out in place, keeping its id.
  resolve  Remove handled carve-out(s) from the ledger.";

const ADD_USAGE: &str = "Usage: fno backlog carveout add DESCRIPTION -k KIND [OPTIONS]

  Last resort: record work too big for this PR. Anything smaller, fix here.
  DESCRIPTION is what was left undone, and why. Truncated past 8000 chars
  (never rejected). Paired commands: `fno backlog carveout update <id>`
  corrects one in place (the id survives); `fno backlog carveout resolve <id>`
  retires it.

Options:
  -k, --kind TEXT      deferred (blocked on an open question) | oos-bug
                       (out-of-scope bug) | backfill (a data backfill the
                       merged PR enables). Required.
  --need TEXT          The dependency the work is blocked on: an open question
                       (deferred) or a precondition (backfill).
  -p, --priority TEXT  Priority hint p0-p3 the harvested node should inherit;
                       default p3 at triage.
  --scope TEXT         The role scope this carve-out discharges (e.g. an epic
                       id). The lead orphan check reads this field.
  --severity TEXT      critical|high|medium|low, routed to priority p0..p3 at
                       harvest. A carveout filed to satisfy the fidelity gate
                       is stamped high by provenance, not chosen here.";

const LIST_USAGE: &str = "Usage: fno backlog carveout list [OPTIONS]

  List recorded carve-outs (read-only). Reads the canonical ledger, so it works
  from a linked worktree. A missing ledger prints nothing and exits 0. The
  default scope is this session plus ownerless rows.

Options:
  -k, --kind TEXT      Filter to one kind (deferred | oos-bug | backfill).
  --session-id TEXT    Filter to rows recorded under this session id.
                       Repeatable.
  --pr-number INTEGER  Resolve the merged PR's owning session(s) from
                       ledger.json and filter to them. Mutually exclusive with
                       --session-id.
  -A, --all            Read the whole ledger, not just this session's rows.
  -J, --json           One JSON object per line. With --pr-number, one object
                       carrying sessions_resolved + carveouts.";

const UPDATE_USAGE: &str = "Usage: fno backlog carveout update ID [OPTIONS]

  Correct a carve-out in place, keeping its id. Only the options you pass are
  replaced. Refuses with no field given (2), an id not on the ledger (1), a
  blank --description (2), or an unreadable or unwritable ledger (1). To retire
  a row instead, `fno backlog carveout resolve <id> --reason \"...\"`.

Options:
  -d, --description TEXT  Replacement text. Truncated past 8000 chars, like add.
  -k, --kind TEXT         Reclassify: deferred | oos-bug | backfill.
  --need TEXT             Replacement dependency text.
  -p, --priority TEXT     Replacement priority hint (p0-p3).
  --scope TEXT            Replacement role scope.";

const RESOLVE_USAGE: &str = "Usage: fno backlog carveout resolve ID... [OPTIONS]

  Remove handled carve-out(s) from the ledger. Idempotent: an id not present is
  a no-op. Prints the count actually removed. To correct a carve-out rather
  than retire it, use `fno backlog carveout update`, which preserves the id.

Options:
  -R, --reason TEXT  Why these rows are being retired without a node. Recorded
                     as a carveout_resolved event: the only trace a row leaves
                     when it is removed without becoming tracked work.";

/// The parsed options of one command: value flags, switches, positionals.
#[derive(Default)]
struct Parsed {
    values: Vec<(&'static str, String)>,
    switches: Vec<&'static str>,
    positionals: Vec<String>,
}

impl Parsed {
    fn value(&self, name: &str) -> Option<&str> {
        self.values
            .iter()
            .rev()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v.as_str())
    }

    fn all(&self, name: &str) -> Vec<String> {
        self.values
            .iter()
            .filter(|(k, _)| *k == name)
            .map(|(_, v)| v.clone())
            .collect()
    }

    fn has(&self, name: &str) -> bool {
        self.switches.contains(&name)
    }
}

/// `spellings` maps a flag spelling to its canonical name and whether it takes
/// a value. An unknown option is a usage error, never a positional.
fn parse(args: &[String], spellings: &[(&str, &'static str, bool)]) -> Result<Parsed, String> {
    let mut parsed = Parsed::default();
    let mut i = 0;
    let mut fence = false;
    while i < args.len() {
        let arg = &args[i];
        i += 1;
        if fence || !arg.starts_with('-') || arg == "-" {
            parsed.positionals.push(arg.clone());
            continue;
        }
        if arg == "--" {
            fence = true;
            continue;
        }
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (arg.as_str(), None),
        };
        let Some((_, name, takes_value)) = spellings.iter().find(|(s, _, _)| *s == flag) else {
            return Err(format!("carveout: no such option: {flag}"));
        };
        if !*takes_value {
            parsed.switches.push(name);
            continue;
        }
        let value = match inline {
            Some(v) => v,
            None => {
                let Some(next) = args.get(i) else {
                    return Err(format!("carveout: option {flag} requires a value"));
                };
                i += 1;
                next.clone()
            }
        };
        parsed.values.push((name, value));
    }
    Ok(parsed)
}

fn wants_help(args: &[String]) -> bool {
    args.iter()
        .take_while(|a| a.as_str() != "--")
        .any(|a| a == "--help" || a == "-h")
}

fn usage_error(message: impl AsRef<str>) -> i32 {
    eprintln!("{}", message.as_ref());
    2
}

pub fn run_carveout(tail: &[String]) -> i32 {
    let Some(cmd) = tail.first() else {
        eprintln!("{USAGE}");
        return 2;
    };
    let rest = &tail[1..];
    let (body, usage): (fn(&[String]) -> i32, &str) = match cmd.as_str() {
        "--help" | "-h" => {
            println!("{USAGE}");
            return 0;
        }
        "add" => (cmd_add, ADD_USAGE),
        "list" => (cmd_list, LIST_USAGE),
        "update" => (cmd_update, UPDATE_USAGE),
        "resolve" => (cmd_resolve, RESOLVE_USAGE),
        "_read" => return internal_read(),
        "_session" => return internal_session(),
        other => {
            eprintln!("carveout: no such command: {other}\n{USAGE}");
            return 2;
        }
    };
    if wants_help(rest) {
        println!("{usage}");
        return 0;
    }
    body(rest)
}

// ---------------------------------------------------------------------------
// roots and session identity
// ---------------------------------------------------------------------------

fn pinned_root() -> Option<PathBuf> {
    std::env::var_os("FNO_REPO_ROOT")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn cwd() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// The live worktree: its manifest names the session.
fn session_root() -> PathBuf {
    pinned_root().unwrap_or_else(|| crate::paths::worktree_repo_root(&cwd()))
}

/// The canonical checkout owns the ledger, so a carve-out captured inside a
/// linked worktree survives that worktree's archival.
fn ledger_root() -> PathBuf {
    pinned_root()
        .or_else(|| crate::paths::canonical_repo_root(&cwd()))
        .unwrap_or_else(session_root)
}

/// The `---`-fenced `key: value` scan. Plain scalars only.
fn manifest_keys(text: &str) -> Map<String, Value> {
    let mut keys = Map::new();
    let mut lines = py_splitlines(text).into_iter();
    if lines.next().map(str::trim) != Some("---") {
        return keys;
    }
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            keys.insert(key.trim().to_string(), json!(value.trim()));
        }
    }
    keys
}

/// The active target session: the manifest's `fno_id` (legacy `session_id`),
/// else `$CLAUDECODE_SESSION_ID`. `None` files the row unscoped: capture is
/// never lost over a missing session. The manifest comes from its owning
/// resolver: a worktree session's lives in the project space.
fn resolve_session_id(root: &Path) -> Option<String> {
    let keys = crate::state_path::resolve("target-state", root)
        .and_then(|manifest| std::fs::read_to_string(manifest).ok())
        .map(|text| manifest_keys(&text))
        .unwrap_or_default();
    ["fno_id", "session_id"]
        .into_iter()
        .filter_map(|k| keys.get(k).and_then(Value::as_str))
        .find(|v| !v.is_empty() && *v != "null")
        .map(str::to_string)
        .or_else(|| {
            std::env::var("CLAUDECODE_SESSION_ID")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
}

/// The node this worker provably owns. The claim lockfile is the liveness
/// authority and is harness-agnostic, and a handoff re-points the claim while
/// a stale manifest still names the former node, so the claim goes first. The
/// manifest match is the fallback for a claude session whose claim died
/// mid-run. Zero or two matching claims fall through: never guess.
fn owning_node_id(root: &Path) -> Option<String> {
    let wanted: BTreeSet<String> = crate::claims::ambient_session_candidates()
        .into_iter()
        .map(|c| format!("target-session:{c}"))
        .collect();
    if !wanted.is_empty() {
        let claims = crate::claims::list(Some("node:"), None, false).unwrap_or_default();
        let matches: Vec<&str> = claims
            .iter()
            .filter(|c| wanted.contains(&c.holder))
            .map(|c| c.key.as_str())
            .collect();
        if let [only] = matches.as_slice() {
            if let Some(node) = only.strip_prefix("node:") {
                return Some(node.to_string());
            }
        }
    }
    let session = std::env::var("CLAUDE_CODE_SESSION_ID").ok();
    crate::registry_commands::find_held_node_at(root, session.as_deref())
        .and_then(|held| held.strip_prefix("node:").map(str::to_string))
}

// ---------------------------------------------------------------------------
// ledger writes
// ---------------------------------------------------------------------------

fn truncate_description(text: &str) -> (String, bool) {
    let original = text.chars().count();
    if original <= DESCRIPTION_CAP {
        return (text.to_string(), false);
    }
    let kept: String = text.chars().take(DESCRIPTION_CAP).collect();
    let cut = original - DESCRIPTION_CAP;
    (
        format!("{kept} ... [truncated {cut} of {original} chars]"),
        true,
    )
}

fn mint_id() -> Result<String, String> {
    let mut bytes = [0u8; 4];
    getrandom::fill(&mut bytes).map_err(|e| format!("getrandom failed: {e}"))?;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!("cv-{hex}"))
}

fn is_priority(value: &str) -> bool {
    matches!(value, "p0" | "p1" | "p2" | "p3")
}

fn check_kind(kind: &str) -> Result<(), String> {
    if VALID_KINDS.contains(&kind) {
        return Ok(());
    }
    Err(format!(
        "carveout: invalid --kind '{kind}' (expected one of: {})",
        VALID_KINDS.join(", ")
    ))
}

fn check_priority(priority: &str) -> Result<(), String> {
    if is_priority(priority) {
        return Ok(());
    }
    Err(format!(
        "carveout: invalid --priority '{priority}' (expected p0, p1, p2 or p3)"
    ))
}

/// Raise the owning node's band by one: the carve-out is an artifact saying
/// the stamped band was too low, so no tier grades itself. A bandless node
/// stays bandless, since round-up already reads absent as the strong tier.
/// Non-fatal: a graph failure never loses the row just filed.
fn raise_difficulty_on_outcome(node_id: &str) {
    let graph = crate::backlog::settings::graph_path();
    let _ = crate::backlog::mutate_single_row(&graph, "carveout_outcome", |rows| {
        let Some(obj) = rows
            .iter_mut()
            .find(|r| crate::graph_store::entry_id(r) == Some(node_id))
            .and_then(Value::as_object_mut)
        else {
            return Ok(false);
        };
        let current = obj.get("difficulty").and_then(Value::as_str);
        let Some(at) = DIFFICULTY_LADDER.iter().position(|b| Some(*b) == current) else {
            return Ok(false);
        };
        let raised = DIFFICULTY_LADDER[(at + 1).min(DIFFICULTY_LADDER.len() - 1)];
        if Some(raised) == current {
            return Ok(false);
        }
        super::fields::write_canonical_difficulty(
            obj,
            Some(raised),
            "outcome:carveout",
            &crate::graph_store::now_isoformat(),
            "change",
        );
        Ok(true)
    });
}

fn cmd_add(args: &[String]) -> i32 {
    let parsed = match parse(
        args,
        &[
            ("--kind", "kind", true),
            ("-k", "kind", true),
            ("--need", "need", true),
            ("--priority", "priority", true),
            ("-p", "priority", true),
            ("--scope", "scope", true),
            ("--severity", "severity", true),
        ],
    ) {
        Ok(p) => p,
        Err(e) => return usage_error(e),
    };
    let [description] = parsed.positionals.as_slice() else {
        return usage_error(format!(
            "carveout: add takes exactly one DESCRIPTION\n{ADD_USAGE}"
        ));
    };
    let Some(kind) = parsed.value("kind") else {
        return usage_error("carveout: missing option --kind");
    };
    if let Err(e) = check_kind(kind) {
        return usage_error(e);
    }
    let priority = parsed.value("priority");
    if let Some(Err(e)) = priority.map(check_priority) {
        return usage_error(e);
    }
    let severity = parsed.value("severity");
    if let Some(s) = severity.filter(|s| !VALID_SEVERITIES.contains(s)) {
        return usage_error(format!(
            "carveout: invalid --severity '{s}' (expected one of: {})",
            VALID_SEVERITIES.join(", ")
        ));
    }

    let root = session_root();
    let session_id = resolve_session_id(&root);
    let node = owning_node_id(&root);
    let (text, truncated) = truncate_description(description);
    let id = match mint_id() {
        Ok(id) => id,
        Err(e) => {
            eprintln!("carveout: failed to record carve-out: {e}");
            return 1;
        }
    };
    let row = json!({
        "id": id,
        "ts": chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        "session_id": session_id,
        "kind": kind,
        "priority": priority,
        "need": parsed.value("need"),
        "description": text,
        "truncated": truncated,
        "scope": parsed.value("scope"),
        "severity": severity,
        "node": node,
    });
    if let Err(e) = ledger::append_row(&ledger_root(), &row) {
        eprintln!("carveout: failed to record carve-out: {e}");
        return 1;
    }
    if let Some(node) = &node {
        raise_difficulty_on_outcome(node);
    }
    if session_id.is_none() {
        eprintln!("carveout: no active session; carve-out recorded unscoped");
    }
    // The size test on stderr, never stdout: machine consumers read the id.
    eprintln!(
        "carveout: filed as a last resort - SIZE is the only justification. If this was \
         small enough to fix in this PR, fix it here and `fno backlog carveout resolve` \
         this row. Clears only via `fno backlog retro sweep-carveouts --apply`, which \
         nobody runs unprompted."
    );
    println!("{id}");
    0
}

fn cmd_update(args: &[String]) -> i32 {
    let parsed = match parse(
        args,
        &[
            ("--description", "description", true),
            ("-d", "description", true),
            ("--kind", "kind", true),
            ("-k", "kind", true),
            ("--need", "need", true),
            ("--priority", "priority", true),
            ("-p", "priority", true),
            ("--scope", "scope", true),
        ],
    ) {
        Ok(p) => p,
        Err(e) => return usage_error(e),
    };
    let [cv_id] = parsed.positionals.as_slice() else {
        return usage_error(format!(
            "carveout: update takes exactly one ID\n{UPDATE_USAGE}"
        ));
    };
    if parsed.values.is_empty() {
        return usage_error(
            "carveout: nothing to update; pass at least one of --description / --kind / \
             --need / --priority / --scope",
        );
    }
    let description = parsed.value("description");
    if description.is_some_and(|d| d.trim().is_empty()) {
        return usage_error("carveout: --description cannot be blank");
    }
    let kind = parsed.value("kind");
    if let Some(Err(e)) = kind.map(check_kind) {
        return usage_error(e);
    }
    if let Some(Err(e)) = parsed.value("priority").map(check_priority) {
        return usage_error(e);
    }

    let root = ledger_root();
    // Read before the rewrite, so the warning names a real transition.
    let old_kind = kind.and_then(|_| {
        ledger::read_carveouts(&root, None)
            .ok()?
            .iter()
            .find(|r| py_str(r.get("id")) == *cv_id)
            .map(|r| py_str(r.get("kind")))
    });
    let updated = ledger::rewrite_row(&root, cv_id, |rec| {
        if let Some(d) = description {
            let (text, truncated) = truncate_description(d);
            rec.insert("description".into(), json!(text));
            rec.insert("truncated".into(), json!(truncated));
        }
        for field in ["kind", "need", "priority", "scope"] {
            if let Some(value) = parsed.value(field) {
                rec.insert(field.into(), json!(value));
            }
        }
    });
    let rec = match updated {
        Ok(rec) => rec,
        Err(ledger::RewriteError::NotFound(message)) => {
            eprintln!("carveout: {message}");
            return 1;
        }
        Err(ledger::RewriteError::Failed(message)) => {
            eprintln!("carveout: failed to update {cv_id}: {message}");
            return 1;
        }
    };
    // A warning, not a refusal: crossing the backfill boundary hands the row to
    // a different consumer, and that reclassification is still legitimate.
    if let (Some(old), Some(new)) = (old_kind.as_deref(), kind) {
        if old != new && (old == BACKFILL_KIND || new == BACKFILL_KIND) {
            let owner = if new == BACKFILL_KIND {
                "/fno:ship pr merged"
            } else {
                "the retro-triage harvest"
            };
            eprintln!("carveout: {cv_id} moved {old} -> {new}; it now belongs to {owner} instead");
        }
    }
    if rec.get("truncated").and_then(Value::as_bool) == Some(true) {
        eprintln!("carveout: description truncated at {DESCRIPTION_CAP} chars");
    }
    println!("{cv_id}");
    0
}

fn cmd_resolve(args: &[String]) -> i32 {
    let parsed = match parse(
        args,
        &[("--reason", "reason", true), ("-R", "reason", true)],
    ) {
        Ok(p) => p,
        Err(e) => return usage_error(e),
    };
    if parsed.positionals.is_empty() {
        return usage_error(format!("carveout: missing ID\n{RESOLVE_USAGE}"));
    }
    let mut unique: Vec<String> = Vec::new();
    for id in &parsed.positionals {
        if !unique.contains(id) {
            unique.push(id.clone());
        }
    }
    let root = ledger_root();
    // The removal returns only a count; this read lets the event name the ids
    // actually retired rather than every id typed. An unreadable ledger
    // degrades to "cannot attribute" instead of guessing.
    let present: BTreeSet<String> = ledger::read_carveouts(&root, None)
        .unwrap_or_default()
        .iter()
        .map(|r| py_str(r.get("id")))
        .collect();
    let removed = ledger::consume_carveouts(&root, &unique);
    if removed < unique.len() {
        // A lock timeout or unwritable ledger also returns a low count, so the
        // shortfall must show; an absent id alone stays an idempotent exit 0.
        eprintln!(
            "carveout: resolved {removed} of {} requested id(s); remainder absent or ledger unwritable",
            unique.len()
        );
    }
    // `removed > 0`: at 0 the rows are still on the ledger, nothing was retired.
    if let Some(reason) = parsed.value("reason").filter(|_| removed > 0) {
        let retired: Vec<&str> = unique
            .iter()
            .filter(|id| present.contains(*id))
            .map(String::as_str)
            .collect();
        let named = if retired.is_empty() {
            unique.join(",")
        } else {
            retired.join(",")
        };
        let mut data = Map::new();
        data.insert("carveout_ids".into(), json!(named));
        data.insert("reason".into(), json!(reason));
        data.insert("removed".into(), json!(removed));
        // Rooted at the ledger: a cwd-relative journal lands somewhere that
        // does not survive a linked worktree's archival. The rows are already
        // gone, so a failed emit is named, never a failed resolve.
        let emitter = crate::events::EventEmitter::new(crate::paths::events_path(&root), "backlog");
        if let Err(e) = emitter.emit_fields("carveout_resolved", data) {
            eprintln!(
                "carveout: resolved, but the reason was NOT recorded ({e:?}); re-record it by hand"
            );
        }
    }
    println!("resolved {removed} carve-out(s)");
    0
}

// ---------------------------------------------------------------------------
// reads
// ---------------------------------------------------------------------------

/// A session filter over the ledger. An ownerless row belongs to no session,
/// so an equality filter would hide it from every reader; only the default
/// scope keeps it, since an explicit filter asks a precise ownership question.
fn scope_rows(rows: Vec<Value>, sessions: Option<&[String]>, include_unscoped: bool) -> Vec<Value> {
    let Some(sessions) = sessions else {
        return rows;
    };
    rows.into_iter()
        .filter(|r| match r.get("session_id") {
            None | Some(Value::Null) => include_unscoped,
            owner => sessions.contains(&py_str(owner)),
        })
        .collect()
}

fn cmd_list(args: &[String]) -> i32 {
    let parsed = match parse(
        args,
        &[
            ("--kind", "kind", true),
            ("-k", "kind", true),
            ("--session-id", "session", true),
            ("--pr-number", "pr", true),
            ("--all", "all", false),
            ("-A", "all", false),
            ("--json", "json", false),
            ("-J", "json", false),
        ],
    ) {
        Ok(p) => p,
        Err(e) => return usage_error(e),
    };
    if let Some(extra) = parsed.positionals.first() {
        return usage_error(format!("carveout: unexpected argument {extra:?}"));
    }
    let kind = parsed.value("kind");
    if let Some(Err(e)) = kind.map(check_kind) {
        return usage_error(e);
    }
    let mut sessions = parsed.all("session");
    let pr = match parsed.value("pr").map(str::parse::<i64>) {
        None => None,
        Some(Ok(n)) => Some(n),
        Some(Err(_)) => return usage_error("carveout: --pr-number takes an integer"),
    };
    if pr.is_some() && !sessions.is_empty() {
        return usage_error(
            "carveout: pass --pr-number or --session-id, not both (--pr-number resolves the \
             sessions itself).",
        );
    }
    let json_out = parsed.has("json");

    let mut reason = None;
    if let Some(pr) = pr {
        let gh: &crate::retro::harvest::GhRunner = &crate::retro::harvest::default_gh_runner;
        let slug = crate::retro::cli::current_repo_slug(gh);
        let ledger_json = crate::paths_cli::ledger_json(&cwd());
        (sessions, reason) =
            crate::ledger_axes::pr_sessions(Some(&ledger_json), pr, slug.as_deref());
    }

    // The default scope is this session. --all restores the global read for
    // consumers that fold across sessions.
    let mut include_unscoped = false;
    let mut scoped_to = None;
    let mut no_session = false;
    if pr.is_none() && sessions.is_empty() && !parsed.has("all") {
        match resolve_session_id(&session_root()) {
            Some(current) => {
                sessions = vec![current.clone()];
                scoped_to = Some(current);
                include_unscoped = true;
            }
            // A scoped read that returned nothing here could not be told apart
            // from a clear ledger, so every row prints and the banner says why.
            None => no_session = true,
        }
    }

    let root = ledger_root();
    let everything = match ledger::read_carveouts(&root, kind) {
        Ok(rows) => rows,
        Err(e) => {
            // A failed read is not "no carve-outs": the backfill slot must never
            // read an unreadable ledger as nothing to run.
            eprintln!("carveout: failed to read carve-outs: {e}");
            return 1;
        }
    };
    let total = everything.len();
    // Unresolved ownership is not "filter to nothing": the caller sees every
    // row read-only plus the reason, and consumes none.
    let filter = (!sessions.is_empty()).then_some(sessions.as_slice());
    let rows = scope_rows(everything, filter, include_unscoped);

    if let Some(pr) = pr {
        if json_out {
            println!(
                "{}",
                json!({
                    "pr_number": pr,
                    "sessions_resolved": sessions,
                    "reason": reason,
                    "consumable": !sessions.is_empty(),
                    "carveouts": rows,
                })
            );
            return 0;
        }
        if let Some(r) = &reason {
            eprintln!("carveout: {r}; listing read-only, not consumable");
        }
    }
    if no_session {
        eprintln!(
            "carveout: no active session; listing all {} row(s) across every session. Pass --all to ask for this explicitly.",
            rows.len()
        );
    } else if let Some(current) = scoped_to {
        // Always a banner, empty especially: "0 of 39" reads as scoping, while
        // silence reads as a clear ledger.
        eprintln!(
            "carveout: scoped to session {current}; showing {} of {total} row(s). Pass --all for every session.",
            rows.len()
        );
    }
    for r in &rows {
        if json_out {
            println!("{r}");
        } else {
            println!("{}", render_row(r));
        }
    }
    0
}

fn text_or<'a>(row: &'a Value, key: &str, default: &'a str) -> &'a str {
    row.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or(default)
}

/// A JSON null renders as the placeholder, never as the word "null". The node
/// shows because it is whose close this row blocks.
fn render_row(row: &Value) -> String {
    let description = text_or(row, "description", "");
    let first = py_splitlines(description).first().copied().unwrap_or("");
    let need = text_or(row, "need", "");
    let node = text_or(row, "node", "");
    let suffix = if need.is_empty() {
        String::new()
    } else {
        format!("  (need: {need})")
    };
    let owner = if node.is_empty() {
        String::new()
    } else {
        format!("  (node: {node})")
    };
    format!(
        "{} [{}] {first}{suffix}{owner}",
        text_or(row, "id", "?"),
        text_or(row, "kind", "?")
    )
}

// ---------------------------------------------------------------------------
// the transport the remaining Python readers ride
// ---------------------------------------------------------------------------

/// stdin `{root, kind?, session_ids?}`, stdout `{"carveouts": [...]}`. A ledger
/// that exists but cannot be read exits 1 with the reason on stderr.
fn internal_read() -> i32 {
    let mut raw = String::new();
    let _ = std::io::stdin().read_to_string(&mut raw);
    let payload: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    let root = payload
        .get("root")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .unwrap_or_else(ledger_root);
    let kind = payload.get("kind").and_then(Value::as_str);
    let sessions: Option<Vec<String>> = payload
        .get("session_ids")
        .and_then(Value::as_array)
        .map(|ids| ids.iter().map(|v| py_str(Some(v))).collect());
    match ledger::read_carveouts(&root, kind) {
        Ok(rows) => {
            let rows = scope_rows(rows, sessions.as_deref(), false);
            println!("{}", json!({ "carveouts": rows }));
            0
        }
        Err(e) => {
            eprintln!("carveout: failed to read carve-outs: {e}");
            1
        }
    }
}

/// stdout `{"session_id": ...}`: the session a durable ask gets bound to.
fn internal_session() -> i32 {
    println!(
        "{}",
        json!({ "session_id": resolve_session_id(&session_root()) })
    );
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_counts_chars_and_names_the_original_length() {
        let (text, truncated) = truncate_description(&"é".repeat(DESCRIPTION_CAP + 5));
        assert!(truncated);
        assert!(text.ends_with(" ... [truncated 5 of 8005 chars]"));
        assert_eq!(truncate_description("short"), ("short".to_string(), false));
    }

    #[test]
    fn an_ownerless_row_rides_only_the_default_scope() {
        let rows = vec![
            json!({"id": "a", "session_id": "s1"}),
            json!({"id": "b", "session_id": null}),
            json!({"id": "c", "session_id": "s2"}),
        ];
        let want = ["s1".to_string()];
        let ids = |rows: Vec<Value>| -> Vec<String> {
            rows.iter().map(|r| py_str(r.get("id"))).collect()
        };
        assert_eq!(ids(scope_rows(rows.clone(), Some(&want), false)), ["a"]);
        assert_eq!(ids(scope_rows(rows, Some(&want), true)), ["a", "b"]);
    }
}
