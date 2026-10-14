//! The orientation report, ported from `fno/target/orient.py` (the subset
//! `fno do target init`'s post-step prints). Read-only, never raises: each
//! line resolves independently and degrades to `unknown` plus its resolving
//! command, never a stack trace. The parity contract is the frozen init
//! goldens, which carry this report byte for byte on the states a fresh init
//! lands in.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;

// ── the manifest read ─────────────────────────────────────────────────────────

/// The merged session manifest: the frontmatter keys plus the body
/// `key: value` lines the helper appends below it (claim keys,
/// graph_node_id); prose lines carry no snake_case key and drop out.
pub fn read_manifest(project_root: &Path) -> Option<BTreeMap<String, String>> {
    let path = manifest_path(project_root)?;
    let text = std::fs::read_to_string(path).ok()?;
    let mut raw = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line == "---" {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        // A prose sentence with a colon is not a manifest key: keys are
        // snake_case identifiers.
        let key = key.trim();
        if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            continue;
        }
        let value = value.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
            .unwrap_or(value);
        raw.insert(key.to_string(), value.to_string());
        if key == "target_claim_holder" {
            // The body keys are the manifest's last fields; later lines are
            // prose about the manifest, not fields.
            break;
        }
    }
    if raw.is_empty() {
        None
    } else {
        Some(raw)
    }
}

/// The session manifest path: the worktree slice of the space when that
/// exists, else the pre-space checkout manifest (the Python read's exact
/// order: space first when present, legacy fallback when not).
fn manifest_path(project_root: &Path) -> Option<PathBuf> {
    let space = crate::paths::worktree_space_dir(project_root).join("target-state.md");
    if space.is_file() {
        return Some(space);
    }
    let legacy = project_root.join(".fno").join("target-state.md");
    legacy.is_file().then_some(legacy)
}

fn manifest_str(raw: &Option<BTreeMap<String, String>>, key: &str) -> String {
    raw.as_ref()
        .and_then(|m| m.get(key))
        .map(|v| v.trim().to_string())
        .unwrap_or_default()
}

// ── liveness ─────────────────────────────────────────────────────────────────

/// Best-effort: is `pid` a running process on THIS host? Biased toward LIVE
/// on any uncertainty: a false-live costs one autonomous /think, a false-dead
/// would archive a still-running session's manifest.
fn pid_alive(pid_val: &str) -> bool {
    let Ok(pid) = pid_val.trim().parse::<i32>() else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    match unsafe { libc::kill(pid, 0) } {
        0 => true,
        // EPERM is exists-but-not-ours: biased live.
        -1 => std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM),
        _ => false,
    }
}

/// The claim lockfile state for `claim_key` (free|live|suspect|stale|
/// corrupted), or None when the read degrades -- the caller must NOT treat
/// that as confirmed-dead.
fn claim_state(claim_key: &str) -> Option<&'static str> {
    let (state, _record) = crate::claims::status(claim_key, None);
    Some(state.as_str())
}

/// `(state, reason)` where state is live | dead | none. The node claim is the
/// ONLY durable liveness signal (session-pid anchored + TTL-protected);
/// `owner_pid` is the transient init wrapper pid that dies seconds after
/// init, so it can only ever PROVE life. DEAD is asserted solely from a claim
/// confirmed absent/expired; every other shape biases live.
fn manifest_liveness(raw: &Option<BTreeMap<String, String>>) -> (String, String) {
    let Some(_) = raw else {
        return ("none".into(), "no manifest".into());
    };
    let claim_key = manifest_str(raw, "target_claim_key");
    if !claim_key.is_empty() {
        let state = claim_state(&claim_key);
        if matches!(state, Some("live") | Some("suspect")) {
            return (
                "live".into(),
                format!("claim {claim_key} {}", state.unwrap()),
            );
        }
        if matches!(state, Some("free") | Some("stale")) {
            return (
                "dead".into(),
                format!("claim {claim_key} {}", state.unwrap()),
            );
        }
        // corrupted / unreadable -> claim signal unavailable, cannot confirm
        return (
            "live".into(),
            format!("claim {claim_key} unreadable (biased live)"),
        );
    }
    if pid_alive(&manifest_str(raw, "owner_pid")) {
        return ("live".into(), "owner_pid alive".into());
    }
    (
        "live".into(),
        "no claim key; owner_pid transient (biased live)".into(),
    )
}

/// Authority fails CLOSED: a stamped grant anchored by a LIVE claim, nothing
/// else. Liveness's bias toward live is right for `attended` and wrong here,
/// where a stale grant silently un-prompts every session that reads it -- so
/// no claim, no authority, which is also why a free-text run cannot hold one.
pub fn authority_granted(raw: &Option<BTreeMap<String, String>>) -> bool {
    let Some(map) = raw else {
        return false;
    };
    if map
        .get("authority")
        .map(|v| v.trim().to_ascii_lowercase())
        .as_deref()
        != Some("full")
    {
        return false;
    }
    let claim_key = manifest_str(raw, "target_claim_key");
    !claim_key.is_empty() && matches!(claim_state(&claim_key), Some("live") | Some("suspect"))
}

// ── git helpers (self-contained so orient never imports target_init) ─────────

fn git_out(cwd: &Path, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

fn is_linked_worktree(cwd: &Path) -> bool {
    let (Some(gdir), Some(common)) = (
        git_out(cwd, &["rev-parse", "--git-dir"]),
        git_out(cwd, &["rev-parse", "--git-common-dir"]),
    ) else {
        return false;
    };
    let abs = |p: &str| -> PathBuf {
        let path = Path::new(p);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            cwd.join(path)
        }
    };
    abs(&gdir) != abs(&common)
}

/// The shared graph-rows read: one load, both readers. A load error
/// propagates so a caller can degrade distinctly (not-in-graph vs
/// unreadable).
fn graph_rows() -> Result<Vec<serde_json::Value>, String> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let graph_path = crate::org_board::scope::graph_json_path(&cwd);
    let store = crate::backlog::api::Store::new(&graph_path);
    crate::graph_store::read_rows_where(
        &store.graph,
        &crate::backlog::RowQuery {
            with_blockers: true,
            ..Default::default()
        },
    )
    .map_err(|e| e.to_string())
}

/// The graph entry for `node_id` by id (case-insensitive), or a read error so
/// the node line can degrade distinctly (not-in-graph vs unreadable).
fn graph_entry(node_id: &str) -> Result<Option<serde_json::Value>, String> {
    let low = node_id.to_ascii_lowercase();
    graph_rows().map(|rows| {
        rows.into_iter().find(|e| {
            e.get("id")
                .and_then(|v| v.as_str())
                .map(|id| id.to_ascii_lowercase() == low)
                .unwrap_or(false)
        })
    })
}

fn graph_find(graph: &[serde_json::Value], token: &str) -> Option<serde_json::Value> {
    let low = token.to_ascii_lowercase();
    graph
        .iter()
        .find(|e| {
            ["id", "slug"].iter().any(|k| {
                e.get(k)
                    .and_then(|v| v.as_str())
                    .map(|v| v.to_ascii_lowercase() == low)
                    .unwrap_or(false)
            })
        })
        .cloned()
}

// ── the report lines ──────────────────────────────────────────────────────────

fn node_line(node_id: Option<&str>, manifest_raw: &Option<BTreeMap<String, String>>) -> String {
    let Some(node_id) = node_id else {
        return "fresh (no node bound)".to_string();
    };
    let resolve = format!("resolve: fno backlog get {node_id}");
    let entry = match graph_entry(node_id) {
        Ok(e) => e,
        Err(exc) => return format!("unknown (graph unreadable: {exc}) | {resolve}"),
    };
    let Some(entry) = entry else {
        return format!("unknown (not in graph) | {resolve}");
    };
    let status = entry
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    // pr_number 0 reads like absence (Python truthiness).
    let pr = entry
        .get("pr_number")
        .and_then(|v| v.as_i64())
        .filter(|p| *p != 0);
    // `done` is terminal FIRST: an advisory / no-ship node is `done` without
    // a PR and must not fall through to claim/fresh.
    if status == "done" {
        let Some(pr) = pr else {
            return "done (no PR)".to_string();
        };
        if entry.get("merge_status").and_then(|v| v.as_str()) == Some("merged") {
            return format!("shipped (PR #{pr} merged)");
        }
        return format!("shipped (PR #{pr}, awaiting merge)");
    }
    if let Some(pr) = pr {
        return format!("half-done (PR #{pr})");
    }
    // In-progress: the current manifest itself holds this node's claim.
    if manifest_str(manifest_raw, "target_claim_key") == format!("node:{node_id}") {
        let holder = {
            let holder = manifest_str(manifest_raw, "target_claim_holder");
            if holder.is_empty() {
                "this session".to_string()
            } else {
                holder
            }
        };
        return format!("in-progress (claim: {holder})");
    }
    if status == "blocked" {
        return "blocked (open dependency)".to_string();
    }
    format!(
        "fresh ({})",
        if status.is_empty() { "ready" } else { &status }
    )
}

fn attended_line(manifest_raw: &Option<BTreeMap<String, String>>) -> String {
    let (state, reason) = manifest_liveness(manifest_raw);
    // A DEAD manifest means the owning session is gone: resolve to ATTENDED
    // regardless of the stale stamped value, and NAME it.
    if state == "dead" {
        return format!("true (dead manifest: {reason}; attended)");
    }
    if let Some(map) = manifest_raw {
        if let Some(attended) = map.get("attended") {
            let val = attended.trim().to_ascii_lowercase();
            let mut line = format!("{val} (manifest, live: {reason})");
            if authority_granted(manifest_raw) {
                line.push_str("; authority: full (beastmode)");
            }
            return line;
        }
    }
    // No manifest yet: resolve from the substrate -- FNO_AGENT_SELF (stamped
    // into every spawned worker) is the reliable not-an-operator signal.
    let truthy = |key: &str| {
        std::env::var(key)
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false)
    };
    if truthy("FNO_AGENT_SELF")
        || truthy("FNO_BG")
        || std::env::var("TARGET_UNATTENDED").as_deref() == Ok("1")
    {
        return "false (substrate: spawned/bg worker)".to_string();
    }
    "true (substrate: operator session)".to_string()
}

fn manifest_live_line(manifest_raw: &Option<BTreeMap<String, String>>) -> String {
    let (state, reason) = manifest_liveness(manifest_raw);
    if state == "dead" {
        return format!(
            "dead ({reason}) | archive: \
             fno do state archive --path .fno/target-state.md --type target"
        );
    }
    if state == "none" {
        return "none (no manifest)".to_string();
    }
    format!("live ({reason})")
}

fn worktree_line(project_root: &Path, node_id: Option<&str>) -> String {
    if is_linked_worktree(project_root) {
        return project_root.to_string_lossy().to_string();
    }
    let hint = node_id.unwrap_or("<node>");
    format!("on canonical main -- create with: fno do target start {hint}")
}

fn tests_line(project_root: &Path) -> String {
    let mut cmds: Vec<&str> = Vec::new();
    if project_root.join("pyproject.toml").is_file()
        || project_root.join("cli/pyproject.toml").is_file()
    {
        cmds.push("pytest");
    }
    if project_root.join("Cargo.toml").is_file() || project_root.join("crates").is_dir() {
        cmds.push("cargo test");
    }
    if project_root.join("package.json").is_file() {
        cmds.push("npm test");
    }
    if cmds.is_empty() {
        return "unknown | resolve: set your repo's test command".to_string();
    }
    cmds.join(" | ")
}

// ── the plan line: reconcile_plan's summary ──────────────────────────────────

/// Backticked token that looks like a repo path: a slash and a dotted
/// extension, no whitespace. The slash requirement excludes dotted config
/// keys and bare verbs, the common prose false positives.
fn path_token_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"`([^`\s]+/[^`\s]+\.[A-Za-z0-9_]+)`").unwrap())
}

/// Render the plan line from an ALREADY-RESOLVED binding, so the plan line
/// and boundary-reconcile answer off one read.
fn plan_line_from_bound(bound_plan: Option<&str>, project_root: &Path) -> String {
    let Some(bound) = bound_plan.filter(|b| !b.trim().is_empty()) else {
        return "none (no plan bound)".to_string();
    };
    let file_part = bound.split('#').next().unwrap_or(bound);
    let text = match std::fs::read_to_string(file_part) {
        Ok(t) => t,
        Err(exc) => return format!("unknown (plan unreadable: {exc})"),
    };
    let mut seen: Vec<String> = Vec::new();
    for cap in path_token_re().captures_iter(&text) {
        let token = cap[1].to_string();
        if !seen.contains(&token) {
            seen.push(token);
        }
    }
    let mut present = 0usize;
    let mut stale = 0usize;
    for token in &seen {
        if project_root.join(token.trim_start_matches('/')).exists() {
            present += 1;
        } else {
            stale += 1;
        }
    }
    let total = present + stale;
    if total == 0 {
        return "none (no file paths found in plan)".to_string();
    }
    let mut parts = vec![format!("{present} present")];
    if stale > 0 {
        parts.push(format!("{stale} stale-reference"));
    }
    format!("{total} paths: {}", parts.join(", "))
}

/// The plan the node is actually bound to. An empty manifest field is a
/// CARRIER miss, not an unbound node: when a node is bound, its graph entry's
/// plan_path is the truth. A graph read error propagates.
fn bound_plan_path(
    plan_path: Option<&str>,
    node_id: Option<&str>,
) -> Result<Option<String>, String> {
    if plan_path.map(|p| !p.trim().is_empty()).unwrap_or(false) || node_id.is_none() {
        return Ok(plan_path.map(str::to_string));
    }
    let entry = graph_entry(node_id.unwrap_or(""))?;
    let stored = entry
        .as_ref()
        .and_then(|e| e.get("plan_path"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    Ok(if stored.is_empty() {
        None
    } else {
        Some(stored)
    })
}

// ── boundary-reconcile (the port of fno/plan/boundary.py) ────────────────────

struct BlockerVerdict {
    blocker_id: String,
    verdict: String,
    pr_number: Option<i64>,
    completed_at: Option<String>,
    reason: Option<String>,
}

/// The plan frontmatter's `reconcile_against:` list (additive escape hatch
/// for deps not modeled as blockers). Empty on any parse failure.
fn reconcile_against(text: &str) -> Vec<String> {
    let mut in_fm = false;
    let mut value: Option<String> = None;
    for line in text.lines() {
        let line = line.trim_end();
        if line.trim() == "---" {
            if in_fm {
                break;
            }
            in_fm = true;
            continue;
        }
        if in_fm {
            if let Some(rest) = line.trim().strip_prefix("reconcile_against:") {
                value = Some(rest.trim().to_string());
            }
        }
    }
    let Some(raw) = value else {
        return Vec::new();
    };
    let raw = raw.trim();
    if raw.is_empty() {
        return Vec::new();
    }
    let inner = raw
        .strip_prefix('[')
        .and_then(|r| r.strip_suffix(']'))
        .unwrap_or(raw);
    inner
        .split(',')
        .map(|t| t.trim().trim_matches(['"', '\'']).to_string())
        .filter(|t| !t.is_empty())
        .collect()
}

fn is_marker_heading(s: &str) -> bool {
    s.starts_with('#') && s.to_lowercase().contains("landed")
}

/// True if the doc carries ANY landed-marker heading -- Step 0 has run at
/// least once, so the file mtime is a poisoned freshness proxy.
fn has_any_marker(text: &str) -> bool {
    text.lines().any(|l| is_marker_heading(l.trim()))
}

/// True if a landed-marker heading naming the blocker (id OR PR number) is
/// already in the doc. Both keys match on a TOKEN boundary.
fn marker_present(text: &str, blocker_id: &str, pr_number: Option<i64>) -> bool {
    let id_re = Regex::new(&format!(
        r"(?i)(?<![\w-]){}(?![\w-])",
        regex::escape(blocker_id)
    ))
    .ok();
    let pr_re = pr_number.map(|pr| Regex::new(&format!(r"#{pr}(?!\d)")).unwrap());
    for line in text.lines() {
        let s = line.trim();
        if !is_marker_heading(s) {
            continue;
        }
        if id_re.as_ref().map(|re| re.is_match(s)) == Some(true) {
            return true;
        }
        if let Some(re) = &pr_re {
            if re.is_match(s) {
                return true;
            }
        }
    }
    false
}

/// True when the file was edited AFTER the blocker merged. None on an
/// unparseable timestamp (the caller renders unknown).
fn fresh_after(completed_at: &str, mtime: std::time::SystemTime) -> Option<bool> {
    use chrono::TimeZone;
    let text = completed_at.trim().replace('Z', "+00:00");
    let dt = chrono::DateTime::parse_from_rfc3339(&text)
        .or_else(|_| {
            chrono::NaiveDateTime::parse_from_str(&text, "%Y-%m-%dT%H:%M:%S").map(|n| {
                chrono::FixedOffset::east_opt(0)
                    .unwrap()
                    .from_utc_datetime(&n)
            })
        })
        .or_else(|_| {
            chrono::NaiveDate::parse_from_str(&text, "%Y-%m-%d").map(|d| {
                chrono::FixedOffset::east_opt(0)
                    .unwrap()
                    .from_utc_datetime(&d.and_hms_opt(0, 0, 0).unwrap())
            })
        })
        .ok()?;
    let mtime_secs = mtime
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    Some(mtime_secs > dt.timestamp())
}

/// The file whose mtime/marker we check: the plan (`#anchor` stripped) when it
/// exists, else the node's brief when `has_brief`, else None.
fn resolve_target(node: &serde_json::Value, plan_or_brief_path: Option<&str>) -> Option<PathBuf> {
    if let Some(plan) = plan_or_brief_path.filter(|p| !p.trim().is_empty()) {
        let p = PathBuf::from(plan.split('#').next().unwrap_or(plan));
        if p.exists() {
            return Some(p);
        }
    }
    let nid = node.get("id").and_then(|v| v.as_str())?;
    let has_brief = node
        .get("has_brief")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if has_brief {
        if let Some(bp) = crate::backlog::autobrief::briefs_dir() {
            let bp = bp.join(format!("{nid}.md"));
            if bp.exists() {
                return Some(bp);
            }
        }
    }
    None
}

fn evaluate_blocker(
    entry: Option<&serde_json::Value>,
    explicit: bool,
    token: &str,
    text: &str,
    mtime: std::time::SystemTime,
    doc_markered: bool,
) -> Option<BlockerVerdict> {
    let Some(entry) = entry else {
        // An explicit `reconcile_against:` entry surfaces as unknown so a
        // mis-listed id is visible; a graph blocker not in the graph skips.
        return explicit.then(|| BlockerVerdict {
            blocker_id: token.to_string(),
            verdict: "unknown".into(),
            pr_number: None,
            completed_at: None,
            reason: Some("not in graph".into()),
        });
    };
    let bid = entry
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("(unknown)")
        .to_string();
    let status = entry
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if status != "done" {
        return explicit.then(|| BlockerVerdict {
            blocker_id: bid,
            verdict: "unknown".into(),
            pr_number: None,
            completed_at: None,
            reason: Some(format!("not done ({status})")),
        });
    }
    let pr = entry.get("pr_number").and_then(|v| v.as_i64());
    let completed = entry
        .get("completed_at")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let Some(pr) = pr else {
        // Done doc / DoneAdvisory node: no diff to read.
        return explicit.then(|| BlockerVerdict {
            blocker_id: bid,
            verdict: "unknown".into(),
            pr_number: None,
            completed_at: completed,
            reason: Some("done blocker without PR".into()),
        });
    };
    if marker_present(text, &bid, Some(pr)) {
        return Some(BlockerVerdict {
            blocker_id: bid,
            verdict: "reconciled".into(),
            pr_number: Some(pr),
            completed_at: completed,
            reason: None,
        });
    }
    let Some(completed_at) = completed.clone() else {
        return Some(BlockerVerdict {
            blocker_id: bid,
            verdict: "unknown".into(),
            pr_number: Some(pr),
            completed_at: None,
            reason: Some("no completed_at".into()),
        });
    };
    let Some(fresh) = fresh_after(&completed_at, mtime) else {
        return Some(BlockerVerdict {
            blocker_id: bid,
            verdict: "unknown".into(),
            pr_number: Some(pr),
            completed_at: completed,
            reason: Some("bad completed_at".into()),
        });
    };
    // mtime-fresh is trustworthy ONLY on an un-touched doc: once Step 0 has
    // appended any marker, that append bumped the mtime past this un-markered
    // blocker's completed_at. Bias to stale over masking.
    let verdict = if fresh && !doc_markered {
        "fresh"
    } else {
        "stale"
    };
    Some(BlockerVerdict {
        blocker_id: bid,
        verdict: verdict.into(),
        pr_number: Some(pr),
        completed_at: completed,
        reason: None,
    })
}

/// Collapse per-blocker verdicts to one line. STALE > unknown > reconciled >
/// fresh -- a single stale blocker is the actionable signal Step 0 keys on.
fn render_boundary(verdicts: &[BlockerVerdict]) -> String {
    if verdicts.is_empty() {
        // Covers both "no blockers" and "blockers all skipped as not-stale".
        return "fresh (no landed blocker to reconcile)".to_string();
    }
    let stale: Vec<&BlockerVerdict> = verdicts.iter().filter(|v| v.verdict == "stale").collect();
    let unknown: Vec<&BlockerVerdict> =
        verdicts.iter().filter(|v| v.verdict == "unknown").collect();
    let reconciled: Vec<&BlockerVerdict> = verdicts
        .iter()
        .filter(|v| v.verdict == "reconciled")
        .collect();
    if !stale.is_empty() {
        let clauses: Vec<String> = stale
            .iter()
            .map(|v| {
                let pr = match v.pr_number {
                    Some(pr) => format!("PR #{pr}"),
                    None => "no PR".to_string(),
                };
                let merged = v
                    .completed_at
                    .as_ref()
                    .map(|c| format!(", merged {}", &c[..c.len().min(10)]))
                    .unwrap_or_default();
                format!("{} ({pr}{merged})", v.blocker_id)
            })
            .collect();
        return format!("STALE vs {} - Step 0 required", clauses.join(", "));
    }
    if !unknown.is_empty() {
        let clauses: Vec<String> = unknown
            .iter()
            .map(|v| format!("{}: {}", v.blocker_id, v.reason.clone().unwrap_or_default()))
            .collect();
        return format!("unknown ({})", clauses.join("; "));
    }
    if !reconciled.is_empty() {
        let clauses: Vec<String> = reconciled
            .iter()
            .map(|v| format!("{} marker present", v.blocker_id))
            .collect();
        return format!("reconciled ({})", clauses.join(", "));
    }
    "fresh (no done blocker newer than plan)".to_string()
}

fn boundary_line(node_id: Option<&str>, plan_path: Option<&str>, _project_root: &Path) -> String {
    let Some(node_id) = node_id else {
        return "fresh (no node bound)".to_string();
    };
    let graph = match graph_rows() {
        Ok(g) => g,
        Err(exc) => return format!("unknown (graph unreadable: {exc})"),
    };
    let Some(entry) = graph_find(&graph, node_id) else {
        return "unknown (not in graph)".to_string();
    };
    let Some(target) = resolve_target(&entry, plan_path) else {
        return "fresh (no landed blocker to reconcile)".to_string();
    };
    let (text, mtime) = match (
        std::fs::read_to_string(&target),
        std::fs::metadata(&target).and_then(|m| m.modified()),
    ) {
        (Ok(text), Ok(mtime)) => (text, mtime),
        (Err(exc), _) => {
            let blocked: Vec<String> = entry
                .get("blocked_by")
                .and_then(|v| v.as_array())
                .map(|rows| {
                    rows.iter()
                        .filter_map(|b| b.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            return render_boundary(
                &blocked
                    .iter()
                    .map(|b| BlockerVerdict {
                        blocker_id: b.clone(),
                        verdict: "unknown".into(),
                        pr_number: None,
                        completed_at: None,
                        reason: Some(format!("plan unreadable: {exc}")),
                    })
                    .collect::<Vec<_>>(),
            );
        }
        (_, Err(_)) => {
            // An unreadable stat is a total read failure for this line; the
            // blocked_by rows all degrade the same way.
            let blocked: Vec<String> = entry
                .get("blocked_by")
                .and_then(|v| v.as_array())
                .map(|rows| {
                    rows.iter()
                        .filter_map(|b| b.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            return render_boundary(
                &blocked
                    .iter()
                    .map(|b| BlockerVerdict {
                        blocker_id: b.clone(),
                        verdict: "unknown".into(),
                        pr_number: None,
                        completed_at: None,
                        reason: Some("plan unreadable: stat failed".to_string()),
                    })
                    .collect::<Vec<_>>(),
            );
        }
    };
    let mut pairs: Vec<(String, bool)> = entry
        .get("blocked_by")
        .and_then(|v| v.as_array())
        .map(|rows| {
            rows.iter()
                .filter_map(|b| b.as_str().map(|s| (s.to_string(), false)))
                .collect()
        })
        .unwrap_or_default();
    let seen: Vec<String> = pairs.iter().map(|(t, _)| t.to_lowercase()).collect();
    for tok in reconcile_against(&text) {
        if !seen.contains(&tok.to_lowercase()) {
            pairs.push((tok, true));
        }
    }
    let doc_markered = has_any_marker(&text);
    let mut verdicts: Vec<BlockerVerdict> = Vec::new();
    for (token, explicit) in &pairs {
        let found = graph_find(&graph, token);
        if let Some(v) =
            evaluate_blocker(found.as_ref(), *explicit, token, &text, mtime, doc_markered)
        {
            verdicts.push(v);
        }
    }
    render_boundary(&verdicts)
}

// ── the done-when line: config.review read ────────────────────────────────────

use crate::loopcheck::settings::{parse_settings_result, PeerEntry, Settings};

/// The three-file settings chain the Python loader walks, with the same
/// missing-file note per file. The YAML settings files are noted when absent
/// but not parsed (config.toml is the native canonical surface -- `loopcheck`
/// reads nothing else), so a gate that lives only in settings.yaml is a
/// declared gap, not a silent downgrade.
fn load_review_config(project_root: &Path) -> Settings {
    let mut settings = Settings::default();
    let home = std::env::var("HOME").unwrap_or_default();
    let mut chain: Vec<PathBuf> = vec![
        project_root.join(".fno").join("config.toml"),
        project_root.join(".fno").join("settings.yaml"),
    ];
    if !home.is_empty() {
        chain.push(PathBuf::from(&home).join(".fno").join("settings.yaml"));
    }
    for path in &chain {
        match std::fs::read_to_string(path) {
            Err(_) => {
                eprintln!(
                    "config file at {} is missing; using defaults",
                    path.display()
                );
            }
            Ok(content) => {
                if path.extension().map(|e| e == "toml") == Some(true) {
                    match parse_settings_result(&content) {
                        Ok(parsed) => settings = parsed,
                        Err(e) => {
                            eprintln!(
                                "config file at {} failed to parse: {e}; using defaults",
                                path.display()
                            );
                        }
                    }
                }
            }
        }
    }
    settings
}

/// The must-have-reviewed login list: github_apps (or its legacy alias) UNION
/// the posting identity of every identity-backed peer. The orient line does
/// not carry loop-check's same-model sentinel: it narrows its claim with the
/// per-login producer clause instead.
fn required_bots(settings: &Settings) -> Vec<String> {
    let mut logins: Vec<String> = settings
        .github_apps
        .as_ref()
        .or(settings.required_bots.as_ref())
        .cloned()
        .unwrap_or_default();
    for peer in &settings.peers {
        let id = peer
            .identity
            .clone()
            .or_else(|| settings.peer_identity.clone());
        if let Some(id) = id {
            if !logins.iter().any(|l| l == &id) {
                logins.push(id);
            }
        }
    }
    logins
}

/// The honored-if-present reviewer logins: unset resolves to the built-in
/// default, an explicit `[]` is a real opt-out, a non-empty list EXTENDS the
/// default.
fn optional_bots(settings: &Settings) -> Vec<String> {
    const DEFAULT_OPTIONAL_APPS: &[&str] = &["chatgpt-codex-connector"];
    match &settings.optional_apps {
        None => DEFAULT_OPTIONAL_APPS
            .iter()
            .map(|s| s.to_string())
            .collect(),
        Some(list) if list.is_empty() => Vec::new(),
        Some(list) => {
            let mut resolved: Vec<String> = DEFAULT_OPTIONAL_APPS
                .iter()
                .map(|s| s.to_string())
                .collect();
            for login in list {
                if !resolved.iter().any(|r| r == login) {
                    resolved.push(login.clone());
                }
            }
            resolved
        }
    }
}

fn peer_provider(peer: &PeerEntry) -> String {
    peer.provider.trim().to_string()
}

fn peer_identity(peer: &PeerEntry, shared: Option<&String>) -> Option<String> {
    peer.identity
        .clone()
        .filter(|s| !s.is_empty())
        .or_else(|| shared.cloned())
        .filter(|s| !s.is_empty())
}

/// Local-attestation gates: `config.review.reviewers` name their producer
/// directly, and identity-free peers collapse into one composite `peer`
/// requirement. No GitHub reviewer ever posts either -- a head-pinned
/// attestation is the only evidence, which is why the producer travels with
/// the gate.
fn local_review_gates(settings: &Settings) -> Vec<String> {
    let mut gates: Vec<String> = Vec::new();
    for name in &settings.reviewers {
        match crate::loopcheck::self_review_floor::reviewer_entry(name) {
            None => gates.push(name.clone()),
            Some((inv, self_cert, _)) => {
                let mut producer = inv.to_string();
                if self_cert {
                    producer.push_str(" [self-cert: asserts no review evidence]");
                }
                gates.push(format!("{name} -> {producer}"));
            }
        }
    }
    let named: Vec<String> = settings
        .peers
        .iter()
        .filter(|p| peer_identity(p, settings.peer_identity.as_ref()).is_none())
        .map(peer_provider)
        .filter(|p| !p.is_empty())
        .collect();
    if !named.is_empty() {
        let mut unique: Vec<String> = Vec::new();
        for p in named {
            if !unique.contains(&p) {
                unique.push(p);
            }
        }
        let runnable: Vec<String> = unique
            .iter()
            .filter(|p| {
                crate::review_policy::DISPATCHABLE_PROVIDERS.contains(&p.to_lowercase().as_str())
            })
            .cloned()
            .collect();
        if runnable.len() == 1 {
            gates.push(format!(
                "peer -> /fno:review peer {} --attest (cross-model only)",
                runnable[0]
            ));
        } else if !runnable.is_empty() {
            gates.push(format!(
                "peer -> /fno:review peer <provider> --attest (cross-model only, configured: {})",
                runnable.join(", ")
            ));
        } else {
            gates.push(format!(
                "peer -> no /fno:review peer runner for [{}]",
                unique.join(", ")
            ));
        }
    }
    gates
}

/// The inline self-review verb and refusal remedy. The lane is
/// `/fno:review` on a slash surface and `$fno:review` on codex; an unknown
/// harness gets the claude spelling (advisory text, never a gate). `None`
/// from the diff sizing leaves the `<level>` placeholder in place.
fn self_review_clause(project_root: &Path) -> String {
    let harness = crate::claims::resolve_harness().unwrap_or_default();
    let spelled = if harness == "codex" {
        "$fno:review"
    } else {
        "/fno:review"
    };
    let level = crate::review_level::diff_review_level(project_root);
    let level = level.unwrap_or("<level>");
    let harness = if harness.is_empty() {
        "unknown"
    } else {
        harness.as_str()
    };
    format!(
        "self-review required for code ({harness}): run `{spelled} {level} \
         --comment` (the lane emits its own attestation; \
         `bash skills/review/scripts/emit-attestation.sh code-review` is the \
         recovery path when emission fails); on refusal, report the literal \
         refusal and stop"
    )
}

fn done_when_line(manifest_raw: &Option<BTreeMap<String, String>>, project_root: &Path) -> String {
    let is = |key: &str, want: &str| manifest_str(manifest_raw, key).to_ascii_lowercase() == want;
    if is("no_ship", "true") || is("advisory", "true") {
        return "advisory: written + eval-green (no PR)".to_string();
    }
    // ONE settings load for the whole line; a missing file names itself.
    let settings = load_review_config(project_root);
    let mut bots = required_bots(&settings);
    let mut optional = optional_bots(&settings);
    let local = local_review_gates(&settings);
    // no_external skips loop-check's GitHub-login reads entirely: announcing
    // those gates anyway sends the session off to satisfy reviews nothing is
    // waiting on. Local attestations are unaffected.
    if is("no_external", "true") {
        bots = Vec::new();
        optional = Vec::new();
    }
    let bots_str = if !bots.is_empty() {
        bots.join(", ")
    } else if is("no_external", "true") {
        "none (--no-external skips every login gate)".to_string()
    } else if !local.is_empty() {
        "no App bot".to_string()
    } else {
        "none (PR + CI only)".to_string()
    };
    let mut line = format!("PR + CI green + reviewed by [{bots_str}]");
    if !local.is_empty() {
        line.push_str(&format!(" + local attestation [{}]", local.join("; ")));
    }
    if !optional.is_empty() {
        line.push_str(&format!(
            " (optional if present: [{}])",
            optional.join(", ")
        ));
    }
    // Self-review floor: when the obligation is on and no local lane is
    // configured, name this harness's verb so a session serves itself.
    if local.is_empty() && settings.self_review_required.unwrap_or(true) {
        let clause = self_review_clause(project_root);
        if !clause.is_empty() {
            line.push_str(&format!("; {clause}"));
        }
    }
    if is("attended", "false") {
        line.push_str("; bg -> hand off the merge");
    }
    line
}

// ── assembly + render ─────────────────────────────────────────────────────────

/// Resolve all eight orientation lines. Read-only; never raises.
pub fn report(project_root: &Path) -> String {
    let manifest_raw = read_manifest(project_root);
    let node_id = {
        let nid = manifest_str(&manifest_raw, "graph_node_id");
        (!nid.is_empty() && nid != "null").then_some(nid)
    };
    let plan_path = {
        let pp = manifest_str(&manifest_raw, "plan_path");
        let pp = pp.trim_matches(['"', '\'']).to_string();
        (!pp.is_empty() && pp != "null").then_some(pp)
    };
    // boundary-reconcile reads the SAME binding the plan line does; a graph
    // read error degrades to the constant the Python twin renders.
    let (bound_plan, plan_text) = match bound_plan_path(plan_path.as_deref(), node_id.as_deref()) {
        Ok(Some(bound)) => {
            let text = plan_line_from_bound(Some(&bound), project_root);
            (Some(bound), text)
        }
        Ok(None) => (None, "none (no plan bound)".to_string()),
        Err(_) => (
            plan_path.clone(),
            "unknown (plan unreadable: graph unreadable)".to_string(),
        ),
    };
    let lines: Vec<(&str, String)> = vec![
        ("node", node_line(node_id.as_deref(), &manifest_raw)),
        ("attended", attended_line(&manifest_raw)),
        ("worktree", worktree_line(project_root, node_id.as_deref())),
        ("tests", tests_line(project_root)),
        ("plan", plan_text),
        (
            "boundary-reconcile",
            boundary_line(node_id.as_deref(), bound_plan.as_deref(), project_root),
        ),
        ("manifest-live", manifest_live_line(&manifest_raw)),
        ("done-when", done_when_line(&manifest_raw, project_root)),
    ];
    let width = lines.iter().map(|(l, _)| l.len()).max().unwrap_or(0) + 2;
    lines
        .iter()
        .map(|(label, value)| format!("{:<width$} {}", format!("{label}:"), value))
        .collect::<Vec<_>>()
        .join("\n")
}
