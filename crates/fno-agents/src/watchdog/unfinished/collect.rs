//! The unfinished-work IO seam: gather the observations, then publish a snapshot.
//!
//! Read-only apart from one `git fetch origin main` per repository, the GitHub PR
//! reads, and the publish sequence's own stamps, events and fleet task.

use super::{
    classify, escalate_unfinished, finding_identity, fresh_identities, snapshot_payload,
    snapshot_signature, ClaimView, NodeObs, Observations, OwnerProbe, PrObs, Snapshot, WorktreeObs,
    DIMENSIONS, UNKNOWN_DIM,
};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The fetch is the report's only network leg and the tick's deadline does not
/// bound it: a stalled remote would hold the tick open until the alarm kills
/// every later leg. Past this ceiling the repository reads unknown, the same
/// verdict as any other failed fetch.
const FETCH_TIMEOUT_S: u64 = 120;

/// Ceiling on one PR read, and the floor under which the next is not started.
/// Measured over eight real open PRs: 1.07s merge-state + 1.42s reviews = 2.49s
/// idle, roughly 4x that under fleet load.
const PR_READ_BUDGET_S: f64 = 30.0;
const PR_READ_FLOOR_S: f64 = 3.0;
const REVIEWS_LEG_FLOOR_S: f64 = 1.0;

fn norm(path: &str) -> String {
    Path::new(path)
        .components()
        .collect::<PathBuf>()
        .to_string_lossy()
        .into_owned()
}

fn truthy_str(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
    }
}

/// One `git fetch origin main` per repository. Failure is a verdict (the metric
/// for that repository reads unknown), never a guessed count.
fn fetch_origin_main(repo: &Path) -> bool {
    crate::worktree::git_bounded(repo, &["fetch", "origin", "main"], FETCH_TIMEOUT_S)
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// Commits on the worktree's HEAD not on `origin/main`. None when the read fails
/// or answers anything but a plain integer.
fn ahead_of_main(worktree: &Path) -> Option<i64> {
    let out = crate::worktree::git_out(worktree, &["rev-list", "--count", "origin/main..HEAD"])?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let text = text.trim();
    if text.is_empty() || !text.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    text.parse::<i64>().ok()
}

/// Number of porcelain entries in the worktree. None when the read fails: a
/// failed read is unknown dirt, never zero dirt.
fn dirty_path_count(worktree: &Path) -> Option<i64> {
    let out = crate::worktree::git_out(worktree, &["status", "--porcelain"])?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Some(text.lines().filter(|line| !line.trim().is_empty()).count() as i64)
}

/// Every project scope the report covers. The scheduled tick and the manual
/// command resolve their roots here so the two surfaces cannot disagree on scope:
/// the invoking repo (when it is a checkout) plus the distinct cwd of every
/// tracked sidecar.
pub fn report_roots() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    let root = crate::target_gates::resolve_repo_root();
    if root.join(".git").exists() {
        roots.push(root);
    }
    let name = crate::tracker::backend_name(None);
    let sidecars = crate::tracker::backend(&name).and_then(|tracker| tracker.sidecar_all());
    // No sidecar store means the repo root only.
    if let Ok(sidecars) = sidecars {
        for (_id, fields) in sidecars {
            if let Some(cwd) = truthy_str(fields.get("cwd")) {
                let path = PathBuf::from(norm(&cwd));
                if !roots.contains(&path) {
                    roots.push(path);
                }
            }
        }
    }
    roots
}

/// The whole-graph read, archived residents hidden. An unreadable graph stays
/// apart from an empty one.
fn read_graph_strict() -> Result<Vec<Value>, String> {
    let store = crate::backlog::api::Store::new(&crate::graph_get::default_graph_path());
    let rows = crate::backlog::api::rows(&store).map_err(|e| e.0)?;
    Ok(rows
        .into_iter()
        .filter(|row| !truthy(row.get("archived_at")))
        .collect())
}

/// Raw registry rows indexed by each row's own `cwd`, plus an ok flag. A missing
/// registry is a legitimate empty fleet (ok); one that exists and fails to parse
/// reads every candidate unmeasurable.
fn registry_rows_by_cwd() -> (HashMap<String, Vec<Value>>, bool) {
    let path = crate::worktree::status::registry_path();
    let text = match crate::registry_read::registry_text(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return (HashMap::new(), true);
        }
        Err(_) => return (HashMap::new(), false),
    };
    let Ok(Value::Object(document)) = serde_json::from_str::<Value>(&text) else {
        return (HashMap::new(), false);
    };
    let mut by_cwd: HashMap<String, Vec<Value>> = HashMap::new();
    let rows = document.get("agents").and_then(Value::as_array);
    for row in rows.into_iter().flatten() {
        if let Some(cwd) = truthy_str(row.get("cwd")) {
            by_cwd.entry(norm(&cwd)).or_default().push(row.clone());
        }
    }
    (by_cwd, true)
}

fn session_handle(row: &Value) -> Option<String> {
    truthy_str(row.get("harness_session_id")).or_else(|| truthy_str(row.get("short_id")))
}

/// The repository's main worktree from any linked row: the shared git dir. A
/// normal repo answers `<repo>/.git`; a bare container answers the bare repo
/// itself (there is no main tree to exclude). Falls back to the first listed row
/// only when git will not answer.
fn main_worktree(first_listed: &str) -> String {
    match crate::git_paths::common_dir(Path::new(first_listed)) {
        Some(common) => {
            let is_dot_git = common.file_name().is_some_and(|name| name == ".git");
            let main = if is_dot_git {
                common.parent().map(Path::to_path_buf).unwrap_or(common)
            } else {
                common
            };
            norm(&main.to_string_lossy())
        }
        None => norm(first_listed),
    }
}

fn parse_iso_epoch(stamp: Option<&str>) -> Option<f64> {
    let stamp = stamp.filter(|s| !s.is_empty())?;
    crate::backlog_ready::parse_iso_str(stamp).map(|ms| ms as f64 / 1000.0)
}

/// No recorded start token on a claim row: the existence check stands on its own.
/// Only a confident absence reads dead; a probe that cannot see the process reads
/// unreadable, never dead.
fn default_pid_alive(pid: Option<i64>) -> Option<bool> {
    let pid = pid?;
    if pid <= 1 || pid > 0x7FFF_FFFF {
        return Some(false);
    }
    match crate::claims::probe_pid(pid as i32) {
        crate::claims::PidProbe::Absent => Some(false),
        crate::claims::PidProbe::Refused => None,
        crate::claims::PidProbe::Created(_) => {
            match crate::footprint_cause::pid_alive(pid as u32, None) {
                Some(false) => Some(false),
                _ => Some(true),
            }
        }
    }
}

fn claim_view(node_id: &str) -> ClaimView {
    // A store failure reads corrupted here, the same answer the claim command
    // prints: an answer that excludes the node, never a failed read.
    let (state, record) = crate::claims::status(&format!("node:{node_id}"), None);
    ClaimView {
        state: Some(state.as_str().to_string()),
        holder: record
            .as_ref()
            .map(|r| r.holder.clone())
            .filter(|holder| !holder.is_empty()),
        pid: record.as_ref().and_then(|r| r.pid).map(i64::from),
    }
}

/// Owner probes, one per unique handle.
struct Probes<'a> {
    registry_by_cwd: &'a HashMap<String, Vec<Value>>,
    rows: Option<Vec<crate::state::RegistryEntry>>,
    stores: crate::session_truth::Stores,
    cache: HashMap<String, OwnerProbe>,
}

impl Probes<'_> {
    /// One truth resolution, age and basis together: an mtime-derived age must
    /// reach the classifier labelled, or the stat lie reads as positive evidence.
    fn truth_pair(&self, handle: &str) -> (Option<f64>, Option<String>) {
        let now_s = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        let payload = crate::session_truth::resolve_payload(
            self.rows.as_deref(),
            handle,
            now_s,
            &self.stores,
            crate::session_truth::cursor::global(),
        );
        (
            payload.get("last_activity_age_s").and_then(Value::as_f64),
            truthy_str(payload.get("last_activity_basis")),
        )
    }

    fn probe_for(&mut self, handle: &str, view: Option<&ClaimView>) -> OwnerProbe {
        if let Some(cached) = self.cache.get(handle).cloned() {
            // A later pass may carry claim evidence the first touch lacked. Merge
            // the more informative reading rather than baking in whichever pass
            // touched the handle first: claim state and its pid are facts about
            // the handle, not about the pass that found them.
            let Some(view) = view.filter(|_| cached.claim_state.is_none()) else {
                return cached;
            };
            let merged = OwnerProbe {
                pid_alive: cached.pid_alive.or_else(|| default_pid_alive(view.pid)),
                claim_state: view.state.clone(),
                ..cached
            };
            self.cache.insert(handle.to_string(), merged.clone());
            return merged;
        }
        let stored_exited = self.registry_by_cwd.values().flatten().any(|row| {
            row.get("status").and_then(Value::as_str) == Some("exited")
                && session_handle(row).as_deref() == Some(handle)
        });
        let (age, basis) = self.truth_pair(handle);
        let probe = OwnerProbe {
            handle: handle.to_string(),
            pid_alive: default_pid_alive(view.and_then(|v| v.pid)),
            transcript_age_s: age,
            last_activity_basis: basis,
            claim_state: view.and_then(|v| v.state.clone()),
            stored_exited,
        };
        self.cache.insert(handle.to_string(), probe.clone());
        probe
    }

    /// The registry's handles for `paths`, then the claim's holder when the claim
    /// is not free, each probed once.
    fn owners_of(&mut self, paths: &[&str], view: Option<&ClaimView>) -> Vec<OwnerProbe> {
        let mut handles: Vec<String> = Vec::new();
        for path in paths {
            let rows = self.registry_by_cwd.get(&norm(path));
            for row in rows.into_iter().flatten() {
                if let Some(handle) = session_handle(row) {
                    if !handles.contains(&handle) {
                        handles.push(handle);
                    }
                }
            }
        }
        let holder = view
            .filter(|v| matches!(v.state.as_deref(), Some("live" | "suspect" | "stale")))
            .and_then(|v| v.holder.clone());
        if let Some(holder) = &holder {
            if !handles.contains(holder) {
                handles.push(holder.clone());
            }
        }
        handles
            .iter()
            .map(|handle| {
                let held = holder.as_deref() == Some(handle.as_str());
                self.probe_for(handle, if held { view } else { None })
            })
            .collect()
    }
}

struct PrCandidate {
    node_id: String,
    pr_number: i64,
    pr_url: Option<String>,
    repo_dir: Option<String>,
    repo_slug: Option<String>,
}

/// A cwd that does not exist on disk or lacks a `.git` entry is unresolvable.
fn resolve_repo_dir(node: &Value) -> Option<String> {
    let raw = truthy_str(node.get("_resolved_cwd")).or_else(|| truthy_str(node.get("cwd")))?;
    let path = Path::new(&raw);
    (path.is_dir() && path.join(".git").exists()).then(|| raw.clone())
}

/// One candidate per (node, PR) pair. A superseded node and a node with a
/// `completed_at` are never candidates: this report passes no clock, and without
/// one the watcher's grace window has nothing to age against.
fn discover_open_prs(entries: &[Value]) -> Vec<PrCandidate> {
    let mut candidates: Vec<PrCandidate> = Vec::new();
    for node in entries {
        if truthy(node.get("superseded_by")) || truthy(node.get("completed_at")) {
            continue;
        }
        let node_id = node
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let repo_dir = resolve_repo_dir(node);
        for (pr_number, pr_url) in crate::backlog::merge_evidence::node_pr_refs(node) {
            candidates.push(PrCandidate {
                node_id: node_id.clone(),
                pr_number,
                repo_slug: crate::backlog::merge_evidence::repo_slug_from_url(pr_url.as_deref()),
                pr_url,
                repo_dir: repo_dir.clone(),
            });
        }
    }
    candidates
}

/// The merge-state leg, then the activity leg, inside one budget. The legs have no
/// timeout of their own, so a worker thread carries them and the deadline reaches
/// inside the call: a read that overruns reads as failed, and its thread finishes
/// alone when its gh child returns.
fn read_pr_state(
    candidate: &PrCandidate,
    budget_s: f64,
) -> Result<(Option<String>, Option<String>), String> {
    let number = candidate.pr_number;
    let slug = candidate.repo_slug.clone();
    let cwd = candidate.repo_dir.clone();
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(read_pr_legs(number, slug, cwd, budget_s));
    });
    receiver
        .recv_timeout(Duration::from_secs_f64(budget_s.max(0.0)))
        .unwrap_or_else(|_| {
            Err(format!(
                "read_pr_state #{number} timed out after {budget_s:.1}s"
            ))
        })
}

fn read_pr_legs(
    number: i64,
    slug: Option<String>,
    cwd: Option<String>,
    budget_s: f64,
) -> Result<(Option<String>, Option<String>), String> {
    let started = Instant::now();
    let merge_state = crate::backlog::merge_state::query_pr_merge_state(
        number,
        slug.as_deref(),
        cwd.as_deref(),
        false,
    )
    .map_err(|e| e.message)?;
    let remaining = budget_s - started.elapsed().as_secs_f64();
    if remaining < REVIEWS_LEG_FLOOR_S {
        return Err(format!(
            "read_pr_state #{number} left {remaining:.2}s of its {budget_s:.1}s budget after the merge-state read, under the {REVIEWS_LEG_FLOOR_S:.1}s the reviews read costs; it was not started"
        ));
    }
    let activity_cwd = cwd.unwrap_or_else(|| {
        std::env::current_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| ".".to_string())
    });
    let answer = crate::pr_status_facts::run_op(
        "status-pr-activity",
        &json!({
            "op": "status-pr-activity",
            "cwd": activity_cwd,
            "slug": slug.unwrap_or_default(),
            "pr": number,
            "reviewers": [],
        }),
    );
    let answer: Value = serde_json::from_str(&answer)
        .map_err(|e| format!("read_pr_state #{number} (reviews) failed: {e}"))?;
    if truthy(answer.get("error")) {
        let error = answer.get("error").and_then(Value::as_str).unwrap_or("");
        return Err(format!("read_pr_state #{number} (reviews) failed: {error}"));
    }
    Ok((
        Some(merge_state.state).filter(|state| !state.is_empty()),
        truthy_str(answer.get("opened_at")),
    ))
}

/// Gather every observation the classifier needs.
fn collect_observations(roots: &[PathBuf], now_s: f64, deadline: Option<Instant>) -> Observations {
    let mut warnings: Vec<String> = Vec::new();
    let budget_left = || -> Option<f64> {
        deadline.map(|at| {
            let now = Instant::now();
            if at >= now {
                at.duration_since(now).as_secs_f64()
            } else {
                -now.duration_since(at).as_secs_f64()
            }
        })
    };

    // Graph: global, one strict read.
    let (entries, graph_ok): (Vec<Value>, bool) = match read_graph_strict() {
        Ok(entries) => (entries, true),
        Err(e) => {
            warnings.push(format!("graph unreadable: {e}"));
            (Vec::new(), false)
        }
    };
    let mut entries_by_id: Map<String, Value> = Map::new();
    for entry in &entries {
        if let Some(id) = truthy_str(entry.get("id")) {
            entries_by_id.insert(id, entry.clone());
        }
    }
    let container_ids = crate::backlog::next::container_ids(&entries);

    let (registry_by_cwd, registry_ok) = registry_rows_by_cwd();
    if !registry_ok {
        warnings.push("registry unreadable".to_string());
    }

    // Worktrees: enumerate once per root, fetch once per root.
    let mut worktree_rows: Vec<(String, String, Option<String>)> = Vec::new();
    let mut fetched_ok: HashMap<String, bool> = HashMap::new();
    let mut unscanned: Vec<String> = Vec::new();
    for root in roots {
        let root_str = root.to_string_lossy().into_owned();
        if budget_left().is_some_and(|left| left <= 0.0) {
            unscanned.push(root_str);
            continue;
        }
        let listed = crate::worktree::status::porcelain(root).unwrap_or_default();
        // The main worktree comes from the shared git dir rather than the
        // listing's first row: a bare worktree-container contributes no main row
        // at all, so "first listed" would name a linked tree and silently exclude
        // it from the dirty dimension. Clearing commands name the main tree, never
        // whichever linked worktree happened to run the report.
        let main_path = match listed.first() {
            Some((_, path)) => main_worktree(path),
            None => norm(&root_str),
        };
        let fetched = fetch_origin_main(root);
        fetched_ok.insert(main_path.clone(), fetched);
        if !fetched {
            warnings.push(format!("git fetch origin main failed for {root_str}"));
        }
        for (branch, path) in listed {
            if norm(&path) == main_path {
                continue; // the main worktree: shared surface, excluded
            }
            if !Path::new(&path).is_dir() {
                warnings.push(format!("worktree vanished before scan: {path}"));
                continue;
            }
            worktree_rows.push((path, main_path.clone(), branch));
        }
    }

    // Claims per node. The native read answers every key, so the dimension's
    // claims flag stays up; a corrupted store excludes nodes through its state.
    let claim_views: HashMap<String, ClaimView> = entries_by_id
        .keys()
        .map(|id| (id.clone(), claim_view(id)))
        .collect();

    let registry_entries =
        crate::state::load_registry(&crate::paths::AgentsHome::from_env().registry_json())
            .ok()
            .map(|registry| registry.entries);
    let mut probes = Probes {
        registry_by_cwd: &registry_by_cwd,
        rows: registry_entries,
        stores: crate::session_truth::Stores::ambient(),
        cache: HashMap::new(),
    };

    // Worktree observations. The node resolver reads the store's raw rows, the
    // same read its own command makes, once per sweep rather than once per tree.
    let resolver_rows: Vec<Value> = if entries_by_id.is_empty() {
        Vec::new()
    } else {
        crate::graph_store::read_rows_strict(&crate::worktree::state_root().join("graph.db"))
            .unwrap_or_default()
    };
    let mut worktrees: Vec<WorktreeObs> = Vec::new();
    for (path, repo, branch) in &worktree_rows {
        let node_id: Option<String> = if entries_by_id.is_empty() {
            None
        } else {
            let resolved = crate::worktree::resolve_lenient(Path::new(path));
            crate::worktree::stranded::resolve_node_id(
                &resolved.to_string_lossy(),
                Some(branch.as_deref().unwrap_or("")),
                &resolver_rows,
            )
            .map(|(id, _row)| id)
        };
        let dirty = dirty_path_count(Path::new(path));
        let ahead = if fetched_ok.get(repo).copied().unwrap_or(false) {
            ahead_of_main(Path::new(path))
        } else {
            None
        };
        let view = node_id.as_ref().and_then(|id| claim_views.get(id));
        let owner_probes = probes.owners_of(&[path.as_str()], view);
        worktrees.push(WorktreeObs {
            path: path.clone(),
            repo_root: repo.clone(),
            branch: branch.clone(),
            dirty_count: dirty,
            ahead_count: ahead,
            node_id,
            owner_probes,
        });
    }

    // Node observations.
    let mut nodes: Vec<NodeObs> = Vec::new();
    for (node_id, entry) in &entries_by_id {
        let view = claim_views.get(node_id);
        let wt = worktrees
            .iter()
            .find(|w| w.node_id.as_deref() == Some(node_id.as_str()));
        let node_cwd =
            truthy_str(entry.get("_resolved_cwd")).or_else(|| truthy_str(entry.get("cwd")));
        let mut worktree_path: Option<String> = wt.map(|w| w.path.clone());
        if worktree_path.is_none() {
            worktree_path = node_cwd
                .clone()
                .filter(|cwd| worktrees.iter().any(|w| &w.path == cwd));
        }
        let candidates: Vec<&str> = [worktree_path.as_deref(), node_cwd.as_deref()]
            .into_iter()
            .flatten()
            .collect();
        let owner_probes = probes.owners_of(&candidates, view);
        nodes.push(NodeObs {
            node_id: node_id.clone(),
            status: entry
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            is_container: container_ids.contains(node_id),
            touched_at_epoch: parse_iso_epoch(entry.get("touched_at").and_then(Value::as_str)),
            cwd: node_cwd,
            worktree_path,
            ahead_count: wt.and_then(|w| w.ahead_count),
            claim: view.cloned(),
            owner_probes,
        });
    }

    // PR observations through the canonical reader.
    let mut prs: Vec<PrObs> = Vec::new();
    let mut github_ok = true;
    let mut prs_unscanned = false;
    let node_probes: HashMap<&str, &Vec<OwnerProbe>> = nodes
        .iter()
        .map(|n| (n.node_id.as_str(), &n.owner_probes))
        .collect();
    for candidate in discover_open_prs(&entries) {
        let left = budget_left();
        if left.is_some_and(|left| left < PR_READ_FLOOR_S) {
            // Out of budget mid-scan: the unread candidates stay unread, and the
            // dimension reads unknown rather than counting only what fit before
            // the deadline. The floor, not zero: a read that cannot finish is an
            // unscanned candidate wearing a failure's costume.
            prs_unscanned = true;
            break;
        }
        // A clamped budget is the deadline reaching inside one call; the
        // checkpoint above only ever fires between them.
        let budget = left.map_or(PR_READ_BUDGET_S, |left| left.min(PR_READ_BUDGET_S));
        let clamped = budget < PR_READ_BUDGET_S;
        let number = candidate.pr_number;
        let mut pr_state: Option<String> = None;
        let mut opened_epoch: Option<f64> = None;
        match read_pr_state(&candidate, budget) {
            Ok((state, opened_at)) => {
                pr_state = state;
                opened_epoch = parse_iso_epoch(opened_at.as_deref());
            }
            Err(error) => {
                if clamped {
                    // The budget bound, not GitHub. Blaming the API here would
                    // report an outage every time a tick ran short.
                    prs_unscanned = true;
                    warnings.push(format!(
                        "pr read for {number} ran out of tick budget ({budget:.1}s): {error}"
                    ));
                } else {
                    github_ok = false;
                    warnings.push(format!("pr read failed for {number}: {error}"));
                }
            }
        }
        let owner_probes = node_probes
            .get(candidate.node_id.as_str())
            .map(|probes| (*probes).clone())
            .unwrap_or_default();
        prs.push(PrObs {
            pr_number: number,
            pr_url: candidate.pr_url.clone(),
            node_id: Some(candidate.node_id.clone()).filter(|id| !id.is_empty()),
            state: pr_state,
            opened_at_epoch: opened_epoch,
            owner_probes,
        });
    }

    Observations {
        now_epoch: now_s,
        graph_ok,
        claims_ok: true,
        registry_ok,
        github_ok,
        nodes,
        worktrees,
        prs,
        unscanned_roots: unscanned,
        prs_unscanned,
        warnings,
    }
}

/// The one producer: collect then classify. Both the manual command and the
/// scheduled tick call this, so their reports cannot diverge.
pub fn build_report(roots: &[PathBuf], now_s: f64) -> Snapshot {
    build_report_with_deadline(roots, now_s, None)
}

/// [`build_report`] under the tick's budget: late roots stay unscanned and their
/// dimensions read unknown, never clean.
pub fn build_report_with_deadline(
    roots: &[PathBuf],
    now_s: f64,
    deadline: Option<Instant>,
) -> Snapshot {
    classify(&collect_observations(roots, now_s, deadline))
}

/// The one publish sequence, shared by the manual command and the tick: mail
/// (change-gated, push not pull), sweep-file stamps (the positive complete marker
/// only on a fully measured scan), the scan event plus fresh finding events, and
/// the deduplicated durable task. Returns the payload callers render or JSON-emit.
/// `log` receives human-status lines.
pub fn publish_report(
    snapshot: &Snapshot,
    source: &str,
    now_s: f64,
    mail_to: &str,
    log: Option<&dyn Fn(&str)>,
) -> Value {
    let note = |line: String| {
        if let Some(log) = log {
            log(&line);
        }
    };
    let payload = snapshot_payload(snapshot);

    // Mail before the sweep-file write: the change gate compares against the
    // previous sweep's stamps, and only a settled-ok mail advances them.
    let (ok, receipt, signature) = crate::watchdog::unfinished_mail_gate(snapshot, mail_to);
    if !ok {
        note(format!("watchdog mail: {receipt}"));
    }

    // The event gate is its own stamp, advanced by this publish regardless of the
    // mail outcome: with no recipient configured the mail stamp rightly never
    // moves, and chaining the event gate to it would re-emit every finding on
    // every cadence.
    let prev_events_sig = crate::watchdog::last_unfinished_events_signature();
    let events_sig = snapshot_signature(snapshot);
    let stamps = json!({
        "counts": payload["counts"],
        "complete": payload["complete"],
        "signature": signature,
        "events_signature": events_sig,
    });
    crate::watchdog::write_sweep_file(source, None, now_s, None, Some(&stamps));

    let unknown: Vec<String> = DIMENSIONS
        .iter()
        .filter(|dim| payload["dimensions"][**dim]["state"] == UNKNOWN_DIM)
        .map(|dim| (*dim).to_string())
        .collect();
    let mut scan = Map::new();
    scan.insert("complete".to_string(), payload["complete"].clone());
    scan.insert("finding_count".to_string(), json!(snapshot.findings.len()));
    for dim in DIMENSIONS {
        if !payload["counts"][dim].is_null() {
            scan.insert(dim.to_string(), payload["counts"][dim].clone());
        }
    }
    scan.insert("unknown_dimensions".to_string(), json!(unknown));
    scan.insert("warnings".to_string(), payload["warnings"].clone());
    crate::watchdog::emit_event("watchdog_unfinished_work_scan", Value::Object(scan));

    let fresh = fresh_identities(snapshot, &prev_events_sig);
    for finding in &snapshot.findings {
        if fresh.contains(&finding_identity(finding)) {
            crate::watchdog::emit_event(
                "watchdog_unfinished_work_finding",
                json!({
                    "kind": finding.kind,
                    "subject": finding.subject,
                    "basis": finding.basis,
                    "clear_command": finding.clear_command,
                    "node": finding.node_id,
                    "pr_number": finding.pr_number,
                    "cwd": finding.cwd,
                    "age_s": finding.age_s.map(|age| age as i64),
                }),
            );
        }
    }

    // An incomplete scan escalates what it did reach and names what it did not.
    // Withholding the whole ask was permanent, not transient: a fetch against a
    // deleted worktree root fails on every future run too.
    let home = crate::paths::AgentsHome::from_env();
    let store = crate::provider_cap::questions_path(&home);
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".to_string());
    match escalate_unfinished(&snapshot.findings, &store, &cwd, &unknown) {
        Ok((outcome, _id)) if outcome == "none" => {
            note("watchdog escalation: no unfinished-work findings".to_string());
        }
        Ok((outcome, id)) => {
            let incomplete = if unknown.is_empty() {
                String::new()
            } else {
                format!(", incomplete: {}", unknown.join(", "))
            };
            note(format!(
                "watchdog escalation: {outcome} {id} ({} finding(s){incomplete})",
                snapshot.findings.len()
            ));
        }
        Err(error) => note(format!("watchdog escalation failed: {error}")),
    }

    payload
}
