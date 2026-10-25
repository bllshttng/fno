//! Land classified candidates by mode.
//!
//! Autonomous runs create nodes active: the loop cannot block on a human
//! ack. Interactive runs create them already queued in the same mutation,
//! so a human acks them with `fno backlog pick` and no create-ok/queue-fail
//! window exists. A low or nit candidate (inbox tier) becomes one inbox line
//! instead. An absent or unreadable mode reads as interactive, the safe side.
//! Nodes are scoped to the PR's canonical repo root, never the run cwd.

use std::collections::BTreeSet;

use serde_json::{json, Value};

use super::classify::severity_to_difficulty;
use super::dedup::{anchor_verdicts, trailer};
use super::reconcile_findings::ScanFn;
use super::{take_chars, Candidate, TIER_INBOX};

pub const MODE_AUTONOMOUS: &str = "autonomous";
pub const MODE_INTERACTIVE: &str = "interactive";

pub const OUTCOME_ACTIVE: &str = "active";
pub const OUTCOME_QUEUED: &str = "queued";
pub const OUTCOME_INBOX: &str = "inbox";
pub const OUTCOME_FAILED: &str = "failed";
pub const OUTCOME_SKIPPED: &str = "skipped";

#[derive(Clone, Debug, PartialEq)]
pub struct LandResult {
    pub outcome: String,
    pub candidate: Candidate,
    pub node_id: Option<String>,
    pub error: Option<String>,
    /// The skip reason, e.g. `fixed-on-main`.
    pub reason: Option<String>,
}

impl LandResult {
    fn new(outcome: &str, candidate: &Candidate) -> Self {
        LandResult {
            outcome: outcome.to_string(),
            candidate: candidate.clone(),
            node_id: None,
            error: None,
            reason: None,
        }
    }
}

/// The landing mode a trigger payload asks for; interactive by default.
pub fn resolve_mode(sentinel: Option<&Value>) -> &'static str {
    let mode = sentinel
        .and_then(|s| s.get("mode"))
        .map(|m| super::py_str(Some(m)).to_lowercase())
        .unwrap_or_default();
    if mode == MODE_AUTONOMOUS {
        MODE_AUTONOMOUS
    } else {
        MODE_INTERACTIVE
    }
}

/// One node a land asks the create seam to mint.
#[derive(Clone, Debug, Default)]
pub struct CreateRequest {
    pub title: String,
    pub details: String,
    pub priority: String,
    pub difficulty: String,
    pub project: Option<String>,
    pub cwd: Option<String>,
    pub domain: String,
    pub queued: bool,
    pub caused_by: Option<String>,
}

pub type CreateFn = dyn Fn(&CreateRequest) -> Result<String, String>;
pub type InboxFn = dyn Fn(&Candidate) -> Result<(), String>;

/// Mint a retro node through the locked graph write. A queued node carries
/// `queued_at` from birth. `caused_by` lands only when that node exists, so
/// a stale sentinel id never dangles.
pub fn default_create(req: &CreateRequest) -> Result<String, String> {
    if !["p0", "p1", "p2", "p3"].contains(&req.priority.as_str()) {
        return Err(format!(
            "invalid priority '{}'. Must be: p0, p1, p2, p3",
            req.priority
        ));
    }
    let graph = crate::backlog::settings::graph_path();
    let here = std::env::current_dir().unwrap_or_default();
    let mut minted = String::new();
    let mut born = Value::Null;
    crate::graph_store::mutate_rows(
        &graph,
        crate::graph_store::DEFAULT_LOCK_TIMEOUT,
        None,
        None,
        |rows| {
            let known: BTreeSet<String> = rows
                .iter()
                .filter_map(|r| r.get("id").and_then(Value::as_str))
                .map(str::to_string)
                .collect();
            let id = crate::backlog::create_cli::mint_node_id(&known)
                .map_err(crate::graph_store::StoreError::Invalid)?;
            let caused = req.caused_by.clone().filter(|c| known.contains(c.as_str()));
            let (origin, evidence, _) = crate::backlog::create_cli::stamp_request_origin(
                "organic",
                "retro_land",
                caused.as_deref(),
            );
            let (sid, harness, scwd, snode, splan, _dropped) =
                crate::backlog::create_cli::session_provenance(&here, None, Some(&known));
            let now = crate::graph_store::now_isoformat();
            let mut node = json!({
                "id": id,
                "parent": null,
                "tags": [],
                "title": req.title,
                "type": "feature",
                "project": req.project,
                "cwd": req.cwd,
                "status": "idea",
                "priority": req.priority,
                "blocks_everything": false,
                "difficulty": req.difficulty,
                "difficulty_history": [{"value": req.difficulty, "source": "retro", "ts": now}],
                "domain": req.domain,
                "blocked_by": [],
                "session_id": null,
                "locked_at": null,
                "completed_at": null,
                "has_brief": false,
                "roadmap_id": null,
                "vision_path": null,
                "details": req.details,
                "size": null,
                "batch": null,
                "cost_usd": null,
                "cost_sessions": [],
                "plan_path": null,
                "pr_number": null,
                "pr_url": null,
                "merge_status": null,
                "created_at": now,
                "source": null,
                "source_kind": "organic",
                "source_project": null,
                "source_inbox_msg": null,
                "artifact_url": null,
                "completion_note": null,
                "source_session_id": sid,
                "source_harness": harness,
                "source_cwd": scwd,
                "source_node_id": snode,
                "source_plan_path": splan,
                "request_origin": origin,
                "origin_evidence": evidence,
            });
            if let Some(obj) = node.as_object_mut() {
                if req.queued {
                    obj.insert("queued_at".into(), json!(now));
                    obj.insert(
                        "queued_reason".into(),
                        json!("retro-triage (interactive): awaiting human ack"),
                    );
                }
                if let Some(c) = &caused {
                    obj.insert("caused_by".into(), json!(c));
                }
            }
            minted = id;
            born = node.clone();
            rows.push(node);
            Ok(true)
        },
    )
    .map_err(|e| e.to_string())?;
    // Born-with-why: gate-first and strictly non-fatal.
    let _ = crate::backlog::birth::on_node_born(&graph, &born, None);
    Ok(minted)
}

/// Append a low or nit finding as one inbox line through the locked writer.
pub fn default_inbox(c: &Candidate) -> Result<(), String> {
    let raw = if !c.finding_text.is_empty() {
        &c.finding_text
    } else if !c.body.is_empty() {
        &c.body
    } else {
        &c.title
    };
    let mut why = raw.trim().replace('\n', " ");
    if super::char_len(&why) > 120 {
        why = format!("{}…", take_chars(&why, 119).trim_end());
    }
    let source = c
        .source_pr
        .map_or_else(|| "retro-triage".to_string(), |n| format!("PR #{n}"));
    crate::backlog::capture::add_inbox_item(&c.title, &source, &why, &c.priority)
}

/// The seams and scope of one land pass.
pub struct LandOptions<'a> {
    pub mode: &'a str,
    pub project: Option<String>,
    pub cwd: Option<String>,
    pub domain: String,
    pub caused_by: Option<String>,
    pub create: &'a CreateFn,
    pub inbox: &'a InboxFn,
    pub anchor_scan: Option<&'a ScanFn>,
    pub dedup_entries: Option<&'a [Value]>,
}

impl<'a> LandOptions<'a> {
    /// The real seams for `mode`: live graph create and inbox writer, the
    /// live anchor scan, no dedup roster.
    pub fn live(mode: &'a str, project: Option<String>, cwd: Option<String>) -> Self {
        LandOptions {
            mode,
            project,
            cwd,
            domain: "code".to_string(),
            caused_by: None,
            create: &default_create,
            inbox: &default_inbox,
            anchor_scan: Some(&super::reconcile_findings::live_scan),
            dedup_entries: None,
        }
    }
}

/// Land each candidate by mode and tier. A per-candidate failure is
/// recorded, never raised, so partial progress persists and a rerun dedups
/// what landed.
pub fn land_candidates(candidates: &[Candidate], opts: &LandOptions) -> Vec<LandResult> {
    let interactive = opts.mode == MODE_INTERACTIVE;
    // One batched anchor scan per harvest, never one per candidate.
    let verdicts = opts
        .anchor_scan
        .map(|scan| anchor_verdicts(candidates, scan))
        .unwrap_or_default();
    let mut results = Vec::new();
    for c in candidates {
        if c.uncited {
            continue;
        }
        if c.tier == TIER_INBOX {
            results.push(match (opts.inbox)(c) {
                Ok(()) => LandResult::new(OUTCOME_INBOX, c),
                Err(e) => LandResult {
                    error: Some(e),
                    ..LandResult::new(OUTCOME_FAILED, c)
                },
            });
            continue;
        }
        let mut details = format!("{}\n\n{}", c.body, trailer(c.source_pr, &c.content_hash));
        // A finding already addressed on its source PR is never minted. An
        // unresolvable scan files anyway, with a note.
        if opts.anchor_scan.is_some() {
            match verdicts.get(&c.source_id).map(String::as_str) {
                Some("dead") => {
                    eprintln!(
                        "skipped: fixed-on-main - finding addressed on source PR #{} ({})",
                        super::dedup::pr_label(c.source_pr),
                        take_chars(&c.title, 60)
                    );
                    results.push(LandResult {
                        reason: Some("fixed-on-main".to_string()),
                        ..LandResult::new(OUTCOME_SKIPPED, c)
                    });
                    continue;
                }
                Some("unresolvable") => details.push_str(
                    "\n\n[anchor-unverified: filing-time anchor check could not resolve the anchor]",
                ),
                _ => {}
            }
        }
        let req = CreateRequest {
            title: c.title.clone(),
            details,
            priority: c.priority.clone(),
            difficulty: severity_to_difficulty(c.extra_str("severity")),
            project: opts.project.clone(),
            cwd: opts.cwd.clone(),
            domain: opts.domain.clone(),
            queued: interactive,
            caused_by: opts.caused_by.clone(),
        };
        let node_id = match (opts.create)(&req) {
            Ok(id) => id,
            Err(e) => {
                results.push(LandResult {
                    error: Some(e),
                    ..LandResult::new(OUTCOME_FAILED, c)
                });
                continue;
            }
        };
        // The filing-time dedup net every birth path shares (warn-only).
        if let Some(entries) = opts.dedup_entries.filter(|e| !e.is_empty()) {
            let stub =
                json!({"id": node_id, "title": c.title, "details": c.body, "domain": opts.domain});
            crate::backlog::create_cli::warn_similar_nodes(&stub, entries, &node_id);
        }
        results.push(LandResult {
            node_id: Some(node_id),
            ..LandResult::new(
                if interactive {
                    OUTCOME_QUEUED
                } else {
                    OUTCOME_ACTIVE
                },
                c,
            )
        });
    }
    results
}

pub fn has_failures(results: &[LandResult]) -> bool {
    results.iter().any(|r| r.outcome == OUTCOME_FAILED)
}
