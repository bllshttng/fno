//! Native backlog projections: the kanban classifier every board shares,
//! the post-publish view pass (`render-views`: graph.md plus the configured
//! local board targets), and the read verbs `view`, `board`, `bases`,
//! `roadmap` and `album`.
//!
//! Every projection reads the authoritative rows from the store. The HTML
//! board itself is the front binary's `fno board-render` snapshot; this
//! module resolves the configured targets and hands them over.

use crate::backlog_ready::{
    dependents_fanout, epics_with_child_progress, get_str, in_progress_epic_ids, is_dict,
    live_epic_for, make_effective_priority, orphan_ids, parse_iso_str, priority_name,
    selection_sort_key, truthy, Term,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub(crate) const UNSCOPED: &str = "(unscoped)";

/// Canonical column order, left to right. graph.md and the roadmap fold
/// both read it, so the two boards can never disagree on the set.
const KANBAN_COLUMNS: [&str; 6] = ["In Progress", "Now", "Next", "Later", "Triage", "Done"];

/// Done shows only the most recent completions.
const DONE_CAP: usize = 10;

fn id_of(e: &Value) -> Option<&str> {
    e.get("id").and_then(Value::as_str)
}

/// Python `str()` of a scalar row value, for the human renders.
fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        other => other.to_string(),
    }
}

/// The node's swimlane: its project, or the unscoped lane.
pub(crate) fn project_key(e: &Value) -> String {
    match get_str(e, "project") {
        Some(p) if !p.trim().is_empty() => p.to_string(),
        _ => UNSCOPED.to_string(),
    }
}

/// The sole kanban column authority: roadmap, deferred and superseded rows
/// are off-board; done, in-progress, underway epics and queued rows route
/// by state; the rest ride their effective priority.
pub(crate) fn kanban_column(
    e: &Value,
    in_progress_epics: &BTreeSet<String>,
    effective: &BTreeMap<String, String>,
    by_id: &BTreeMap<String, Value>,
    child_progress: &BTreeSet<String>,
) -> Option<&'static str> {
    if get_str(e, "type") == Some("roadmap") {
        return None;
    }
    if truthy(e.get("completed_at")) {
        return Some("Done");
    }
    let status = get_str(e, "status");
    if status == Some("done") {
        return Some("Done");
    }
    if matches!(status, Some("deferred") | Some("superseded")) {
        return None;
    }
    if status == Some("in_progress") {
        return Some("In Progress");
    }
    if id_of(e).is_some_and(|id| in_progress_epics.contains(id)) {
        return Some("In Progress");
    }
    if truthy(e.get("queued_at")) {
        return Some("Triage");
    }
    // The effective-priority map only covers rows with an id; a row outside
    // it takes the same live-epic promotion directly.
    let priority = match id_of(e).and_then(|id| effective.get(id)) {
        Some(p) => p.clone(),
        None => match live_epic_for(e, by_id, child_progress) {
            Some(epic) => priority_name(&epic).min(priority_name(e)),
            None => priority_name(e),
        },
    };
    match priority.as_str() {
        "p0" | "p1" => Some("Now"),
        "p3" => Some("Later"),
        _ => Some("Next"),
    }
}

/// One board's whole-graph facts, computed once: the column router and the
/// swimlane selection order (project lane, then the selection key).
pub(crate) struct Board {
    by_id: BTreeMap<String, Value>,
    child_progress: BTreeSet<String>,
    /// Underway epics for the column: derived from the rows alone.
    underway: BTreeSet<String>,
    /// Underway epics for the order: a live claim counts as child progress.
    underway_claimed: BTreeSet<String>,
    effective: BTreeMap<String, String>,
    dependents: BTreeMap<String, i64>,
    orphans: BTreeSet<String>,
    now_ms: i64,
}

impl Board {
    pub(crate) fn new(entries: &[Value], claimed: &BTreeSet<String>) -> Board {
        let by_id: BTreeMap<String, Value> = entries
            .iter()
            .filter(|e| is_dict(e))
            .filter_map(|e| id_of(e).map(|id| (id.to_string(), e.clone())))
            .collect();
        let child_progress = epics_with_child_progress(&by_id);
        let underway = in_progress_epic_ids(entries, &by_id, &child_progress, &BTreeSet::new());
        let underway_claimed = in_progress_epic_ids(entries, &by_id, &child_progress, claimed);
        let effective = make_effective_priority(&by_id, &child_progress);
        Board {
            dependents: dependents_fanout(entries),
            orphans: orphan_ids(entries, &by_id),
            now_ms: chrono::Utc::now().timestamp_millis(),
            by_id,
            child_progress,
            underway,
            underway_claimed,
            effective,
        }
    }

    /// The read model's effective priorities. Demand reads these (the fold
    /// Python's `make_effective_priority` served); the column is never an
    /// input, so no read can reorder the board on its own.
    pub(crate) fn effective(&self) -> &BTreeMap<String, String> {
        &self.effective
    }

    pub(crate) fn column(&self, e: &Value) -> Option<&'static str> {
        kanban_column(
            e,
            &self.underway,
            &self.effective,
            &self.by_id,
            &self.child_progress,
        )
    }

    pub(crate) fn underway(&self) -> &BTreeSet<String> {
        &self.underway
    }

    pub(crate) fn is_orphan(&self, id: &str) -> bool {
        self.orphans.contains(id)
    }

    fn order_key(&self, e: &Value) -> (bool, String, Vec<Term>) {
        let lane = project_key(e);
        let key = selection_sort_key(
            e,
            &self.by_id,
            &self.child_progress,
            &self.dependents,
            &self.effective,
            &self.orphans,
            &self.underway_claimed,
            self.now_ms,
        );
        (lane == UNSCOPED, lane, key)
    }

    /// Stable sort into board order.
    pub(crate) fn sort(&self, rows: &mut Vec<&Value>) {
        rows.sort_by_cached_key(|e| self.order_key(e));
    }
}

// ---------------------------------------------------------------------------
// graph.md
// ---------------------------------------------------------------------------

fn one_line(v: Option<&Value>) -> String {
    v.and_then(Value::as_str)
        .unwrap_or("")
        .replace('\n', " ")
        .trim()
        .to_string()
}

fn kanban_card(e: &Value, by_id: &BTreeMap<String, Value>, board: &Board) -> String {
    let title = Some(one_line(e.get("title")))
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| "(untitled)".to_string());
    let eid = e.get("id").map(py_str).unwrap_or_else(|| "?".to_string());
    let priority = e
        .get("priority")
        .filter(|p| truthy(Some(p)))
        .map(py_str)
        .unwrap_or_else(|| "p2".to_string());
    let is_done = truthy(e.get("completed_at")) || get_str(e, "status") == Some("done");
    let marker = if is_done { "[x]" } else { "[ ]" };
    let mut header = format!(
        "- {marker} **{title}** `{eid}` · {} · {priority}",
        project_key(e)
    );
    if board.is_orphan(&eid) {
        header.push_str(" · [orphan]");
    }
    let mut body: Vec<String> = Vec::new();
    if let Some(plan) = e.get("plan_path").filter(|p| truthy(Some(p))) {
        body.push(format!("  {}", py_str(plan)));
    }
    if !is_done {
        let open_blockers: Vec<String> = e
            .get("blocked_by")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter_map(|bid| {
                let blocker = by_id.get(bid)?;
                if truthy(blocker.get("completed_at")) {
                    return None;
                }
                let title = get_str(blocker, "title")
                    .filter(|t| !t.is_empty())
                    .unwrap_or("?");
                Some(format!(
                    "{bid} ({})",
                    title.chars().take(40).collect::<String>()
                ))
            })
            .collect();
        if !open_blockers.is_empty() {
            body.push(format!("  blocked by: {}", open_blockers.join(", ")));
        }
    }
    if truthy(e.get("deferred_at")) && !is_done {
        let reason = get_str(e, "deferred_reason").unwrap_or("").trim();
        body.push(if reason.is_empty() {
            "  deferred".to_string()
        } else {
            format!("  deferred: {reason}")
        });
    }
    // A contained, not-done row sits in its priority column; without this
    // line the card reads as dispatchable work that actually ships inside
    // the owner's PR.
    if let Some(owner) = get_str(e, "contained_in").filter(|o| !o.is_empty()) {
        if !is_done {
            body.push(format!("  ships inside: {owner}"));
        }
    }
    if is_done {
        if let Some(url) = e.get("pr_url").filter(|u| truthy(Some(u))) {
            body.push(format!("  {}", py_str(url)));
        }
        for extra in e
            .get("additional_prs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|x| x.is_object())
        {
            let label = match extra.get("url").filter(|u| truthy(Some(u))) {
                Some(url) => py_str(url),
                None => match extra.get("number").filter(|n| !n.is_null()) {
                    Some(n) => format!("#{}", py_str(n)),
                    None => continue,
                },
            };
            let note = get_str(extra, "note").unwrap_or("").trim();
            body.push(if note.is_empty() {
                format!("  {label}")
            } else {
                format!("  {label} - {note}")
            });
        }
    }
    // A non-http(s) link renders as code so a markdown auto-linker cannot
    // turn a javascript: or data: URI into a clickable anchor.
    let artifact = get_str(e, "artifact_url").unwrap_or("").trim();
    if !artifact.is_empty() {
        body.push(
            if artifact.starts_with("https://") || artifact.starts_with("http://") {
                format!("  artifact: {artifact}")
            } else {
                format!("  artifact: `{artifact}`")
            },
        );
    }
    if body.is_empty() {
        header
    } else {
        format!("{header}\n{}", body.join("\n"))
    }
}

/// The branch-provenance section: every non-clean branch row the pr-watch
/// stranded leg cached under each node cwd and each configured workspace
/// project, so a repo whose nodes are all archived still reports.
fn provenance_lines(entries: &[Value]) -> Vec<String> {
    let mut roots: BTreeSet<String> = entries
        .iter()
        .filter_map(|e| get_str(e, "cwd"))
        .filter(|c| !c.is_empty())
        .map(str::to_string)
        .collect();
    if !cfg!(test) {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        roots.extend(crate::territory::workspace_paths(&cwd).into_values());
    }
    let mut lines = Vec::new();
    for root in &roots {
        if !Path::new(root).is_dir() {
            continue;
        }
        let path = Path::new(root).join(".fno").join("branch-provenance.json");
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let Ok(Value::Array(rows)) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        for row in rows.iter().filter(|r| r.is_object()) {
            lines.push(provenance_line(row));
        }
    }
    if lines.is_empty() {
        return lines;
    }
    let mut out = vec!["## Branch Provenance".to_string(), String::new()];
    out.extend(lines);
    out.push(String::new());
    out
}

fn provenance_line(row: &Value) -> String {
    let remote = match row.get("has_remote") {
        Some(Value::Bool(false)) => "no remote",
        Some(v) if truthy(Some(v)) => "has remote",
        _ => "remote unknown",
    };
    let unpushed = row
        .get("unpushed")
        .filter(|u| truthy(Some(u)))
        .map(py_str)
        .unwrap_or_else(|| "0".to_string());
    let mut parts = vec![remote.to_string(), format!("{unpushed} unpushed")];
    parts.push(match row.get("pr_number").filter(|p| truthy(Some(p))) {
        Some(pr) => format!("PR #{}", py_str(pr)),
        None => "no PR".to_string(),
    });
    if truthy(row.get("live")) {
        parts.push("LIVE".to_string());
    }
    let age = row
        .get("age")
        .filter(|a| truthy(Some(a)))
        .map(py_str)
        .unwrap_or_else(|| "unknown".to_string());
    parts.push(format!("newest commit {age}"));
    let label = match row.get("node").filter(|n| truthy(Some(n))) {
        Some(node) => format!("**{}**", py_str(node)),
        None => "*(unmapped)*".to_string(),
    };
    let branch = row
        .get("branch")
        .filter(|b| truthy(Some(b)))
        .map(py_str)
        .unwrap_or_else(|| "no branch".to_string());
    format!("- {label} ({branch}): {}", parts.join(", "))
}

/// The graph.md board: In Progress, Now, Next, Later, Triage, then the ten
/// newest Done. `obsidian` adds the Kanban plugin's frontmatter and settings
/// block, which are inert noise outside a vault.
pub(crate) fn render_graph_md(entries: &[Value], obsidian: bool) -> String {
    let rows: Vec<&Value> = entries.iter().filter(|e| is_dict(e)).collect();
    let by_id: BTreeMap<String, Value> = rows
        .iter()
        .filter_map(|e| id_of(e).map(|id| (id.to_string(), (*e).clone())))
        .collect();
    let board = Board::new(entries, &BTreeSet::new());
    let mut columns: BTreeMap<&str, Vec<&Value>> = BTreeMap::new();
    for e in &rows {
        if let Some(col) = board.column(e) {
            columns.entry(col).or_default().push(*e);
        }
    }
    for (col, items) in columns.iter_mut() {
        if *col == "Done" {
            items.sort_by(|a, b| {
                get_str(b, "completed_at")
                    .unwrap_or("")
                    .cmp(get_str(a, "completed_at").unwrap_or(""))
            });
            items.truncate(DONE_CAP);
        } else {
            board.sort(items);
        }
    }
    let mut lines: Vec<String> = Vec::new();
    if obsidian {
        lines.extend(["---", "kanban-plugin: board", "---", ""].map(String::from));
    }
    for col in KANBAN_COLUMNS {
        lines.push(format!("## {col}"));
        lines.push(String::new());
        match columns.get(col) {
            Some(items) if !items.is_empty() => {
                for e in items {
                    lines.push(kanban_card(e, &by_id, &board));
                    lines.push(String::new());
                }
            }
            _ => lines.push(String::new()),
        }
    }
    // Before the Obsidian footer: the kanban:settings block must stay last.
    lines.extend(provenance_lines(entries));
    if obsidian {
        lines.extend(
            [
                "***",
                "",
                "%% kanban:settings",
                "```",
                r#"{"kanban-plugin":"board"}"#,
                "```",
                "%%",
            ]
            .map(String::from),
        );
    }
    format!("{}\n", lines.join("\n"))
}

// ---------------------------------------------------------------------------
// Paths, config, and the front binary handoff
// ---------------------------------------------------------------------------

fn state_dir() -> PathBuf {
    super::settings::state_dir().unwrap_or_else(|| PathBuf::from(".fno"))
}

fn graph_md_path() -> PathBuf {
    state_dir().join("pages").join("graph.md")
}

fn graph_html_path() -> PathBuf {
    state_dir().join("pages").join("graph.html")
}

fn expand(path: &str) -> PathBuf {
    super::settings::expand_home(path)
}

/// `Path.resolve()` for a path that may not exist yet: the longest existing
/// ancestor is canonicalized and the rest appended.
fn resolve(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    let mut tail = Vec::new();
    let mut probe = absolute.as_path();
    loop {
        if let Ok(real) = probe.canonicalize() {
            return tail.iter().rev().fold(real, |acc, part| acc.join(part));
        }
        match (probe.parent(), probe.file_name()) {
            (Some(parent), Some(name)) => {
                tail.push(name.to_os_string());
                probe = parent;
            }
            _ => return absolute,
        }
    }
}

fn write_file(path: &Path, content: &str) -> Result<(), String> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    crate::graph_store::write_atomic(path, content).map_err(|e| e.to_string())
}

/// The Obsidian vault name from the GLOBAL config, when the vault is on.
fn global_obsidian_vault(global: &Value) -> Option<String> {
    let obs = global.get("obsidian")?;
    if !truthy(obs.get("enabled")) {
        return None;
    }
    obs.get("vault").filter(|v| truthy(Some(v))).map(py_str)
}

/// Whether the merged settings name an enabled vault (`paths.vault_root`).
fn obsidian_enabled() -> bool {
    let merged = super::advance_settings::load_merged(None);
    merged
        .get("obsidian")
        .is_some_and(|o| truthy(o.get("enabled")) && truthy(o.get("vault")))
}

#[derive(Debug, Clone, PartialEq)]
struct RenderTarget {
    path: String,
    scope: String,
    projection: String,
}

const TARGET_KEYS: [&str; 4] = ["path", "project", "scope", "projection"];

/// State files a render target must never overwrite.
fn state_file_collisions(path: &Path) -> Vec<PathBuf> {
    let graph = super::settings::graph_path();
    let dir = graph.parent().map(Path::to_path_buf).unwrap_or_default();
    let name = graph
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let resolved = resolve(path);
    [
        graph.clone(),
        graph_md_path(),
        dir.join("graph-archive.json"),
        state_dir().join("ledger.json"),
        dir.join(format!("{name}.bak")),
        dir.join(format!("{name}.lock")),
        dir.join("backups").join(format!("{name}.bak")),
    ]
    .into_iter()
    .filter(|state| resolve(state) == resolved)
    .collect()
}

fn default_target() -> RenderTarget {
    RenderTarget {
        path: graph_html_path().to_string_lossy().into_owned(),
        scope: "all".to_string(),
        projection: "local".to_string(),
    }
}

/// One `backlog.render_targets` row, validated: unknown keys refuse the
/// row (a misspelled scope would otherwise publish every project), the path
/// must be absolute or `~/`-rooted.
fn parse_target(row: &Value) -> Result<RenderTarget, String> {
    let Some(map) = row.as_object() else {
        return Err(format!(
            "skipping malformed backlog.render_targets row: {row}"
        ));
    };
    let path_label = map.get("path").map(py_str).unwrap_or_else(|| "None".into());
    let unknown: Vec<String> = map
        .keys()
        .filter(|k| !TARGET_KEYS.contains(&k.as_str()))
        .map(|k| format!("'{k}'"))
        .collect();
    if !unknown.is_empty() {
        return Err(format!(
            "skipping backlog.render_targets row '{path_label}': unknown key(s) {}; a \
             misspelled scope would otherwise publish every project",
            unknown.join(", ")
        ));
    }
    let path = map
        .get("path")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    if path.is_empty() {
        return Err(
            "skipping malformed backlog.render_targets row: path must not be empty".to_string(),
        );
    }
    if !expand(path).is_absolute() {
        return Err(format!(
            "skipping malformed backlog.render_targets row: path must be absolute or \
             ~/-rooted (it is written from arbitrary cwds), got: '{path}'"
        ));
    }
    let text = |key: &str| map.get(key).and_then(Value::as_str).map(str::to_string);
    // An empty scope would read as no scope and publish every project.
    let named = |key: &str| -> Result<Option<String>, String> {
        match map.get(key).and_then(Value::as_str).map(str::trim) {
            None => Ok(None),
            Some("") => Err(format!(
                "skipping malformed backlog.render_targets row '{path_label}': {key} must \
                 not be empty"
            )),
            Some("*") if key == "scope" => Ok(Some("all".to_string())),
            Some(v) => Ok(Some(v.to_string())),
        }
    };
    let scope = match (named("scope")?, named("project")?) {
        (Some(s), Some(p)) if s != p => {
            return Err(format!(
                "skipping malformed backlog.render_targets row '{path_label}': scope and \
                 project must name the same value"
            ))
        }
        (Some(s), _) => s,
        (None, Some(p)) => p,
        (None, None) => "all".to_string(),
    };
    Ok(RenderTarget {
        path: path.to_string(),
        scope,
        projection: text("projection").unwrap_or_else(|| "local".to_string()),
    })
}

/// `config.backlog.render_targets` from the GLOBAL config only: a render
/// fired from any cwd must not depend on a project-local list. Every bad
/// row warns and costs that one board; the canonical board row is always
/// present.
fn configured_targets() -> Vec<RenderTarget> {
    let (global, unreadable) = super::advance_settings::load_global_merged();
    let block = global.get("backlog");
    let rows = match block.and_then(|b| b.get("render_targets")) {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(rows)) => rows.clone(),
        Some(other) => {
            let kind = match other {
                Value::Object(_) => "dict",
                Value::String(_) => "str",
                Value::Bool(_) => "bool",
                _ => "int",
            };
            eprintln!(
                "Warning: config.backlog.render_targets is {kind}, not an array of tables - \
                 ignoring it (use [[backlog.render_targets]], not \
                 [backlog.render_targets.<name>])"
            );
            Vec::new()
        }
    };
    if !unreadable.is_empty() && rows.is_empty() && block.is_some() {
        let names: Vec<String> = unreadable.iter().map(|p| p.display().to_string()).collect();
        eprintln!(
            "Warning: backlog.render_targets may be disabled: global config unreadable: {}",
            names.join(", ")
        );
    }
    let mut out: Vec<RenderTarget> = Vec::new();
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    for row in &rows {
        let target = match parse_target(row) {
            Ok(t) => t,
            Err(message) => {
                eprintln!("Warning: {message}");
                continue;
            }
        };
        let expanded = expand(&target.path);
        let clashes = state_file_collisions(&expanded);
        if !clashes.is_empty() {
            let names: Vec<String> = clashes.iter().map(|p| p.display().to_string()).collect();
            eprintln!(
                "Warning: skipping render target {}: collides with graph state file {}; \
                 refusing to overwrite it",
                target.path,
                names.join(", ")
            );
            continue;
        }
        if !seen.insert(resolve(&expanded)) {
            eprintln!(
                "Warning: skipping duplicate render target path {}: duplicate render target \
                 path; first target kept",
                target.path
            );
            continue;
        }
        out.push(target);
    }
    warn_shadowed_local_rows(&out);
    let canonical = resolve(&graph_html_path());
    if !out.iter().any(|t| resolve(&expand(&t.path)) == canonical) {
        out.insert(0, default_target());
    }
    out
}

/// A project-local `render_targets` list validates in the settings loader but
/// never renders; say so instead of failing silently.
fn warn_shadowed_local_rows(honored: &[RenderTarget]) {
    let merged = super::advance_settings::load_merged(None);
    let Some(Value::Array(rows)) = merged.get("backlog").and_then(|b| b.get("render_targets"))
    else {
        return;
    };
    let mut local: Vec<RenderTarget> = rows.iter().filter_map(|r| parse_target(r).ok()).collect();
    let mut honored = honored.to_vec();
    let key = |t: &RenderTarget| (t.path.clone(), t.scope.clone(), t.projection.clone());
    local.sort_by_key(key);
    honored.sort_by_key(key);
    if local.is_empty() || local == honored {
        return;
    }
    let detail = if honored.is_empty() {
        format!("{} project-local row(s) ignored", local.len())
    } else {
        "a project-local list shadows the global one and is ignored".to_string()
    };
    eprintln!(
        "Warning: backlog.render_targets is honored from the GLOBAL config file only; {detail}"
    );
}

const BOARD_RENDER_TIMEOUT_SECS: u64 = 600;

fn board_render(request: &Value) -> Result<std::process::Output, String> {
    // The front binary: the `FNO_BIN` pin, else the sibling or installed `fno`.
    let mut cmd = std::process::Command::new(crate::scrape::fno_bin());
    cmd.arg("board-render");
    crate::bounded_cmd::output_with_timeout_stdin(
        cmd,
        BOARD_RENDER_TIMEOUT_SECS,
        &request.to_string(),
    )
    .map_err(|e| e.to_string())
}

/// Write every configured `local` board target through `fno board-render`
/// and return the count of failures. Retired projections warn and skip.
pub(crate) fn render_local_targets() -> usize {
    let (global, _) = super::advance_settings::load_global_merged();
    let mut local = Vec::new();
    for target in configured_targets() {
        if target.projection == "local" {
            local.push(json!({
                "path": expand(&target.path).to_string_lossy(),
                "scope": target.scope,
            }));
        } else {
            eprintln!(
                "Warning: render target {}: projection '{}' is retired; the web backlog \
                 page replaces the rendered boards (fno mux serve --web / fno backlog view)",
                target.path, target.projection
            );
        }
    }
    if local.is_empty() {
        return 0;
    }
    let count = local.len();
    let request = json!({"targets": local, "vault": global_obsidian_vault(&global)});
    let done = match board_render(&request) {
        Ok(done) => done,
        Err(e) => {
            eprintln!("Warning: local board render failed: {e}");
            return count;
        }
    };
    let stderr = String::from_utf8_lossy(&done.stderr).trim().to_string();
    if !done.status.success() {
        if stderr.contains("No such command 'board-render'")
            || stderr.contains("No such command \"board-render\"")
        {
            // Install lag, not a failed render: the keeper's next settled
            // tick retries after the update.
            eprintln!(
                "Warning: local board render skipped: the installed fno predates \
                 board-render; run `fno doctor update --rust`"
            );
            return 0;
        }
        eprintln!(
            "Warning: board render exited {}: {}",
            done.status.code().unwrap_or(-1),
            stderr.chars().take(300).collect::<String>()
        );
    }
    let stdout = String::from_utf8_lossy(&done.stdout);
    let receipt: Value = match serde_json::from_str(if stdout.trim().is_empty() {
        "{}"
    } else {
        &stdout
    }) {
        Ok(r) => r,
        Err(_) => {
            eprintln!(
                "Warning: board render receipt was unreadable: {}",
                stdout.chars().take(200).collect::<String>()
            );
            return count;
        }
    };
    let failed = receipt
        .get("failed")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for row in &failed {
        eprintln!(
            "Warning: render target {} failed: {}",
            row.get("path").map(py_str).unwrap_or_default(),
            row.get("error").map(py_str).unwrap_or_default()
        );
    }
    failed.len()
}

/// Live (unarchived) rows, strict: an unreadable store is an error, never an
/// empty board.
fn live_rows(graph: &Path) -> Result<Vec<Value>, String> {
    let query = crate::backlog::RowQuery {
        include_archived: false,
        ..Default::default()
    };
    crate::graph_store::read_rows_where_strict(graph, &query).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// render-views
// ---------------------------------------------------------------------------

/// The canonical post-publish view pass: graph.md, then (for the configured
/// store) every local board target. Returns one message per failed view.
pub(crate) fn render_views(graph: &Path, canonical: bool) -> Vec<String> {
    let mut failures = Vec::new();
    let mut entries = match live_rows(graph) {
        Ok(rows) => rows,
        Err(e) => return vec![format!("graph read failed: {e}")],
    };
    // Blocked is a read-time overlay recompute never stores; without it a
    // newly blocked sibling renders stale.
    crate::graph_store::apply_readiness_overlay(&mut entries);
    let md_target = if canonical {
        graph_md_path()
    } else {
        graph.with_file_name("graph.md")
    };
    if let Err(e) = write_file(&md_target, &render_graph_md(&entries, obsidian_enabled())) {
        failures.push(format!("graph.md render failed: {e}"));
    }
    if canonical {
        let failed = render_local_targets();
        if failed > 0 {
            failures.push(format!("local board render: {failed} target(s) failed"));
        }
    }
    failures
}

fn wants_help(tail: &[String]) -> bool {
    tail.iter().any(|a| a == "--help" || a == "-h")
}

fn usage_error(verb: &str, arg: &str) -> i32 {
    eprintln!("fno backlog {verb}: unexpected argument '{arg}' (see --help)");
    2
}

/// `fno backlog render-views [--graph PATH]`: replay the post-publish views.
/// A failed view exits 1 so the keeper withholds its rendered stamp and its
/// backoff retries the pass.
pub fn run_render_views(tail: &[String]) -> i32 {
    if wants_help(tail) {
        println!(
            "Usage: fno backlog render-views [--graph PATH]\n\n  Replay the canonical \
             post-publish views: graph.md and the configured board targets."
        );
        return 0;
    }
    let configured = super::settings::graph_path();
    let graph = match tail {
        [] => configured.clone(),
        [flag, path] if flag == "--graph" => PathBuf::from(path),
        [other, ..] => return usage_error("render-views", other),
    };
    let failures = render_views(&graph, resolve(&graph) == resolve(&configured));
    for failure in &failures {
        eprintln!("Warning: {failure}");
    }
    if failures.is_empty() {
        0
    } else {
        eprintln!("render-views: {} view(s) failed", failures.len());
        1
    }
}

// ---------------------------------------------------------------------------
// view
// ---------------------------------------------------------------------------

fn open_with_system(path: &Path) {
    let launched = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
            .arg(path)
            .status()
            .map(|_| ())
    } else if cfg!(target_os = "windows") {
        std::process::Command::new("cmd")
            .args(["/C", "start", ""])
            .arg(path)
            .status()
            .map(|_| ())
    } else {
        let opener = ["xdg-open", "wslview"].into_iter().find_map(|name| {
            std::env::var_os("PATH").and_then(|paths| {
                std::env::split_paths(&paths)
                    .map(|dir| dir.join(name))
                    .find(|p| p.is_file())
            })
        });
        match opener {
            Some(bin) => std::process::Command::new(bin)
                .arg(path)
                .status()
                .map(|_| ()),
            None => {
                eprintln!("No xdg-open / wslview found; file rendered but not opened.");
                return;
            }
        }
    };
    if let Err(e) = launched {
        eprintln!("Could not launch opener: {e}");
    }
}

/// `fno backlog view`: render the board page and open it with the system's
/// `.html` handler. `FNO_NO_OPEN=1` renders and prints the path only.
pub fn run_view(tail: &[String]) -> i32 {
    if wants_help(tail) {
        println!(
            "Usage: fno backlog view\n\n  Render the backlog as HTML and open it with the \
             system's default handler.\n  Set FNO_NO_OPEN=1 to skip the launch step and \
             just print the path."
        );
        return 0;
    }
    if let Some(arg) = tail.first() {
        return usage_error("view", arg);
    }
    let failures = render_local_targets();
    if failures > 0 {
        eprintln!(
            "Error: local board render failed ({failures} target(s)); see the warnings above"
        );
        return 1;
    }
    let page = graph_html_path();
    println!("{}", page.display());
    if std::env::var("FNO_NO_OPEN").as_deref() == Ok("1") {
        return 0;
    }
    open_with_system(&page);
    0
}

// ---------------------------------------------------------------------------
// board
// ---------------------------------------------------------------------------

const MAX_ROWS_PER_SECTION: usize = 5;
const MAX_TITLE_WORDS: usize = 12;
const RECENT_WINDOW_MS: i64 = 24 * 3_600_000;
const FALLBACK_RECENT_COUNT: usize = 5;
/// A stamp from the future is not an age. A row written while the board
/// renders is legitimately a second or two ahead of `now`; a garbage stamp
/// misses by decades, so a minute of headroom separates the two.
const FUTURE_STAMP_TOLERANCE_S: f64 = -60.0;

const BOARD_SECTIONS: [(&str, &str); 3] = [
    ("just_finished", "Just finished"),
    ("in_progress", "In progress"),
    ("on_deck", "On deck"),
];

fn cap_words(text: &str) -> String {
    let title = text.replace('\n', " ");
    let title = title.trim();
    let title = if title.is_empty() {
        "(untitled)"
    } else {
        title
    };
    let words: Vec<&str> = title.split_whitespace().collect();
    if words.len() <= MAX_TITLE_WORDS {
        return title.to_string();
    }
    format!("{}…", words[..MAX_TITLE_WORDS].join(" "))
}

fn format_age(seconds: f64) -> String {
    if seconds < FUTURE_STAMP_TOLERANCE_S {
        return "unknown age".to_string();
    }
    let s = (seconds.trunc() as i64).max(0);
    if s < 3600 {
        format!("{}m ago", (s / 60).max(1))
    } else if s < 86_400 {
        format!("{}h ago", s / 3600)
    } else {
        format!("{}d ago", s / 86_400)
    }
}

fn iso_ms(v: Option<&Value>) -> Option<i64> {
    v.and_then(Value::as_str).and_then(parse_iso_str)
}

fn age_from_iso(v: Option<&Value>, now_ms: i64) -> String {
    match iso_ms(v) {
        Some(ts) => format_age((now_ms - ts) as f64 / 1000.0),
        None => "unknown age".to_string(),
    }
}

/// A cache row's epoch stamp as a finite float, 0 when absent or garbage.
fn finite_or_zero(v: &Value) -> f64 {
    let parsed = match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        Value::Bool(b) => Some(f64::from(u8::from(*b))),
        _ => None,
    };
    parsed.filter(|f| f.is_finite()).unwrap_or(0.0)
}

fn age_from_epoch(ts: Option<&Value>, now_ms: i64) -> String {
    let ts = ts.map(finite_or_zero).unwrap_or(0.0);
    if ts == 0.0 {
        return "unknown age".to_string();
    }
    format_age(now_ms as f64 / 1000.0 - ts)
}

/// `owner--repo` from a PR URL; never touches git or the network.
fn slug_key_from_pr_url(url: &str) -> Option<String> {
    static PR_URL: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"/([^/]+)/([^/]+)/pull/\d+").expect("static regex")
    });
    let caps = PR_URL.captures(url)?;
    Some(format!("{}--{}", &caps[1], &caps[2]))
}

fn in_progress_fact(e: &Value, now_ms: i64, cache: Option<&Path>) -> String {
    let Some(pr) = e.get("pr_number").filter(|p| truthy(Some(p))).map(py_str) else {
        return "no PR yet".to_string();
    };
    let row = slug_key_from_pr_url(get_str(e, "pr_url").unwrap_or(""))
        .zip(cache)
        .and_then(|(slug, dir)| crate::pr_status::cache::newest_row(dir, &slug, &pr));
    let Some(row) = row else {
        return format!("PR {pr} unknown (no cached row)");
    };
    let verdict = row
        .get("output")
        .and_then(|o| o.get("verdict"))
        .filter(|v| truthy(Some(v)))
        .map(py_str)
        .unwrap_or_else(|| "unknown".to_string());
    format!(
        "PR {pr} checks {verdict} (as of {})",
        age_from_epoch(row.get("ts"), now_ms)
    )
}

fn board_row(e: &Value, fact: String) -> Value {
    json!({
        "id": e.get("id").cloned().unwrap_or_else(|| json!("?")),
        "title": cap_words(get_str(e, "title").unwrap_or("")),
        "fact": fact,
    })
}

fn section(rows: Vec<Value>, more: usize, note: Option<&str>, unknown: Option<String>) -> Value {
    json!({"rows": rows, "more": more, "note": note, "unknown": unknown})
}

fn unreadable_board(reason: &str) -> Value {
    let mut out = serde_json::Map::new();
    for (key, _) in BOARD_SECTIONS {
        out.insert(
            key.to_string(),
            section(Vec::new(), 0, None, Some(reason.to_string())),
        );
    }
    Value::Object(out)
}

/// Just finished / In progress / On deck, off the graph and the pr-status
/// cache only: showing the board never spends a GitHub read.
fn compute_board(
    entries: &[Value],
    project: Option<&str>,
    claims: Result<BTreeSet<String>, String>,
    now_ms: i64,
    cache: Option<&Path>,
) -> Value {
    let all: Vec<&Value> = entries.iter().filter(|e| is_dict(e)).collect();
    let by_id: BTreeMap<&str, &Value> = all
        .iter()
        .filter_map(|e| id_of(e).map(|i| (i, *e)))
        .collect();
    let scoped: Vec<Value> = all
        .iter()
        .filter(|e| project.is_none_or(|p| project_key(e) == p))
        .map(|e| (*e).clone())
        .collect();
    // A claims fault renders the In progress section unknown, never an empty
    // "nobody is working on anything".
    let (claimed, claims_reason) = match claims {
        Ok(set) => (set, None),
        Err(e) => (
            BTreeSet::new(),
            Some(format!("live claim state unavailable ({e})")),
        ),
    };
    let board = Board::new(&scoped, &claimed);
    let underway = board.underway();

    let completed_ms = |e: &&Value| iso_ms(e.get("completed_at")).unwrap_or(i64::MIN);
    let mut completed: Vec<&Value> = scoped
        .iter()
        .filter(|e| truthy(e.get("completed_at")))
        .collect();
    completed.sort_by_key(|e| std::cmp::Reverse(completed_ms(e)));
    let recent: Vec<&Value> = completed
        .iter()
        .copied()
        .filter(|e| now_ms.saturating_sub(completed_ms(e)) <= RECENT_WINDOW_MS)
        .collect();
    let used_fallback = recent.is_empty() && !completed.is_empty();
    let finished: Vec<&Value> = if recent.is_empty() {
        completed
            .iter()
            .copied()
            .take(FALLBACK_RECENT_COUNT)
            .collect()
    } else {
        recent
    };
    let just_finished = section(
        finished
            .iter()
            .take(MAX_ROWS_PER_SECTION)
            .map(|e| {
                let age = age_from_iso(e.get("completed_at"), now_ms);
                let fact = match e.get("pr_number").filter(|p| truthy(Some(p))) {
                    Some(pr) => format!("PR {} merged {age}", py_str(pr)),
                    None => format!("merged {age}"),
                };
                board_row(e, fact)
            })
            .collect(),
        finished.len().saturating_sub(MAX_ROWS_PER_SECTION),
        used_fallback.then_some("no completions in the last 24h - showing the most recent"),
        None,
    );

    let mut in_progress_ids: BTreeSet<&str> = BTreeSet::new();
    let in_progress = match claims_reason {
        Some(reason) => section(Vec::new(), 0, None, Some(reason)),
        None => {
            let mut rows: Vec<&Value> = scoped
                .iter()
                .filter(|e| {
                    id_of(e).is_some_and(|id| {
                        !truthy(e.get("completed_at"))
                            && (claimed.contains(id)
                                || get_str(e, "status") == Some("in_progress")
                                || underway.contains(id))
                    })
                })
                .collect();
            board.sort(&mut rows);
            in_progress_ids = rows.iter().filter_map(|e| id_of(e)).collect();
            section(
                rows.iter()
                    .take(MAX_ROWS_PER_SECTION)
                    .map(|e| board_row(e, in_progress_fact(e, now_ms, cache)))
                    .collect(),
                rows.len().saturating_sub(MAX_ROWS_PER_SECTION),
                None,
                None,
            )
        }
    };

    // A node the graph itself marks in_progress, or an underway epic, never
    // falls to On deck because the live-claims read failed.
    let mut deck: Vec<&Value> = scoped.iter().collect();
    board.sort(&mut deck);
    deck.retain(|e| {
        !matches!(board.column(e), None | Some("Done"))
            && id_of(e).is_some_and(|id| {
                !in_progress_ids.contains(id)
                    && get_str(e, "status") != Some("in_progress")
                    && !underway.contains(id)
            })
    });
    let on_deck_fact = |e: &Value| -> String {
        for bid in e
            .get("blocked_by")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if by_id
                .get(bid)
                .is_some_and(|b| !truthy(b.get("completed_at")))
            {
                return format!("blocked by {bid}");
            }
        }
        "ready".to_string()
    };
    let on_deck = section(
        deck.iter()
            .take(MAX_ROWS_PER_SECTION)
            .map(|e| board_row(e, on_deck_fact(e)))
            .collect(),
        deck.len().saturating_sub(MAX_ROWS_PER_SECTION),
        None,
        None,
    );
    json!({"just_finished": just_finished, "in_progress": in_progress, "on_deck": on_deck})
}

fn print_board(board: &Value) {
    for (key, title) in BOARD_SECTIONS {
        let sec = board.get(key).cloned().unwrap_or(Value::Null);
        println!("{title}");
        if let Some(unknown) = sec.get("unknown").filter(|u| truthy(Some(u))) {
            println!("  (unknown: {})", py_str(unknown));
            println!();
            continue;
        }
        if let Some(note) = sec.get("note").filter(|n| truthy(Some(n))) {
            println!("  ({})", py_str(note));
        }
        let rows = sec
            .get("rows")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if rows.is_empty() {
            println!("  (none)");
        }
        for row in rows {
            println!(
                "  {}  {}   {}",
                row.get("id").map(py_str).unwrap_or_default(),
                row.get("title").map(py_str).unwrap_or_default(),
                row.get("fact").map(py_str).unwrap_or_default()
            );
        }
        if let Some(more) = sec.get("more").and_then(Value::as_u64).filter(|m| *m > 0) {
            println!("  ... and {more} more");
        }
        println!();
    }
}

fn emit_board(board: &Value, json_out: bool) {
    if json_out {
        println!("{}", super::render::py_json_compact(board));
    } else {
        print_board(board);
    }
}

fn node_claims() -> Result<BTreeSet<String>, String> {
    crate::claims::list(Some("node:"), None, false).map(|records| {
        records
            .iter()
            .filter_map(|rec| rec.key.strip_prefix("node:").map(str::to_string))
            .collect()
    })
}

/// `fno backlog board [--project P] [--json]`.
pub fn run_board(tail: &[String]) -> i32 {
    if wants_help(tail) {
        println!(
            "Usage: fno backlog board [--project P] [--json]\n\n  Just finished / In progress \
             / On deck - the board in one glance.\n  Reads only the graph and the on-disk \
             pr-status cache, never GitHub."
        );
        return 0;
    }
    let mut project: Option<String> = None;
    let mut json_out = false;
    let mut args = tail.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--json" | "-J" => json_out = true,
            "--project" => match args.next() {
                Some(p) => project = Some(p.clone()),
                None => return usage_error("board", arg),
            },
            other => match other.strip_prefix("--project=") {
                Some(p) => project = Some(p.to_string()),
                None => return usage_error("board", other),
            },
        }
    }
    let backend = super::workflows::active_backend_name();
    if backend != "graph" {
        emit_board(
            &unreadable_board(&format!(
                "board is unavailable under the {backend} tracker backend"
            )),
            json_out,
        );
        return 1;
    }
    let entries = match live_rows(&super::settings::graph_path()) {
        Ok(rows) => rows,
        Err(e) => {
            emit_board(
                &unreadable_board(&format!("graph unreadable ({e})")),
                json_out,
            );
            return 1;
        }
    };
    let project = project.or_else(|| super::triage::detect_project(&entries));
    let cwd = std::env::current_dir().unwrap_or_default();
    let cache = crate::agents_config::pr_status_cache_dir(&cwd);
    let board = compute_board(
        &entries,
        project.as_deref(),
        node_claims(),
        chrono::Utc::now().timestamp_millis(),
        cache.as_deref(),
    );
    emit_board(&board, json_out);
    0
}

// ---------------------------------------------------------------------------
// bases
// ---------------------------------------------------------------------------

const GENERATED_MARKER: &str = "# GENERATED by `fno backlog bases`";

/// The epic and mission progress Bases, epics first for a stable receipt.
fn bases() -> [(&'static str, String); 2] {
    let header =
        format!("{GENERATED_MARKER} - edits are overwritten. Regenerate: fno backlog bases");
    let prio = r#"if(priority == "p1", "🔴 p1", if(priority == "p2", "🟡 p2", if(priority == "p0", "🚨 p0", "⚪ p3")))"#;
    let folder = "internal/fno/plans";
    let epics = format!(
        "{header}\nfilters:\n  and:\n    - file.inFolder(\"{folder}\")\n    - type == \"epic\"\n\
formulas:\n  prio: {prio}\nproperties:\n  file.name:\n    displayName: Epic\n  progress:\n    \
displayName: Progress\n  children_done:\n    displayName: Done\n  children_total:\n    \
displayName: Total\n  children_in_flight:\n    displayName: In flight\n  children_blocked:\n    \
displayName: Blocked\n  waves_total:\n    displayName: Waves\n  formula.prio:\n    displayName: \
Priority\n  parent_slug:\n    displayName: Mission\nviews:\n  - type: table\n    name: Epics\n    \
order:\n      - file.name\n      - progress\n      - children_done\n      - children_total\n      \
- children_in_flight\n      - children_blocked\n      - waves_total\n      - formula.prio\n      \
- status\n    sort:\n      - property: children_blocked\n        direction: DESC\n      - \
property: children_total\n        direction: DESC\n    columnSize:\n      file.name: 380\n  - \
type: table\n    name: By mission\n    groupBy:\n      property: parent_slug\n      direction: \
ASC\n    order:\n      - file.name\n      - progress\n      - children_done\n      - \
children_total\n      - waves_total\n      - formula.prio\n"
    );
    let missions = format!(
        "{header}\nfilters:\n  and:\n    - file.inFolder(\"{folder}\")\n    - type == \"epic\"\n    \
- parent == null\nformulas:\n  prio: {prio}\nproperties:\n  file.name:\n    displayName: \
Mission\n  progress:\n    displayName: Progress\n  children_done:\n    displayName: Done\n  \
children_total:\n    displayName: Total\n  children_in_flight:\n    displayName: In flight\n  \
children_blocked:\n    displayName: Blocked\n  formula.prio:\n    displayName: Priority\n\
views:\n  - type: table\n    name: Missions\n    order:\n      - file.name\n      - progress\n      \
- children_done\n      - children_total\n      - children_in_flight\n      - children_blocked\n      \
- formula.prio\n      - status\n    sort:\n      - property: children_total\n        direction: \
DESC\n    columnSize:\n      file.name: 380\n"
    );
    [("epics.base", epics), ("missions.base", missions)]
}

/// Write one generated base, guarding a hand-authored file: `written`,
/// `unchanged`, or `refused` when the target's first line lacks the marker.
fn write_base(path: &Path, content: &str) -> &'static str {
    if path.exists() {
        let existing = std::fs::read_to_string(path).unwrap_or_default();
        if !existing
            .lines()
            .next()
            .is_some_and(|first| first.starts_with(GENERATED_MARKER))
        {
            return "refused";
        }
        if existing == content {
            return "unchanged";
        }
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::write(path, content) {
        Ok(()) => "written",
        Err(_) => "refused",
    }
}

/// `fno backlog bases [--out DIR]`: emit the canonical epic/mission Bases.
pub fn run_bases(tail: &[String]) -> i32 {
    if wants_help(tail) {
        println!(
            "Usage: fno backlog bases [--out DIR]\n\n  Emit the canonical epic/mission progress \
             Base files.\n  Prints one line per file: written | unchanged | refused."
        );
        return 0;
    }
    let out_dir = match tail {
        [] => match super::triage::inbox_markdown_path()
            .and_then(|p| p.parent().map(Path::to_path_buf))
        {
            Some(dir) => dir,
            None => {
                eprintln!(
                    "fno backlog bases: no capture-inbox directory resolves here; pass --out"
                );
                return 1;
            }
        },
        [flag, dir] if flag == "--out" => PathBuf::from(dir),
        [other, ..] => return usage_error("bases", other),
    };
    for (name, content) in bases() {
        let target = out_dir.join(name);
        let action = write_base(&target, &content);
        println!("{action}: {}", target.display());
    }
    0
}

// ---------------------------------------------------------------------------
// roadmap
// ---------------------------------------------------------------------------

/// The statuses a PUBLIC backlog page shows.
const PUBLIC_BACKLOG_STATUSES: [&str; 4] = ["in_progress", "ready", "blocked", "idea"];
/// Public columns and labels: active work folds into Now, Triage into Later.
const PUBLIC_COLUMNS: [(&str, &str); 4] = [
    ("Now", "Now"),
    ("Next", "Next"),
    ("Later", "Later"),
    ("Done", "Shipped"),
];

fn is_public_in(e: &Value, project: &str) -> bool {
    is_dict(e)
        && e.get("public") != Some(&Value::Bool(false))
        && project_key(e) == project
        // The quarantine default-deny: an imported request row publishes
        // only when the owner flipped it public. backlog_snapshot.rs
        // select_public_rows holds the fno-side twin of this rule.
        && (e.get("source_kind").and_then(Value::as_str) != Some("external_request")
            || e.get("public") == Some(&Value::Bool(true)))
}

fn public_columns<'a>(
    entries: &'a [Value],
    project: &str,
) -> BTreeMap<&'static str, Vec<&'a Value>> {
    let board = Board::new(entries, &BTreeSet::new());
    let mut cols: BTreeMap<&'static str, Vec<&Value>> = PUBLIC_COLUMNS
        .iter()
        .map(|(c, _)| (*c, Vec::new()))
        .collect();
    for e in entries.iter().filter(|e| is_public_in(e, project)) {
        let col = match board.column(e) {
            Some("In Progress") => "Now",
            Some("Triage") => "Later",
            Some(col) => col,
            None => continue,
        };
        if let Some(items) = cols.get_mut(col) {
            items.push(e);
        }
    }
    for items in cols.values_mut() {
        board.sort(items);
    }
    cols
}

fn render_public_roadmap_md(entries: &[Value], project: &str) -> String {
    let cols = public_columns(entries, project);
    let mut out = vec![format!("# {project} roadmap"), String::new()];
    for (col, label) in PUBLIC_COLUMNS {
        let items = &cols[col];
        if items.is_empty() {
            continue;
        }
        out.push(format!("## {label}"));
        out.push(String::new());
        for e in items {
            let title = Some(one_line(e.get("title")))
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| "(untitled)".to_string());
            let meta: Vec<String> = ["priority", "size"]
                .iter()
                .filter_map(|k| e.get(*k).filter(|v| truthy(Some(v))).map(py_str))
                .collect();
            out.push(if meta.is_empty() {
                format!("- {title}")
            } else {
                format!("- {title} _({})_", meta.join(" · "))
            });
        }
        out.push(String::new());
    }
    format!("{}\n", out.join("\n").trim_end())
}

/// Drop rows whose public title carries a leak class (the write-time title
/// gate's vocabulary), warn once with the count, publish the rest.
fn omit_leaky_rows(entries: Vec<Value>, project: &str) -> Vec<Value> {
    let mut candidates: Vec<&Value> = public_columns(&entries, project)
        .into_values()
        .flatten()
        .collect();
    candidates.extend(entries.iter().filter(|e| {
        is_public_in(e, project)
            && PUBLIC_BACKLOG_STATUSES.contains(&crate::org_board::prs::derived_status(e).as_str())
    }));
    let mut dropped: BTreeSet<String> = BTreeSet::new();
    let mut classes: BTreeSet<&'static str> = BTreeSet::new();
    let mut offenders = 0usize;
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for e in candidates {
        let id = e.get("id").map(py_str).unwrap_or_else(|| "?".to_string());
        if !seen.insert(id.clone()) {
            continue;
        }
        let found = super::title_gate::title_leak_classes(&one_line(e.get("title")));
        if !found.is_empty() {
            offenders += 1;
            classes.extend(found);
            dropped.insert(id);
        }
    }
    if offenders == 0 {
        return entries;
    }
    eprintln!(
        "Warning: omitted {offenders} public row(s) from the {project} roadmap render: title \
         matched {}; the rest published.",
        classes.into_iter().collect::<Vec<_>>().join(", ")
    );
    entries
        .into_iter()
        .filter(|e| !dropped.contains(&e.get("id").map(py_str).unwrap_or_else(|| "?".to_string())))
        .collect()
}

fn render_public_backlog_html(project: &str, out: &str) -> bool {
    let request = json!({
        "targets": [{"path": expand(out).to_string_lossy()}],
        "public": {"project": project},
    });
    match board_render(&request) {
        Err(e) => {
            eprintln!("Warning: public backlog HTML render failed: {e}");
            false
        }
        Ok(done) if !done.status.success() => {
            let mut err = String::from_utf8_lossy(&done.stderr).trim().to_string();
            if err.contains("unknown field") {
                err.push_str(
                    " (the installed fno predates the public board render; run `fno doctor \
                     update --rust`)",
                );
            }
            eprintln!(
                "Warning: public backlog HTML render failed: {}",
                err.chars().take(300).collect::<String>()
            );
            false
        }
        Ok(_) => true,
    }
}

/// `fno backlog roadmap [--project P] [--out PATH] [--backlog-html PATH]`:
/// the public roadmap and open-work projections through one leak gate.
pub fn run_roadmap(tail: &[String]) -> i32 {
    if wants_help(tail) {
        println!(
            "Usage: fno backlog roadmap [--project P] [--out PATH] [--backlog-html PATH]\n\n  \
             Render public roadmap and backlog projections through one leak gate."
        );
        return 0;
    }
    let (mut project, mut out, mut backlog_html) = (None, None, None);
    let mut args = tail.iter();
    while let Some(arg) = args.next() {
        let slot = match arg.as_str() {
            "--project" => &mut project,
            "--out" => &mut out,
            "--backlog-html" => &mut backlog_html,
            other => return usage_error("roadmap", other),
        };
        match args.next() {
            Some(v) => *slot = Some(v.clone()),
            None => return usage_error("roadmap", arg),
        }
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    let Some(project) = project.or_else(|| {
        super::create_cli::detect_project(Path::new(&super::create_cli::repo_root(&cwd)))
    }) else {
        eprintln!("Error: no project given and none mapped to the cwd; pass --project.");
        return 1;
    };
    if super::workflows::active_backend_name() != "graph" {
        eprintln!(
            "fno backlog: roadmap: footnote-minted node metadata (size/type/project/dep/model \
             pins/persistent slug) lives only in the default graph store and is unavailable \
             under an external tracker backend"
        );
        return 2;
    }
    // The whole read keeps archived rows, so shipped work stays on the page.
    let entries = match crate::graph_store::read_rows_strict(&super::settings::graph_path()) {
        Ok(rows) => rows,
        Err(e) => {
            eprintln!("Error: canonical graph read failed: {e}");
            return 1;
        }
    };
    let entries = omit_leaky_rows(entries, &project);
    if let Some(html) = &backlog_html {
        if !render_public_backlog_html(&project, html) {
            eprintln!("Error: public backlog HTML render failed; see the warning above");
            return 1;
        }
    }
    let md = render_public_roadmap_md(&entries, &project);
    match out {
        Some(out) => {
            let path = expand(&out);
            if let Err(e) = write_file(&path, &md) {
                eprintln!("Error: roadmap write failed: {e}");
                return 1;
            }
            println!("{}", path.display());
        }
        None => print!("{md}"),
    }
    0
}

// ---------------------------------------------------------------------------
// album
// ---------------------------------------------------------------------------

fn album_stamp(e: &Value) -> String {
    ["completed_at", "updated", "created_at"]
        .iter()
        .find_map(|k| get_str(e, k).filter(|s| !s.is_empty()))
        .unwrap_or("")
        .to_string()
}

fn album_gift(e: &Value) -> String {
    if let Some(pr) = e.get("pr_number").filter(|p| truthy(Some(p))) {
        return format!("PR #{}", py_str(pr));
    }
    if let Some(url) = e.get("pr_url").filter(|u| truthy(Some(u))) {
        let url = py_str(url);
        let last = url
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or("")
            .to_string();
        return format!("PR {last}");
    }
    "no gift".to_string()
}

fn parse_count(verb: &str, flag: &str, value: Option<&String>) -> Result<i64, i32> {
    match value.and_then(|v| v.parse::<i64>().ok()) {
        Some(n) => Ok(n),
        None => {
            eprintln!("fno backlog {verb}: {flag} needs an integer");
            Err(2)
        }
    }
}

/// `fno backlog album [--limit N] [--offset N] [--project P] [--json]`:
/// browse shipped work, done archive residents newest first.
pub fn run_album(tail: &[String]) -> i32 {
    if wants_help(tail) {
        println!(
            "Usage: fno backlog album [--limit N] [--offset N] [--project P] [--json]\n\n  \
             Browse shipped work: done nodes from the archive, newest first.\n  `fno backlog \
             get <id>` still resolves one archived node; `fno backlog unarchive <id>` brings \
             one back."
        );
        return 0;
    }
    let (mut limit, mut offset, mut project, mut json_out) = (20i64, 0i64, None, false);
    let mut args = tail.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--json" | "-J" => json_out = true,
            "--limit" => match parse_count("album", arg, args.next()) {
                Ok(n) => limit = n,
                Err(code) => return code,
            },
            "--offset" => match parse_count("album", arg, args.next()) {
                Ok(n) => offset = n,
                Err(code) => return code,
            },
            "--project" => match args.next() {
                Some(p) => project = Some(p.clone()),
                None => return usage_error("album", arg),
            },
            other => return usage_error("album", other),
        }
    }
    if super::workflows::active_backend_name() != "graph" {
        eprintln!(
            "fno backlog album: the album renders the local archive; unavailable under an \
             external tracker backend"
        );
        return 2;
    }
    // Archive residents are store rows stamped archived_at, not a side file.
    let rows = match crate::graph_store::read_rows(&super::settings::graph_path()) {
        Ok(rows) => rows,
        Err(e) => {
            eprintln!("fno backlog album: archive read failed: {e}");
            return 1;
        }
    };
    let mut shipped: Vec<Value> = rows
        .into_iter()
        .filter(|e| {
            is_dict(e)
                && truthy(e.get("archived_at"))
                && crate::org_board::prs::derived_status(e) == "done"
                && !truthy(e.get("superseded_by"))
                && project
                    .as_deref()
                    .is_none_or(|p| get_str(e, "project") == Some(p))
        })
        .collect();
    shipped.sort_by_key(|e| std::cmp::Reverse(album_stamp(e)));
    let offset = offset.max(0) as usize;
    let page: Vec<&Value> = shipped
        .iter()
        .skip(offset)
        .take(limit.max(0) as usize)
        .collect();

    if json_out {
        let cards: Vec<Value> = page
            .iter()
            .map(|e| {
                let mut card = serde_json::Map::new();
                for key in ["id", "title", "completed_at"] {
                    card.insert(key.into(), e.get(key).cloned().unwrap_or(Value::Null));
                }
                // A url-less pr_number is a real gift: reconcile records the
                // pair independently, so the machine surface keeps it.
                for key in ["pr_number", "pr_url"] {
                    if let Some(v) = e.get(key).filter(|v| truthy(Some(v))) {
                        card.insert(key.into(), v.clone());
                    }
                }
                Value::Object(card)
            })
            .collect();
        println!("{}", super::render::py_json_pretty(&Value::Array(cards)));
        return 0;
    }
    if shipped.is_empty() {
        println!("The album is empty.");
        return 0;
    }
    if page.is_empty() {
        if offset >= shipped.len() {
            println!(
                "album: {} shipped, offset {offset} is past the end",
                shipped.len()
            );
        } else {
            println!(
                "album: {} shipped, --limit {limit} shows nothing",
                shipped.len()
            );
        }
        return 0;
    }
    println!(
        "album: {} shipped, showing {}-{}",
        shipped.len(),
        offset + 1,
        offset + page.len()
    );
    for e in &page {
        let stamp = album_stamp(e);
        let date: String = if stamp.is_empty() {
            "?".to_string()
        } else {
            stamp.chars().take(10).collect()
        };
        let title = ["title", "slug", "id"]
            .iter()
            .find_map(|k| e.get(*k).filter(|v| truthy(Some(v))))
            .map(py_str)
            .unwrap_or_else(|| "None".to_string());
        let id = e
            .get("id")
            .map(py_str)
            .unwrap_or_else(|| "None".to_string());
        println!("{date}  {id}  {title}  {}", album_gift(e));
    }
    let remaining = shipped.len().saturating_sub(page.len() + offset);
    if remaining > 0 {
        println!("... {remaining} more. Raise --limit or page with --offset.");
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_md_cards_carry_lane_blockers_and_done_links() {
        let entries = vec![
            json!({"id": "x-1", "title": "Blocked thing", "status": "ready", "priority": "p2",
                   "project": "fno", "blocked_by": ["x-2"], "plan_path": "internal/p.md"}),
            json!({"id": "x-2", "title": "The blocker", "status": "ready", "priority": "p1",
                   "type": "feature"}),
            json!({"id": "x-3", "title": "Shipped", "status": "done", "project": "fno",
                   "completed_at": "2026-01-02T00:00:00Z", "pr_url": "https://github.com/o/r/pull/9",
                   "additional_prs": [{"number": 10, "note": "follow-up"}],
                   "artifact_url": "javascript:alert(1)"}),
        ];
        let md = render_graph_md(&entries, true);
        assert!(md.starts_with("---\nkanban-plugin: board\n---\n\n## In Progress\n\n\n## Now\n"));
        assert!(md.contains(
            "- [ ] **Blocked thing** `x-1` · fno · p2\n  internal/p.md\n  blocked by: x-2 (The blocker)"
        ));
        assert!(md.contains("- [ ] **The blocker** `x-2` · (unscoped) · p1 · [orphan]"));
        assert!(md.contains(
            "- [x] **Shipped** `x-3` · fno · p2\n  https://github.com/o/r/pull/9\n  #10 - follow-up\n  artifact: `javascript:alert(1)`"
        ));
        assert!(md.ends_with("%% kanban:settings\n```\n{\"kanban-plugin\":\"board\"}\n```\n%%\n"));
        let plain = render_graph_md(&entries, false);
        assert!(plain.starts_with("## In Progress\n"));
        assert!(!plain.contains("kanban:settings"));
    }

    #[test]
    fn provenance_names_unmapped_branches_and_never_asserts_an_unproven_absence() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_string_lossy().into_owned();
        let entries = vec![json!({"id": "x-1", "cwd": root})];
        assert!(provenance_lines(&entries).is_empty());
        std::fs::create_dir_all(dir.path().join(".fno")).unwrap();
        let cache = dir.path().join(".fno").join("branch-provenance.json");
        std::fs::write(&cache, "{not json").unwrap();
        assert!(provenance_lines(&entries).is_empty());
        let rows = json!([
            {"branch": "feature/x-1", "node": "x-1", "has_remote": false,
             "unpushed": 23, "pr_number": null, "live": false, "age": "33 hours ago"},
            {"branch": "fix/loose", "node": null, "has_remote": null, "unpushed": 0,
             "live": true, "age": "12 hours ago"},
        ]);
        std::fs::write(&cache, rows.to_string()).unwrap();
        assert_eq!(
            provenance_lines(&entries),
            vec![
                "## Branch Provenance",
                "",
                "- **x-1** (feature/x-1): no remote, 23 unpushed, no PR, newest commit 33 hours ago",
                "- *(unmapped)* (fix/loose): remote unknown, 0 unpushed, no PR, LIVE, newest commit 12 hours ago",
                "",
            ]
        );
    }

    #[test]
    fn done_column_keeps_the_ten_newest() {
        let entries: Vec<Value> = (0..12)
            .map(|i| {
                json!({"id": format!("x-{i}"), "title": format!("t{i}"), "status": "done",
                       "completed_at": format!("2026-01-{:02}T00:00:00Z", i + 1)})
            })
            .collect();
        let md = render_graph_md(&entries, false);
        assert!(md.contains("`x-11`"));
        assert!(!md.contains("`x-1` "));
        assert!(!md.contains("`x-0` "));
    }

    #[test]
    fn swimlanes_put_named_projects_before_the_unscoped_lane() {
        let entries = vec![
            json!({"id": "x-1", "title": "loose", "status": "ready", "priority": "p2"}),
            json!({"id": "x-2", "title": "zed", "status": "ready", "priority": "p2", "project": "zed"}),
            json!({"id": "x-3", "title": "alpha", "status": "ready", "priority": "p2", "project": "alpha"}),
        ];
        let board = Board::new(&entries, &BTreeSet::new());
        let mut rows: Vec<&Value> = entries.iter().collect();
        board.sort(&mut rows);
        let ids: Vec<&str> = rows.iter().filter_map(|e| id_of(e)).collect();
        assert_eq!(ids, vec!["x-3", "x-2", "x-1"]);
    }

    #[test]
    fn board_sections_read_unknown_claims_and_never_fall_to_on_deck() {
        let now = parse_iso_str("2026-01-10T12:00:00Z").unwrap();
        let entries = vec![
            json!({"id": "x-1", "title": "running", "status": "in_progress", "project": "p"}),
            json!({"id": "x-2", "title": "waiting", "status": "ready", "project": "p",
                   "blocked_by": ["x-1"]}),
            json!({"id": "x-3", "title": "shipped", "status": "done", "project": "p",
                   "completed_at": "2026-01-10T11:00:00Z", "pr_number": 7}),
        ];
        let board = compute_board(&entries, Some("p"), Err("boom".into()), now, None);
        assert_eq!(
            board["in_progress"]["unknown"],
            json!("live claim state unavailable (boom)")
        );
        assert_eq!(
            board["just_finished"]["rows"][0]["fact"],
            json!("PR 7 merged 1h ago")
        );
        let deck = board["on_deck"]["rows"].as_array().unwrap();
        assert_eq!(deck.len(), 1);
        assert_eq!(deck[0]["fact"], json!("blocked by x-1"));
    }

    #[test]
    fn ages_floor_at_a_minute_and_refuse_future_stamps() {
        assert_eq!(format_age(5.0), "1m ago");
        assert_eq!(format_age(-30.0), "1m ago");
        assert_eq!(format_age(-120.0), "unknown age");
        assert_eq!(format_age(7200.0), "2h ago");
        assert_eq!(format_age(200_000.0), "2d ago");
        assert_eq!(age_from_epoch(Some(&json!("NaN")), 0), "unknown age");
    }

    #[test]
    fn bases_refuse_a_hand_authored_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("epics.base");
        let content = &bases()[0].1;
        assert_eq!(write_base(&path, content), "written");
        assert_eq!(write_base(&path, content), "unchanged");
        std::fs::write(&path, "hand made\n").unwrap();
        assert_eq!(write_base(&path, content), "refused");
        assert!(content.starts_with(GENERATED_MARKER));
    }

    #[test]
    fn roadmap_folds_columns_and_omits_leaky_titles() {
        let entries = vec![
            json!({"id": "x-1", "title": "Ship it", "status": "in_progress", "project": "p",
                   "priority": "p1", "size": "M"}),
            json!({"id": "x-2", "title": "Leak /Users/me/secret", "status": "ready",
                   "project": "p"}),
            json!({"id": "x-3", "title": "Private", "status": "ready", "project": "p",
                   "public": false}),
            json!({"id": "x-4", "title": "Waiting", "status": "ready", "project": "p",
                   "queued_at": "2026-01-01"}),
            json!({"id": "x-ext-null", "title": "Visitor ask", "status": "ready",
                   "project": "p", "source_kind": "external_request"}),
            json!({"id": "x-ext-pub", "title": "Visitor ask published",
                   "status": "ready", "project": "p",
                   "source_kind": "external_request", "public": true}),
        ];
        let kept = omit_leaky_rows(entries, "p");
        assert_eq!(kept.len(), 5);
        let md = render_public_roadmap_md(&kept, "p");
        assert_eq!(
            md,
            "# p roadmap\n\n## Now\n\n- Ship it _(p1 · M)_\n\n## Later\n\n- Waiting\n- Visitor ask published\n"
        );
    }

    #[test]
    fn render_targets_refuse_unknown_keys_and_relative_paths() {
        assert!(parse_target(&json!({"path": "/tmp/b.html", "scpoe": "x"}))
            .unwrap_err()
            .contains("unknown key(s) 'scpoe'"));
        assert!(parse_target(&json!({"path": "rel/b.html"}))
            .unwrap_err()
            .contains("must be absolute"));
        let target = parse_target(&json!({"path": "/tmp/b.html", "project": "fno"})).unwrap();
        assert_eq!(target.scope, "fno");
        assert_eq!(target.projection, "local");
        // An empty scope would widen to every project; it refuses instead.
        assert!(parse_target(&json!({"path": "/tmp/b.html", "scope": " "}))
            .unwrap_err()
            .contains("scope must not be empty"));
        let star = parse_target(&json!({"path": "/tmp/b.html", "scope": "*"})).unwrap();
        assert_eq!(star.scope, "all");
    }
}
