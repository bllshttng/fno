//! The merge gates that used to live in the Python merge command, ported so
//! `authorized_merge::decide` is the one merge decision. Each gate
//! evaluates one input and answers `Option<Blocker>`: None clears. The pure
//! halves take their facts as arguments so unit tests need no filesystem or
//! network; the fetching halves ride the caller's `Probes` handle.

use crate::authorized_merge::{
    node_carries_tag, pr_bound_entry, Blocker, PrFacts, ProbeOutcome, Probes, MAIN_REPAIR_TAG,
};
use crate::loopcheck::coverage_gate::{refusal_line, GateState};
use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The coverage gate, polarity kept. The native verdict is the one
/// computation: it applies the operator waiver overlay inside itself, so a
/// waived PR reads Covered with the `override: ` note, and the waiver
/// travels with the gate's answer instead of a caller re-deriving it.
/// Returns (blocker, waiver).
pub(crate) fn coverage_gate<P: Probes>(
    probes: &P,
    cwd: &Path,
    pr: u64,
) -> (Option<Blocker>, Option<String>) {
    let v = probes.coverage_verdict(cwd, pr);
    match v.state {
        GateState::Covered => {
            // The prefix literal matches Python's OVERRIDE_NOTE_PREFIX.
            (None, v.note.strip_prefix("override: ").map(str::to_string))
        }
        GateState::Refused => {
            let line = refusal_line(&v.refusal, &v.note);
            (
                Some(Blocker::held(
                    "review_coverage_uncovered",
                    if line.is_empty() {
                        "unreviewed merge refused".to_string()
                    } else {
                        format!("unreviewed merge refused: {line}")
                    },
                )),
                None,
            )
        }
        GateState::Unanswered => (
            Some(Blocker::unknown(
                "review_coverage_unknown",
                if v.note.is_empty() {
                    "coverage probe failed, merge refused".to_string()
                } else {
                    format!("coverage probe failed, merge refused: {}", v.note)
                },
            )),
            None,
        ),
    }
}

/// The posted-review gate. Fires only when an attestation already exists on
/// `facts.head_ref` (the no-attestation gap is `coverage_gate`'s job, via
/// `review_coverage_uncovered` - the two blockers never name the same gap)
/// but no GitHub review yet carries that round's marker. Reads the newest
/// BRANCH attestation, not an exact-head one: a round law d-608344c1 still
/// carries forward after a rebase (`review_freshness::Freshness::counts()`)
/// never gets a fresh attestation event at the new head, so an exact-head
/// lookup would go silent on exactly the rounds this gate exists to catch.
/// `publish_review::publish_round` resolves the SAME newest-branch round
/// when it posts, so the two stay pinned to one definition of "the round
/// that needs posting". `Held`, never `Refused`: `fno do pr publish-review`
/// already runs automatically at ship-create time and at every `finalize`
/// terminal decision (`crates/fno-agents/src/finalize.rs`), so this normally
/// self-clears with no operator action - it never re-derives what counts as
/// "reviewed" (law d-608344c1 owns that), it only checks that a round the
/// ledger already counted got PUBLISHED.
pub(crate) fn review_posted_gate<P: Probes>(
    probes: &P,
    cwd: &Path,
    facts: &PrFacts,
) -> Option<Blocker> {
    let journals = [crate::paths::events_path(cwd)];
    let Some(attestation) =
        crate::publish_review::newest_branch_attestation(&journals, &facts.head_ref)
    else {
        return None;
    };
    let reviewed_head = attestation
        .get("head_sha")
        .and_then(Value::as_str)
        .unwrap_or(facts.head_sha.as_str());
    match probes.review_posted(cwd, facts, reviewed_head) {
        ProbeOutcome::Clear => None,
        ProbeOutcome::Refused(reason) => Some(Blocker::held("review_not_posted", reason)),
        ProbeOutcome::Inconclusive(reason) => {
            Some(Blocker::unknown("review_posted_unknown", reason))
        }
    }
}

/// `<root>/.fno/stub-manifest-<node>.json` (stub_manifest.py's own layout).
fn stub_manifest_path(root: &Path, node_id: &str) -> PathBuf {
    root.join(".fno")
        .join(format!("stub-manifest-{node_id}.json"))
}

/// The node-side half of the stub-manifest gate, pure over graph entries: the
/// `dep=contract` node this PR closes, or None. The graph read degrades to
/// None (the default hard merge path), exactly as the Python `dep` did.
pub(crate) fn contract_node_for_pr(entries: &[Value], pr: u64) -> Option<String> {
    for entry in entries {
        let dep = entry.get("dep").and_then(Value::as_str);
        if dep != Some("contract") {
            continue;
        }
        let numbers = pr_numbers_of(entry);
        if numbers.contains(&pr) {
            return entry.get("id").and_then(Value::as_str).map(str::to_owned);
        }
    }
    None
}

/// Every PR number a graph row carries: the persisted `pr_number` field plus
/// the `additional_prs` entries. A contract dependent whose PR is recorded
/// only in `additional_prs` must still be found or the gate is bypassed.
fn pr_numbers_of(entry: &Value) -> Vec<u64> {
    pr_refs_of(entry).into_iter().map(|r| r.number).collect()
}

/// One PR a graph row records, with the URL it names when it names one.
pub(crate) struct PrRef {
    pub(crate) number: u64,
    pub(crate) url: Option<String>,
    pub(crate) primary: bool,
}

/// Every PR ref a graph row carries: `pr_number` with its `pr_url`, then the
/// `additional_prs` entries (typed rows carry `{number}` or `{url}` objects;
/// legacy rows carry bare ints or `/pull/<n>` URL strings).
pub(crate) fn pr_refs_of(entry: &Value) -> Vec<PrRef> {
    let mut out = Vec::new();
    let text = |v: Option<&Value>| v.and_then(Value::as_str).map(str::to_owned);
    if let Some(n) = entry.get("pr_number").and_then(Value::as_u64) {
        out.push(PrRef {
            number: n,
            url: text(entry.get("pr_url")),
            primary: true,
        });
    }
    if let Some(list) = entry.get("additional_prs").and_then(Value::as_array) {
        for raw in list {
            let (number, url) = match raw {
                Value::Number(n) => (n.as_u64(), None),
                Value::Object(o) => {
                    let url = text(o.get("url"));
                    let number = o.get("number").and_then(Value::as_u64);
                    (
                        number.or_else(|| url.as_deref().and_then(pull_number_from_url)),
                        url,
                    )
                }
                Value::String(url) => (pull_number_from_url(url), Some(url.clone())),
                _ => (None, None),
            };
            if let Some(number) = number {
                out.push(PrRef {
                    number,
                    url,
                    primary: false,
                });
            }
        }
    }
    out
}

/// The PR number a `/pull/<n>` URL names, if it names one.
fn pull_number_from_url(url: &str) -> Option<u64> {
    let rest = url.split("/pull/").nth(1)?;
    rest.split('/').next()?.parse::<u64>().ok()
}

/// The stub-manifest gate, pure over an already-read manifest value: a
/// contract dependent whose manifest is not `reconciled: true` holds the
/// merge (mocks would ship). An unreadable manifest of a known contract node
/// fails CLOSED, as the Python did.
pub(crate) fn stub_manifest_blocker(root: &Path, node_id: &str) -> Option<Blocker> {
    let path = stub_manifest_path(root, node_id);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            return Some(Blocker::held(
                "stub_manifest_unreconciled",
                format!(
                    "contract dependent {node_id} carries a malformed stub-manifest \
                     (cannot prove stubs are gone): {e}; reconcile before merge"
                ),
            ))
        }
    };
    let manifest: Value = match serde_json::from_str(&raw) {
        Ok(value) => value,
        Err(e) => {
            return Some(Blocker::held(
                "stub_manifest_unreconciled",
                format!(
                    "contract dependent {node_id} carries a malformed stub-manifest \
                     (cannot prove stubs are gone): {e}; reconcile before merge"
                ),
            ))
        }
    };
    if manifest.get("reconciled").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let stubs = manifest
        .get("stubs")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    Some(Blocker::held(
        "stub_manifest_unreconciled",
        format!(
            "contract dependent {node_id} carries a unreconciled stub-manifest \
             ({stubs} stub(s)); reconcile before merge"
        ),
    ))
}

/// The whole stub-manifest gate over graph entries. An unreadable store
/// degrades to clear (never block a normal merge on our own read).
pub(crate) fn stub_manifest_gate(root: &Path, entries: &[Value], pr: u64) -> Option<Blocker> {
    contract_node_for_pr(entries, pr).and_then(|node| stub_manifest_blocker(root, &node))
}

/// Whether a path is documentation (loopcheck's own classifier; one copy).
fn is_documentation_path(path: &str) -> bool {
    crate::loopcheck::is_documentation_path(path)
}

/// The plan-fidelity gate: a PR bound to a plan whose declared deliverables
/// did not all ship refuses, unless the PR payload is documentation-only.
/// Fail-open on a degraded probe (the Python polarity), fail-refused only
/// when the fidelity verdict itself says so.
pub(crate) fn plan_fidelity_blocker<P: Probes>(probes: &P, cwd: &Path, pr: u64) -> Option<Blocker> {
    let plan_path = ledger_plan_path(cwd, pr)?;
    let plan_path = plan_path.trim();
    if plan_path.is_empty() {
        return None;
    }
    if !pr_payload_is_code(probes, cwd, pr) {
        return None;
    }
    let args = vec![
        "plan".to_string(),
        "fidelity".to_string(),
        plan_path.to_string(),
        "--json".to_string(),
    ];
    match probes.fno_shell(cwd, &args) {
        Ok((Some(0), stdout, _)) => {
            let verdict: Value = serde_json::from_slice(&stdout).ok()?;
            if verdict.get("refused").and_then(Value::as_bool) == Some(true) {
                let reason = verdict
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("uncovered shortfall");
                Some(Blocker::refused(
                    "plan_fidelity_refused",
                    format!("plan fidelity refused: {reason}"),
                ))
            } else {
                None
            }
        }
        Ok(_) => {
            breadcrumb("plan fidelity probe answered nonzero; proceeding (fail-open)");
            None
        }
        Err(error) => {
            breadcrumb(&format!(
                "plan fidelity probe unavailable ({error}); proceeding (fail-open)"
            ));
            None
        }
    }
}

/// The plan_path bound to this PR's delivery row in the ledger, or None.
/// PR numbers are per-repo and the ledger is global, so when several rows
/// match, the one whose pr_url names this checkout's remote wins; otherwise
/// the first match answers (fail-open, as the Python did). A missing or
/// broken ledger is None: no signal, no gate.
fn ledger_plan_path(cwd: &Path, pr: u64) -> Option<String> {
    use std::process::Command;

    // Read-only: the ledger is global (the Python writer appends to the
    // same file), so an undeclared test process answers None, the same
    // no-signal shape as a missing ledger, instead of reading the
    // operator's real history.
    if cfg!(test) && !crate::paths::test_root_declared() {
        return None;
    }
    let rows = crate::loopcheck::ledger_file_rows(&crate::paths_cli::ledger_json(cwd));
    if rows.is_empty() {
        return None;
    }
    let origin = Command::new("git")
        .args(["config", "--get", "remote.origin.url"])
        .current_dir(cwd)
        .output()
        .ok()
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .unwrap_or_default();
    // The pr_url names `owner/repo` inside its path; the remote url names the
    // same pair in ssh, https, and .git-suffixed spellings. Compare the
    // normalized pair, never the raw url: a substring test on the raw form
    // never matches and every scoping election falls to first-match.
    let slug = repo_slug_from_origin(&origin).unwrap_or_default();
    let matches: Vec<&Value> = rows
        .iter()
        .filter(|row| row.get("pr_number").and_then(Value::as_u64) == Some(pr))
        .collect();
    let picked = matches
        .iter()
        .copied()
        .find(|row| {
            !slug.is_empty()
                && row
                    .get("pr_url")
                    .and_then(Value::as_str)
                    .is_some_and(|url| url.contains(&slug))
        })
        .or_else(|| matches.first().copied());
    picked.and_then(|row| {
        row.get("plan_path")
            .and_then(Value::as_str)
            .map(str::to_owned)
    })
}

/// The PR carries a code payload (at least one non-documentation changed
/// file). A read miss answers false, which skips the gate (fail-open: the
/// fidelity guard never wedges a merge because gh hiccuped).
fn pr_payload_is_code<P: Probes>(probes: &P, cwd: &Path, pr: u64) -> bool {
    let args = vec![
        "pr".to_string(),
        "view".to_string(),
        pr.to_string(),
        "--json".to_string(),
        "files".to_string(),
    ];
    let Ok((true, stdout)) = probes.run_gh(cwd, &args) else {
        return false;
    };
    let Ok(payload) = serde_json::from_str::<Value>(&stdout) else {
        return false;
    };
    let Some(files) = payload.get("files").and_then(Value::as_array) else {
        return false;
    };
    files.iter().any(|file| {
        file.get("path")
            .or_else(|| file.get("filename"))
            .and_then(Value::as_str)
            .is_some_and(|path| !is_documentation_path(path))
    })
}

fn breadcrumb(message: &str) {
    let mut err = std::io::stderr();
    let _ = writeln!(err, "pr-merge: {message}");
}

/// The overlap gate (parallel-mode G4, LD#9), polarity kept: only arms while
/// live lanes run; `_behind_by` miss disarms; base-move and PR-file misses
/// HOLD (fail closed) because the base already moved. Its remedy pulls main
/// into a branch GitHub calls mergeable, so it rides `require_fresh_ci`
/// (default off) with `ci_base_stale`: by default only a conflict pulls main.
pub(crate) fn overlap_blocker<P: Probes>(
    probes: &P,
    cwd: &Path,
    facts_number: u64,
) -> Option<Blocker> {
    if !probes.require_fresh_ci(cwd) || probes.live_lanes(cwd) == 0 {
        return None;
    }
    let behind = behind_by(probes, cwd, facts_number);
    if behind == 0 {
        return None;
    }
    // The probes already know the base moved, so a read that fails holds:
    // an under-reported move must never read as a clear. This is the Python
    // miss contract, kept verbatim.
    let base_paths = match base_move_paths(probes, cwd, facts_number) {
        Some(paths) => paths,
        None => {
            return Some(Blocker::held(
                "base_overlap",
                "stale base: overlap probe unavailable (base moved; could not \
                 compare file sets); run fno do pr push, then retry",
            ));
        }
    };
    let pr_paths = match pr_file_paths(probes, cwd, facts_number) {
        Some(paths) => paths,
        None => {
            return Some(Blocker::held(
                "base_overlap",
                "stale base: overlap probe unavailable (the PR's own file \
                 read failed); run fno do pr push, then retry",
            ));
        }
    };
    let overlap = overlaps(&base_paths, &pr_paths);
    if overlap.is_empty() {
        return None;
    }
    let shown = overlap
        .iter()
        .take(3)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    let extra = if overlap.len() > 3 {
        format!(" and {} more", overlap.len() - 3)
    } else {
        String::new()
    };
    Some(Blocker::held(
        "base_overlap",
        format!(
            "stale base: base move touches files this PR also changes ({shown}{extra}); \
             run fno do pr push, then retry"
        ),
    ))
}

/// (base, head) ref names for a PR, or None on any read miss.
pub(crate) fn pr_base_head_refs<P: Probes>(
    probes: &P,
    cwd: &Path,
    pr: u64,
) -> Option<(String, String)> {
    let args = vec![
        "pr".to_string(),
        "view".to_string(),
        pr.to_string(),
        "--json".to_string(),
        "baseRefName,headRefName".to_string(),
    ];
    let (ok, stdout) = probes.run_gh(cwd, &args).ok()?;
    if !ok {
        return None;
    }
    let refs: Value = serde_json::from_str(&stdout).ok()?;
    let base = refs.get("baseRefName").and_then(Value::as_str)?;
    let head = refs.get("headRefName").and_then(Value::as_str)?;
    if base.is_empty() || head.is_empty() {
        return None;
    }
    Some((base.to_string(), head.to_string()))
}

/// Commits the PR head is behind its base. 0 on any probe miss (never block
/// a merge because our own read failed).
fn behind_by<P: Probes>(probes: &P, cwd: &Path, pr: u64) -> u64 {
    let Some((base, head)) = pr_base_head_refs(probes, cwd, pr) else {
        breadcrumb(
            "stale-base probe unavailable (pr refs unreadable); merging without freshness hold",
        );
        return 0;
    };
    let args = vec![
        "api".to_string(),
        format!("repos/{{owner}}/{{repo}}/compare/{base}...{head}"),
        "-q".to_string(),
        ".behind_by".to_string(),
    ];
    match probes.run_gh(cwd, &args) {
        Ok((true, stdout)) => stdout.trim().parse::<u64>().unwrap_or_else(|_| {
            breadcrumb(
                "stale-base probe unavailable (gh compare failed); merging without freshness hold",
            );
            0
        }),
        _ => {
            breadcrumb(
                "stale-base probe unavailable (gh compare failed); merging without freshness hold",
            );
            0
        }
    }
}

/// Files the BASE branch gained since the PR head diverged, or None (HOLD).
/// Truncation is a miss: an under-reported move fails in the merging direction.
fn base_move_paths<P: Probes>(probes: &P, cwd: &Path, pr: u64) -> Option<Vec<String>> {
    let Some((base, head)) = pr_base_head_refs(probes, cwd, pr) else {
        breadcrumb("overlap probe unavailable (pr refs unreadable); holding for a rebase");
        return None;
    };
    base_move_paths_between(probes, cwd, &base, &head)
}

/// [`base_move_paths`] for a caller that already holds the base ref and head.
pub(crate) fn base_move_paths_between<P: Probes>(
    probes: &P,
    cwd: &Path,
    base: &str,
    head: &str,
) -> Option<Vec<String>> {
    let args = vec![
        "api".to_string(),
        format!("repos/{{owner}}/{{repo}}/compare/{head}...{base}"),
        "--jq".to_string(),
        "{truncated: .truncated, names: [.files[] | ((.filename // empty), (.previous_filename // empty))]}"
            .to_string(),
    ];
    let (ok, stdout) = probes.run_gh(cwd, &args).ok()?;
    if !ok {
        breadcrumb("overlap probe unavailable (gh reverse compare failed); holding for a rebase");
        return None;
    }
    let payload: Value = serde_json::from_str(&stdout).ok()?;
    let names = payload.get("names").and_then(Value::as_array)?;
    let mut paths = Vec::with_capacity(names.len());
    for name in names {
        match name.as_str() {
            Some(path) if !path.is_empty() => paths.push(path.to_string()),
            _ => {
                breadcrumb("overlap probe unavailable (compare file list carries non-string entries); holding for a rebase");
                return None;
            }
        }
    }
    if payload
        .get("truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || paths.len() >= 300
    {
        breadcrumb(&format!(
            "overlap probe unavailable (compare truncated, {} files; caps at 300); holding for a rebase",
            paths.len()
        ));
        return None;
    }
    Some(paths)
}

/// The PR's own changed file paths, the old name of a rename included, or
/// None (HOLD). An EMPTY list is a real answer: a PR with no diff cannot
/// overlap anything.
pub(crate) fn pr_file_paths<P: Probes>(probes: &P, cwd: &Path, pr: u64) -> Option<Vec<String>> {
    let args = vec![
        "api".to_string(),
        format!("repos/{{owner}}/{{repo}}/pulls/{pr}/files"),
        "--paginate".to_string(),
        "--jq".to_string(),
        ".[] | (.filename // empty), (.previous_filename // empty)".to_string(),
    ];
    let (ok, stdout) = probes.run_gh(cwd, &args).ok()?;
    if !ok {
        breadcrumb("overlap probe unavailable (gh pr files read failed); holding for a rebase");
        return None;
    }
    Some(
        stdout
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect(),
    )
}

/// Sorted intersection of two changed-file lists, documentation paths dropped
/// from both sides first. Empty: no semantic conflict the merge could carry.
fn overlaps(base_paths: &[String], pr_paths: &[String]) -> Vec<String> {
    let base: std::collections::BTreeSet<&str> = base_paths
        .iter()
        .map(String::as_str)
        .filter(|p| !is_documentation_path(p))
        .collect();
    let pr: std::collections::BTreeSet<&str> = pr_paths
        .iter()
        .map(String::as_str)
        .filter(|p| !is_documentation_path(p))
        .collect();
    base.intersection(&pr).map(|p| (*p).to_string()).collect()
}

/// The visual-approval gate: a PR whose changed files touch a configured
/// paint path holds until an ANSWERED question page names the PR, or a
/// role-recorded decision row transcribes the user's chat approval of this
/// exact head. The user's look is the only clear, mechanizing the prose
/// rulings this gate replaces (rebrand and splash PRs merged with no user
/// look). `merge.visual_paint_paths` lists the lines the gate watches; an
/// explicit empty list disarms it, and no key at all arms footnote's own mux
/// client paths wherever that tree exists (`agents_config::visual_paint_paths`).
pub(crate) fn visual_approval_blocker<P: Probes>(
    probes: &P,
    cwd: &Path,
    pr: u64,
    head: &str,
) -> Option<Blocker> {
    let paint_paths = crate::agents_config::visual_paint_paths(cwd);
    if paint_paths.is_empty() {
        return None;
    }
    let Some(files) = pr_file_paths(probes, cwd, pr) else {
        // The files list is this gate's one instrument; an unreadable list
        // must not release a paint PR, so it reads unknown (never a verdict),
        // the same class the pin uses for an unreadable covered head.
        return Some(Blocker::unknown(
            "visual_paint_paths_unknown",
            format!(
                "PR {pr}: the changed-file list was unreadable, so the visual-approval gate cannot answer"
            ),
        ));
    };
    let touched: Vec<&str> = files
        .iter()
        .filter(|f| path_matches_paint(f, &paint_paths))
        .map(String::as_str)
        .collect();
    if touched.is_empty() {
        return None;
    }
    // Ruling: a declared main-repair node IS the red-main fix; its lane
    // already merges through red, so the look holds nothing. Fail toward
    // holding: only a positive tag on the bound node exempts.
    let entry = pr_bound_entry(cwd, pr);
    if entry
        .as_ref()
        .is_some_and(|e| node_carries_tag(e, MAIN_REPAIR_TAG))
    {
        return None;
    }
    // The page auto-files on the first hold read of this head: node, PR,
    // paint files, and the mux shots as --image, deduped per head. Best-effort:
    // a failed filing never softens the hold, it only names the remedy.
    let root = vault_root(cwd);
    let node_id = entry.as_ref().and_then(crate::graph_store::entry_id);
    if answered_question_names_pr(&root, pr, head)
        || node_id
            .as_deref()
            .is_some_and(|node| role_chat_clears_pr(probes, cwd, pr, head, node))
        || approval_carries(
            probes,
            cwd,
            &root,
            pr,
            head,
            node_id.as_deref(),
            &paint_paths,
        )
    {
        return None;
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let shots = node_id
        .as_deref()
        .map(|n| mux_shots(&root, home.as_deref(), n))
        .unwrap_or_default();
    let mut detail = format!(
        "PR {pr} touches the paint surface the config lists ({}); full CI is held at \
         this head, and the daemon cancels every run but rust-ci (the shots) until the \
         look clears, then reruns them. The user's look is the only clear: an answered question page naming the PR and its head sha, or a role-recorded \
         decision whose rationale attests the chat and whose text names the PR and \
         its head sha (`fno backlog decide <node> 'Approved: PR {pr} at {head}' \
         --authority role --rationale 'user in chat: <the user's words>'`). Ask via \
         `fno inbox outstanding ask`, then the user answers the page or the lead \
         records the chat approval. Pass each shot with --image.",
        touched.join(", ")
    );
    match (node_id.as_deref(), shots.as_slice()) {
        (Some(node), []) => detail.push_str(&format!(
            "; no shots at internal/fno/mux/{node}-*.png: capture them first, then \
             re-read status - the gate files no empty page"
        )),
        (None, _) => detail.push_str(
            "; no node is bound to this PR (by pr_number or additional_prs), so the \
             page has no shots key: bind the node, then re-read status",
        ),
        (Some(node), shots) => {
            let short = head.chars().take(8).collect::<String>();
            let mux = crate::escalation::vault_dir_with_home(&root, home.as_deref(), "mux");
            let shots: Vec<PathBuf> = shots.iter().map(|s| mux.join(s)).collect();
            detail.push_str(&file_visual_question(
                &root,
                pr,
                head,
                &short,
                node,
                &touched.join(", "),
                &shots,
            ));
        }
    }
    Some(Blocker::held("visual_approval", detail))
}

/// How many earlier commits of the PR the carry walks, newest first.
const CARRY_WALK: usize = 30;

/// The user's look carries to a head whose own paint patch is unchanged: a
/// main merge or a test-only commit moves the head but not what the user
/// saw. The newest earlier commit of this PR that the user approved is the
/// reference. The PR's paint patch against its base, at that commit and at
/// the head, must match file for file. Any paint change holds, and so does
/// every unreadable read.
fn approval_carries<P: Probes>(
    probes: &P,
    cwd: &Path,
    root: &Path,
    pr: u64,
    head: &str,
    node: Option<&str>,
    paint_paths: &[String],
) -> bool {
    let Some(commits) = gh_lines(
        probes,
        cwd,
        &[
            "api",
            &format!("repos/{{owner}}/{{repo}}/pulls/{pr}/commits"),
            "--paginate",
            "--jq",
            ".[].sha",
        ],
    ) else {
        return false;
    };
    let roles = node
        .map(|n| role_approval_texts(probes, cwd, pr, n))
        .unwrap_or_default();
    let Some(approved) = commits
        .iter()
        .rev()
        .filter(|sha| sha.as_str() != head)
        .take(CARRY_WALK)
        .find(|sha| {
            answered_question_names_pr(root, pr, sha)
                || roles.iter().any(|text| text.contains(sha.as_str()))
        })
    else {
        return false;
    };
    let Some(base) = gh_lines(
        probes,
        cwd,
        &[
            "api",
            &format!("repos/{{owner}}/{{repo}}/pulls/{pr}"),
            "--jq",
            ".base.ref",
        ],
    )
    .and_then(|l| l.into_iter().next()) else {
        return false;
    };
    match (
        paint_patch(probes, cwd, &base, approved, paint_paths),
        paint_patch(probes, cwd, &base, head, paint_paths),
    ) {
        (Some(was), Some(now)) => !now.is_empty() && was == now,
        _ => false,
    }
}

/// The PR's paint patch at one commit: each paint file the three-dot
/// compare against the base changes, with its status and its patch body.
/// Hunk headers drop out, so a main merge that only shifts line numbers
/// still matches. A file with no patch (binary or too large) keys on its
/// blob sha instead.
fn paint_patch<P: Probes>(
    probes: &P,
    cwd: &Path,
    base: &str,
    sha: &str,
    paint_paths: &[String],
) -> Option<std::collections::BTreeMap<String, String>> {
    let lines = gh_lines(
        probes,
        cwd,
        &[
            "api",
            &format!("repos/{{owner}}/{{repo}}/compare/{base}...{sha}"),
            "--jq",
            ".files[] | [.filename, .status, (.patch // \"\"), .sha]",
        ],
    )?;
    let mut out = std::collections::BTreeMap::new();
    for line in lines {
        let row: Vec<String> = serde_json::from_str(&line).ok()?;
        let [name, status, patch, blob] = row.as_slice() else {
            return None;
        };
        if !path_matches_paint(name, paint_paths) {
            continue;
        }
        let body = if patch.is_empty() {
            format!("{status} blob {blob}")
        } else {
            let hunks: Vec<&str> = patch.lines().filter(|l| !l.starts_with("@@")).collect();
            format!("{status}\n{}", hunks.join("\n"))
        };
        out.insert(name.clone(), body);
    }
    Some(out)
}

/// One `gh` read as its non-empty trimmed lines; `None` on a failed read.
fn gh_lines<P: Probes>(probes: &P, cwd: &Path, args: &[&str]) -> Option<Vec<String>> {
    let args: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
    let (ok, stdout) = probes.run_gh(cwd, &args).ok()?;
    ok.then(|| {
        stdout
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_owned)
            .collect()
    })
}

/// The repo root whose `.fno/config.toml` carries the project id: the vault
/// paths (`internal/<project>/...`) resolve wrong from a worktree, whose
/// checkout has no project id and falls to the git-remote slug.
fn vault_root(cwd: &Path) -> PathBuf {
    crate::paths::canonical_repo_root(cwd).unwrap_or_else(|| cwd.to_path_buf())
}

/// The mux captures for one node's PR: `internal/<project>/mux/<node>-*.png`
/// top level, plus files inside a `<node>-shots/` folder, as vault-relative
/// embed names. Sorted, so the embedded list is stable.
fn mux_shots(root: &Path, home: Option<&Path>, node: &str) -> Vec<String> {
    let dir = crate::escalation::vault_dir_with_home(root, home, "mux");
    let Ok(read) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut entries: Vec<_> = read.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    let mut out = Vec::new();
    for e in entries {
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with(node) && name.to_ascii_lowercase().ends_with(".png") {
            out.push(name);
        } else if name == format!("{node}-shots") && e.path().is_dir() {
            let Ok(sub) = std::fs::read_dir(e.path()) else {
                continue;
            };
            let mut sub_entries: Vec<_> = sub.flatten().collect();
            sub_entries.sort_by_key(|e| e.file_name());
            out.extend(sub_entries.iter().filter_map(|e| {
                let sub_name = e.file_name().to_string_lossy().to_string();
                sub_name
                    .to_ascii_lowercase()
                    .ends_with(".png")
                    .then(|| format!("{node}-shots/{sub_name}"))
            }));
        }
    }
    out
}

/// True when a question page (open or archived) already names this PR at this
/// head: the once-per-head dedup. An open page keeps the hold; it only stops
/// a second identical page.
fn visual_page_filed(root: &Path, pr: u64, head: &str) -> bool {
    let dir = crate::escalation::questions_dir(root);
    let dirs = [dir.clone(), dir.join("done")];
    dirs.iter().any(|d| {
        crate::lead_answers::read_question_pages(d).is_ok_and(|pages| {
            pages
                .iter()
                .any(|(_stem, text)| page_names_pr(text, pr) && text.contains(head))
        })
    })
}

/// The question-file text the gate files: title names the PR, the context
/// carries head + paint files, and `why_user: taste` is what lets a
/// reversible question reach the user at all. The shots ride `--image`, so
/// the page shows them under its Screens heading.
fn visual_question_markdown(pr: u64, head: &str, short: &str, node: &str, files: &str) -> String {
    format!(
        "---\n\
         recommend: 1\n\
         why_user: taste - whether the paint looks right is the user's call\n\
         ---\n\n\
         May PR {pr} merge as painted? ({node} at {short})\n\n\
         ## Blocked because\n\
         The visual-approval gate holds PR {pr} at head {head}: its diff touches the \
         paint surface the config lists ({files}). The user's look is the only clear.\n\n\
         ## Options\n\
         1. Merge as painted. What happens next: the answered page or a role decision \
         clears the gate and the PR merges\n\
         2. Hold for changes. What happens next: say what to change; the worker \
         reshoots and the PR re-asks at its new head\n\n\
         ## Why these options\n\
         Option 1 ships exactly the paint in the shots under Screens. Option 2 keeps the \
         surface unchanged until you name the change.\n\n\
         ## Recommendation\n\
         1, because the shots show this head's own paint; the diff is already \
         review-green and CI-green.\n\n\
         ## Not thought through\n\
         Whether the shots cover every surface PR {pr} paints: they are the \
         mux captures named internal/fno/mux/{node}-*.png.\n\n\
         ## Reversible\n\
         yes\n\n\
         ## Meanwhile\n\
         The PR stays held and green; this page files once per head.\n"
    )
}

/// File the question through the ask command (journal + index now, the page
/// itself materialized by the attention arm's next beat). Best-effort: the
/// returned line appends to the hold detail, never softens the hold. The
/// intake's dedup (same subject + node already open) refuses before a second
/// page is written; a re-ask at a new head closes the older head's question
/// at the intake, so one paint question stays open per PR. Any other
/// non-zero exit is a real failure, surfaced with the ask's own stderr so a
/// refused page is never mislabeled as filed.
fn file_visual_question(
    root: &Path,
    pr: u64,
    head: &str,
    short: &str,
    node: &str,
    files: &str,
    shots: &[PathBuf],
) -> String {
    if visual_page_filed(root, pr, head) {
        return format!("; the question page for PR {pr} at {short} is already filed");
    }
    let path = std::env::temp_dir().join(format!(
        "fno-visual-ask-{pr}-{short}-{}.md",
        std::process::id()
    ));
    let markdown = visual_question_markdown(pr, head, short, node, files);
    if let Err(e) = std::fs::write(&path, markdown) {
        return format!("; the question could not be staged: {e}");
    }
    let subject = format!("visual approval PR {pr} at {short}");
    let out = Command::new(crate::scrape::fno_bin())
        .current_dir(root)
        .args(["inbox", "outstanding", "ask", "--question-file"])
        .arg(&path)
        .args(["--node", node])
        .args(["--subject", &subject])
        .args(
            shots
                .iter()
                .flat_map(|s| [std::ffi::OsStr::new("--image"), s.as_os_str()]),
        )
        .output();
    match out {
        Ok(o) if o.status.success() => {
            format!("; question page filed (subject: {subject})")
        }
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            let last = stderr.trim().lines().last().unwrap_or("");
            let note = if last.is_empty() {
                String::new()
            } else {
                format!(": {last}")
            };
            format!(
                "; the question page could not be filed (ask exited {:?}{note}); file it by \
                 hand: fno inbox outstanding ask --question-file {} --node {node} --image <each shot>",
                o.status.code(),
                path.display()
            )
        }
        Err(e) => format!("; the question page could not be filed: {e}"),
    }
}

/// Does one changed file match the paint-path list? Two pattern shapes: a
/// `dir/**` subtree and a bare file name (any directory). `ponytail:` no glob
/// engine; a mid-pattern `**` or `*` wildcard is added when a config needs it.
pub(crate) fn path_matches_paint(path: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|p| {
        let p = p.trim();
        if let Some(prefix) = p.strip_suffix("/**") {
            let prefix = prefix.trim_end_matches('/');
            return path.starts_with(prefix)
                && (path.len() == prefix.len() || path[prefix.len()..].starts_with('/'));
        }
        if !p.contains('/') {
            return path.rsplit('/').next() == Some(p);
        }
        path == p
    })
}

/// True when an ANSWERED question page in this project's questions directory
/// names the PR at this exact head and its answer picks the approve option:
/// the pages the lead check-in reads, parsed the same way. "Hold for
/// changes" or "none of these" is an answer too, and it must keep the hold.
/// Head-scoped like the role row: a push after the user looked holds again.
fn answered_question_names_pr(root: &Path, pr: u64, head: &str) -> bool {
    if head.is_empty() {
        return false;
    }
    let dir = crate::escalation::questions_dir(root);
    // Answered pages are archived into done/ after the fact; an approval
    // must not lapse because its page moved there.
    let mut dirs = vec![dir.clone()];
    dirs.push(dir.join("done"));
    dirs.iter().any(|d| {
        crate::lead_answers::read_question_pages(d).is_ok_and(|pages| {
            pages.iter().any(|(_stem, text)| {
                crate::attention_file::parse_page(text)
                    .map(|(front, _)| {
                        front.status == "answered"
                            && front.answer.as_deref().is_some_and(answer_approves)
                    })
                    .unwrap_or(false)
                    && page_names_pr(text, pr)
                    && text.contains(head)
            })
        })
    })
}

/// Does a recorded answer pick the approve option? A numbered answer
/// approves only as option 1 (the filed page's "Merge as painted"). A bare
/// answer approves when it opens with an approve word. Anything else holds.
fn answer_approves(answer: &str) -> bool {
    let a = answer.trim().to_ascii_lowercase();
    let digits: String = a.chars().take_while(char::is_ascii_digit).collect();
    if !digits.is_empty() {
        return digits == "1";
    }
    ["merge", "approve", "yes"].iter().any(|w| a.starts_with(w))
}

/// The page text names the PR: a word-start `pr` (any case), an optional
/// `#`/`-`/`/`/`:`/space separator run, then the PR number, not followed by
/// another digit. Prose like "the PR is 3058" or "PRs 3058" never matches.
pub(crate) fn page_names_pr(text: &str, pr: u64) -> bool {
    let needle = pr.to_string();
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i + 2 <= bytes.len() {
        let is_pr =
            bytes[i].to_ascii_lowercase() == b'p' && bytes[i + 1].to_ascii_lowercase() == b'r';
        if !is_pr {
            i += 1;
            continue;
        }
        let word_start = i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_');
        i += 1;
        if !word_start {
            continue;
        }
        let mut j = i + 1; // first byte after "pr"
                           // One separator run, spaces and symbols in any order, so "PR #3036"
                           // and "pr - 3036" read like "PR 3036".
        while matches!(
            bytes.get(j),
            Some(b'#') | Some(b'-') | Some(b'/') | Some(b':') | Some(b' ')
        ) {
            j += 1;
        }
        let rest = &text[j.min(text.len())..];
        if rest.starts_with(&needle)
            && !rest[needle.len()..]
                .chars()
                .next()
                .map(|c| c.is_ascii_digit())
                .unwrap_or(false)
        {
            return true;
        }
    }
    false
}

/// The row text the PR/head match reads: decision plus rationale, the two
/// free-text fields a recording carries.
fn row_text(row: &Value) -> String {
    format!(
        "{}\n{}",
        row.get("decision").and_then(Value::as_str).unwrap_or(""),
        row.get("rationale").and_then(Value::as_str).unwrap_or("")
    )
}

/// The rationale attests the approval came from the user's own chat words.
/// `superuser in chat` contains `user in chat`, so one casefold covers both
/// spellings the leads already record.
fn row_is_chat_attested(row: &Value) -> bool {
    row.get("rationale")
        .and_then(Value::as_str)
        .is_some_and(|r| r.to_lowercase().contains("user in chat"))
}

/// True when a live role decision row transcribes the user's chat approval
/// of this PR at this head: authority `role`, the PR's bound node as subject
/// (the decide door lets only the lead whose territory covers that node mint
/// it), a chat-attested rationale, and text naming both the PR and the exact
/// head sha the user looked at. Head-scoped like the operator head grant: a
/// push invalidates the approval. An unreadable index never clears.
pub(crate) fn role_chat_clears_pr<P: Probes>(
    probes: &P,
    cwd: &Path,
    pr: u64,
    head: &str,
    node: &str,
) -> bool {
    !head.is_empty()
        && role_approval_texts(probes, cwd, pr, node)
            .iter()
            .any(|text| text.contains(head))
}

/// The text of every live role row on this node that approves this PR, at
/// any head: one decisions read, so a caller can test many heads with it.
fn role_approval_texts<P: Probes>(probes: &P, cwd: &Path, pr: u64, node: &str) -> Vec<String> {
    let args: Vec<String> = [
        "backlog",
        "decisions",
        "--lane",
        "unattributed",
        "--state",
        "all",
        "--limit",
        "200",
        "--json",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    let Some((Some(0), out, _)) = probes.fno_shell(cwd, &args).ok() else {
        return Vec::new();
    };
    let Ok(payload) = serde_json::from_slice::<Value>(&out) else {
        return Vec::new();
    };
    payload
        .get("decisions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|row| {
            row.get("authority_source").and_then(Value::as_str) == Some("role")
                && row.get("subject").and_then(Value::as_str) == Some(node)
                && matches!(
                    row.get("lifecycle").and_then(Value::as_str),
                    Some("live") | Some("unscoped")
                )
                && row_is_chat_attested(row)
        })
        .map(row_text)
        .filter(|text| page_names_pr(text, pr))
        .collect()
}

/// The `owner/name` pair a remote url names, normalized across the ssh
/// (`git@host:owner/repo.git`), https (`https://host/owner/repo.git`), and
/// trailing-slash spellings, so the ledger scoping test reads the same
/// repository whatever form the checkout's origin takes.
pub(crate) fn repo_slug_from_origin(url: &str) -> Option<String> {
    let trimmed = url.trim().trim_end_matches('/');
    let trimmed = trimmed.strip_suffix(".git").unwrap_or(trimmed);
    let mut parts: Vec<&str> = trimmed
        .split(['/', ':'])
        .filter(|p| !p.is_empty())
        .collect();
    if parts.len() < 2 {
        return None;
    }
    let name = parts.pop()?;
    let owner = parts.pop()?;
    Some(format!("{owner}/{name}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn mux_shots_scans_node_prefix_and_shots_folder() {
        let tmp = std::env::temp_dir().join(format!("mg-shots-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        // The space fallback resolves from the state root; under test that
        // must be declared, or the hermetic guard refuses.
        let lock = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let spaces = tmp.join("spaces");
        std::fs::create_dir_all(&spaces).unwrap();
        let saved = std::env::var("FNO_SPACES_DIR").ok();
        std::env::set_var("FNO_SPACES_DIR", &spaces);
        let mux = crate::escalation::vault_dir_with_home(&tmp, None, "mux");
        std::fs::create_dir_all(mux.join("x-aaaa-shots")).unwrap();
        std::fs::write(mux.join("x-aaaa-bell-dark.png"), "x").unwrap();
        std::fs::write(mux.join("x-aaaa-bell-light.png"), "x").unwrap();
        std::fs::write(mux.join("x-other-dark.png"), "x").unwrap();
        std::fs::write(mux.join("x-aaaa-shots/one-after.png"), "x").unwrap();
        std::fs::write(mux.join("x-aaaa-shots/notes.txt"), "png-noted.txt").unwrap();
        let shots = mux_shots(&tmp, None, "x-aaaa");
        assert_eq!(
            shots,
            vec![
                "x-aaaa-bell-dark.png".to_string(),
                "x-aaaa-bell-light.png".to_string(),
                "x-aaaa-shots/one-after.png".to_string(),
            ]
        );
        match saved {
            Some(v) => std::env::set_var("FNO_SPACES_DIR", v),
            None => std::env::remove_var("FNO_SPACES_DIR"),
        }
        drop(lock);
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn an_unreconciled_manifest_of_a_contract_node_holds() {
        // AC6's gate half: mocks would ship; the merge holds by code.
        let root = std::env::temp_dir().join(format!("x53c5-stub-{}", std::process::id()));
        std::fs::create_dir_all(root.join(".fno")).unwrap();
        let node = "x-test";
        std::fs::write(
            root.join(".fno").join(format!("stub-manifest-{node}.json")),
            r#"{"reconciled": false, "stubs": [{"stub_id": "a"}]}"#,
        )
        .unwrap();
        let blocker = stub_manifest_blocker(&root, node).expect("held");
        assert_eq!(blocker.code, "stub_manifest_unreconciled");
        assert!(blocker.detail.contains("1 stub(s)"), "{}", blocker.detail);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_reconciled_manifest_clears_and_a_missing_one_never_holds() {
        let root = std::env::temp_dir().join(format!("x53c5-stub2-{}", std::process::id()));
        std::fs::create_dir_all(root.join(".fno")).unwrap();
        let node = "x-test";
        std::fs::write(
            root.join(".fno").join(format!("stub-manifest-{node}.json")),
            r#"{"reconciled": true, "stubs": []}"#,
        )
        .unwrap();
        assert!(stub_manifest_blocker(&root, node).is_none());
        // No manifest carried: nothing to hold against.
        assert!(stub_manifest_blocker(&root, "x-never-wrote").is_none());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn contract_node_reads_the_persisted_pr_fields() {
        // The store persists `pr_number` + `additional_prs`; a dependent
        // recorded only in the additional list must still be found.
        let entries = vec![json!({
            "id": "x-3333",
            "dep": "contract",
            "pr_number": 7,
            "additional_prs": [
                {"number": 42},
                {"url": "https://github.com/o/r/pull/99"},
                105,
            ],
        })];
        for pr in [7u64, 42, 99, 105] {
            assert_eq!(
                contract_node_for_pr(&entries, pr).as_deref(),
                Some("x-3333"),
                "pr {pr} must match"
            );
        }
        assert_eq!(contract_node_for_pr(&entries, 8), None);
    }

    #[test]
    fn a_page_names_the_pr_only_at_a_word_start_with_the_number() {
        assert!(page_names_pr("May PR 3036 merge?", 3036));
        assert!(page_names_pr("pr-3036 ships the splash", 3036));
        assert!(page_names_pr("(PR #3036)", 3036));
        assert!(!page_names_pr("PRs 3036 land", 3036));
        assert!(!page_names_pr("the PR is 3036", 3036));
        assert!(!page_names_pr("PR 30365", 3036));
        assert!(!page_names_pr("plain 3036", 3036));
    }
}
